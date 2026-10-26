//! Git reads for the local indexer, via `gix` (RFC 0002 §7.1: gix for all
//! reads, the `git` CLI only for network operations, which local indexing
//! never needs).
//!
//! What is read:
//!
//! - the repository containing the root (`gix::discover`, so a subdirectory
//!   of a repository is indexed as part of it);
//! - `remote.origin.url` from the repository configuration → repository id;
//! - `HEAD` → commit id (absent / unborn → `"worktree"`) and the checked-out
//!   branch name (none when detached);
//! - the `HEAD` tree, flattened to `path → blob id`, for the dirty check.
//!
//! **Dirty check.** Instead of `gix` status (index + dirwalk + attribute
//! machinery, which needs most of gix's optional features), the indexer
//! compares the blob SHAs it already computed for every indexed file with the
//! `HEAD` tree. The snapshot is dirty when an indexed file is not in `HEAD` or
//! has different content, or when a regular file of the `HEAD` tree that the
//! walk did not list no longer exists on disk (a deletion). Files the walk
//! excludes (hard excludes, `.gitignore`d tracked files) are not compared:
//! `dirty` describes the *indexed* content. Known limitation: content filters
//! (`core.autocrlf`, `.gitattributes` `text`/`eol`, Git LFS smudge) make the
//! worktree bytes differ from the stored blob, so such files report dirty.
//! Submodule contents are excluded from the comparison; symlinks are never
//! indexed and never compared.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// What the indexer learns from the git repository containing the root.
#[derive(Debug, Clone, Default)]
pub(crate) struct GitInfo {
    /// Normalized `remote.origin.url`, if any.
    pub repo_id: Option<String>,
    /// Hex id of the `HEAD` commit; `None` when `HEAD` is unborn.
    pub commit: Option<String>,
    /// Short name of the checked-out branch (`main`); `None` when detached.
    pub branch: Option<String>,
    /// Regular files of the `HEAD` tree under the root: root-relative path →
    /// hex blob id.
    pub head_blobs: BTreeMap<String, String>,
    /// Root-relative paths of submodules (gitlinks) under the root.
    pub submodules: BTreeSet<String>,
}

/// Reads git metadata for `root` (canonical). `None` when `root` is not inside
/// a git working tree or the repository cannot be read.
pub(crate) fn read(root: &Path) -> Option<GitInfo> {
    let repo = gix::discover(root).ok()?;
    let workdir = std::fs::canonicalize(repo.workdir()?).ok()?;
    let prefix = root.strip_prefix(&workdir).ok()?;
    let prefix: Vec<String> =
        prefix.components().map(|c| c.as_os_str().to_str().map(str::to_string)).collect::<Option<_>>()?;
    let prefix = prefix.join("/");

    let mut info = GitInfo {
        repo_id: repo
            .config_snapshot()
            .string("remote.origin.url")
            .and_then(|u| crate::normalize_remote(&u.to_string())),
        branch: repo.head_name().ok().flatten().map(|n| n.shorten().to_string()),
        ..Default::default()
    };
    let Ok(commit) = repo.head_commit() else {
        return Some(info);
    };
    info.commit = Some(commit.id.to_string());
    let tree = commit.tree().ok()?;
    let mut recorder = gix::traverse::tree::Recorder::default();
    tree.traverse().breadthfirst(&mut recorder).ok()?;
    for entry in recorder.records {
        let Ok(path) = std::str::from_utf8(entry.filepath.as_ref()) else { continue };
        let rel = if prefix.is_empty() {
            path
        } else {
            match path.strip_prefix(prefix.as_str()).and_then(|r| r.strip_prefix('/')) {
                Some(r) => r,
                None => continue,
            }
        };
        let kind = entry.mode.kind();
        match kind {
            gix::object::tree::EntryKind::Blob | gix::object::tree::EntryKind::BlobExecutable => {
                info.head_blobs.insert(rel.to_string(), entry.oid.to_string());
            }
            gix::object::tree::EntryKind::Commit => {
                info.submodules.insert(rel.to_string());
            }
            _ => {}
        }
    }
    Some(info)
}

/// Whether the indexed content differs from `HEAD` (see module docs).
/// `indexed` yields `(root-relative path, blob sha)` of every indexed file.
pub(crate) fn is_dirty<'a>(info: &GitInfo, root: &Path, indexed: impl IntoIterator<Item = (&'a str, &'a str)>) -> bool {
    if info.commit.is_none() {
        return true;
    }
    let in_submodule =
        |p: &str| info.submodules.iter().any(|s| p.strip_prefix(s.as_str()).is_some_and(|r| r.starts_with('/')));
    let mut seen = BTreeSet::new();
    for (path, sha) in indexed {
        if in_submodule(path) {
            continue;
        }
        match info.head_blobs.get(path) {
            Some(head) if head.eq_ignore_ascii_case(sha) => {
                seen.insert(path);
            }
            _ => return true,
        }
    }
    info.head_blobs
        .keys()
        .filter(|p| !seen.contains(p.as_str()))
        .any(|p| std::fs::symlink_metadata(root.join(p)).is_err())
}
