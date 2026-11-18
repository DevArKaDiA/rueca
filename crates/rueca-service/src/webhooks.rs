//! Webhook hints (RFC 0008 §7.3, §7.10): what a code host's delivery
//! names, and how it reaches a job's desired state.
//!
//! A delivery is a **hint**, never the truth: [`parse`] reads only the
//! repository and the refs it names (GitHub `push`, `create`, `delete`,
//! `pull_request`; GitLab Push, Tag Push and Merge Request hooks; Bitbucket
//! Cloud `repo:push` and `pullrequest:*`), and [`apply_hint`] asks the host
//! with `git ls-remote` what those refs point to now, then merges the
//! answer into the job by compare-and-swap ([`desired_update`]). Commits in
//! a payload are never indexed as such, so a duplicated, reordered or
//! stale delivery converges on the host's current state, and a payload a
//! host truncated (GitHub: no event past 3 tags or 5,000 branches at once;
//! GitLab: none past 3 branches or tags in one push; Bitbucket: `truncated`
//! commit lists) costs only latency: adaptive polling (§7.4) catches up.
//!
//! What a hint changes ([`plan`]):
//!
//! | Hint | Desired state | Catalog |
//! |---|---|---|
//! | branch pushed, created | the default branch, or a branch matching `branches`, or a branch already desired (an on-demand ref) → its listed commit | — |
//! | branch deleted (not listed any more) | removed (never the default) | removed |
//! | tag pushed, created | a tag matching `tags`, or already desired → its listed (peeled) commit | — |
//! | tag deleted | nothing: tags never expire (§7.10) | — |
//! | pull request opened, updated, reopened | `pr/<n>` (only with `prs = true`) → `refs/pull/<n>/head`, else `refs/merge-requests/<n>/head`, else the source branch's tip (Bitbucket Cloud lists no pull request refs) | `closed_at` cleared |
//! | pull request closed, merged, declined | `pr/<n>` removed | `closed_at = now`: the ref stays served until `pr_after_close` (7 d), when garbage collection expires it |
//!
//! A ref the hint names that the configuration does not track is skipped.
//! A pull request from a fork on Bitbucket (its source branch is in
//! another repository) cannot be confirmed and is skipped until polling or
//! an admin request names it.

use std::collections::BTreeSet;

use anyhow::Context;
use serde::Serialize;
use serde_json::Value;

use crate::catalog::RefKind;
use crate::jobs::{DesiredRef, Job, desired_update, get_job};
use crate::reconcile::{RepoSpec, catalog_update};
use crate::remote::{RemoteLister, RemoteRefs, repo_id_of};
use rueca_storage::Storage;

/// A code host that sends webhooks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WebhookHost {
    GitHub,
    GitLab,
    Bitbucket,
}

impl WebhookHost {
    pub const ALL: [WebhookHost; 3] = [WebhookHost::GitHub, WebhookHost::GitLab, WebhookHost::Bitbucket];

    /// `github`, `gitlab`, `bitbucket`: the route (`/webhooks/<name>`) and
    /// the `[indexing].webhooks` key.
    pub fn as_str(self) -> &'static str {
        match self {
            WebhookHost::GitHub => "github",
            WebhookHost::GitLab => "gitlab",
            WebhookHost::Bitbucket => "bitbucket",
        }
    }

    pub fn parse(name: &str) -> Option<WebhookHost> {
        WebhookHost::ALL.into_iter().find(|h| h.as_str() == name)
    }
}

/// One ref a delivery names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "change", rename_all = "snake_case")]
pub enum RefHint {
    /// A branch or tag pushed, created or deleted (full ref name); whether
    /// the payload says it was deleted is informative only: the listing
    /// decides.
    Ref { full: String, deleted: bool },
    /// A pull request opened, updated or reopened.
    PrHead {
        number: u64,
        /// Its source branch, when it lives in the same repository.
        source_branch: Option<String>,
    },
    /// A pull request closed, merged or declined.
    PrClosed { number: u64 },
}

/// What a delivery names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Hint {
    pub host: WebhookHost,
    /// The host's event name (`push`, `pull_request`, `Merge Request Hook`,
    /// `repo:push`, …).
    pub event: String,
    /// URLs naming the repository (clone, SSH and web URLs), matched
    /// against the configured repositories by repository id.
    pub urls: Vec<String>,
    pub refs: Vec<RefHint>,
}

/// A parsed delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed {
    Hint(Hint),
    /// An event that changes nothing indexed (`ping`, an approval, …).
    Ignored(String),
}

/// Parses a delivery of `host` with event header `event` (GitHub
/// `X-GitHub-Event`, GitLab `X-Gitlab-Event`, Bitbucket `X-Event-Key`).
/// `Err`: the body is not the JSON the event documents.
pub fn parse(host: WebhookHost, event: &str, body: &[u8]) -> anyhow::Result<Parsed> {
    let v: Value = serde_json::from_slice(body).context("the body is not JSON")?;
    anyhow::ensure!(v.is_object(), "the body is not a JSON object");
    let refs = match host {
        WebhookHost::GitHub => github_refs(event, &v)?,
        WebhookHost::GitLab => gitlab_refs(event, &v)?,
        WebhookHost::Bitbucket => bitbucket_refs(event, &v)?,
    };
    let Some(refs) = refs else {
        return Ok(Parsed::Ignored(format!("event `{event}` changes nothing indexed")));
    };
    let urls = repository_urls(host, &v);
    anyhow::ensure!(!urls.is_empty(), "the payload names no repository");
    Ok(Parsed::Hint(Hint { host, event: event.to_string(), urls, refs }))
}

fn str_at<'a>(v: &'a Value, pointer: &str) -> Option<&'a str> {
    v.pointer(pointer).and_then(Value::as_str).filter(|s| !s.is_empty())
}

fn u64_at(v: &Value, pointer: &str) -> Option<u64> {
    v.pointer(pointer).and_then(Value::as_u64)
}

fn is_zero_commit(c: Option<&str>) -> bool {
    c.is_some_and(|c| !c.is_empty() && c.bytes().all(|b| b == b'0'))
}

/// GitHub: `push`, `create`, `delete`, `pull_request`.
fn github_refs(event: &str, v: &Value) -> anyhow::Result<Option<Vec<RefHint>>> {
    Ok(match event {
        "push" => {
            let full = str_at(v, "/ref").context("push: no `ref`")?.to_string();
            let deleted = v.get("deleted").and_then(Value::as_bool).unwrap_or(false);
            Some(vec![RefHint::Ref { full, deleted }])
        }
        "create" | "delete" => {
            let name = str_at(v, "/ref").context("create/delete: no `ref`")?;
            let full = match str_at(v, "/ref_type") {
                Some("tag") => format!("refs/tags/{name}"),
                Some("branch") => format!("refs/heads/{name}"),
                _ => return Ok(None),
            };
            Some(vec![RefHint::Ref { full, deleted: event == "delete" }])
        }
        "pull_request" => {
            let number = u64_at(v, "/number")
                .or_else(|| u64_at(v, "/pull_request/number"))
                .context("pull_request: no `number`")?;
            pr_hint(str_at(v, "/action").unwrap_or(""), number, None).map(|h| vec![h])
        }
        _ => None,
    })
}

/// The hint of a pull request action (GitHub's and GitLab's verbs).
fn pr_hint(action: &str, number: u64, source_branch: Option<String>) -> Option<RefHint> {
    match action {
        "opened" | "open" | "synchronize" | "reopened" | "reopen" | "update" | "created" | "updated" => {
            Some(RefHint::PrHead { number, source_branch })
        }
        "closed" | "close" | "merge" | "merged" | "fulfilled" | "rejected" | "declined" => {
            Some(RefHint::PrClosed { number })
        }
        _ => None,
    }
}

/// GitLab: Push Hook, Tag Push Hook, Merge Request Hook (by `object_kind`,
/// which agrees with the `X-Gitlab-Event` header).
fn gitlab_refs(event: &str, v: &Value) -> anyhow::Result<Option<Vec<RefHint>>> {
    let kind = str_at(v, "/object_kind").unwrap_or(match event {
        "Push Hook" => "push",
        "Tag Push Hook" => "tag_push",
        "Merge Request Hook" => "merge_request",
        _ => "",
    });
    Ok(match kind {
        "push" | "tag_push" => {
            let full = str_at(v, "/ref").context("push: no `ref`")?.to_string();
            let deleted = is_zero_commit(str_at(v, "/after"));
            Some(vec![RefHint::Ref { full, deleted }])
        }
        "merge_request" => {
            let number = u64_at(v, "/object_attributes/iid").context("merge_request: no `object_attributes.iid`")?;
            let same =
                u64_at(v, "/object_attributes/source_project_id") == u64_at(v, "/object_attributes/target_project_id");
            let branch = str_at(v, "/object_attributes/source_branch").filter(|_| same).map(str::to_string);
            pr_hint(str_at(v, "/object_attributes/action").unwrap_or(""), number, branch).map(|h| vec![h])
        }
        _ => None,
    })
}

/// Bitbucket Cloud: `repo:push`, `pullrequest:created|updated|fulfilled|rejected`.
fn bitbucket_refs(event: &str, v: &Value) -> anyhow::Result<Option<Vec<RefHint>>> {
    if event == "repo:push" {
        let changes = v.pointer("/push/changes").and_then(Value::as_array).context("repo:push: no `push.changes`")?;
        return Ok(Some(changes.iter().filter_map(bitbucket_change).collect()));
    }
    let Some(action) = event.strip_prefix("pullrequest:") else { return Ok(None) };
    let number = u64_at(v, "/pullrequest/id").context("pullrequest: no `pullrequest.id`")?;
    let same = str_at(v, "/pullrequest/source/repository/full_name") == str_at(v, "/repository/full_name");
    let branch = str_at(v, "/pullrequest/source/branch/name").filter(|_| same).map(str::to_string);
    Ok(pr_hint(action, number, branch).map(|h| vec![h]))
}

/// One `push.changes[]` entry: `new` (null when deleted) or `old`.
fn bitbucket_change(c: &Value) -> Option<RefHint> {
    let side = c.get("new").filter(|n| !n.is_null()).or_else(|| c.get("old"))?;
    let name = str_at(side, "/name")?;
    let full = match str_at(side, "/type")? {
        "tag" => format!("refs/tags/{name}"),
        "branch" | "named_branch" => format!("refs/heads/{name}"),
        _ => return None,
    };
    Some(RefHint::Ref { full, deleted: c.get("new").is_none_or(Value::is_null) })
}

/// Every URL of the payload that names the repository.
fn repository_urls(host: WebhookHost, v: &Value) -> Vec<String> {
    let pointers: &[&str] = match host {
        WebhookHost::GitHub => &["/repository/clone_url", "/repository/ssh_url", "/repository/html_url"],
        WebhookHost::GitLab => {
            &["/project/git_http_url", "/project/git_ssh_url", "/project/web_url", "/repository/git_http_url"]
        }
        WebhookHost::Bitbucket => &["/repository/links/html/href"],
    };
    let mut out: Vec<String> = pointers.iter().filter_map(|p| str_at(v, p)).map(str::to_string).collect();
    if host == WebhookHost::Bitbucket
        && let Some(name) = str_at(v, "/repository/full_name")
    {
        out.push(format!("https://bitbucket.org/{name}"));
    }
    out
}

/// The configured repository a hint names (by repository id), if any.
pub fn match_repo<'a>(specs: &'a [RepoSpec], hint: &Hint) -> Option<&'a RepoSpec> {
    let ids: BTreeSet<String> = hint.urls.iter().filter_map(|u| repo_id_of(u).ok()).collect();
    specs.iter().find(|s| ids.contains(&s.repo))
}

/// The `ls-remote` patterns that confirm a hint's refs (`HEAD` included,
/// for the default branch).
pub fn patterns(hint: &Hint) -> Vec<String> {
    let mut out = vec!["HEAD".to_string()];
    for r in &hint.refs {
        match r {
            RefHint::Ref { full, .. } => out.push(full.clone()),
            RefHint::PrHead { number, source_branch } => {
                out.push(format!("refs/pull/{number}/head"));
                out.push(format!("refs/merge-requests/{number}/head"));
                out.extend(source_branch.iter().map(|b| format!("refs/heads/{b}")));
            }
            RefHint::PrClosed { .. } => {}
        }
    }
    out.dedup();
    out
}

/// One change to a job and its catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Change {
    /// Desire `name` at `commit` (and clear a pull request's `closed_at`).
    Set {
        #[serde(rename = "ref")]
        name: String,
        kind: RefKind,
        commit: String,
    },
    /// A branch the host no longer lists: out of the desired state and the
    /// catalog.
    Remove {
        #[serde(rename = "ref")]
        name: String,
    },
    /// A closed pull request: out of the desired state, `closed_at` set.
    Close {
        #[serde(rename = "ref")]
        name: String,
    },
}

/// What [`plan`] decided against the listing `remote`, plus the refs it
/// skipped (untracked, unconfirmed) with why.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Plan {
    pub changes: Vec<Change>,
    pub skipped: Vec<String>,
}

/// The changes `hint` asks of the job `job` (its current desired state)
/// of repository `spec`, given what the host lists now (module docs).
pub fn plan(spec: &RepoSpec, job: Option<&Job>, remote: &RemoteRefs, hint: &Hint) -> Plan {
    let default = spec.default_branch.clone().or_else(|| remote.head.clone()).or_else(|| {
        job.and_then(|j| j.desired.iter().find(|(_, d)| d.kind == RefKind::Default)).map(|(n, _)| n.clone())
    });
    let ctx = Ctx { spec, job, remote, default: default.as_deref() };
    let mut out = Plan::default();
    for r in &hint.refs {
        match r {
            RefHint::Ref { full, .. } => ctx.plan_ref(full, &mut out),
            RefHint::PrHead { number, source_branch } => ctx.plan_pr(*number, source_branch.as_deref(), &mut out),
            RefHint::PrClosed { number } => out.changes.push(Change::Close { name: format!("pr/{number}") }),
        }
    }
    out
}

struct Ctx<'a> {
    spec: &'a RepoSpec,
    job: Option<&'a Job>,
    remote: &'a RemoteRefs,
    default: Option<&'a str>,
}

impl Ctx<'_> {
    /// The kind the job already desires `name` as.
    fn desired_kind(&self, name: &str) -> Option<RefKind> {
        self.job.and_then(|j| j.desired.get(name)).map(|d| d.kind)
    }

    fn plan_ref(&self, full: &str, out: &mut Plan) {
        if let Some(branch) = full.strip_prefix("refs/heads/") {
            self.plan_branch(full, branch, out);
        } else if let Some(tag) = full.strip_prefix("refs/tags/") {
            let tracked = self.spec.tag_matches(tag) || self.desired_kind(tag) == Some(RefKind::Tag);
            match self.remote.refs.get(full) {
                Some(commit) if tracked => {
                    out.changes.push(Change::Set { name: tag.to_string(), kind: RefKind::Tag, commit: commit.clone() })
                }
                Some(_) => out.skipped.push(format!("{tag}: tag not in `tags`")),
                None => out.skipped.push(format!("{tag}: tag not listed (tags never expire)")),
            }
        } else {
            out.skipped.push(format!("{full}: not a branch or tag"));
        }
    }

    fn plan_branch(&self, full: &str, branch: &str, out: &mut Plan) {
        let kind = if self.default == Some(branch) {
            Some(RefKind::Default)
        } else if self.spec.branch_matches(branch) {
            Some(RefKind::Branch)
        } else {
            self.desired_kind(branch)
        };
        let Some(kind) = kind else {
            out.skipped.push(format!("{branch}: branch not in `branches`"));
            return;
        };
        match self.remote.refs.get(full) {
            Some(commit) => out.changes.push(Change::Set { name: branch.to_string(), kind, commit: commit.clone() }),
            None if kind == RefKind::Default => out.skipped.push(format!("{branch}: the default branch is not listed")),
            None => out.changes.push(Change::Remove { name: branch.to_string() }),
        }
    }

    fn plan_pr(&self, number: u64, source_branch: Option<&str>, out: &mut Plan) {
        let name = format!("pr/{number}");
        if !self.spec.prs {
            out.skipped.push(format!("{name}: `prs` is off for this repository"));
            return;
        }
        let refs = &self.remote.refs;
        let listed = refs
            .get(&format!("refs/pull/{number}/head"))
            .or_else(|| refs.get(&format!("refs/merge-requests/{number}/head")))
            .or_else(|| source_branch.and_then(|b| refs.get(&format!("refs/heads/{b}"))));
        match listed {
            Some(commit) => out.changes.push(Change::Set { name, kind: RefKind::Pr, commit: commit.clone() }),
            None => out.skipped.push(format!("{name}: no head listed (a fork's branch, or already gone)")),
        }
    }
}

/// What [`apply_hint`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Applied {
    pub repo: String,
    pub plan: Plan,
    /// The job's desired state changed.
    pub changed: bool,
}

/// Confirms `hint` against the host (`git ls-remote` through `lister`,
/// unless the hint only closes pull requests) and merges the [`plan`] into
/// the job of `spec` and its catalog at `now`.
pub fn apply_hint(
    storage: &dyn Storage,
    spec: &RepoSpec,
    lister: &dyn RemoteLister,
    hint: &Hint,
    now: u64,
) -> anyhow::Result<Applied> {
    let needs_listing = hint.refs.iter().any(|r| !matches!(r, RefHint::PrClosed { .. }));
    let remote = if needs_listing {
        let patterns = patterns(hint);
        let p: Vec<&str> = patterns.iter().map(String::as_str).collect();
        lister.ls_remote(&spec.url, &p).map_err(anyhow::Error::from).context("confirming the hint")?
    } else {
        RemoteRefs::default()
    };
    let job = get_job(storage, &spec.repo)?;
    let plan = plan(spec, job.as_ref(), &remote, hint);
    let out = desired_update(storage, &spec.repo, Some(&spec.url), now, |job| merge(job, &plan.changes, now))?;
    apply_to_catalog(storage, &spec.repo, &plan.changes, now)?;
    Ok(Applied { repo: spec.repo.clone(), changed: out.written, plan })
}

/// Applies `changes` to `job`; whether anything changed. A ref expired at
/// the same commit stays out (as with polling).
pub fn merge(job: &mut Job, changes: &[Change], now: u64) -> bool {
    let mut changed = false;
    for c in changes {
        changed |= match c {
            Change::Set { name, commit, .. } if job.expired.get(name) == Some(commit) => false,
            Change::Set { name, kind, commit } => {
                job.set_desired(name, DesiredRef { commit: commit.clone(), kind: *kind, seen_at: now })
            }
            Change::Remove { name } | Change::Close { name } => job.remove_desired(name),
        };
    }
    changed
}

/// Removed branches leave the catalog, closed pull requests get
/// `closed_at`, reopened ones lose it.
fn apply_to_catalog(storage: &dyn Storage, repo: &str, changes: &[Change], now: u64) -> anyhow::Result<()> {
    let touches = changes.iter().any(|c| match c {
        Change::Set { kind, .. } => *kind == RefKind::Pr,
        Change::Remove { .. } | Change::Close { .. } => true,
    });
    if !touches {
        return Ok(());
    }
    catalog_update(storage, repo, |c| {
        changes.iter().fold(false, |acc, change| {
            acc | match change {
                Change::Remove { name } => c.remove_ref(name, now),
                Change::Close { name } => c.close_pr(name, now),
                Change::Set { name, kind: RefKind::Pr, .. } => c.reopen_pr(name),
                Change::Set { .. } => false,
            }
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(prs: bool) -> RepoSpec {
        RepoSpec::new("https://github.com/acme/billing.git", None, &["release/*".into()], &["v*".into()], prs).unwrap()
    }

    fn remote(refs: &[(&str, &str)]) -> RemoteRefs {
        RemoteRefs {
            head: Some("main".into()),
            refs: refs.iter().map(|(r, c)| (r.to_string(), c.to_string())).collect(),
        }
    }

    fn hint(refs: Vec<RefHint>) -> Hint {
        Hint {
            host: WebhookHost::GitHub,
            event: "push".into(),
            urls: vec!["https://github.com/acme/billing.git".into()],
            refs,
        }
    }

    fn push(full: &str) -> RefHint {
        RefHint::Ref { full: full.into(), deleted: false }
    }

    #[test]
    fn branches_follow_the_globs_and_the_listing() {
        let r = remote(&[("refs/heads/main", "a"), ("refs/heads/release/2", "b"), ("refs/heads/feature", "c")]);
        let h = hint(vec![
            push("refs/heads/main"),
            push("refs/heads/release/2"),
            push("refs/heads/feature"),
            push("refs/heads/release/gone"),
        ]);
        let p = plan(&spec(false), None, &r, &h);
        assert_eq!(
            p.changes,
            [
                Change::Set { name: "main".into(), kind: RefKind::Default, commit: "a".into() },
                Change::Set { name: "release/2".into(), kind: RefKind::Branch, commit: "b".into() },
                Change::Remove { name: "release/gone".into() },
            ]
        );
        assert_eq!(p.skipped, ["feature: branch not in `branches`"]);
    }

    #[test]
    fn tags_and_pull_requests() {
        let r = remote(&[("refs/tags/v2", "t"), ("refs/tags/nightly", "n"), ("refs/pull/7/head", "p")]);
        let h = hint(vec![
            push("refs/tags/v2"),
            push("refs/tags/nightly"),
            push("refs/tags/v3"),
            RefHint::PrHead { number: 7, source_branch: None },
            RefHint::PrClosed { number: 8 },
        ]);
        let p = plan(&spec(true), None, &r, &h);
        assert_eq!(
            p.changes,
            [
                Change::Set { name: "v2".into(), kind: RefKind::Tag, commit: "t".into() },
                Change::Set { name: "pr/7".into(), kind: RefKind::Pr, commit: "p".into() },
                Change::Close { name: "pr/8".into() },
            ]
        );
        assert_eq!(p.skipped.len(), 2, "{:?}", p.skipped);
        let off = plan(&spec(false), None, &r, &hint(vec![RefHint::PrHead { number: 7, source_branch: None }]));
        assert!(off.changes.is_empty() && off.skipped[0].contains("`prs` is off"));
    }

    #[test]
    fn a_pull_request_falls_back_to_its_source_branch() {
        let r = remote(&[("refs/heads/feature", "f")]);
        let h = hint(vec![RefHint::PrHead { number: 3, source_branch: Some("feature".into()) }]);
        assert_eq!(patterns(&h), ["HEAD", "refs/pull/3/head", "refs/merge-requests/3/head", "refs/heads/feature"]);
        let p = plan(&spec(true), None, &r, &h);
        assert_eq!(p.changes, [Change::Set { name: "pr/3".into(), kind: RefKind::Pr, commit: "f".into() }]);
    }

    #[test]
    fn merge_respects_expiry_tombstones() {
        let mut job = Job::new(crate::jobs::JobKind::Index, "r", None, 1);
        job.expired.insert("pr/1".into(), "old".into());
        let set = |c: &str| Change::Set { name: "pr/1".into(), kind: RefKind::Pr, commit: c.into() };
        assert!(!merge(&mut job, &[set("old")], 5), "the expired commit stays out");
        assert!(merge(&mut job, &[set("new")], 5));
        assert!(merge(&mut job, &[Change::Close { name: "pr/1".into() }], 6));
        assert!(job.desired.is_empty());
    }
}
