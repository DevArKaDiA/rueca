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
//! ```
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
            repos.push(OrgRepoConfig { source, id: r.id });
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

fn git(args: &[&str], cwd: Option<&Path>) -> anyhow::Result<()> {
    let mut cmd = Command::new("git");
    if let Some(d) = cwd {
        cmd.arg("-C").arg(d);
    }
    let out = cmd.args(args).output().context("cannot run `git` (needed for `url` repositories)")?;
    if !out.status.success() {
        anyhow::bail!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
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
    /// Index only this member (its working tree or a path inside it), then
    /// update the organization; members never indexed are left out of the
    /// generation until they are.
    pub only: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OrgIndexReport {
    pub org: String,
    /// One report per repository indexed by this run.
    pub indexed: Vec<IndexReport>,
    /// The organization layer update.
    pub update: OrgUpdate,
    /// Wall-clock milliseconds of the repository indexing and of the
    /// organization layer update.
    pub repos_ms: u64,
    pub org_ms: u64,
    pub duration_ms: u64,
}

/// Indexes the members of an organization (all, or [`OrgIndexOptions::only`])
/// and updates the organization layer.
pub fn index_org(opts: &OrgIndexOptions) -> anyhow::Result<OrgIndexReport> {
    let started = Instant::now();
    let cfg = &opts.config;
    let only = match &opts.only {
        Some(p) => Some(std::fs::canonicalize(p).with_context(|| format!("cannot access {}", p.display()))?),
        None => None,
    };
    let mut targets = Vec::new();
    for r in &cfg.repos {
        let root = cfg.root_of(r, &opts.store);
        if let Some(o) = &only {
            let canon = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
            if !o.starts_with(&canon) {
                continue;
            }
        }
        targets.push((r, root));
    }
    if only.is_some() && targets.is_empty() {
        anyhow::bail!(
            "`{}` is not a repository of organization `{}`",
            opts.only.as_deref().unwrap_or(Path::new("")).display(),
            cfg.name
        );
    }
    let mut indexed = Vec::new();
    for (r, root) in targets {
        if let RepoSource::Url(u) = &r.source {
            sync_clone(u, &root).with_context(|| format!("cloning {u}"))?;
        }
        let report = index_worktree(&IndexOptions {
            root: root.clone(),
            store: opts.store.clone(),
            repo: r.id.clone(),
            budget: opts.budget.clone(),
            threads: opts.threads,
        })
        .with_context(|| format!("indexing {}", root.display()))?;
        indexed.push(report);
    }
    let repos_ms = started.elapsed().as_millis() as u64;
    let t = Instant::now();
    let store = Store::open(&opts.store)?;
    let linker = PackageLinker::new(&opts.store);
    let update = org::update_org_with(&store, &cfg.name, &cfg.members(&opts.store), &linker)?;
    for e in &update.link_errors {
        warn!(org = %cfg.name, "linking: {e}");
    }
    let org_ms = t.elapsed().as_millis() as u64;
    if !update.missing.is_empty() {
        warn!(org = %cfg.name, missing = ?update.missing, "repositories not indexed yet are left out");
    }
    info!(org = %cfg.name, generation = update.generation, repos = update.repos, ms = org_ms, "organization updated");
    Ok(OrgIndexReport {
        org: cfg.name.clone(),
        indexed,
        update,
        repos_ms,
        org_ms,
        duration_ms: started.elapsed().as_millis() as u64,
    })
}

/// Updates every organization of the store whose current generation has a
/// member at `root` (after `rueca index <root>`): only that repository's
/// part changes.
pub fn update_orgs_containing(store_dir: &Path, root: &Path) -> anyhow::Result<Vec<OrgUpdate>> {
    let canon = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let canon = canon.to_string_lossy();
    let store = Store::open(store_dir)?;
    let mut out = Vec::new();
    for g in org::list_orgs(store_dir)? {
        if !g.repos.iter().any(|r| r.root == canon) {
            continue;
        }
        let members: Vec<OrgMember> =
            g.repos.iter().map(|r| OrgMember { root: r.root.clone(), url: r.url.clone() }).collect();
        out.push(org::update_org_with(&store, &g.name, &members, &PackageLinker::new(store_dir))?);
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
             [[repo]]\nurl = \"https://github.com/acme/ship.git\"\n",
            base,
        )
        .unwrap();
        assert_eq!(cfg.name, "acme");
        assert_eq!(cfg.repos[0].source, RepoSource::Path(PathBuf::from("/etc/orgs/billing")));
        assert_eq!(cfg.repos[1].id.as_deref(), Some("github.com/acme/pay"));
        let store = Path::new("/store");
        assert_eq!(cfg.root_of(&cfg.repos[2], store), PathBuf::from("/store/orgs/acme/clones/github.com/acme/ship"));
        let members = cfg.members(store);
        assert_eq!(members[2].url.as_deref(), Some("https://github.com/acme/ship.git"));
        assert_eq!(members[0].url, None);
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
        ] {
            assert!(OrgConfig::parse(bad, base).is_err(), "{bad:?}");
        }
        assert_eq!(
            clone_dir(Path::new("/s"), "a", "file:///tmp/x y"),
            PathBuf::from("/s/orgs/a/clones/file____tmp_x_y")
        );
    }
}
