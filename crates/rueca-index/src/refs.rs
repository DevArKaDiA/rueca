//! Ref snapshots (RFC 0006 §9.3): a branch, pull request, tag or commit of a
//! repository indexed as a snapshot of its own, next to the working tree.
//!
//! [`index_ref`] runs three steps:
//!
//! 1. **Resolve** ([`resolve`]) the name to a commit, reading refs and objects
//!    with `gix`: `refs/heads/<name>`, `refs/remotes/origin/<name>`,
//!    `refs/remotes/<name>` (so `origin/main` works), `refs/tags/<name>`
//!    (annotated tags peeled), `HEAD`, a full `refs/…` name, or a commit (full,
//!    or a unique prefix of at least 7 hex digits). A branch found nowhere is
//!    fetched from `origin` (`git fetch origin
//!    +refs/heads/<name>:refs/remotes/origin/<name>`); `pr/<n>` is always
//!    fetched (`refs/pull/<n>/head`, then `refs/merge-requests/<n>/head`)
//!    into `refs/rueca/pr/<n>`, which keeps the objects reachable and serves
//!    as the offline fallback. Fetches are the only writes into the
//!    repository: objects, remote-tracking refs and `refs/rueca/…`; never a
//!    branch, never the working tree or the index. A shallow repository is
//!    fetched with `--depth 1`.
//! 2. **Export** ([`export`]) the commit's tree (the root's subtree when the
//!    root is a subdirectory of its repository) to
//!    `<store>/refs/<repo-slug>/<ref-slug>/` ([`export_dir`]), writing every
//!    blob byte for byte from the object database (no `git archive`: its
//!    `export-ignore` / `export-subst` attributes and content filters would
//!    change files and their blob ids). Symlinks and submodules are not
//!    exported (the working-tree walk does not index them either). The export
//!    is written to a temporary directory and swapped in.
//! 3. **Index** the export with [`crate::index_worktree`], pinned to the ref
//!    ([`IndexOptions::ref_name`], [`IndexOptions::commit`]): the snapshot's
//!    `branch` is the ref name, `commit` the resolved commit, `dirty` false,
//!    the repository id that of the source root ([`crate::repo_id`]); the
//!    catalog entry is marked as a ref snapshot
//!    (`rueca_segment::CatalogEntry::ref_name`). The facts cache is keyed by
//!    blob, so a second ref re-extracts only the files that differ.
//!
//! Re-indexing a ref whose commit, repository id and Rueca version did not
//! change, and whose export is still there, does nothing
//! ([`RefIndexReport::unchanged`]). [`remove_ref`] drops a ref snapshot: its
//! catalog entry, its segment (unless still referenced) and its export.
//!
//! Commits are recorded under their 12-digit form (`--ref 7696364` is the ref
//! `7696364aef89`); every other name is recorded as given.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::Context;
use rayon::prelude::*;
use rueca_extract::Budget;
use rueca_model::git_blob_sha;
use rueca_segment::{CatalogEntry, Store};
use serde::Serialize;
use tracing::{info, warn};

use crate::{IndexOptions, IndexReport, PhaseTimings, git, index_worktree};

/// Hex digits of a commit recorded as its ref name.
const SHORT_COMMIT: usize = 12;
/// Shortest commit prefix accepted as a ref name.
const MIN_COMMIT_PREFIX: usize = 7;
/// Longest ref slug before the hash suffix.
const MAX_SLUG: usize = 64;

/// What a ref name resolved through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RefKind {
    /// A local or remote-tracking branch.
    Branch,
    /// A pull / merge request head (`pr/<n>`).
    Pr,
    /// A tag (annotated tags peeled to their commit).
    Tag,
    /// A commit named by its id (or `HEAD`).
    Commit,
}

impl RefKind {
    /// `branch`, `pr`, `tag` or `commit`.
    pub fn as_str(self) -> &'static str {
        match self {
            RefKind::Branch => "branch",
            RefKind::Pr => "pr",
            RefKind::Tag => "tag",
            RefKind::Commit => "commit",
        }
    }
}

/// A ref name resolved to a commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedRef {
    /// The name the snapshot records: the name given, or the 12-digit form
    /// of a commit.
    pub name: String,
    pub kind: RefKind,
    /// Full hex commit id.
    pub commit: String,
    /// What was read: the full ref name (`refs/remotes/origin/feature/x`),
    /// or `commit`.
    pub source: String,
    /// The ref was fetched from `origin` by this resolution.
    pub fetched: bool,
}

/// Resolves ref `name` of the repository containing `repo_dir` (see the
/// module docs for the order). Fetches from `origin` when a branch is not
/// found locally and for every `pr/<n>`.
pub fn resolve(repo_dir: &Path, name: &str) -> anyhow::Result<ResolvedRef> {
    validate_ref_name(name)?;
    let root = fs::canonicalize(repo_dir).with_context(|| format!("cannot access {}", repo_dir.display()))?;
    let repo = open_repo(&root)?;
    if let Some(n) = pr_number(name) {
        return resolve_pr(&repo, &root, n);
    }
    if let Some(r) = resolve_local(&repo, name)? {
        return Ok(r);
    }
    resolve_fetched_branch(&repo, &root, name)
}

/// The repository containing `root`, or an error naming it.
fn open_repo(root: &Path) -> anyhow::Result<gix::Repository> {
    git::discover(root)
        .map(|(repo, _)| repo)
        .ok_or_else(|| anyhow::anyhow!("{} is not inside a git repository", root.display()))
}

/// Rejects names git would not accept as a ref (and anything that could be
/// read as an option or a refspec).
fn validate_ref_name(name: &str) -> anyhow::Result<()> {
    let bad_char =
        |c: char| c.is_whitespace() || c.is_control() || matches!(c, ':' | '?' | '*' | '[' | '\\' | '^' | '~');
    let invalid = name.is_empty()
        || name.len() > 255
        || name.starts_with(['-', '/', '.'])
        || name.ends_with(['/', '.'])
        || name.ends_with(".lock")
        || name.contains("..")
        || name.contains("//")
        || name.contains("@{")
        || name.chars().any(bad_char);
    anyhow::ensure!(!invalid, "`{name}` is not a valid ref name");
    Ok(())
}

/// `n` of `pr/<n>`.
fn pr_number(name: &str) -> Option<&str> {
    name.strip_prefix("pr/").filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

fn is_hex(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The full ref names a short name may denote, in resolution order.
fn candidates(name: &str) -> Vec<(String, RefKind)> {
    if name.starts_with("refs/") {
        let kind = if name.starts_with("refs/tags/") { RefKind::Tag } else { RefKind::Branch };
        return vec![(name.to_string(), kind)];
    }
    let mut out =
        vec![(format!("refs/heads/{name}"), RefKind::Branch), (format!("refs/remotes/origin/{name}"), RefKind::Branch)];
    if name.contains('/') {
        out.push((format!("refs/remotes/{name}"), RefKind::Branch));
    }
    out.push((format!("refs/tags/{name}"), RefKind::Tag));
    out
}

/// The commit full ref `full` points to (tags peeled); `None` when it does
/// not exist.
fn peel(repo: &gix::Repository, full: &str) -> anyhow::Result<Option<String>> {
    let found = repo.try_find_reference(full).map_err(|e| anyhow::anyhow!("reading {full}: {e}"))?;
    let Some(mut r) = found else { return Ok(None) };
    let commit = r.peel_to_commit().map_err(|e| anyhow::anyhow!("{full} does not point to a commit: {e}"))?;
    Ok(Some(commit.id.to_string()))
}

/// Resolution without the network: refs, `HEAD`, then a commit id.
fn resolve_local(repo: &gix::Repository, name: &str) -> anyhow::Result<Option<ResolvedRef>> {
    if name == "HEAD" {
        let head = repo.head_commit().map_err(|e| anyhow::anyhow!("reading HEAD: {e}"))?;
        return Ok(Some(commit_ref(head.id.to_string())));
    }
    for (full, kind) in candidates(name) {
        if let Some(commit) = peel(repo, &full)? {
            return Ok(Some(ResolvedRef { name: name.to_string(), kind, commit, source: full, fetched: false }));
        }
    }
    Ok(commit_by_prefix(repo, name)?.map(commit_ref))
}

/// A commit named by its id, recorded under its 12-digit form.
fn commit_ref(commit: String) -> ResolvedRef {
    let name = commit[..SHORT_COMMIT.min(commit.len())].to_string();
    ResolvedRef { name, kind: RefKind::Commit, commit, source: "commit".into(), fetched: false }
}

/// The commit whose id starts with `name` (≥ 7 hex digits); `None` when no
/// object has that prefix; an error when several do or it is not a commit.
fn commit_by_prefix(repo: &gix::Repository, name: &str) -> anyhow::Result<Option<String>> {
    if !is_hex(name) || name.len() < MIN_COMMIT_PREFIX || name.len() > 40 {
        return Ok(None);
    }
    let prefix = gix::hash::Prefix::from_hex(name).map_err(|e| anyhow::anyhow!("`{name}`: {e}"))?;
    let found = repo.objects.lookup_prefix(prefix, None).map_err(|e| anyhow::anyhow!("looking up `{name}`: {e}"))?;
    let id = match found {
        None => return Ok(None),
        Some(Err(())) => anyhow::bail!("commit prefix `{name}` is ambiguous; give more digits"),
        Some(Ok(id)) => id,
    };
    let object = repo.find_object(id).map_err(|e| anyhow::anyhow!("reading {id}: {e}"))?;
    let commit =
        object.peel_to_commit().map_err(|_| anyhow::anyhow!("`{name}` names an object that is not a commit"))?;
    Ok(Some(commit.id.to_string()))
}

/// Whether the repository has an `origin` remote to fetch from.
fn has_origin(repo: &gix::Repository) -> bool {
    repo.config_snapshot().string("remote.origin.url").is_some()
}

/// `git fetch origin +<src>:<dst>` (with `--depth 1` in a shallow
/// repository).
fn fetch(repo: &gix::Repository, root: &Path, src: &str, dst: &str) -> anyhow::Result<()> {
    let refspec = format!("+{src}:{dst}");
    let mut args = vec!["fetch", "--quiet", "--no-tags", "--no-write-fetch-head"];
    if repo.is_shallow().unwrap_or(false) {
        args.extend(["--depth", "1"]);
    }
    args.extend(["origin", refspec.as_str()]);
    crate::org::git(&args, Some(root))
}

/// A branch found nowhere locally: fetched from `origin` into its
/// remote-tracking ref.
fn resolve_fetched_branch(repo: &gix::Repository, root: &Path, name: &str) -> anyhow::Result<ResolvedRef> {
    let mut tried: Vec<String> = candidates(name).into_iter().map(|(full, _)| full).collect();
    if is_hex(name) && name.len() >= MIN_COMMIT_PREFIX {
        tried.push("a commit".into());
    }
    let tried = tried.join(", ");
    let missing = || format!("ref `{name}` not found in {}", root.display());
    if name.starts_with("refs/") || !has_origin(repo) {
        anyhow::bail!("{} (tried {tried}; no `origin` remote to fetch from)", missing());
    }
    let dst = format!("refs/remotes/origin/{name}");
    fetch(repo, root, &format!("refs/heads/{name}"), &dst)
        .map_err(|e| anyhow::anyhow!("{} (tried {tried}, fetch from origin: {e:#})", missing()))?;
    let commit = peel(&open_repo(root)?, &dst)?.ok_or_else(|| anyhow::anyhow!("{} after fetching {dst}", missing()))?;
    Ok(ResolvedRef { name: name.to_string(), kind: RefKind::Branch, commit, source: dst, fetched: true })
}

/// `pr/<n>`: fetched from `origin` (GitHub's `refs/pull/<n>/head`, then
/// GitLab's `refs/merge-requests/<n>/head`) into `refs/rueca/pr/<n>`; the
/// previously fetched head when the fetch fails.
fn resolve_pr(repo: &gix::Repository, root: &Path, n: &str) -> anyhow::Result<ResolvedRef> {
    let name = format!("pr/{n}");
    let dst = format!("refs/rueca/pr/{n}");
    let sources = [format!("refs/pull/{n}/head"), format!("refs/merge-requests/{n}/head")];
    let mut errors = Vec::new();
    if has_origin(repo) {
        for src in &sources {
            match fetch(repo, root, src, &dst) {
                Ok(()) => {
                    let commit = peel(&open_repo(root)?, &dst)?
                        .ok_or_else(|| anyhow::anyhow!("{dst} missing after fetching {src}"))?;
                    return Ok(ResolvedRef { name, kind: RefKind::Pr, commit, source: src.clone(), fetched: true });
                }
                Err(e) => errors.push(format!("{e:#}")),
            }
        }
    } else {
        errors.push("no `origin` remote".into());
    }
    if let Some(commit) = peel(repo, &dst)? {
        warn!(%name, "cannot fetch {name} from origin; using the head fetched earlier");
        return Ok(ResolvedRef { name, kind: RefKind::Pr, commit, source: dst, fetched: false });
    }
    anyhow::bail!(
        "ref `{name}` not found in {} (tried {} from origin): {}",
        root.display(),
        sources.join(", "),
        errors.join("; ")
    )
}

/// A filesystem-safe, deterministic directory name for ref `name`: ASCII
/// letters, digits, `.`, `_` and `-` kept, every other run of characters one
/// `-` (`feature/x` → `feature-x-…`, `pr/123` → `pr-123-…`). When anything
/// was replaced or dropped, or the name has upper-case letters
/// (case-insensitive filesystems), the first 8 hex digits of the name's
/// hash are appended, so distinct names never share a directory.
pub fn ref_slug(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let mut slug: String = out.trim_start_matches(['.', '-']).chars().take(MAX_SLUG).collect();
    let changed = slug != name || name.bytes().any(|b| b.is_ascii_uppercase());
    if slug.is_empty() {
        slug.push_str("ref");
    }
    if changed {
        slug.push('-');
        slug.push_str(&git_blob_sha(name.as_bytes())[..8]);
    }
    slug
}

/// `<store>/refs/<repo-slug>/<ref-slug>/`: where ref `name` of repository
/// `repo_id` is exported ([`rueca_segment::repo_slug`], [`ref_slug`]).
pub fn export_dir(store: &Path, repo_id: &str, name: &str) -> PathBuf {
    store.join("refs").join(rueca_segment::repo_slug(repo_id)).join(ref_slug(name))
}

/// What [`export`] wrote.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ExportReport {
    pub files: u64,
    pub bytes: u64,
}

/// One blob of the exported tree.
struct ExportEntry {
    rel: String,
    oid: gix::ObjectId,
}

/// Writes the tree of `commit` (the subtree at `repo_dir`'s path when it is a
/// subdirectory of its working tree) to `dest`, replacing what was there.
/// Regular and executable files only; nothing is written into the
/// repository.
pub fn export(repo_dir: &Path, commit: &str, dest: &Path) -> anyhow::Result<ExportReport> {
    let root = fs::canonicalize(repo_dir).with_context(|| format!("cannot access {}", repo_dir.display()))?;
    let (repo, prefix) =
        git::discover(&root).ok_or_else(|| anyhow::anyhow!("{} is not inside a git repository", root.display()))?;
    let entries = tree_entries(&repo, commit, &prefix)?;
    let parent = dest.parent().context("export directory has no parent")?;
    fs::create_dir_all(parent)?;
    let tmp = tempfile::Builder::new().prefix(".export-").tempdir_in(parent)?;
    let report = write_entries(&repo, &entries, tmp.path())?;
    if dest.exists() {
        fs::remove_dir_all(dest).with_context(|| format!("removing the previous export {}", dest.display()))?;
    }
    fs::rename(tmp.path(), dest).with_context(|| format!("moving the export to {}", dest.display()))?;
    Ok(report)
}

/// The regular files of `commit`'s tree under `prefix` (relative to it).
fn tree_entries(repo: &gix::Repository, commit: &str, prefix: &str) -> anyhow::Result<Vec<ExportEntry>> {
    let id = gix::ObjectId::from_hex(commit.as_bytes()).map_err(|e| anyhow::anyhow!("commit `{commit}`: {e}"))?;
    let commit_obj = repo.find_commit(id).map_err(|e| anyhow::anyhow!("reading commit {commit}: {e}"))?;
    let tree = commit_obj.tree().map_err(|e| anyhow::anyhow!("reading the tree of {commit}: {e}"))?;
    let mut recorder = gix::traverse::tree::Recorder::default();
    tree.traverse().breadthfirst(&mut recorder).map_err(|e| anyhow::anyhow!("walking the tree of {commit}: {e}"))?;
    let mut out = Vec::new();
    for entry in recorder.records {
        if !matches!(
            entry.mode.kind(),
            gix::object::tree::EntryKind::Blob | gix::object::tree::EntryKind::BlobExecutable
        ) {
            continue;
        }
        let Ok(path) = std::str::from_utf8(entry.filepath.as_ref()) else { continue };
        if let Some(rel) = under_prefix(path, prefix).filter(|r| safe_rel(r)) {
            out.push(ExportEntry { rel: rel.to_string(), oid: entry.oid });
        }
    }
    Ok(out)
}

/// `path` relative to directory `prefix` (`""` = the top).
fn under_prefix<'a>(path: &'a str, prefix: &str) -> Option<&'a str> {
    if prefix.is_empty() {
        return Some(path);
    }
    path.strip_prefix(prefix).and_then(|r| r.strip_prefix('/'))
}

/// A relative path that stays inside the export (no `.`, `..`, `.git` or
/// empty components).
fn safe_rel(rel: &str) -> bool {
    rel.split('/').all(|c| !matches!(c, "" | "." | ".." | ".git") && !c.contains('\\'))
}

/// Creates the directories, then writes the blobs in parallel.
fn write_entries(repo: &gix::Repository, entries: &[ExportEntry], dest: &Path) -> anyhow::Result<ExportReport> {
    let dirs: BTreeSet<&str> = entries.iter().filter_map(|e| e.rel.rsplit_once('/').map(|(d, _)| d)).collect();
    for d in dirs {
        fs::create_dir_all(dest.join(d))?;
    }
    // gix is built without its `parallel` feature (its handles are not
    // `Sync`): each worker opens the repository once.
    let git_dir = repo.git_dir().to_path_buf();
    let bytes = entries
        .par_iter()
        .map_init(
            || gix::open(&git_dir).map_err(|e| e.to_string()),
            |repo, e| -> anyhow::Result<u64> {
                let repo = repo.as_ref().map_err(|err| anyhow::anyhow!("opening {}: {err}", git_dir.display()))?;
                let blob = repo.find_object(e.oid).map_err(|err| anyhow::anyhow!("reading {}: {err}", e.rel))?;
                fs::write(dest.join(&e.rel), &blob.data).with_context(|| format!("writing {}", e.rel))?;
                Ok(blob.data.len() as u64)
            },
        )
        .collect::<anyhow::Result<Vec<u64>>>()?;
    Ok(ExportReport { files: bytes.len() as u64, bytes: bytes.iter().sum() })
}

/// Options of [`index_ref`].
#[derive(Debug, Clone, Default)]
pub struct RefIndexOptions {
    /// Working tree (or a directory inside it) of the source repository;
    /// the ref snapshot covers the same directory.
    pub root: PathBuf,
    /// Ref to index: a branch, `pr/<n>`, a tag or a commit.
    pub name: String,
    /// Store directory.
    pub store: PathBuf,
    /// Repository id override (as `IndexOptions::repo`); default: the
    /// source root's id ([`crate::repo_id`]).
    pub repo: Option<String>,
    pub budget: Budget,
    /// Worker threads; `None` = available parallelism.
    pub threads: Option<usize>,
}

/// What [`index_ref`] did.
#[derive(Debug, Clone, Serialize)]
pub struct RefIndexReport {
    /// The ref name the snapshot records.
    #[serde(rename = "ref")]
    pub ref_name: String,
    pub kind: RefKind,
    /// Full commit of the snapshot.
    pub commit: String,
    pub repo: String,
    /// What the name resolved through (`refs/heads/x`, `commit`, …).
    pub source: String,
    /// The ref was fetched from `origin`.
    pub fetched: bool,
    /// The export directory (the snapshot's root).
    pub export_dir: String,
    /// Same commit as the snapshot already indexed: nothing was exported or
    /// indexed (`index` describes the existing segment, with zero counts of
    /// work done).
    pub unchanged: bool,
    pub export: ExportReport,
    pub resolve_ms: u64,
    pub export_ms: u64,
    /// The whole run (resolution, export and indexing).
    pub duration_ms: u64,
    /// The indexing report of the export.
    pub index: IndexReport,
}

/// Resolves, exports and indexes a ref of `opts.root`'s repository (see the
/// module docs); a no-op when the snapshot is already at that commit.
pub fn index_ref(opts: &RefIndexOptions) -> anyhow::Result<RefIndexReport> {
    let started = Instant::now();
    let root = fs::canonicalize(&opts.root).with_context(|| format!("cannot access {}", opts.root.display()))?;
    let t = Instant::now();
    let resolved = resolve(&root, &opts.name)?;
    let resolve_ms = t.elapsed().as_millis() as u64;
    let repo = crate::repo_id(&root, opts.repo.as_deref());
    let store = Store::open(&opts.store).with_context(|| format!("cannot open store {}", opts.store.display()))?;
    let dest = export_dir(&fs::canonicalize(&opts.store)?, &repo, &resolved.name);
    let existing = entry_at(&store, &dest)?;
    check_owner(existing.as_ref(), &repo, &resolved.name, &dest)?;
    let base = ReportBase { resolved, repo, dest, resolve_ms, started };
    match existing.as_ref().and_then(|e| current_report(&store, e, &base.resolved, &base.dest)) {
        Some(index) => {
            info!(repo = %base.repo, r#ref = %base.resolved.name, commit = %base.resolved.commit, "ref snapshot unchanged");
            Ok(base.finish(index, ExportReport::default(), 0, true))
        }
        None => reindex(opts, &root, base),
    }
}

/// An export directory holds one ref of one repository: a different one
/// there (two repository ids with the same slug) is an error, not an
/// overwrite.
fn check_owner(existing: Option<&CatalogEntry>, repo: &str, name: &str, dest: &Path) -> anyhow::Result<()> {
    match existing {
        Some(e) if e.repo != repo || e.ref_name.as_deref() != Some(name) => anyhow::bail!(
            "{} already holds a snapshot of `{}` (ref {}); remove it first",
            dest.display(),
            e.repo,
            e.ref_name.as_deref().unwrap_or("none")
        ),
        _ => Ok(()),
    }
}

/// What [`index_ref`] knows before exporting.
struct ReportBase {
    resolved: ResolvedRef,
    repo: String,
    dest: PathBuf,
    resolve_ms: u64,
    started: Instant,
}

impl ReportBase {
    fn finish(self, index: IndexReport, export: ExportReport, export_ms: u64, unchanged: bool) -> RefIndexReport {
        RefIndexReport {
            ref_name: self.resolved.name,
            kind: self.resolved.kind,
            commit: self.resolved.commit,
            repo: self.repo,
            source: self.resolved.source,
            fetched: self.resolved.fetched,
            export_dir: self.dest.to_string_lossy().into_owned(),
            unchanged,
            export,
            resolve_ms: self.resolve_ms,
            export_ms,
            duration_ms: self.started.elapsed().as_millis() as u64,
            index,
        }
    }
}

/// Exports and indexes the ref.
fn reindex(opts: &RefIndexOptions, root: &Path, base: ReportBase) -> anyhow::Result<RefIndexReport> {
    let t = Instant::now();
    let export = export(root, &base.resolved.commit, &base.dest)?;
    let export_ms = t.elapsed().as_millis() as u64;
    let index = index_worktree(&IndexOptions {
        root: base.dest.clone(),
        store: opts.store.clone(),
        repo: Some(base.repo.clone()),
        budget: opts.budget.clone(),
        threads: opts.threads,
        ref_name: Some(base.resolved.name.clone()),
        commit: Some(base.resolved.commit.clone()),
    })
    .with_context(|| format!("indexing ref `{}` exported at {}", base.resolved.name, base.dest.display()))?;
    info!(
        repo = %base.repo,
        r#ref = %base.resolved.name,
        commit = %base.resolved.commit,
        files = export.files,
        export_ms,
        index_ms = index.duration_ms,
        "ref snapshot indexed"
    );
    Ok(base.finish(index, export, export_ms, false))
}

/// The catalog entry whose root is `dir`.
fn entry_at(store: &Store, dir: &Path) -> anyhow::Result<Option<CatalogEntry>> {
    let root = dir.to_string_lossy();
    Ok(store.entries()?.into_iter().find(|e| e.root == root))
}

/// A report of the existing snapshot of entry `e` when it is current: same
/// commit, written by this Rueca version, export still on disk. `None`
/// means re-export and re-index.
fn current_report(store: &Store, e: &CatalogEntry, resolved: &ResolvedRef, dest: &Path) -> Option<IndexReport> {
    if !dest.is_dir() {
        return None;
    }
    let seg = store.open_segment(&e.segment_id).ok()?;
    let meta = seg.meta();
    let snap = &meta.snapshot;
    let current = snap.commit == resolved.commit && snap.rueca_version == env!("CARGO_PKG_VERSION");
    current.then(|| IndexReport {
        segment: meta.clone(),
        root: e.root.clone(),
        files_total: u64::from(meta.file_count),
        files_extracted: 0,
        files_skipped: 0,
        facts_cache_hits: 0,
        references_total: 0,
        references_resolved: 0,
        duration_ms: 0,
        files_partial: 0,
        files_unsupported: 0,
        resolved_by_strategy: Default::default(),
        references_builtin: 0,
        references_external: 0,
        timings: PhaseTimings::default(),
    })
}

/// What [`remove_ref`] removed.
#[derive(Debug, Clone, Serialize)]
pub struct RefRemoval {
    #[serde(rename = "ref")]
    pub ref_name: String,
    pub repo: String,
    /// The export directory.
    pub export_dir: String,
    /// The catalog entry's segment, when there was an entry.
    pub segment_id: Option<String>,
    /// The export directory existed and was deleted.
    pub export_removed: bool,
}

/// Drops ref snapshot `name` of repository `repo_id`: its catalog entry
/// ([`Store::unregister`], which deletes the segment unless another entry
/// or a kept organization generation references it) and its export.
/// `name` is the ref as indexed (a commit may be given by any prefix of at
/// least 7 digits). An error when the repository has no such ref snapshot.
pub fn remove_ref(store_dir: &Path, repo_id: &str, name: &str) -> anyhow::Result<RefRemoval> {
    let store = Store::open(store_dir)?;
    let store_dir = fs::canonicalize(store_dir)?;
    let refs: Vec<CatalogEntry> =
        store.entries()?.into_iter().filter(|e| e.repo == repo_id && e.is_ref_export()).collect();
    let entry = refs.iter().find(|e| e.ref_name.as_deref().is_some_and(|r| names_ref(r, name)));
    let dir = entry.map_or_else(|| export_dir(&store_dir, repo_id, name), |e| PathBuf::from(&e.root));
    let removed = match entry {
        Some(e) => store.unregister(&e.root)?,
        None => None,
    };
    let refs_dir = store_dir.join("refs");
    let export_removed = dir.starts_with(&refs_dir) && dir.is_dir();
    if export_removed {
        fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))?;
        if let Some(parent) = dir.parent() {
            // The repository's directory goes with its last ref.
            let _ = fs::remove_dir(parent);
        }
    }
    if removed.is_none() && !export_removed {
        let names: Vec<&str> = refs.iter().filter_map(|e| e.ref_name.as_deref()).collect();
        let names = if names.is_empty() { "none".to_string() } else { names.join(", ") };
        anyhow::bail!("ref `{name}` of repository `{repo_id}` is not indexed; indexed refs: {names}");
    }
    Ok(RefRemoval {
        ref_name: entry.and_then(|e| e.ref_name.clone()).unwrap_or_else(|| name.to_string()),
        repo: repo_id.to_string(),
        export_dir: dir.to_string_lossy().into_owned(),
        segment_id: removed.map(|e| e.segment_id),
        export_removed,
    })
}

/// Whether `given` names the ref recorded as `recorded`: equal, or both
/// commits with one a prefix (≥ 7 digits) of the other.
fn names_ref(recorded: &str, given: &str) -> bool {
    if recorded == given {
        return true;
    }
    let (r, g) = (recorded.to_ascii_lowercase(), given.to_ascii_lowercase());
    is_hex(&r) && is_hex(&g) && g.len() >= MIN_COMMIT_PREFIX && (r.starts_with(&g) || g.starts_with(&r))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_are_safe_and_distinct() {
        assert_eq!(ref_slug("main"), "main");
        assert_eq!(ref_slug("v1.2.0"), "v1.2.0");
        let fx = ref_slug("feature/x");
        assert!(fx.starts_with("feature-x-") && fx.len() == "feature-x-".len() + 8, "{fx}");
        assert!(ref_slug("pr/123").starts_with("pr-123-"));
        assert_ne!(ref_slug("feature/x"), ref_slug("feature-x"));
        assert_ne!(ref_slug("Main"), ref_slug("main"));
        assert_eq!(ref_slug("feature/x"), fx, "deterministic");
        let long = "a/".repeat(100);
        assert!(ref_slug(&long).len() <= MAX_SLUG + 9);
        let p = export_dir(Path::new("/s"), "github.com/o/r", "main");
        assert_eq!(p, PathBuf::from("/s/refs/github-com-o-r/main"));
    }

    #[test]
    fn ref_names_and_matching() {
        for ok in ["main", "feature/x", "pr/7", "v1.2", "refs/heads/x", "origin/main", "HEAD", "abc1234"] {
            assert!(validate_ref_name(ok).is_ok(), "{ok}");
        }
        for bad in ["", "-x", "a..b", "a b", "x:y", "x~1", "x^", "a//b", "x/", ".x", "x.lock", "@{u}", "a\\b"] {
            assert!(validate_ref_name(bad).is_err(), "{bad:?}");
        }
        assert_eq!(pr_number("pr/12"), Some("12"));
        assert_eq!(pr_number("pr/x"), None);
        assert_eq!(pr_number("pr/"), None);
        assert!(names_ref("feature/x", "feature/x"));
        assert!(names_ref("7696364aef89", "7696364"));
        assert!(names_ref("7696364aef89", "7696364AEF89a22e"));
        assert!(!names_ref("7696364aef89", "769636"));
        assert!(!names_ref("feature/x", "feature"));
        assert_eq!(under_prefix("web/a.ts", "web"), Some("a.ts"));
        assert_eq!(under_prefix("webx/a.ts", "web"), None);
        assert!(safe_rel("a/b.py") && !safe_rel("a/../b") && !safe_rel(".git/config"));
    }
}
