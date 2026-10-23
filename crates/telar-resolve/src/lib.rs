//! Turns the facts of every file in a snapshot into a [`ResolvedGraph`].
//!
//! Contract (RFC 0001 §7 step 3): deterministic — same inputs in the same
//! order produce an identical graph (sorted iteration, stable tie-breaks).

use telar_extract::{ProjectFiles, Registry};
use telar_model::{FileFacts, Language, ResolvedGraph, SnapshotInfo};

/// One file of the snapshot as seen by the resolver.
#[derive(Debug, Clone)]
pub struct FileInput {
    /// Repository-relative path, `/` separators.
    pub path: String,
    pub blob_sha: String,
    pub language: Option<Language>,
    pub size: u64,
    /// `None` when the file was not extracted (unsupported language or skipped).
    pub facts: Option<FileFacts>,
    /// `ok`, `partial`, or `skipped:<reason>`; copied into `FileEntry::status`.
    pub status: String,
}

#[derive(Debug, Clone, Default)]
pub struct ResolveStats {
    pub references_total: u64,
    pub references_resolved: u64,
    /// Count per strategy name (`import_resolved`, `same_module`, ...).
    pub resolved_by_strategy: std::collections::BTreeMap<String, u64>,
}

pub struct ResolveOutput {
    pub graph: ResolvedGraph,
    pub stats: ResolveStats,
}

/// Resolves a snapshot. `files` may be in any order; the resolver sorts by path.
pub fn resolve(_snapshot: SnapshotInfo, _files: Vec<FileInput>, _project: &ProjectFiles, _registry: &Registry) -> ResolveOutput {
    todo!("resolver")
}

