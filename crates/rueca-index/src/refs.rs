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
//!    +refs/heads/<name>:refs/remotes/origin/<name>`, then the tag
//!    `+refs/tags/<name>:refs/tags/<name>`, then, for a full commit id, the
//!    commit into `refs/rueca/commit/<id>`); `pr/<n>` is always
//!    fetched (`refs/pull/<n>/head`, then `refs/merge-requests/<n>/head`)
//!    into `refs/rueca/pr/<n>`, which keeps the objects reachable and serves
//!    as the offline fallback. Fetches are the only writes into the
//!    repository: objects, remote-tracking refs, tags fetched by name and
//!    `refs/rueca/…`; never a branch, never the working tree or the index.
//!    A shallow repository is fetched with `--depth 1`; `git` never prompts
//!    (`GIT_TERMINAL_PROMPT=0`, no askpass).
//! 2. **Export** ([`export`]) the commit's tree (the root's subtree when the
//!    root is a subdirectory of its repository) to
//!    `<store>/refs/<repo-dir>/<ref-slug>/` ([`export_dir`]; the directory
//!    of a repository names its id and the root's path inside it, so two
//!    subdirectory roots of one repository never share an export), writing every
//!    blob byte for byte from the object database (no `git archive`: its
//!    `export-ignore` / `export-subst` attributes and content filters would
//!    change files and their blob ids). Symlinks and submodules are not
//!    exported (the working-tree walk does not index them either). The export
//!    is written to a temporary directory and swapped in. [`export_with`]
//!    also reads a bare repository (a service's mirror, RFC 0008 §7.7) and,
//!    given a gap size, lists blobs the repository lacks (a partial clone's
//!    filtered-out blobs) instead of failing. An owner marker
//!    (`<repo-dir>/.owners/<ref-slug>.json`: repository id, path inside the
//!    repository, ref) records whose export it is. A commit with no file
//!    under the root's path is an error, not an empty snapshot.
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
//! catalog entry, its segment (unless still referenced) and its export; it
//! deletes only an export that the catalog or the owner marker says is that
//! ref of that repository root.
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

use crate::{AbsentFile, IndexOptions, IndexReport, PhaseTimings, git, index_worktree};

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
/// module docs for the order). Fetches from `origin` when the name is not
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
    resolve_fetched(&repo, &root, name)
}

/// The repository containing `root`, or an error naming it.
fn open_repo(root: &Path) -> anyhow::Result<gix::Repository> {
    git::discover(root)
        .map(|(repo, _)| repo)
        .ok_or_else(|| anyhow::anyhow!("{} is not inside a git repository", root.display()))
}

/// Rejects names git would not accept as a ref (and anything that could be
/// read as an option or a refspec).
pub(crate) fn validate_ref_name(name: &str) -> anyhow::Result<()> {
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
pub(crate) fn pr_number(name: &str) -> Option<&str> {
    name.strip_prefix("pr/").filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

pub(crate) fn is_hex(s: &str) -> bool {
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

/// A name found nowhere locally, fetched from `origin`: a full commit id as
/// that commit (into `refs/rueca/commit/<id>`, which keeps it reachable),
/// else a branch (into its remote-tracking ref), else a tag (into
/// `refs/tags/<name>`).
fn resolve_fetched(repo: &gix::Repository, root: &Path, name: &str) -> anyhow::Result<ResolvedRef> {
    let mut tried: Vec<String> = candidates(name).into_iter().map(|(full, _)| full).collect();
    if is_hex(name) && name.len() >= MIN_COMMIT_PREFIX {
        tried.push("a commit".into());
    }
    let tried = tried.join(", ");
    let missing = || format!("ref `{name}` not found in {}", root.display());
    if name.starts_with("refs/") || !has_origin(repo) {
        anyhow::bail!("{} (tried {tried}; no `origin` remote to fetch from)", missing());
    }
    let mut errors = Vec::new();
    for (src, dst, kind) in remote_sources(name) {
        if let Err(e) = fetch(repo, root, &src, &dst) {
            errors.push(format!("{e:#}"));
            continue;
        }
        let commit =
            peel(&open_repo(root)?, &dst)?.ok_or_else(|| anyhow::anyhow!("{} after fetching {dst}", missing()))?;
        let resolved = match kind {
            RefKind::Commit => commit_ref(commit),
            _ => ResolvedRef { name: name.to_string(), kind, commit, source: String::new(), fetched: false },
        };
        return Ok(ResolvedRef { source: dst, fetched: true, ..resolved });
    }
    anyhow::bail!("{} (tried {tried}, fetch from origin: {})", missing(), errors.join("; "))
}

/// What [`resolve_fetched`] fetches for `name`, in order: `(source on the
/// remote, local destination, kind)`.
fn remote_sources(name: &str) -> Vec<(String, String, RefKind)> {
    let mut out = Vec::with_capacity(3);
    if name.len() == 40 && is_hex(name) {
        let id = name.to_ascii_lowercase();
        out.push((id.clone(), format!("refs/rueca/commit/{id}"), RefKind::Commit));
    }
    out.push((format!("refs/heads/{name}"), format!("refs/remotes/origin/{name}"), RefKind::Branch));
    out.push((format!("refs/tags/{name}"), format!("refs/tags/{name}"), RefKind::Tag));
    out
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

/// The path of `root` inside the working tree of its git repository (`/`
/// separators, empty at the top or outside git): what tells two
/// subdirectory roots of one repository apart.
pub fn root_prefix(root: &Path) -> String {
    let root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    git::discover(&root).map(|(_, prefix)| prefix).unwrap_or_default()
}

/// `<store>/refs/<repo-dir>/`: the directory holding the ref exports of
/// repository `repo_id` at path `prefix` inside it ([`root_prefix`]).
/// `<repo-dir>` is the id's slug ([`rueca_segment::repo_slug`]), then
/// `--<prefix slug>` for a subdirectory root, then the first 8 hex digits
/// of the hash of the id and the prefix, so that ids with the same slug and
/// different roots of one repository never share it
/// (`github.com/o/r` → `github-com-o-r-<hash>`, its root `web` →
/// `github-com-o-r--web-<hash>`).
pub fn repo_export_dir(store: &Path, repo_id: &str, prefix: &str) -> PathBuf {
    store.join("refs").join(repo_dir_name(repo_id, prefix))
}

/// `<repo-dir>`, the directory name of repository `repo_id` at path
/// `prefix` inside it (see [`repo_export_dir`]); also the per-repository
/// directory of a service store (RFC 0008 §5.1).
pub fn repo_dir_name(repo_id: &str, prefix: &str) -> String {
    let mut dir = rueca_segment::repo_slug(repo_id);
    let mut key = repo_id.to_string();
    if !prefix.is_empty() {
        dir.push_str("--");
        dir.push_str(&rueca_segment::repo_slug(prefix));
        key.push('\n');
        key.push_str(prefix);
    }
    dir.push('-');
    dir.push_str(&git_blob_sha(key.as_bytes())[..8]);
    dir
}

/// `<store>/refs/<repo-dir>/<ref-slug>/`: where ref `name` of repository
/// `repo_id` at path `prefix` inside it is exported ([`repo_export_dir`],
/// [`ref_slug`]).
pub fn export_dir(store: &Path, repo_id: &str, prefix: &str, name: &str) -> PathBuf {
    repo_export_dir(store, repo_id, prefix).join(ref_slug(name))
}

/// Who an export directory belongs to: written next to it as
/// `<repo-dir>/.owners/<ref-slug>.json` (a ref slug never starts with `.`,
/// so it never names an export; the export itself is indexed, so the marker
/// stays out of it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
struct RefOwner {
    repo: String,
    /// [`root_prefix`] of the source root.
    prefix: String,
    #[serde(rename = "ref")]
    ref_name: String,
}

impl RefOwner {
    /// The marker of export directory `dir`.
    fn path(dir: &Path) -> Option<PathBuf> {
        let parent = dir.parent()?;
        let name = dir.file_name()?.to_str()?;
        Some(parent.join(".owners").join(format!("{name}.json")))
    }

    /// The marker of `dir`, if there is a readable one.
    fn read(dir: &Path) -> Option<RefOwner> {
        let text = fs::read(RefOwner::path(dir)?).ok()?;
        serde_json::from_slice(&text).ok()
    }

    fn write(&self, dir: &Path) -> anyhow::Result<()> {
        let path = RefOwner::path(dir).context("export directory has no parent")?;
        if let Some(d) = path.parent() {
            fs::create_dir_all(d)?;
        }
        fs::write(&path, serde_json::to_vec(self)?).with_context(|| format!("writing {}", path.display()))
    }

    /// Whether this marker says ref `name` (any spelling [`names_ref`]
    /// accepts) of repository `repo` at `prefix`.
    fn names(&self, repo: &str, prefix: &str, name: &str) -> bool {
        self.repo == repo && self.prefix == prefix && names_ref(&self.ref_name, name)
    }
}

/// What [`export`] wrote.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ExportReport {
    pub files: u64,
    pub bytes: u64,
    /// Blobs the repository does not have, not written ([`export_with`]
    /// with a gap size; a partial clone's filtered-out blobs).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub absent: Vec<AbsentFile>,
}

/// One blob of the exported tree.
struct ExportEntry {
    rel: String,
    oid: gix::ObjectId,
}

/// Writes the tree of `commit` (the subtree at `repo_dir`'s path when it is a
/// subdirectory of its working tree) to `dest`, replacing what was there.
/// Regular and executable files only; nothing is written into the
/// repository. A commit with no such file there is an error (the root's
/// directory does not exist at that commit), and `dest` is left as it was.
/// `repo_dir` may also be a bare repository (the whole tree is exported).
/// A blob the repository does not have is an error.
pub fn export(repo_dir: &Path, commit: &str, dest: &Path) -> anyhow::Result<ExportReport> {
    export_with(repo_dir, commit, dest, None)
}

/// [`export`]; with `gap_size`, a blob the repository does not have (a
/// partial clone's filter left it out: blobs of at least `gap_size` bytes,
/// RFC 0008 §7.6) is not an error but listed in [`ExportReport::absent`]
/// with `gap_size` as its size, to be indexed as a coverage gap
/// ([`crate::index_worktree_with`]). Objects are read with `gix`, which
/// never fetches a missing one.
pub fn export_with(repo_dir: &Path, commit: &str, dest: &Path, gap_size: Option<u64>) -> anyhow::Result<ExportReport> {
    let root = fs::canonicalize(repo_dir).with_context(|| format!("cannot access {}", repo_dir.display()))?;
    let (repo, prefix) = git::discover_or_bare(&root)
        .ok_or_else(|| anyhow::anyhow!("{} is not inside a git repository", root.display()))?;
    let entries = tree_entries(&repo, commit, &prefix)?;
    if entries.is_empty() {
        let at = if prefix.is_empty() { "the top of the repository".to_string() } else { format!("`{prefix}`") };
        anyhow::bail!("commit {} has no files under {at}", &commit[..SHORT_COMMIT.min(commit.len())]);
    }
    let parent = dest.parent().context("export directory has no parent")?;
    fs::create_dir_all(parent)?;
    let tmp = tempfile::Builder::new().prefix(".export-").tempdir_in(parent)?;
    let report = write_entries(&repo, &entries, tmp.path(), gap_size)?;
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

/// Creates the directories, then writes the blobs in parallel; with
/// `gap_size`, missing blobs are listed instead of failing.
fn write_entries(
    repo: &gix::Repository,
    entries: &[ExportEntry],
    dest: &Path,
    gap_size: Option<u64>,
) -> anyhow::Result<ExportReport> {
    let dirs: BTreeSet<&str> = entries.iter().filter_map(|e| e.rel.rsplit_once('/').map(|(d, _)| d)).collect();
    for d in dirs {
        fs::create_dir_all(dest.join(d))?;
    }
    // gix is built without its `parallel` feature (its handles are not
    // `Sync`): each worker opens the repository once.
    let git_dir = repo.git_dir().to_path_buf();
    let written = entries
        .par_iter()
        .map_init(
            || gix::open(&git_dir).map_err(|e| e.to_string()),
            |repo, e| -> anyhow::Result<Option<u64>> {
                let repo = repo.as_ref().map_err(|err| anyhow::anyhow!("opening {}: {err}", git_dir.display()))?;
                write_blob(repo, e, dest, gap_size.is_some())
            },
        )
        .collect::<anyhow::Result<Vec<Option<u64>>>>()?;
    let mut report = ExportReport::default();
    for (e, w) in entries.iter().zip(&written) {
        match w {
            Some(n) => {
                report.files += 1;
                report.bytes += n;
            }
            None => report.absent.push(AbsentFile {
                path: e.rel.clone(),
                blob_sha: e.oid.to_string(),
                size: gap_size.unwrap_or_default(),
            }),
        }
    }
    Ok(report)
}

/// Writes one blob; `None` when the repository does not have it and gaps
/// are allowed.
fn write_blob(repo: &gix::Repository, e: &ExportEntry, dest: &Path, gaps: bool) -> anyhow::Result<Option<u64>> {
    let blob = if gaps {
        match repo.try_find_object(e.oid).map_err(|err| anyhow::anyhow!("reading {}: {err}", e.rel))? {
            Some(b) => b,
            None => return Ok(None),
        }
    } else {
        repo.find_object(e.oid).map_err(|err| anyhow::anyhow!("reading {}: {err}", e.rel))?
    };
    fs::write(dest.join(&e.rel), &blob.data).with_context(|| format!("writing {}", e.rel))?;
    Ok(Some(blob.data.len() as u64))
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
    let owner = RefOwner { repo, prefix: root_prefix(&root), ref_name: resolved.name.clone() };
    let dest = export_dir(&fs::canonicalize(&opts.store)?, &owner.repo, &owner.prefix, &resolved.name);
    let existing = entry_at(&store, &dest)?;
    check_owner(existing.as_ref(), &owner, &dest)?;
    let base = ReportBase { resolved, repo: owner.repo.clone(), dest, resolve_ms, started };
    match existing.as_ref().and_then(|e| current_report(&store, e, &base.resolved, &base.dest)) {
        Some(index) => {
            info!(repo = %base.repo, r#ref = %base.resolved.name, commit = %base.resolved.commit, "ref snapshot unchanged");
            Ok(base.finish(index, ExportReport::default(), 0, true))
        }
        None => reindex(opts, &root, &owner, base),
    }
}

/// An export directory holds one ref of one repository root: a different
/// one there, by its catalog entry or its owner marker (two repository ids
/// or roots whose directory names collide), is an error, not an overwrite.
fn check_owner(existing: Option<&CatalogEntry>, owner: &RefOwner, dest: &Path) -> anyhow::Result<()> {
    if let Some(e) = existing
        && (e.repo != owner.repo || e.ref_name.as_deref() != Some(owner.ref_name.as_str()))
    {
        anyhow::bail!(
            "{} already holds a snapshot of `{}` (ref {}); remove it first",
            dest.display(),
            e.repo,
            e.ref_name.as_deref().unwrap_or("none")
        );
    }
    match RefOwner::read(dest) {
        Some(o) if o != *owner => anyhow::bail!(
            "{} already holds ref {} of `{}` at `{}`; remove it first",
            dest.display(),
            o.ref_name,
            o.repo,
            if o.prefix.is_empty() { "." } else { o.prefix.as_str() }
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

/// Exports the ref, records its owner and indexes it.
fn reindex(opts: &RefIndexOptions, root: &Path, owner: &RefOwner, base: ReportBase) -> anyhow::Result<RefIndexReport> {
    let t = Instant::now();
    let export = export(root, &base.resolved.commit, &base.dest)
        .map_err(|e| anyhow::anyhow!("ref `{}`: {e:#}", base.resolved.name))?;
    owner.write(&base.dest)?;
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

/// Drops ref snapshot `name` of repository `repo_id` at path `prefix`
/// inside it ([`root_prefix`] of the root it was indexed from): its catalog
/// entry ([`Store::unregister`], which deletes the segment unless another
/// entry or a kept organization generation references it) and its export.
/// `name` is the ref as indexed (a commit may be given by any prefix of at
/// least 7 digits). Only a directory that the catalog or its owner marker
/// says is that ref of that root is deleted. An error when the root has no
/// such ref snapshot.
pub fn remove_ref(store_dir: &Path, repo_id: &str, prefix: &str, name: &str) -> anyhow::Result<RefRemoval> {
    let store = Store::open(store_dir)?;
    let store_dir = fs::canonicalize(store_dir)?;
    let repo_dir = repo_export_dir(&store_dir, repo_id, prefix);
    let refs: Vec<CatalogEntry> = store
        .entries()?
        .into_iter()
        .filter(|e| e.repo == repo_id && e.is_ref_export() && Path::new(&e.root).parent() == Some(repo_dir.as_path()))
        .collect();
    let entry = refs.iter().find(|e| e.ref_name.as_deref().is_some_and(|r| names_ref(r, name)));
    let owned = || owned_export(&repo_dir, repo_id, prefix, name);
    let (dir, recorded) = match entry {
        Some(e) => (Some(PathBuf::from(&e.root)), e.ref_name.clone()),
        None => owned().map_or((None, None), |(d, o)| (Some(d), Some(o.ref_name))),
    };
    let removed = match entry {
        Some(e) => store.unregister(&e.root)?,
        None => None,
    };
    let export_removed = match &dir {
        Some(d) if d.starts_with(&repo_dir) && d.is_dir() => {
            fs::remove_dir_all(d).with_context(|| format!("removing {}", d.display()))?;
            true
        }
        _ => false,
    };
    if let Some(d) = &dir {
        drop_owner(&repo_dir, d);
    }
    if removed.is_none() && !export_removed {
        let names: Vec<&str> = refs.iter().filter_map(|e| e.ref_name.as_deref()).collect();
        let names = if names.is_empty() { "none".to_string() } else { names.join(", ") };
        anyhow::bail!("ref `{name}` of repository `{repo_id}` is not indexed; indexed refs: {names}");
    }
    Ok(RefRemoval {
        ref_name: recorded.unwrap_or_else(|| name.to_string()),
        repo: repo_id.to_string(),
        export_dir: dir.unwrap_or_else(|| export_dir(&store_dir, repo_id, prefix, name)).to_string_lossy().into_owned(),
        segment_id: removed.map(|e| e.segment_id),
        export_removed,
    })
}

/// The export under `repo_dir` whose owner marker names ref `name` of
/// `repo` at `prefix` (an export without a catalog entry), with its marker.
fn owned_export(repo_dir: &Path, repo: &str, prefix: &str, name: &str) -> Option<(PathBuf, RefOwner)> {
    let candidate = repo_dir.join(ref_slug(name));
    if let Some(o) = RefOwner::read(&candidate).filter(|o| o.names(repo, prefix, name)) {
        return Some((candidate, o));
    }
    // A commit given by another prefix than the one recorded.
    let markers = fs::read_dir(repo_dir.join(".owners")).ok()?;
    markers.flatten().find_map(|m| {
        let file = m.file_name();
        let slug = file.to_str()?.strip_suffix(".json")?;
        let dir = repo_dir.join(slug);
        RefOwner::read(&dir).filter(|o| o.names(repo, prefix, name)).map(|o| (dir, o))
    })
}

/// Removes the owner marker of export `dir`, then the marker directory and
/// the repository's directory when they are empty (the last ref gone).
fn drop_owner(repo_dir: &Path, dir: &Path) {
    if let Some(marker) = RefOwner::path(dir) {
        let _ = fs::remove_file(marker);
    }
    let _ = fs::remove_dir(repo_dir.join(".owners"));
    let _ = fs::remove_dir(repo_dir);
}

/// Whether `given` names the ref recorded as `recorded`: equal, or both
/// commits with one a prefix (≥ 7 digits) of the other.
pub(crate) fn names_ref(recorded: &str, given: &str) -> bool {
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
        let p = export_dir(Path::new("/s"), "github.com/o/r", "", "main");
        let hash = &git_blob_sha(b"github.com/o/r")[..8];
        assert_eq!(p, PathBuf::from(format!("/s/refs/github-com-o-r-{hash}/main")));
    }

    #[test]
    fn export_dirs_are_per_repository_and_root() {
        let s = Path::new("/s");
        // Ids with one slug, and two roots of one repository, never share.
        assert_ne!(repo_export_dir(s, "acme/my.lib", ""), repo_export_dir(s, "acme/my-lib", ""));
        assert_eq!(rueca_segment::repo_slug("acme/my.lib"), rueca_segment::repo_slug("acme/my-lib"));
        let web = export_dir(s, "github.com/o/r", "web", "main");
        let api = export_dir(s, "github.com/o/r", "api", "main");
        assert_ne!(web, api);
        assert!(web.parent().unwrap().file_name().unwrap().to_str().unwrap().starts_with("github-com-o-r--web-"));
        assert_ne!(web, export_dir(s, "github.com/o/r", "", "main"));
        let owner = RefOwner { repo: "r".into(), prefix: "web".into(), ref_name: "7696364aef89".into() };
        assert!(owner.names("r", "web", "7696364") && !owner.names("r", "api", "7696364aef89"));
        assert!(!owner.names("other", "web", "7696364aef89"));
        assert_eq!(RefOwner::path(&web).unwrap(), web.parent().unwrap().join(".owners/main.json"));
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
