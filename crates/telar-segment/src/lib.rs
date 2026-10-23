//! Immutable segments, content packs and the local store (RFC 0001 §8,
//! amended by RFC 0002).
//!
//! Store layout (a directory, `$TELAR_HOME` or `~/.cache/telar` by default):
//!
//! ```text
//! <store>/catalog.json            local catalog: indexed roots → current segment
//! <store>/segments/<id>.tlseg     one bundle file per segment
//! <store>/packs/<pack-id>.tlpack  content packs (zstd frames, blob_sha-keyed)
//! <store>/tmp/                    scratch space for writers
//! ```
//!
//! A segment is written once by [`write_segment`] and opened read-only by
//! [`Segment::open`]. All query-time data comes from the segment and the
//! content packs; nothing reads the original working tree.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use telar_model::{
    EdgeKind, EdgeMeta, FileEntry, FileId, Label, Language, Node, NodeId, ResolvedGraph, SnapshotInfo,
};

pub const FORMAT_VERSION: u32 = 1;

pub type Result<T> = std::result::Result<T, SegmentError>;

#[derive(Debug, thiserror::Error)]
pub enum SegmentError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("corrupt segment: {0}")]
    Corrupt(String),
    #[error("unsupported format version {0}")]
    Version(u32),
    #[error("invalid query: {0}")]
    InvalidQuery(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("{0}")]
    Other(String),
}

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

/// Supplies file contents to the writer. Called once per file in `graph.files`
/// whose blob is not already present in a content pack.
pub trait ContentSource: Sync {
    fn content(&self, file: &FileEntry) -> std::io::Result<Vec<u8>>;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SegmentMeta {
    /// Unique id; also the file stem under `segments/`.
    pub id: String,
    pub format_version: u32,
    pub snapshot: SnapshotInfo,
    pub node_count: u32,
    pub edge_count: u32,
    pub file_count: u32,
    /// Size of the `.tlseg` file.
    pub bytes: u64,
    /// Bytes added to content packs by this write (0 if all blobs were known).
    pub content_bytes_added: u64,
}

/// Writes `graph` as a new segment into `store` and appends unseen blobs to
/// content packs. Atomic: the segment file appears only when complete.
pub fn write_segment(_graph: &ResolvedGraph, _contents: &dyn ContentSource, _store: &Path) -> Result<SegmentMeta> {
    todo!()
}

// ---------------------------------------------------------------------------
// Local catalog
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CatalogEntry {
    /// Absolute, canonicalized working-tree root.
    pub root: String,
    pub repo: String,
    pub segment_id: String,
    pub indexed_at: u64,
}

/// The local catalog (`catalog.json`). Small and rewritten atomically.
pub struct Store {
    pub dir: PathBuf,
}

impl Store {
    /// Opens (creating directories if needed) the store at `dir`.
    pub fn open(_dir: &Path) -> Result<Store> {
        todo!()
    }
    /// Default store dir: `$TELAR_HOME`, else `~/.cache/telar`.
    pub fn default_dir() -> PathBuf {
        todo!()
    }
    /// Records `meta` as the current segment for `root`, replacing any previous
    /// entry. Garbage-collects the replaced segment file.
    pub fn register(&self, _root: &str, _meta: &SegmentMeta) -> Result<()> {
        todo!()
    }
    pub fn entries(&self) -> Result<Vec<CatalogEntry>> {
        todo!()
    }
    /// The entry whose root equals `root` or is the nearest ancestor of it.
    pub fn entry_for_path(&self, _path: &Path) -> Result<Option<CatalogEntry>> {
        todo!()
    }
    pub fn segment_path(&self, segment_id: &str) -> PathBuf {
        self.dir.join("segments").join(format!("{segment_id}.tlseg"))
    }
    pub fn open_segment(&self, segment_id: &str) -> Result<Segment> {
        Segment::open(&self.segment_path(segment_id), &self.dir)
    }
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

/// One adjacency entry: the node on the other end of the edge.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EdgeRef {
    pub other: NodeId,
    pub kind: EdgeKind,
    pub meta: EdgeMeta,
}

/// BM25 search over symbol names, qualified names, signatures, docs and paths,
/// with code-aware tokenization (camelCase / snake_case / path splitting).
#[derive(Debug, Clone, Default)]
pub struct SymbolQuery {
    pub text: String,
    /// Empty = all labels except `File`/`Folder`/`Module`.
    pub labels: Vec<Label>,
    /// Glob on the file path (`src/**/*.py`).
    pub path_glob: Option<String>,
    pub language: Option<Language>,
    pub limit: usize,
    pub offset: usize,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SymbolHit {
    pub node: NodeId,
    pub score: f32,
}

/// Literal or regex search over file contents. Regex literals are planned into
/// n-gram queries; candidates are verified against content packs. Patterns
/// with no extractable literal fall back to a bounded scan.
#[derive(Debug, Clone)]
pub struct ContentQuery {
    pub pattern: String,
    pub regex: bool,
    pub case_insensitive: bool,
    pub path_glob: Option<String>,
    pub language: Option<Language>,
    pub max_matches: usize,
    pub context_lines: u32,
    /// Upper bound on bytes verified; hitting it sets `truncated`.
    pub scan_budget_bytes: u64,
}

impl Default for ContentQuery {
    fn default() -> Self {
        ContentQuery {
            pattern: String::new(),
            regex: false,
            case_insensitive: false,
            path_glob: None,
            language: None,
            max_matches: 100,
            context_lines: 0,
            scan_budget_bytes: 256 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContentMatch {
    pub file: FileId,
    /// 1-based line number.
    pub line: u32,
    pub text: String,
    pub before: Vec<String>,
    pub after: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ContentSearchResult {
    pub matches: Vec<ContentMatch>,
    pub truncated: bool,
    /// Files whose content was verified.
    pub files_scanned: u32,
    /// Files selected by the n-gram index before verification.
    pub candidate_files: u32,
}

/// A read-only, opened segment. Cheap to share across threads (`Send + Sync`).
pub struct Segment {
    _private: (),
}

impl Segment {
    /// Opens a segment file; `store` locates content packs.
    pub fn open(_path: &Path, _store: &Path) -> Result<Segment> {
        todo!()
    }
    pub fn meta(&self) -> &SegmentMeta {
        todo!()
    }
    pub fn snapshot(&self) -> &SnapshotInfo {
        todo!()
    }
    pub fn nodes(&self) -> &[Node] {
        todo!()
    }
    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes()[id as usize]
    }
    pub fn files(&self) -> &[FileEntry] {
        todo!()
    }
    pub fn file(&self, id: FileId) -> &FileEntry {
        &self.files()[id as usize]
    }
    pub fn file_by_path(&self, _path: &str) -> Option<FileId> {
        todo!()
    }
    /// Outgoing edges of `node`, optionally filtered by kind, in stable order.
    pub fn out_edges(&self, _node: NodeId, _kind: Option<EdgeKind>) -> Vec<EdgeRef> {
        todo!()
    }
    /// Incoming edges of `node`, optionally filtered by kind, in stable order.
    pub fn in_edges(&self, _node: NodeId, _kind: Option<EdgeKind>) -> Vec<EdgeRef> {
        todo!()
    }
    /// Exact match on `Node::qualified_name`.
    pub fn nodes_by_qualified_name(&self, _qualified_name: &str) -> Vec<NodeId> {
        todo!()
    }
    /// Exact match on `Node::name`.
    pub fn nodes_by_name(&self, _name: &str) -> Vec<NodeId> {
        todo!()
    }
    /// Symbol nodes defined in `file`, sorted by span start.
    pub fn nodes_in_file(&self, _file: FileId) -> Vec<NodeId> {
        todo!()
    }
    pub fn search_symbols(&self, _query: &SymbolQuery) -> Result<Vec<SymbolHit>> {
        todo!()
    }
    pub fn search_content(&self, _query: &ContentQuery) -> Result<ContentSearchResult> {
        todo!()
    }
    /// Full content of a file, from the content packs.
    pub fn content(&self, _file: FileId) -> Result<Vec<u8>> {
        todo!()
    }
}
