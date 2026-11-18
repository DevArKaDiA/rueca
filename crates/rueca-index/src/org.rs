//! Organizations in local mode (RFC 0006 §7): a `rueca-org.toml` names the
//! organization and its repositories; [`index_org`] indexes each repository
//! with the unchanged repository pipeline ([`crate::index_worktree`]) and
//! then builds or updates the organization layer
//! ([`rueca_segment::org::update_org_with`] with the [`PackageLinker`]:
//! routing profiles, package registries and cross-repository edges).
//!
//! ```toml
//! name = "acme"
//!
//! [[repo]]
//! path = "~/src/billing"          # a working tree (`~` and paths relative to this file)
//!
//! [[repo]]
//! url = "https://github.com/acme/payments"   # cloned under <store>/orgs/acme/clones/
//! id = "github.com/acme/payments"            # optional repository id override
//! refs = ["release/2.x", "pr/123"]           # optional: other refs indexed too
//! ```
//!
//! **Refs** (RFC 0006 §9.3): besides its default ref (the member root), a
//! member's `refs` are indexed as ref snapshots ([`crate::refs::index_ref`],
//! exported under `<store>/refs/`) and listed in the generation next to the
//! default. [`index_org`] syncs them for every member it indexes, refreshes
//! the member's ref snapshots indexed earlier from the command line, and
//! removes none; [`index_org_ref`] indexes or removes one ref of one member
//! (`rueca index --org <file> <member> --ref <name> [--remove]`), and
//! [`update_orgs_of_repo`] propagates a plain `rueca index <path> --ref
//! <name>`. A ref snapshot belongs to the member whose repository id it
//! carries. For a `url` member a branch ref is fetched from the remote
//! before it is resolved, so refreshing it follows the remote.
//!
//! A `url` member is cloned with the `git` executable (`git clone --depth 1`)
//! into `<store>/orgs/<name>/clones/<host>/<path>`, and refreshed on the
//! next index (`git fetch --depth 1 origin HEAD` + `git reset --hard
//! FETCH_HEAD`). The organization is incremental: indexing one member
//! ([`OrgIndexOptions::only`]) re-indexes that repository and replaces only
//! its part of the organization index; [`update_orgs_containing`] does the
//! same after a plain `rueca index <path>` of a member.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use anyhow::Context;
use rueca_extract::Budget;
use rueca_segment::Store;
use rueca_segment::org::{self, OrgMember, OrgUpdate};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::link::PackageLinker;
use crate::refs::{self, RefIndexOptions, RefIndexReport, RefRemoval};
use crate::{IndexOptions, IndexReport, index_worktree, normalize_remote};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OrgFile {
    name: String,
    #[serde(default, rename = "repo")]
    repos: Vec<RepoFile>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RepoFile {
    path: Option<String>,
    url: Option<String>,
    id: Option<String>,
    #[serde(default)]
    refs: Vec<String>,
}

/// Where a member's working tree comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoSource {
    /// A local working tree (absolute; `~` expanded, relative paths resolved
    /// against the organization file's directory).
    Path(PathBuf),
    /// A remote cloned under the store.
    Url(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgRepoConfig {
    pub source: RepoSource,
    /// Repository id override (`rueca index --repo`).
    pub id: Option<String>,
    /// Other refs to index (`refs = ["feature/x", "pr/123"]`): branches,
    /// `pr/<n>`, tags or commits.
    pub refs: Vec<String>,
}

/// A parsed `rueca-org.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgConfig {
    pub name: String,
    pub repos: Vec<OrgRepoConfig>,
}

fn expand(path: &str, base: &Path) -> PathBuf {
    let p = match path.strip_prefix("~/").or_else(|| (path == "~").then_some("")) {
        Some(rest) => {
            let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
            if rest.is_empty() { home } else { home.join(rest) }
        }
        None => PathBuf::from(path),
    };
    if p.is_absolute() { p } else { base.join(p) }
}

impl OrgConfig {
    /// Reads and validates an organization file.
    pub fn load(file: &Path) -> anyhow::Result<OrgConfig> {
        let text = std::fs::read_to_string(file).with_context(|| format!("cannot read {}", file.display()))?;
        let base = file.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
        let base = std::fs::canonicalize(&base).unwrap_or(base);
        OrgConfig::parse(&text, &base).with_context(|| format!("invalid organization file {}", file.display()))
    }

    /// Parses an organization file whose relative paths are relative to `base`.
    pub fn parse(text: &str, base: &Path) -> anyhow::Result<OrgConfig> {
        let f: OrgFile = toml::from_str(text)?;
        org::validate_name(&f.name)?;
        let mut repos = Vec::new();
        for (i, r) in f.repos.into_iter().enumerate() {
            let source = match (r.path, r.url) {
                (Some(p), None) => RepoSource::Path(expand(&p, base)),
                (None, Some(u)) => RepoSource::Url(u),
                _ => anyhow::bail!("[[repo]] #{} needs exactly one of `path` or `url`", i + 1),
            };
            let mut ref_names: Vec<String> = Vec::with_capacity(r.refs.len());
            for name in r.refs {
                refs::validate_ref_name(&name).with_context(|| format!("[[repo]] #{} `refs`", i + 1))?;
                if !ref_names.contains(&name) {
                    ref_names.push(name);
                }
            }
            repos.push(OrgRepoConfig { source, id: r.id, refs: ref_names });
        }
        Ok(OrgConfig { name: f.name, repos })
    }

    /// The working tree of a member: its path, or its clone under `store`.
    pub fn root_of(&self, repo: &OrgRepoConfig, store: &Path) -> PathBuf {
        match &repo.source {
            RepoSource::Path(p) => p.clone(),
            RepoSource::Url(u) => clone_dir(store, &self.name, u),
        }
    }

    /// The members as the organization layer sees them (roots and URLs).
    pub fn members(&self, store: &Path) -> Vec<OrgMember> {
        self.repos
            .iter()
            .map(|r| OrgMember {
                root: self.root_of(r, store).to_string_lossy().into_owned(),
                url: match &r.source {
                    RepoSource::Url(u) => Some(u.clone()),
                    RepoSource::Path(_) => None,
                },
                refs: r.refs.clone(),
            })
            .collect()
    }
}

/// `<store>/orgs/<org>/clones/<host>/<path>` for a remote URL (the
/// normalized remote, else the URL with unsafe characters replaced).
pub fn clone_dir(store: &Path, org_name: &str, url: &str) -> PathBuf {
    let rel = normalize_remote(url).unwrap_or_else(|| {
        url.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' }).collect()
    });
    let mut dir = org::org_dir(store, org_name).join("clones");
    for part in rel.split('/').filter(|p| !p.is_empty() && *p != "." && *p != "..") {
        dir.push(part);
    }
    dir
}

/// Runs the `git` executable (clones and fetches: the network operations
/// gix is not used for), failing with its stderr. Credentials come from the
/// user's credential helpers; git never prompts.
pub(crate) fn git(args: &[&str], cwd: Option<&Path>) -> anyhow::Result<()> {
    git_output(args, cwd, &[]).map(|_| ())
}

/// The `git` runner behind the private `git` helper, returning stdout: `env` is added to the
/// child's environment after the no-prompt variables (the service passes
/// `GIT_CONFIG_NOSYSTEM` and credentials as `GIT_CONFIG_COUNT` /
/// `GIT_CONFIG_KEY_<n>` / `GIT_CONFIG_VALUE_<n>`, so they never appear on
/// the command line). A failure names the arguments and git's stderr.
pub fn git_output(args: &[&str], cwd: Option<&Path>, env: &[(&str, &str)]) -> anyhow::Result<String> {
    let mut cmd = Command::new("git");
    // Never wait on a prompt: no terminal prompt, no askpass helper (an
    // empty `GIT_ASKPASS` also overrides `core.askPass` and `SSH_ASKPASS`).
    cmd.env("GIT_TERMINAL_PROMPT", "0").env("GIT_ASKPASS", "");
    for (k, v) in env {
        cmd.env(k, v);
    }
    if let Some(d) = cwd {
        cmd.arg("-C").arg(d);
    }
    let out = cmd.args(args).output().context("cannot run `git` (needed for `url` repositories and fetching refs)")?;
    if !out.status.success() {
        anyhow::bail!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Clones `url` into `dir`, or refreshes an existing clone to the remote's
/// default branch.
pub fn sync_clone(url: &str, dir: &Path) -> anyhow::Result<()> {
    if dir.join(".git").exists() {
        git(&["fetch", "--quiet", "--depth", "1", "origin", "HEAD"], Some(dir))?;
        git(&["reset", "--quiet", "--hard", "FETCH_HEAD"], Some(dir))
    } else {
        if let Some(parent) = dir.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let d = dir.to_string_lossy();
        git(&["clone", "--quiet", "--depth", "1", url, &d], None)
    }
}

#[derive(Debug, Clone)]
pub struct OrgIndexOptions {
    pub config: OrgConfig,
    pub store: PathBuf,
    pub budget: Budget,
    pub threads: Option<usize>,
    /// Index only this member (its working tree or a path inside it; for
    /// [`index_org_ref`] also its `id` or `url`), then update the
    /// organization; members never indexed are left out of the generation
    /// until they are.
    pub only: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OrgIndexReport {
    pub org: String,
    /// One report per repository indexed by this run.
    pub indexed: Vec<IndexReport>,
    /// One report per ref snapshot synced by this run (indexed, or found
    /// unchanged).
    pub refs: Vec<RefIndexReport>,
    /// Refs that could not be synced (`repo@ref: error`); their previous
    /// snapshot, if any, stays in the organization.
    pub ref_errors: Vec<String>,
    /// The organization layer update.
    pub update: OrgUpdate,
    /// Wall-clock milliseconds of the repository indexing, of the ref
    /// snapshots and of the organization layer update.
    pub repos_ms: u64,
    pub refs_ms: u64,
    pub org_ms: u64,
    pub duration_ms: u64,
}

/// The members `only` selects (all without it): those whose root contains
/// the path `only`, or, when no such path exists, the member whose `id` or
/// `url` is `only`.
fn targets<'c>(
    cfg: &'c OrgConfig,
    store: &Path,
    only: Option<&Path>,
) -> anyhow::Result<Vec<(&'c OrgRepoConfig, PathBuf)>> {
    let all = cfg.repos.iter().map(|r| (r, cfg.root_of(r, store)));
    let Some(only) = only else { return Ok(all.collect()) };
    let canon = std::fs::canonicalize(only).ok();
    let out: Vec<(&OrgRepoConfig, PathBuf)> = all
        .filter(|(r, root)| match &canon {
            Some(o) => o.starts_with(std::fs::canonicalize(root).unwrap_or_else(|_| root.clone())),
            None => member_named(r, &only.to_string_lossy()),
        })
        .collect();
    if out.is_empty() {
        anyhow::bail!("`{}` is not a repository of organization `{}`", only.display(), cfg.name);
    }
    Ok(out)
}

/// Whether `name` is member `r`'s id or url.
fn member_named(r: &OrgRepoConfig, name: &str) -> bool {
    r.id.as_deref() == Some(name) || matches!(&r.source, RepoSource::Url(u) if u == name)
}

/// Indexes the members of an organization (all, or [`OrgIndexOptions::only`]),
/// syncs their refs and updates the organization layer.
pub fn index_org(opts: &OrgIndexOptions) -> anyhow::Result<OrgIndexReport> {
    let started = Instant::now();
    let cfg = &opts.config;
    let targets = targets(cfg, &opts.store, opts.only.as_deref())?;
    let mut indexed = Vec::new();
    for (r, root) in &targets {
        if let RepoSource::Url(u) = &r.source {
            sync_clone(u, root).with_context(|| format!("cloning {u}"))?;
        }
        let report = index_worktree(&IndexOptions {
            root: root.clone(),
            store: opts.store.clone(),
            repo: r.id.clone(),
            budget: opts.budget.clone(),
            threads: opts.threads,
            ..IndexOptions::default()
        })
        .with_context(|| format!("indexing {}", root.display()))?;
        indexed.push(report);
    }
    let repos_ms = started.elapsed().as_millis() as u64;
    let t = Instant::now();
    let mut sync = RefSync::default();
    for ((r, root), report) in targets.iter().zip(&indexed) {
        sync.member(opts, r, root, &report.segment.snapshot.repo)?;
    }
    let refs_ms = t.elapsed().as_millis() as u64;
    let t = Instant::now();
    let mut update = update_org(opts)?;
    report_unowned(opts, &sync.reports, &mut update)?;
    let org_ms = t.elapsed().as_millis() as u64;
    Ok(OrgIndexReport {
        org: cfg.name.clone(),
        indexed,
        refs: sync.reports,
        ref_errors: sync.errors,
        update,
        repos_ms,
        refs_ms,
        org_ms,
        duration_ms: started.elapsed().as_millis() as u64,
    })
}

/// Builds or updates the organization layer of `opts.config` with the
/// package linker, logging what it left out.
fn update_org(opts: &OrgIndexOptions) -> anyhow::Result<OrgUpdate> {
    let cfg = &opts.config;
    let store = Store::open(&opts.store)?;
    let linker = PackageLinker::new(&opts.store);
    let update = org::update_org_with(&store, &cfg.name, &cfg.members(&opts.store), &linker)?;
    for e in &update.link_errors {
        warn!(org = %cfg.name, "linking: {e}");
    }
    if !update.missing.is_empty() {
        warn!(org = %cfg.name, missing = ?update.missing, "repositories and refs not indexed yet are left out");
    }
    info!(
        org = %cfg.name,
        generation = update.generation,
        repos = update.repos,
        refs = update.refs,
        ms = update.duration_ms,
        "organization updated"
    );
    Ok(update)
}

/// The ref snapshots synced by [`index_org`].
#[derive(Default)]
struct RefSync {
    reports: Vec<RefIndexReport>,
    errors: Vec<String>,
}

impl RefSync {
    /// Syncs the refs of member `r` (repository `repo`, root `root`): the
    /// configured ones, then the ones indexed earlier.
    fn member(&mut self, opts: &OrgIndexOptions, r: &OrgRepoConfig, root: &Path, repo: &str) -> anyhow::Result<()> {
        let store = Store::open(&opts.store)?;
        let mut names = r.refs.clone();
        // This root's exports only: another root of the repository (a
        // subdirectory member) has its own.
        let mine = refs::repo_export_dir(&std::fs::canonicalize(&opts.store)?, repo, &refs::root_prefix(root));
        let earlier = store
            .entries()?
            .into_iter()
            .filter(|e| e.repo == repo && e.is_ref_export() && Path::new(&e.root).parent() == Some(mine.as_path()));
        for e in earlier {
            let Some(name) = e.ref_name else { continue };
            if !names.iter().any(|n| refs::names_ref(&name, n)) {
                names.push(name);
            }
        }
        for name in names {
            match index_member_ref(opts, r, root, repo, &name) {
                Ok(report) => self.reports.push(report),
                Err(e) => {
                    warn!(%repo, r#ref = %name, "cannot index ref: {e:#}");
                    self.errors.push(format!("{repo}@{name}: {e:#}"));
                }
            }
        }
        Ok(())
    }
}

/// Indexes ref `name` of member `r` (repository `repo`, root `root`); for a
/// `url` member a branch is fetched from the remote first (a tag or a
/// commit the shallow clone lacks is fetched on resolution). The name is
/// validated before anything runs `git` with it.
fn index_member_ref(
    opts: &OrgIndexOptions,
    r: &OrgRepoConfig,
    root: &Path,
    repo: &str,
    name: &str,
) -> anyhow::Result<RefIndexReport> {
    refs::validate_ref_name(name)?;
    if matches!(r.source, RepoSource::Url(_)) {
        refresh_remote_branch(root, name);
    }
    refs::index_ref(&RefIndexOptions {
        root: root.to_path_buf(),
        name: name.to_string(),
        store: opts.store.clone(),
        repo: Some(repo.to_string()),
        budget: opts.budget.clone(),
        threads: opts.threads,
    })
}

/// In a `url` member's clone, updates the remote-tracking ref of branch
/// `name` from `origin` (shallow), so that resolving it follows the remote.
/// Not a branch (`pr/<n>` is fetched on resolution, a commit needs nothing)
/// or not on the remote: nothing happens and resolution decides.
fn refresh_remote_branch(clone: &Path, name: &str) {
    if refs::pr_number(name).is_some() || (refs::is_hex(name) && name.len() >= 7) || name == "HEAD" {
        return;
    }
    let refspec = format!("+refs/heads/{name}:refs/remotes/origin/{name}");
    let args = ["fetch", "--quiet", "--no-tags", "--no-write-fetch-head", "--depth", "1", "origin", refspec.as_str()];
    if let Err(e) = git(&args, Some(clone)) {
        tracing::debug!(r#ref = %name, "no branch on the remote: {e:#}");
    }
}

/// What [`index_org_ref`] did.
#[derive(Debug, Clone, Serialize)]
pub struct OrgRefReport {
    pub org: String,
    pub repo: String,
    /// The ref indexed (absent when removing).
    pub indexed: Option<RefIndexReport>,
    /// The ref removed (with `remove`).
    pub removed: Option<RefRemoval>,
    pub update: OrgUpdate,
    /// Wall-clock milliseconds of the ref snapshot (resolve, export, index or
    /// removal) and of the organization layer update.
    pub ref_ms: u64,
    pub org_ms: u64,
    pub duration_ms: u64,
}

/// Indexes (or, with `remove`, removes) ref `name` of the one member
/// [`OrgIndexOptions::only`] names, then updates the organization: only
/// that ref's entry changes. The member's default must have been indexed
/// for the ref to be listed.
pub fn index_org_ref(opts: &OrgIndexOptions, name: &str, remove: bool) -> anyhow::Result<OrgRefReport> {
    let started = Instant::now();
    let cfg = &opts.config;
    let only = opts.only.as_deref().context("`--ref` with `--org` needs the member (its path, id or url)")?;
    let found = targets(cfg, &opts.store, Some(only))?;
    let [(r, root)] = found.as_slice() else {
        anyhow::bail!(
            "`{}` names {} repositories of organization `{}`; name one",
            only.display(),
            found.len(),
            cfg.name
        )
    };
    if let RepoSource::Url(u) = &r.source
        && !root.join(".git").exists()
    {
        sync_clone(u, root).with_context(|| format!("cloning {u}"))?;
    }
    // Canonical, so the id, the prefix and the export match a plain
    // `rueca index` of the same directory (a member path may go through a
    // symbolic link).
    let root = &std::fs::canonicalize(root).with_context(|| format!("cannot access {}", root.display()))?;
    let repo = crate::repo_id(root, r.id.as_deref());
    let (indexed, removed) = if remove {
        (None, Some(refs::remove_ref(&opts.store, &repo, &refs::root_prefix(root), name)?))
    } else {
        (Some(index_member_ref(opts, r, root, &repo, name)?), None)
    };
    let ref_ms = started.elapsed().as_millis() as u64;
    let t = Instant::now();
    let mut update = update_org(opts)?;
    report_unowned(opts, indexed.iter(), &mut update)?;
    Ok(OrgRefReport {
        org: cfg.name.clone(),
        repo,
        indexed,
        removed,
        update,
        ref_ms,
        org_ms: t.elapsed().as_millis() as u64,
        duration_ms: started.elapsed().as_millis() as u64,
    })
}

/// Records in `update.unowned_refs` (and logs) every ref snapshot of
/// `reports` whose repository is no member of the generation just
/// published: the organization leaves it out, which must not go unnoticed
/// (an export recorded under another id than its member's default).
fn report_unowned<'a>(
    opts: &OrgIndexOptions,
    reports: impl IntoIterator<Item = &'a RefIndexReport>,
    update: &mut OrgUpdate,
) -> anyhow::Result<()> {
    let name = &opts.config.name;
    let g = org::current_generation(&opts.store, name)?;
    for r in reports {
        if g.as_ref().is_none_or(|g| g.repo_index(&r.repo).is_none()) {
            let msg = format!("ref {} of {} belongs to no indexed member of organization `{name}`", r.ref_name, r.repo);
            warn!(org = %name, "{msg}; left out");
            update.unowned_refs.push(msg);
        }
    }
    Ok(())
}

/// The members of generation `g` as an update sees them (no configured
/// refs: the generation does not record the file's).
fn generation_members(g: &org::OrgGeneration) -> Vec<OrgMember> {
    g.repos.iter().map(|r| OrgMember { root: r.default.root.clone(), url: r.url.clone(), refs: Vec::new() }).collect()
}

/// Updates every organization of the store whose current generation has a
/// member at `root` (after `rueca index <root>`): only that repository's
/// part changes.
pub fn update_orgs_containing(store_dir: &Path, root: &Path) -> anyhow::Result<Vec<OrgUpdate>> {
    let canon = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let canon = canon.to_string_lossy();
    update_orgs_where(store_dir, |g| g.repos.iter().any(|r| r.default.root == canon))
}

/// Updates every organization of the store whose current generation holds
/// repository `repo` (after `rueca index <path> --ref <name>` and its
/// `--remove`): the repository's ref entries follow the store, nothing else
/// changes.
pub fn update_orgs_of_repo(store_dir: &Path, repo: &str) -> anyhow::Result<Vec<OrgUpdate>> {
    update_orgs_where(store_dir, |g| g.repo_index(repo).is_some())
}

fn update_orgs_where(store_dir: &Path, holds: impl Fn(&org::OrgGeneration) -> bool) -> anyhow::Result<Vec<OrgUpdate>> {
    let store = Store::open(store_dir)?;
    let mut out = Vec::new();
    for g in org::list_orgs(store_dir)?.into_iter().filter(|g| holds(g)) {
        out.push(org::update_org_with(&store, &g.name, &generation_members(&g), &PackageLinker::new(store_dir))?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_paths_urls_and_ids() {
        let base = Path::new("/etc/orgs");
        let cfg = OrgConfig::parse(
            "name = \"acme\"\n[[repo]]\npath = \"billing\"\n[[repo]]\npath = \"/src/pay\"\nid = \"github.com/acme/pay\"\n\
             [[repo]]\nurl = \"https://github.com/acme/ship.git\"\nrefs = [\"release/1.x\", \"pr/7\", \"pr/7\"]\n",
            base,
        )
        .unwrap();
        assert_eq!(cfg.name, "acme");
        assert_eq!(cfg.repos[0].source, RepoSource::Path(PathBuf::from("/etc/orgs/billing")));
        assert_eq!(cfg.repos[1].id.as_deref(), Some("github.com/acme/pay"));
        assert!(cfg.repos[0].refs.is_empty());
        assert_eq!(cfg.repos[2].refs, ["release/1.x", "pr/7"], "duplicates dropped");
        let store = Path::new("/store");
        assert_eq!(cfg.root_of(&cfg.repos[2], store), PathBuf::from("/store/orgs/acme/clones/github.com/acme/ship"));
        let members = cfg.members(store);
        assert_eq!(members[2].url.as_deref(), Some("https://github.com/acme/ship.git"));
        assert_eq!(members[0].url, None);
        assert_eq!(members[2].refs, ["release/1.x", "pr/7"]);
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        let cfg = OrgConfig::parse("name = \"a\"\n[[repo]]\npath = \"~/x\"\n", base).unwrap();
        assert_eq!(cfg.repos[0].source, RepoSource::Path(Path::new(&home).join("x")));
    }

    #[test]
    fn rejects_bad_files() {
        let base = Path::new("/");
        for bad in [
            "",
            "name = \"../x\"",
            "name = \"a\"\n[[repo]]\n",
            "name = \"a\"\n[[repo]]\npath = \"x\"\nurl = \"y\"\n",
            "name = \"a\"\nrepos = []\n",
            "name = \"a\"\n[[repo]]\npath = \"x\"\nbranch = \"main\"\n",
            "name = \"a\"\n[[repo]]\npath = \"x\"\nrefs = \"main\"\n",
            "name = \"a\"\n[[repo]]\npath = \"x\"\nrefs = [\"a..b\"]\n",
            "name = \"a\"\n[[repo]]\npath = \"x\"\nrefs = [\"\"]\n",
        ] {
            assert!(OrgConfig::parse(bad, base).is_err(), "{bad:?}");
        }
        assert_eq!(
            clone_dir(Path::new("/s"), "a", "file:///tmp/x y"),
            PathBuf::from("/s/orgs/a/clones/file____tmp_x_y")
        );
    }
}
