//! The reconciler (RFC 0008 §7.1, §7.2, §7.4, §7.10): from the configured
//! inventory and adaptive `git ls-remote` polling to the desired state of
//! each job object.
//!
//! Each [`Reconciler::tick`] (run by the updater that holds the lease):
//!
//! 1. **Inventory**: every configured repository ([`RepoSpec`], from
//!    `[[repo]]`) gets a catalog (`repo`, `url`; created empty when absent)
//!    and a job object, once per process.
//! 2. **Polling**: every repository whose `poll.next_at` has passed is
//!    listed with `git ls-remote --symref <url> HEAD 'refs/heads/*'
//!    'refs/tags/*'` (plus `refs/pull/*/head` and
//!    `refs/merge-requests/*/head` when the repository has `prs = true` and
//!    the policy `pr_heads`), at most [`PollPolicy::per_host`] at once per
//!    host; a host that answers `429` is paused for
//!    [`crate::remote::RATE_LIMIT_PAUSE`].
//! 3. **Desired state**: the default branch (`default_branch`, else the
//!    remote's `HEAD`), the branches matching `branches`, the tags matching
//!    `tags`, the pull request heads polled, merged into the job by CAS
//!    ([`merge_polled`]); refs from webhooks and admin requests stay. A
//!    branch the host stopped listing (or that left the globs) leaves the
//!    desired state and the catalog (`removed`); a polled pull request that
//!    vanished gets `closed_at` (expiry after `pr_after_close`); tags never
//!    expire by polling.
//! 4. **State and priority** against the catalog ([`settle`]): `pending`
//!    with the most urgent [`Priority`] of the refs behind, `done` when none
//!    is; leased and failed jobs keep their state.
//! 5. **Schedule**: the next poll is half the time since the polled refs
//!    last changed, clamped to `[poll.min, poll.max]` (45 s – 8 h).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use globset::{GlobBuilder, GlobMatcher};
use rueca_storage::{CasStep, Storage, cas_update};
use serde::Serialize;

use crate::catalog::{RefKind, RepoCatalog};
use crate::jobs::{DesiredRef, Job, JobCache, JobState, PollState, Priority, desired_update};
use crate::remote::{ListError, RemoteLister, RemoteRefs, RemoteUrl, repo_id_of};
use crate::{CAS_ATTEMPTS, ServiceStore, repo_dir};

/// One configured repository (`[[repo]]`).
#[derive(Debug, Clone)]
pub struct RepoSpec {
    pub url: String,
    /// Repository id (`github.com/acme/billing`).
    pub repo: String,
    /// The default branch; `None`: the remote's `HEAD`.
    pub default_branch: Option<String>,
    pub branches: Vec<String>,
    pub tags: Vec<String>,
    pub prs: bool,
    branch_globs: Vec<GlobMatcher>,
    tag_globs: Vec<GlobMatcher>,
}

impl RepoSpec {
    pub fn new(
        url: &str,
        default_branch: Option<&str>,
        branches: &[String],
        tags: &[String],
        prs: bool,
    ) -> anyhow::Result<RepoSpec> {
        Ok(RepoSpec {
            url: url.trim().to_string(),
            repo: repo_id_of(url)?,
            default_branch: default_branch.map(str::to_string),
            branches: branches.to_vec(),
            tags: tags.to_vec(),
            prs,
            branch_globs: compile(branches)?,
            tag_globs: compile(tags)?,
        })
    }

    /// Whether branch `name` matches the `branches` globs.
    pub fn branch_matches(&self, name: &str) -> bool {
        self.branch_globs.iter().any(|g| g.is_match(name))
    }

    /// Whether tag `name` matches the `tags` globs.
    pub fn tag_matches(&self, name: &str) -> bool {
        self.tag_globs.iter().any(|g| g.is_match(name))
    }

    /// The host the repository is polled on (empty for `file://`).
    pub fn host(&self) -> String {
        RemoteUrl::parse(&self.url).map(|u| u.host).unwrap_or_default()
    }
}

/// Ref globs: `*` does not cross `/`.
fn compile(globs: &[String]) -> anyhow::Result<Vec<GlobMatcher>> {
    globs
        .iter()
        .map(|g| {
            GlobBuilder::new(g)
                .literal_separator(true)
                .build()
                .map(|g| g.compile_matcher())
                .with_context(|| format!("ref glob `{g}`"))
        })
        .collect()
}

/// Polling bounds (`[indexing].poll`, `ls_remote_concurrency`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PollPolicy {
    pub min: Duration,
    pub max: Duration,
    /// Poll pull request heads of repositories with `prs = true`.
    pub pr_heads: bool,
    /// `git ls-remote` runs at once per host.
    pub per_host: usize,
}

impl Default for PollPolicy {
    fn default() -> PollPolicy {
        PollPolicy { min: Duration::from_secs(45), max: Duration::from_secs(8 * 3600), pr_heads: false, per_host: 8 }
    }
}

impl PollPolicy {
    /// Half the time since the last change, clamped to `[min, max]`.
    pub fn interval(&self, since_change: u64) -> u64 {
        (since_change / 2).clamp(self.min.as_secs().max(1), self.max.as_secs().max(1))
    }
}

/// The refs a poll found, by ref name.
pub type Polled = BTreeMap<String, (RefKind, String)>;

/// The desired refs of `spec` in a listing: default, branches, tags and,
/// with `pr_heads`, pull request heads.
pub fn desired_from(spec: &RepoSpec, remote: &RemoteRefs, pr_heads: bool) -> anyhow::Result<Polled> {
    let default = spec
        .default_branch
        .clone()
        .or_else(|| remote.head.clone())
        .with_context(|| format!("{}: the remote names no HEAD branch; set default_branch", spec.url))?;
    let mut out = Polled::new();
    let head = remote
        .refs
        .get(&format!("refs/heads/{default}"))
        .with_context(|| format!("{}: the default branch `{default}` is not listed", spec.url))?;
    out.insert(default.clone(), (RefKind::Default, head.clone()));
    for (full, commit) in &remote.refs {
        if let Some((name, kind)) = classify(spec, full, &default, pr_heads) {
            out.insert(name, (kind, commit.clone()));
        }
    }
    Ok(out)
}

/// The ref name and kind a listed ref is desired as, if any.
fn classify(spec: &RepoSpec, full: &str, default: &str, pr_heads: bool) -> Option<(String, RefKind)> {
    if let Some(b) = full.strip_prefix("refs/heads/") {
        let wanted = b != default && spec.branch_matches(b);
        return wanted.then(|| (b.to_string(), RefKind::Branch));
    }
    if let Some(t) = full.strip_prefix("refs/tags/") {
        return spec.tag_matches(t).then(|| (t.to_string(), RefKind::Tag));
    }
    if !(spec.prs && pr_heads) {
        return None;
    }
    let n = full
        .strip_prefix("refs/pull/")
        .or_else(|| full.strip_prefix("refs/merge-requests/"))
        .and_then(|r| r.strip_suffix("/head"))
        .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))?;
    Some((format!("pr/{n}"), RefKind::Pr))
}

/// What a poll changed in the catalog, besides the job.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Merged {
    /// The polled refs changed (a new, moved or removed ref).
    pub changed: bool,
    /// Branches no longer listed (or out of the globs): removed.
    pub removed: Vec<String>,
    /// Polled pull requests no longer listed: closed.
    pub closed: Vec<String>,
}

/// Merges a poll into `job` at `now`: polled refs are set (an expired ref
/// stays out until its commit moves); desired branches not polled are
/// removed, desired pull requests not polled are closed when pull requests
/// are polled; tags not matching the globs follow the listing when listed;
/// on-demand commits and webhook refs stay.
pub fn merge_polled(job: &mut Job, polled: &Polled, remote: &RemoteRefs, polls_prs: bool, now: u64) -> Merged {
    let mut m = Merged::default();
    for (name, (kind, commit)) in polled {
        if job.expired.get(name) == Some(commit) {
            continue;
        }
        m.changed |= job.set_desired(name, DesiredRef { commit: commit.clone(), kind: *kind, seen_at: now });
    }
    let stale: Vec<(String, RefKind)> =
        job.desired.iter().filter(|(n, _)| !polled.contains_key(*n)).map(|(n, d)| (n.clone(), d.kind)).collect();
    for (name, kind) in stale {
        match kind {
            RefKind::Default | RefKind::Branch => {
                job.remove_desired(&name);
                m.removed.push(name);
                m.changed = true;
            }
            RefKind::Pr if polls_prs => {
                job.remove_desired(&name);
                m.closed.push(name);
                m.changed = true;
            }
            RefKind::Tag => {
                if let Some(c) = remote.refs.get(&format!("refs/tags/{name}")) {
                    m.changed |= job.set_desired(&name, DesiredRef { commit: c.clone(), kind, seen_at: now });
                }
            }
            RefKind::Pr | RefKind::Commit => {}
        }
    }
    m
}

/// Sets the job's state and priority from the catalog at `now` (see the
/// module docs, step 4); whether anything changed.
pub fn settle(job: &mut Job, catalog: Option<&RepoCatalog>, now: u64) -> bool {
    let never = catalog.is_none_or(|c| c.refs.is_empty());
    let behind: Vec<Priority> = job
        .desired
        .iter()
        .filter(|(name, d)| catalog.and_then(|c| c.head_commit(name)) != Some(d.commit.as_str()))
        .filter(|(name, d)| !job.ref_backing_off(name, &d.commit, now))
        .map(|(_, d)| Priority::of(d.kind))
        .collect();
    let priority = if never { Some(Priority::NeverIndexed) } else { behind.iter().min().copied() };
    let before = (job.state, job.priority);
    if let Some(p) = priority.filter(|_| !behind.is_empty()) {
        job.priority = p.rank();
    }
    job.state = match (job.state, behind.is_empty()) {
        (JobState::Leased, _) => JobState::Leased,
        (JobState::Failed, false) => JobState::Failed,
        (_, true) => JobState::Done,
        (_, false) => JobState::Pending,
    };
    if job.state == JobState::Done && before.0 == JobState::Failed {
        job.attempts = 0;
        job.not_before = None;
    }
    (job.state, job.priority) != before
}

/// What a reconciler tick did.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ReconcileReport {
    /// Repositories listed with `ls-remote`.
    pub polled: usize,
    /// Repositories whose desired state changed.
    pub changed: Vec<String>,
    /// `repo@ref` removed from catalogs (unlisted branches).
    pub removed: Vec<String>,
    /// `repo@ref` closed (vanished pull requests).
    pub closed: Vec<String>,
    /// Hosts skipped because they are paused after a `429`.
    pub paused_hosts: Vec<String>,
    /// `repo: error` of failed polls.
    pub errors: Vec<String>,
}

/// The reconciler: inventory, polling and desired state (module docs).
pub struct Reconciler {
    pub specs: Vec<RepoSpec>,
    pub policy: PollPolicy,
    lister: Arc<dyn RemoteLister>,
    /// Repositories whose catalog and job exist (checked once per process).
    ensured: Mutex<HashSet<String>>,
    /// Host → paused until (unix seconds).
    paused: Mutex<HashMap<String, u64>>,
}

impl Reconciler {
    pub fn new(specs: Vec<RepoSpec>, policy: PollPolicy, lister: Arc<dyn RemoteLister>) -> Reconciler {
        Reconciler { specs, policy, lister, ensured: Mutex::default(), paused: Mutex::default() }
    }

    /// The configured repository with id `repo`.
    pub fn spec(&self, repo: &str) -> Option<&RepoSpec> {
        self.specs.iter().find(|s| s.repo == repo)
    }

    pub fn lister(&self) -> &dyn RemoteLister {
        self.lister.as_ref()
    }

    /// One tick at `now` (module docs). Errors of one repository are
    /// reported, not raised.
    pub fn tick(&self, svc: &ServiceStore, cache: &mut JobCache, now: u64) -> anyhow::Result<ReconcileReport> {
        let storage = svc.storage.as_ref();
        self.ensure_inventory(storage, now)?;
        let jobs: HashMap<String, Job> =
            cache.scan(storage)?.into_iter().map(|l| (l.job.repo.clone(), l.job)).collect();
        let due: Vec<&RepoSpec> = self
            .specs
            .iter()
            .filter(|s| jobs.get(&s.repo).and_then(|j| j.poll).is_none_or(|p| p.next_at <= now))
            .collect();
        let mut report = ReconcileReport::default();
        let mut by_host: BTreeMap<String, Vec<&RepoSpec>> = BTreeMap::new();
        for s in due {
            by_host.entry(s.host()).or_default().push(s);
        }
        by_host.retain(|host, _| {
            let paused = self.host_paused(host, now);
            if paused {
                report.paused_hosts.push(host.clone());
            }
            !paused
        });
        let results = Mutex::new(Vec::new());
        std::thread::scope(|scope| {
            for (host, specs) in &by_host {
                let queue = Arc::new(Mutex::new(specs.clone()));
                for _ in 0..self.policy.per_host.max(1).min(specs.len()) {
                    let (queue, results) = (queue.clone(), &results);
                    scope.spawn(move || self.drain(storage, host, &queue, results, now));
                }
            }
        });
        for (repo, host, out) in results.into_inner().unwrap_or_else(|p| p.into_inner()) {
            self.record(&mut report, &repo, &host, out, now);
        }
        Ok(report)
    }

    /// One polling worker of `host`: polls repositories off `queue` until
    /// it is empty.
    fn drain(
        &self,
        storage: &dyn Storage,
        host: &str,
        queue: &Mutex<Vec<&RepoSpec>>,
        results: &Mutex<Vec<(String, String, PollOutcome)>>,
        now: u64,
    ) {
        while let Some(spec) = pop(queue) {
            let out = self.poll_one(storage, spec, now);
            results.lock().unwrap_or_else(|p| p.into_inner()).push((spec.repo.clone(), host.to_string(), out));
        }
    }

    fn record(&self, report: &mut ReconcileReport, repo: &str, host: &str, out: PollOutcome, now: u64) {
        match out {
            Ok(m) => {
                report.polled += 1;
                if m.changed {
                    report.changed.push(repo.to_string());
                }
                report.removed.extend(m.removed.iter().map(|r| format!("{repo}@{r}")));
                report.closed.extend(m.closed.iter().map(|r| format!("{repo}@{r}")));
            }
            Err(ListError::RateLimited { pause }) => {
                tracing::warn!(host, pause_s = pause.as_secs(), "host rate limited: polling paused");
                self.paused.lock().unwrap_or_else(|p| p.into_inner()).insert(host.to_string(), now + pause.as_secs());
                report.paused_hosts.push(host.to_string());
            }
            Err(ListError::Other(e)) => {
                tracing::warn!(repo, error = %format!("{e:#}"), "polling failed");
                report.errors.push(format!("{repo}: {e:#}"));
            }
        }
    }

    fn host_paused(&self, host: &str, now: u64) -> bool {
        let mut paused = self.paused.lock().unwrap_or_else(|p| p.into_inner());
        paused.retain(|_, until| *until > now);
        paused.contains_key(host)
    }

    /// Creates the catalog (with its URL) and the job of every configured
    /// repository not yet seen by this process.
    fn ensure_inventory(&self, storage: &dyn Storage, now: u64) -> anyhow::Result<()> {
        let mut ensured = self.ensured.lock().unwrap_or_else(|p| p.into_inner());
        for s in &self.specs {
            if ensured.contains(&s.repo) {
                continue;
            }
            ensure_catalog(storage, &s.repo, Some(&s.url))?;
            desired_update(storage, &s.repo, Some(&s.url), now, |_| false)?;
            ensured.insert(s.repo.clone());
        }
        Ok(())
    }

    /// Polls one repository and writes the result (job, then catalog).
    fn poll_one(&self, storage: &dyn Storage, spec: &RepoSpec, now: u64) -> PollOutcome {
        let polls_prs = spec.prs && self.policy.pr_heads;
        let mut patterns = vec!["HEAD", "refs/heads/*", "refs/tags/*"];
        if polls_prs {
            patterns.extend(["refs/pull/*/head", "refs/merge-requests/*/head"]);
        }
        let listed = self.lister.ls_remote(&spec.url, &patterns);
        let catalog = read_catalog(storage, &spec.repo)?;
        let (remote, polled) = match listed {
            Ok(remote) => {
                let polled = desired_from(spec, &remote, self.policy.pr_heads)?;
                (remote, polled)
            }
            Err(e) => {
                self.reschedule_after_error(storage, spec, now)?;
                return Err(e);
            }
        };
        let mut merged = Merged::default();
        desired_update(storage, &spec.repo, Some(&spec.url), now, |job| {
            merged = merge_polled(job, &polled, &remote, polls_prs, now);
            let schedule = schedule(job.poll, merged.changed, &self.policy, now);
            let changed = merged.changed | settle(job, catalog.as_ref(), now) | (job.poll != Some(schedule));
            job.poll = Some(schedule);
            changed
        })?;
        apply_to_catalog(storage, &spec.repo, &merged, &polled, now)?;
        Ok(merged)
    }

    /// A failed poll is retried after the current interval (at least
    /// `poll.min`).
    fn reschedule_after_error(&self, storage: &dyn Storage, spec: &RepoSpec, now: u64) -> anyhow::Result<()> {
        desired_update(storage, &spec.repo, Some(&spec.url), now, |job| {
            let mut p = job.poll.unwrap_or(PollState { next_at: now, interval_s: 0, changed_at: now });
            p.interval_s = p.interval_s.max(self.policy.min.as_secs());
            p.next_at = now + p.interval_s;
            job.poll = Some(p);
            true
        })?;
        Ok(())
    }
}

type PollOutcome = Result<Merged, ListError>;

fn pop<'a>(queue: &Mutex<Vec<&'a RepoSpec>>) -> Option<&'a RepoSpec> {
    queue.lock().unwrap_or_else(|p| p.into_inner()).pop()
}

/// The next poll after one at `now` (§7.4).
fn schedule(prev: Option<PollState>, changed: bool, policy: &PollPolicy, now: u64) -> PollState {
    let changed_at = match prev {
        Some(p) if !changed => p.changed_at,
        _ => now,
    };
    let interval_s = policy.interval(now.saturating_sub(changed_at));
    PollState { next_at: now + interval_s, interval_s, changed_at }
}

/// Removes unlisted branches (and branches of the catalog that are no
/// longer desired) and closes vanished pull requests, by CAS.
fn apply_to_catalog(
    storage: &dyn Storage,
    repo: &str,
    merged: &Merged,
    polled: &Polled,
    now: u64,
) -> anyhow::Result<()> {
    catalog_update(storage, repo, |c| {
        let stray: Vec<String> = c
            .refs
            .iter()
            .filter(|(n, e)| e.kind == RefKind::Branch && !polled.contains_key(*n))
            .map(|(n, _)| n.clone())
            .collect();
        let mut changed = false;
        for name in merged.removed.iter().chain(&stray) {
            changed |= c.remove_ref(name, now);
        }
        for name in &merged.closed {
            changed |= c.close_pr(name, now);
        }
        changed
    })
}

/// Reads the catalog of `repo` (`None`: none yet).
pub fn read_catalog(storage: &dyn Storage, repo: &str) -> anyhow::Result<Option<RepoCatalog>> {
    let path = ServiceStore::repo_path(&repo_dir(repo, None), "catalog.json");
    let Some(v) = storage.get(&path)? else { return Ok(None) };
    Ok(Some(serde_json::from_slice(&v.value).with_context(|| format!("{path} is not valid"))?))
}

/// A CAS of an existing catalog: `f` edits it and returns whether it
/// changed anything. An absent catalog is left absent.
pub fn catalog_update(
    storage: &dyn Storage,
    repo: &str,
    mut f: impl FnMut(&mut RepoCatalog) -> bool,
) -> anyhow::Result<()> {
    let path = ServiceStore::repo_path(&repo_dir(repo, None), "catalog.json");
    if storage.get(&path)?.is_none() {
        return Ok(());
    }
    cas_update(storage, &path, CAS_ATTEMPTS, |cur: Option<RepoCatalog>| {
        let mut c = cur.with_context(|| format!("{path} vanished"))?;
        Ok(if f(&mut c) { CasStep::Write(c) } else { CasStep::Unchanged(c) })
    })?;
    Ok(())
}

/// Creates the catalog of `repo` when absent (no refs yet) and records its
/// URL when it has none.
pub fn ensure_catalog(storage: &dyn Storage, repo: &str, url: Option<&str>) -> anyhow::Result<()> {
    let path = ServiceStore::repo_path(&repo_dir(repo, None), "catalog.json");
    cas_update(storage, &path, CAS_ATTEMPTS, |cur: Option<RepoCatalog>| {
        let created = cur.is_none();
        let mut c = cur.unwrap_or_else(|| RepoCatalog::new(repo));
        anyhow::ensure!(c.repo == repo, "{path} is the catalog of `{}`, not of `{repo}`", c.repo);
        let set_url = c.url.is_none() && url.is_some();
        if set_url {
            c.url = url.map(str::to_string);
        }
        Ok(if created || set_url { CasStep::Write(c) } else { CasStep::Unchanged(c) })
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::JobKind;

    fn spec(branches: &[&str], tags: &[&str], prs: bool) -> RepoSpec {
        let b: Vec<String> = branches.iter().map(|s| s.to_string()).collect();
        let t: Vec<String> = tags.iter().map(|s| s.to_string()).collect();
        RepoSpec::new("https://github.com/acme/billing.git", None, &b, &t, prs).unwrap()
    }

    fn remote(refs: &[(&str, &str)]) -> RemoteRefs {
        RemoteRefs {
            head: Some("main".into()),
            refs: refs.iter().map(|(r, c)| (r.to_string(), c.to_string())).collect(),
        }
    }

    #[test]
    fn desired_follows_the_globs() {
        let s = spec(&["release/*"], &["v*"], true);
        let r = remote(&[
            ("refs/heads/main", "a"),
            ("refs/heads/release/2.4", "b"),
            ("refs/heads/release/x/y", "c"),
            ("refs/heads/feature", "d"),
            ("refs/tags/v1", "e"),
            ("refs/tags/nightly", "f"),
            ("refs/pull/42/head", "g"),
            ("refs/merge-requests/7/head", "h"),
        ]);
        let names = |p: &Polled| p.iter().map(|(n, (k, c))| format!("{n}:{}:{c}", k.as_str())).collect::<Vec<_>>();
        assert_eq!(
            names(&desired_from(&s, &r, false).unwrap()),
            ["main:default:a", "release/2.4:branch:b", "v1:tag:e"]
        );
        let with_prs = desired_from(&s, &r, true).unwrap();
        assert_eq!(with_prs["pr/42"], (RefKind::Pr, "g".to_string()));
        assert_eq!(with_prs["pr/7"], (RefKind::Pr, "h".to_string()));
        assert!(desired_from(&s, &remote(&[("refs/heads/dev", "x")]), false).is_err(), "default not listed");
    }

    #[test]
    fn merge_removes_branches_closes_prs_and_keeps_tags_and_commits() {
        let s = spec(&["release/*"], &["v*"], true);
        let mut job = Job::new(JobKind::Index, &s.repo, None, 1);
        let r1 = remote(&[("refs/heads/main", "a"), ("refs/heads/release/1", "b"), ("refs/pull/3/head", "c")]);
        let m = merge_polled(&mut job, &desired_from(&s, &r1, true).unwrap(), &r1, true, 10);
        assert!(m.changed && m.removed.is_empty());
        job.set_desired("hotfix-tag", DesiredRef { commit: "t".into(), kind: RefKind::Tag, seen_at: 1 });
        job.set_desired("0123456789ab", DesiredRef { commit: "z".into(), kind: RefKind::Commit, seen_at: 1 });
        let r2 = remote(&[("refs/heads/main", "a2"), ("refs/tags/hotfix-tag", "t2")]);
        let m = merge_polled(&mut job, &desired_from(&s, &r2, true).unwrap(), &r2, true, 20);
        assert_eq!((m.removed, m.closed), (vec!["release/1".to_string()], vec!["pr/3".to_string()]));
        assert_eq!(job.desired["main"].commit, "a2");
        assert_eq!(job.desired["hotfix-tag"].commit, "t2", "a moved tag is re-indexed");
        assert!(job.desired.contains_key("0123456789ab"));
        let m = merge_polled(&mut job, &desired_from(&s, &r2, true).unwrap(), &r2, true, 30);
        assert!(!m.changed, "nothing moved");
    }

    #[test]
    fn settle_sets_priority_and_state() {
        let mut job = Job::new(JobKind::Index, "x", None, 1);
        job.set_desired("main", DesiredRef { commit: "a".into(), kind: RefKind::Default, seen_at: 1 });
        job.set_desired("v1", DesiredRef { commit: "b".into(), kind: RefKind::Tag, seen_at: 1 });
        settle(&mut job, None, 5);
        assert_eq!((job.state, job.priority), (JobState::Pending, 0), "never indexed");
        let mut c = RepoCatalog::new("x");
        c.insert("main", RefKind::Default, snap("a"), &Default::default());
        settle(&mut job, Some(&c), 5);
        assert_eq!((job.state, job.priority), (JobState::Pending, 4), "only the tag is missing");
        c.insert("v1", RefKind::Tag, snap("b"), &Default::default());
        assert!(settle(&mut job, Some(&c), 5));
        assert_eq!(job.state, JobState::Done);
    }

    fn snap(commit: &str) -> crate::SnapshotEntry {
        crate::SnapshotEntry {
            commit: commit.into(),
            segment: format!("s-{commit}"),
            indexed_at: 1,
            rueca_version: "t".into(),
            bytes: 1,
            packs: vec![],
            vectors: None,
            dirty: false,
        }
    }

    #[test]
    fn the_poll_interval_halves_the_quiet_time_within_bounds() {
        let p = PollPolicy::default();
        assert_eq!(p.interval(0), 45);
        assert_eq!(p.interval(8 * 3600), 4 * 3600);
        assert_eq!(p.interval(100 * 3600), 8 * 3600);
        let s = schedule(Some(PollState { next_at: 0, interval_s: 45, changed_at: 1000 }), false, &p, 1000 + 3600);
        assert_eq!((s.interval_s, s.changed_at, s.next_at), (1800, 1000, 1000 + 3600 + 1800));
        assert_eq!(schedule(None, true, &p, 50).interval_s, 45);
    }
}
