//! Local indexing pipeline (RFC 0001 §7): walk a working tree, compute blob
//! SHAs, extract facts in parallel (cached by blob + extractor version),
//! resolve, and write a segment into the local store.

use std::path::PathBuf;

use telar_extract::{Budget, Registry};
use telar_segment::SegmentMeta;

#[derive(Debug, Clone)]
pub struct IndexOptions {
    /// Working-tree root to index.
    pub root: PathBuf,
    /// Store directory (see `telar_segment::Store`).
    pub store: PathBuf,
    /// Repository id override; default `local/<dirname>` or derived from the
    /// git `origin` remote (`github.com/owner/name`).
    pub repo: Option<String>,
    pub budget: Budget,
    /// Worker threads; `None` = available parallelism.
    pub threads: Option<usize>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct IndexReport {
    pub segment: SegmentMeta,
    pub root: String,
    pub files_total: u64,
    pub files_extracted: u64,
    pub files_skipped: u64,
    pub facts_cache_hits: u64,
    pub references_total: u64,
    pub references_resolved: u64,
    pub duration_ms: u64,
}

/// Language packs shipped with this build (Python, TypeScript/TSX/JavaScript).
pub fn default_registry() -> Registry {
    todo!()
}

/// Indexes the working tree at `opts.root` (including uncommitted changes and
/// untracked, non-ignored files) and registers the new segment in the store.
pub fn index_worktree(_opts: &IndexOptions) -> anyhow::Result<IndexReport> {
    todo!()
}
