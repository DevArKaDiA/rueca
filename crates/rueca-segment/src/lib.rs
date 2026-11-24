//! Immutable segments, content packs and the local store (RFC 0001 §8,
//! amended by RFC 0002).
//!
//! A segment is written once by [`write_segment`] and opened read-only by
//! [`Segment::open`]. All query-time data comes from the segment and the
//! content packs; nothing reads the original working tree.
//!
//! # Store layout
//!
//! A directory, `$RUECA_HOME` or `~/.cache/rueca` by default
//! ([`Store::default_dir`]):
//!
//! ```text
//! <store>/.lock                   advisory writer lock (see Concurrency)
//! <store>/catalog.json            local catalog: indexed roots → current segment
//! <store>/segments/<id>.rcseg     one bundle file per segment
//! <store>/packs/<pack-id>.rcpack  content packs (one zstd frame per blob)
//! <store>/packs/index.rcpi        store-level blob index (derived, rebuildable)
//! <store>/orgs/<name>/            organizations: pointer, generations, shared symbol index ([`org`])
//! <store>/vectors/<embed-key>/    store vector cache: f16 rows by vector key ([`vcache`]; `$RUECA_VECTOR_DIR` moves it)
//! <store>/tmp/                    writer scratch space (same filesystem)
//! ```
//!
//! Every file is published by writing it under `tmp/`, fsyncing it, renaming
//! it into place and fsyncing the parent directory, so readers only ever see
//! complete files. Segment ids are `<repo-slug>-<commit[..12]>-<unix-ts>-<rand64>`
//! and pack ids `<unix-ts>-<rand64>`, where `<rand64>` is 64 random bits as
//! 16 lowercase hex digits (RFC 0008 §5.2: two writers of one commit in one
//! second do not collide; stores written before keep their shorter ids,
//! which are only ever compared as strings). Leftovers in `tmp/` older than
//! 24 h (crashed writers) are removed by the next [`write_segment`].
//!
//! # Concurrency
//!
//! **Several writers per store**, threads or processes on one host (RFC 0008
//! §5.5). Every read-modify-write of a mutable store file runs under an
//! advisory exclusive lock on `<store>/.lock` ([`std::fs::File::lock`],
//! [`StoreLock`]): `catalog.json` ([`Store::register`],
//! [`Store::register_ref`], [`Store::unregister`], including the deletion of
//! the segment files they release), `packs/index.rcpi` (re-read under the
//! lock when [`write_segment`] adds a pack or rebuilt the index, so a
//! concurrent writer's pack is never dropped from it), and
//! `orgs/<name>/org.json` with the organization garbage collection
//! ([`org::update_org`]). The lock is held only for those short steps:
//! extraction, the segment write and pack writes run unlocked and in
//! parallel. A writer waiting more than 60 s fails with an error naming the
//! lock file (and the holder's process id, which the holder writes into it).
//! The lock is advisory and per host: a store on a network filesystem shared
//! by several hosts is not supported (service stores use object storage and
//! compare-and-swap instead, RFC 0008 §5.2). It is not reentrant
//! ([`Store::lock`]).
//!
//! **Readers take no lock** and may run concurrently with writers: every
//! publication is an atomic rename, a published file is never modified, and
//! an opened [`Segment`] keeps its own mapping. Two concurrent writers may
//! still store the same blob twice (each in its own pack; the blob index
//! keeps one location), which costs space, never correctness.
//!
//! # Bundle format (`.rcseg`, [`FORMAT_VERSION`] 2)
//!
//! All integers little-endian. Modeled on Quickwit splits: concatenated
//! components, a footer directory, a fixed-size trailer.
//!
//! ```text
//! header   16 B   "RUECASEG" | u32 format_version | u32 flags (= 0)
//! components      each starts 8-byte aligned (zero padding in between)
//! footer          u32 count, then per component:
//!                   u16 name_len | name (UTF-8) | u64 offset | u64 len | [32] blake3(component)
//!                 (footer starts 8-byte aligned)
//! trailer  56 B   u64 footer_offset | u64 footer_len | [32] blake3(footer) | "RCSEGEND"
//! ```
//!
//! `footer_offset + footer_len == file_len - 56`. [`Segment::open`] maps the
//! file and validates, in order: magic, version ([`SegmentError::Version`]),
//! flags, trailer magic, footer bounds and checksum, every component's bounds
//! and BLAKE3 checksum, the zero padding between components, array lengths/alignment, every cross-table id (edge
//! endpoints, edge kinds and strategies, evidence files, CSR offsets, name
//! index permutations, file ↔ node references, trigram offsets, text-file
//! bitmap) and the symbol index against the node table. `vectors.keys`,
//! `vectors.scale` and `vectors.data` are the exception: their bounds and
//! lengths are checked at open, their checksums on the first
//! [`Segment::vectors`] call, so lexical queries never hash them. Any failure is
//! [`SegmentError::Corrupt`]; malformed input never panics in Rueca code.
//! Readers ignore components they do not know, so components can be added
//! without a version bump; any other change bumps [`FORMAT_VERSION`].
//! Version 2 changed the symbol index schema (`body`, `qname_suffix`);
//! version-1 segments are rejected with [`SegmentError::Version`] and must be
//! re-indexed ([`Store::segment_format_version`] reads the header alone, so
//! callers can detect that cheaply).
//!
//! Components (`N` nodes, `E` edges, `F` files; ids are `u32` indexes into
//! the `nodes` / `files` / edge order of the written [`ResolvedGraph`]):
//!
//! | Name | Encoding | Content |
//! |---|---|---|
//! | `meta` | JSON | [`SegmentMeta`] (`bytes` is stored as 0; the reader reports the file size). Fields are added with a serde default (`packs`, RFC 0008); unknown fields are ignored, so older readers open newer segments |
//! | `nodes` | postcard `Vec<Node>` | node table, index = [`NodeId`] |
//! | `files` | postcard `Vec<FileEntry>` | file table, index = [`FileId`] |
//! | `files.modules` (optional) | postcard `Vec<String>` | module name per file as its pack computed it (`""` = none); absent in segments written before RFC 0003 wave R ([`Segment::module_of`] returns `None`) |
//! | `contracts` (optional) | postcard `Vec<ContractRow>` | the contract table (RFC 0006 phase 4): one row per contract hint — node, side, kind, canonical key, raw text, file, span, framework — in `ResolvedGraph::contracts` order; written only when non-empty, absent = no contracts ([`Segment::contracts`]) |
//! | `edge_meta` | `[EdgeMetaRec; E]`, 36 B each | `f32 confidence, u8 strategy, u8 has_evidence, [u8;2] pad, u32 evidence_file, [u32;6] evidence span` (start_byte, end_byte, start_line, start_col, end_line, end_col); strategy = `Strategy` declaration order (0 `syntactic` … 6 `heuristic`, 7 `contract`) |
//! | `adj.out.offsets` | `[u32; N+1]` | CSR row offsets by source node |
//! | `adj.out.targets` | `[u32; E]` | destination node per entry; rows sorted by (kind, dst, edge index) |
//! | `adj.out.kinds` | `[u8; E]` | [`EdgeKind::index`] per entry |
//! | `adj.out.edges` | `[u32; E]` | edge index per entry (row into `edge_meta`) |
//! | `adj.in.*` | same | CSR by destination; rows sorted by (kind, src, edge index) |
//! | `names.qualified` | `[u32; N]` | node ids sorted by (`qualified_name` bytes, id) |
//! | `names.simple` | `[u32; N]` | node ids sorted by (`name` bytes, id) |
//! | `files.by_path` | `[u32; F]` | file ids sorted by (`path` bytes, id) |
//! | `file_nodes.offsets` | `[u32; F+1]` | CSR offsets by file |
//! | `file_nodes.ids` | `[u32; _]` | symbol nodes per file (no File/Folder/Repository), sorted by (span start, id); no span sorts last |
//! | `tantivy/<file>` | tantivy 0.27 files | symbol index: `meta.json` plus the files of its single segment |
//! | `symbols.bodiless` (optional) | roaring bitmap | node ids of C/C++ `Function` / `Method` nodes whose source has no `{` (prototypes, in-class method declarations, `= default` / `= delete`, function-like macros); absent in segments written before it, which then rank declarations like bodies |
//! | `ngrams.keys` | `[u32; K]` | sorted trigram keys `a<<16 \| b<<8 \| c` over ASCII-lowercased bytes |
//! | `ngrams.offsets` | `[u64; K+1]` | byte offsets into `ngrams.postings` |
//! | `ngrams.postings` | roaring bitmaps | per key, the serialized bitmap of file ids containing it |
//! | `ngrams.text_files` | roaring bitmap | file ids that are indexed text (the universe for scans) |
//! | `vectors.meta` (optional) | JSON | [`VectorsMeta`]: the embedding model record, embed and model keys, `dims`, `rows`, `encoding` `i8` (RFC 0007 §6) |
//! | `vectors.nodes` (optional) | `[u32; V]` | node ids with a vector, strictly ascending |
//! | `vectors.keys` (optional) | `[[u8; 32]; V]` | vector keys, the keys of the store vector cache ([`vcache`]) |
//! | `vectors.scale` (optional) | `[f32; V]` | per-row int8 scale |
//! | `vectors.data` (optional) | `[i8; V × dims]` | rows: Matryoshka-truncated to `dims`, re-normalized, quantized |
//!
//! Enum values are stored by declaration index: `Label`, `Language` and
//! `Strategy` variants are only ever appended (RFC 0003 added `Key`,
//! `Document`, `Resource`, `Image`, the data and Go / C / C++ / Java
//! languages and `Contract` without a format bump), so older segments decode
//! unchanged; a segment that uses a newer variant is rejected as corrupt by
//! an older reader. `tests/compat.rs` opens a store written before those
//! additions.
//!
//! Fixed-width arrays are read zero-copy from the map (`bytemuck`); the
//! `nodes`/`files` tables are decoded once at open. Edge kinds are filtered
//! with a binary search inside a CSR row.
//!
//! ## Symbol index
//!
//! One tantivy document per node, **doc id = node id** (one tantivy segment,
//! built by one indexing thread and merged in node order; checked at write
//! and open). Fields:
//!
//! | Field | Content | Boost |
//! |---|---|---|
//! | `name` | node name | 4 |
//! | `qualified_name` | global qualified name | 3 |
//! | `signature` | one-line signature | 1.5 |
//! | `path` | file path | 1 |
//! | `doc` | docstring | 0.5 |
//! | `body` | the source text of the node's span (nested definitions included), from the content packs at write time, at most 32 KiB; none for files, folders and modules | 8 (0.25 for one-chunk queries) |
//! | `name_exact` / `qname_exact` | lowercased name / qualified name, one term | 20 / 10 |
//! | `qname_suffix` | every suffix of ≥ 2 segments of the lowercased qualified name, separators (`.` `:` `::` `/` `\` `#`) normalized to `.` (`pkg.http.Session.send` → `http.session.send`, `session.send`) | 20 (dotted query part), 10 (two adjacent query words) |
//! | `label`, `language` | exact-term filters | — |
//!
//! Text fields use the [`tokenizer`] (camelCase / PascalCase / snake_case /
//! digit / path / dot splitting, lowercased, plus the whole identifier as one
//! token) at index and query time. Query = OR over every distinct (query token
//! × text field), each boosted by the field boost × `1 + ln(count)` of the
//! token in the query (issue text that keeps naming a term is about it); plus
//! whole-query exact matches on name and qualified name; plus `qname_suffix`
//! matches for dotted query parts (`Session.request`, `util:formatAmount`,
//! trailing punctuation ignored) and for pairs of adjacent query words
//! (`session request`). A query that is a single whitespace-free chunk is a
//! name lookup: `body` only breaks ties (boost 0.25), so a definition is not
//! outranked by its callers. BM25 is tantivy's (k1 = 1.2, b = 0.75).
//!
//! The text score is multiplied by a query-independent **prior**, the
//! product of every factor that applies (1 otherwise):
//!
//! - 0.5 for `Field` / `Variable` / `Constant`;
//! - 0.5 for tests (`is_test`, the `Test` label, or a test path: a `test`,
//!   `tests`, `__tests__`, `spec`, `specs`, `testing` or `e2e` directory,
//!   `test_*.py`, `*_test.py`, `conftest.py`, `*.test.*`, `*.spec.*`);
//! - 0.5 for **bodiless** C/C++ callables (`symbols.bodiless`): a prototype
//!   in a header and its body in a source file share the qualified name, and
//!   the prototype's short text would otherwise outscore the body on BM25;
//! - 0.5 for every definition of a **copy** file: an amalgamation such as
//!   `single_include/nlohmann/json.hpp`, detected from the node table — at
//!   least 50 callables / types with a body and ≥ 4 lines, ≥ 60% of them
//!   with a fingerprint (family, label, name, signature, line count) also
//!   found in another file, and no single other file holding more than 70%
//!   of those (fingerprints shared by more than 8 files are idioms, not
//!   copies). An amalgamation copies many files; a file and its one copy,
//!   per-platform siblings and generated stubs explain each other and are
//!   not demoted;
//! - 0.5 for **vendored** paths: a `third_party`, `thirdparty`,
//!   `third-party`, `3rdparty`, `vendored`, `deps` or `dependencies`
//!   directory (`vendor` and `node_modules` are not indexed).
//!
//! Priors are computed at open from the node and file tables and
//! `symbols.bodiless` (a tantivy `tweak_score` collector), so changing them
//! needs no re-index.
//!
//! Several segments ranked together ([`search_symbols_merged`]) use BM25
//! statistics summed over all of them, and an organization ([`org`]) lists
//! the segments' tantivy segments in one index: in both cases scores are
//! comparable across repositories and there is one ranking.
//!
//! Labels and language are exact-term filters (an empty label list excludes
//! `File`, `Folder`, `Module`, `Repository`, and `Key` / `Document` unless the
//! language is a data language); `path_glob` is a post-filter
//! with over-fetching. Results are ordered by score descending, then node id
//! ascending (tantivy breaks ties by doc address, which equals node id, also
//! under `tweak_score`, so pagination is exact). The index is served through
//! a read-only tantivy `Directory` over slices of the bundle map (the Quickwit
//! bundle-directory pattern); nothing is copied at open.
//!
//! ## Content search
//!
//! Text files (no NUL in the first 8 KiB, at most 2 MiB) are indexed by
//! trigram. A query is parsed with `regex_syntax`; a walk over its HIR
//! extracts required literals (exact string sets through concatenation and
//! small classes, AND within a concatenation, OR across alternation; optional
//! parts contribute nothing) and turns them into an AND/OR tree of trigram
//! postings. Patterns without a usable literal take every text file as a
//! candidate. Candidates, in path order, are decompressed from the content
//! packs and verified with `regex` (multi-line); verification stops at
//! `max_matches` or when the next file would exceed `scan_budget_bytes`, and
//! sets `truncated`. One match per line, ordered by (path, line).
//!
//! Both searches take an optional `deadline` (RFC 0008 §6.5): symbol search
//! checks it before scoring each tantivy segment and before each over-fetch
//! round, content search before verifying each candidate file; once it has
//! passed the search returns [`SegmentError::DeadlineExceeded`].
//!
//! # Content pack format (`.rcpack`, version 1)
//!
//! ```text
//! header   16 B   "RUECAPAK" | u32 version | u32 flags (= 0)
//! frames          one zstd frame per blob (level 3, content size + XXH64 checksum)
//! footer          u32 count, then per blob 44 B:
//!                   [20] blob sha-1 | u64 offset | u64 compressed_len | u64 raw_len
//! trailer  48 B   u64 footer_offset | [32] blake3(footer) | "RCPAKEND"
//! ```
//!
//! `packs/index.rcpi` maps every stored blob to its location across packs:
//! `"RUECAPXI" | u32 version | u32 flags | postcard { packs: Vec<pack-id>,
//! entries: Vec<(sha, pack, offset, compressed_len, raw_len)> } | [32] blake3(payload)`.
//! It is a cache: when missing, invalid, or listing a different set of packs
//! than exist on disk, it is rebuilt from pack footers. [`write_segment`]
//! asks the [`ContentSource`] only for blobs absent from the index (or whose
//! stored frame no longer decodes), verifies each returned blob against its
//! git blob SHA-1 ([`rueca_model::git_blob_sha`]), and writes the new blobs
//! to one new pack; [`SegmentMeta::content_bytes_added`] is that pack's size
//! and [`SegmentMeta::packs`] lists the packs holding the snapshot's blobs
//! (that pack plus those the known blobs were read from), so the segment
//! names every pack it needs without the index (RFC 0008 §5.9).
//! Reads check the zstd frame checksum and the raw length.
//!
//! There is **no pack garbage collection** yet: packs only grow, and blobs
//! referenced only by deleted segments are not reclaimed (RFC 0001 §8.4,
//! offline compaction, is future work). Replacing a root in the catalog
//! deletes the replaced segment file.
//!
//! # Catalog
//!
//! `catalog.json` is a JSON array of [`CatalogEntry`] sorted by root,
//! rewritten atomically, under the store lock, on every [`Store::register`]
//! / [`Store::unregister`].
//! Roots are canonicalized; [`Store::entry_for_path`] returns the entry with
//! the longest root that is an ancestor of (or equal to) the canonicalized
//! path. Several entries may share a repository id: worktrees of one
//! repository, and its ref snapshots (RFC 0006 §9.3), whose roots are
//! exports under `<store>/refs/` and which carry
//! [`CatalogEntry::ref_name`].
//!
//! # Deviations from RFC 0001 §8 / RFC 0002 §6 (alpha)
//!
//! - Node and file property tables are postcard, not Arrow IPC, and are
//!   decoded at open (the reader API hands out `&[Node]`). They sit behind
//!   [`Segment`], so moving to Arrow is a format-version change only.
//! - The trigram index is Rueca's own (sorted `u32` keys + roaring postings),
//!   i.e. RFC §8.1's "later" design, instead of a tantivy n-gram field. `fst`
//!   is not used: trigram keys are fixed-width, and name lookups binary-search
//!   id permutations over the already-decoded tables, so an fst would only
//!   duplicate key bytes. It becomes worthwhile for prefix/fuzzy lookups or
//!   once tables are no longer decoded at open.
//! - No hotcache and no object-storage reads: open maps the local file and
//!   checksums every component (cost linear in bundle size).
//! - The manifest has no base segment, visible-blob bitmap, extractor
//!   versions or coverage component yet (delta segments are M2); they can be
//!   added as new components.
//! - Content packs use plain zstd (no trained dictionaries); one frame per
//!   blob already gives random access, so seekable zstd is not needed.
//! - The local catalog is a JSON file rather than SQLite.
//!
//! # Limitations
//!
//! - `u32` ids (fewer than 2³² nodes, edges and files per segment);
//!   little-endian hosts only.
//! - Checksums detect accidental corruption. A deliberately crafted bundle
//!   with valid checksums could still reach panics inside third-party
//!   decoders (tantivy); segments are trusted local artifacts.
//! - The trigram prefilter folds ASCII case only; non-ASCII case variants
//!   rely on case-folded classes of at most 8 code points, else a scan.
#[cfg(target_endian = "big")]
compile_error!("rueca-segment format v2 reads little-endian arrays zero-copy; big-endian hosts are unsupported");

mod bundle;
mod contracts;
mod graph;
mod lock;
mod ngram;
pub mod org;
mod packs;
mod priors;
mod segment;
mod store;
mod symbols;
pub mod tokenizer;
pub mod vcache;
pub mod vectors;
mod writer;

pub use vectors::{Vectors, VectorsMeta, VectorsWrite};

use std::path::{Path, PathBuf};
use std::time::Instant;

use rueca_model::{
    ContractRow, EdgeKind, EdgeMeta, FileEntry, FileId, Label, Language, Node, NodeId, ResolvedGraph, SnapshotInfo,
};
use serde::{Deserialize, Serialize};

pub use lock::StoreLock;

/// Whether `path` looks like test code (a `test`, `tests`, `__tests__`,
/// `spec`, … directory, `test_*.py`, `*.spec.ts`, …): the path part of the
/// test prior.
pub fn is_test_path(path: &str) -> bool {
    priors::is_test_path(path)
}

/// Bundle format version. 2 (alpha.2): the symbol index gained the `body`
/// and `qname_suffix` fields; version-1 segments are rejected with
/// [`SegmentError::Version`] and must be re-indexed.
pub const FORMAT_VERSION: u32 = 2;

pub type Result<T> = std::result::Result<T, SegmentError>;

#[derive(Debug, thiserror::Error)]
pub enum SegmentError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("corrupt segment: {0}")]
    Corrupt(String),
    #[error(
        "unsupported format version {0} (this build reads segment format {FORMAT_VERSION}); re-index with `rueca index`"
    )]
    Version(u32),
    #[error("invalid query: {0}")]
    InvalidQuery(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("{0}")]
    Other(String),
    /// The query's `deadline` passed before the search finished.
    #[error("the search deadline passed")]
    DeadlineExceeded,
}

/// `Err(DeadlineExceeded)` when `deadline` has passed: one `Instant::now()`
/// comparison, cheap enough for a loop over segments or files.
pub(crate) fn check_deadline(deadline: Option<Instant>) -> Result<()> {
    match deadline {
        Some(at) if Instant::now() >= at => Err(SegmentError::DeadlineExceeded),
        _ => Ok(()),
    }
}

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

/// Supplies file contents to the writer. Called once per file in `graph.files`
/// whose blob is not already present in a content pack.
pub trait ContentSource: Sync {
    fn content(&self, file: &FileEntry) -> std::io::Result<Vec<u8>>;

    /// Whether `file`'s content is not available at all: a blob a partial
    /// clone's filter left out (RFC 0008 §7.6), listed as a coverage gap.
    /// Such a file stays in the file table but has no content in the packs
    /// (unless a pack already holds its blob), so `content` is never asked
    /// for it and reading it later fails like a missing blob. Default: no
    /// file is absent.
    fn absent(&self, _file: &FileEntry) -> bool {
        false
    }
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
    /// Size of the `.rcseg` file.
    pub bytes: u64,
    /// Bytes added to content packs by this write (0 if all blobs were known).
    pub content_bytes_added: u64,
    /// The content packs holding the snapshot's blobs, by pack id (the file
    /// stem of `packs/<id>.rcpack`), sorted: the pack this write added, if
    /// any, plus the packs its already-stored blobs were read from. Lets a
    /// service store copy or keep exactly the packs a snapshot needs without
    /// `packs/index.rcpi` (RFC 0008 §5.9). Empty (and not serialized) for an
    /// empty snapshot and in segments written before the field; older
    /// readers ignore it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub packs: Vec<String>,
}

/// Writes `graph` as a new segment into `store` and appends unseen blobs to
/// content packs. Atomic: the segment file appears only when complete.
///
/// Calls [`ContentSource::content`] only for blobs not already stored, verifies
/// each returned blob against `FileEntry::blob_sha` (mismatch → error), and
/// reads already-stored blobs back from the packs to build the n-gram index.
/// Safe to run concurrently with other writers of the store: only the
/// blob-index update takes the store lock (see crate docs).
pub fn write_segment(graph: &ResolvedGraph, contents: &dyn ContentSource, store: &Path) -> Result<SegmentMeta> {
    writer::write_segment(graph, contents, store, None)
}

/// [`write_segment`] with the snapshot's symbol vectors (RFC 0007 §6) in
/// the same bundle: the one-publication path when every vector is already
/// in the store cache.
pub fn write_segment_with(
    graph: &ResolvedGraph,
    contents: &dyn ContentSource,
    store: &Path,
    vectors: Option<&VectorsWrite>,
) -> Result<SegmentMeta> {
    writer::write_segment(graph, contents, store, vectors)
}

// ---------------------------------------------------------------------------
// Local catalog
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CatalogEntry {
    /// Absolute, canonicalized working-tree root (for a ref snapshot: its
    /// export directory under `<store>/refs/`).
    pub root: String,
    pub repo: String,
    pub segment_id: String,
    pub indexed_at: u64,
    /// The ref this entry is a snapshot of when its root is a ref export
    /// (`rueca index --ref`, RFC 0006 §9.3): `feature/x`, `pr/123`, `v1.2`
    /// or a 12-digit commit. `None` for a working tree (the repository's own
    /// root, its default ref). Absent in catalogs written before refs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_name: Option<String>,
}

impl CatalogEntry {
    /// Whether the entry is a ref snapshot exported under the store (not a
    /// working tree).
    pub fn is_ref_export(&self) -> bool {
        self.ref_name.is_some()
    }
}

/// The file-name-safe slug of a repository id used in segment ids and in
/// ref export directories (`github.com/o/r` → `github-com-o-r`): ASCII
/// alphanumerics lowercased, every other run of characters one `-`, at most
/// 48 characters. Not injective; callers that need uniqueness check it.
pub fn repo_slug(repo: &str) -> String {
    writer::slug(repo, 48)
}

/// The local catalog (`catalog.json`). Small and rewritten atomically.
pub struct Store {
    pub dir: PathBuf,
}

impl Store {
    /// Opens (creating directories if needed) the store at `dir`.
    pub fn open(dir: &Path) -> Result<Store> {
        Store::open_impl(dir)
    }
    /// Default store dir: `$RUECA_HOME`, else `~/.cache/rueca`.
    pub fn default_dir() -> PathBuf {
        Store::default_dir_impl()
    }
    /// Records `meta` as the current segment for `root`, replacing any previous
    /// entry. Garbage-collects the replaced segment file.
    ///
    /// `root` is canonicalized. The replaced segment file is deleted unless
    /// another entry or a kept organization generation ([`org`]) still
    /// references it.
    pub fn register(&self, root: &str, meta: &SegmentMeta) -> Result<()> {
        self.register_impl(root, meta, None)
    }
    /// Like [`Store::register`], marking the entry as the snapshot of ref
    /// `ref_name` exported at `root` ([`CatalogEntry::ref_name`]).
    pub fn register_ref(&self, root: &str, meta: &SegmentMeta, ref_name: &str) -> Result<()> {
        self.register_impl(root, meta, Some(ref_name))
    }
    /// Removes the entry of `root` (canonicalized) and returns it; `None`
    /// when no entry has that root. Its segment file is deleted unless
    /// another entry or a kept organization generation still references it
    /// (the same rule as a replacement by [`Store::register`]).
    pub fn unregister(&self, root: &str) -> Result<Option<CatalogEntry>> {
        self.unregister_impl(root)
    }
    /// All catalog entries, sorted by root.
    pub fn entries(&self) -> Result<Vec<CatalogEntry>> {
        self.entries_impl()
    }
    /// The entry whose root equals `root` or is the nearest ancestor of it.
    ///
    /// `path` is canonicalized (nearest existing ancestor if it does not exist).
    pub fn entry_for_path(&self, path: &Path) -> Result<Option<CatalogEntry>> {
        self.entry_for_path_impl(path)
    }
    pub fn segment_path(&self, segment_id: &str) -> PathBuf {
        self.dir.join("segments").join(format!("{segment_id}.rcseg"))
    }
    /// Format version recorded in the header of a stored segment (cheap: reads
    /// 16 bytes). A value other than [`FORMAT_VERSION`] means the segment was
    /// written by another build and must be re-indexed.
    pub fn segment_format_version(&self, segment_id: &str) -> Result<u32> {
        bundle::Bundle::peek_version(&self.segment_path(segment_id))
    }
    pub fn open_segment(&self, segment_id: &str) -> Result<Segment> {
        Segment::open(&self.segment_path(segment_id), &self.dir)
    }
    /// Takes the store's writer lock (`<store>/.lock`, see the crate docs),
    /// waiting at most 60 s; the error names the lock file. Hold it around a
    /// read-modify-write of a store file this crate does not write itself
    /// (e.g. a mirror that installs an organization pointer). Not reentrant:
    /// while holding it, do not call [`Store::register`],
    /// [`Store::unregister`], [`write_segment`] or [`org::update_org`],
    /// which take it themselves.
    pub fn lock(&self) -> Result<StoreLock> {
        StoreLock::acquire(&self.dir)
    }
    /// Publishes a copy of `segment` with `vectors` (RFC 0007 §5.6, the
    /// second publication after a lexical one): a new segment file under a
    /// new id holding every component of `segment` unchanged (but `meta`,
    /// which carries the new id) plus the `vectors.*` components. The
    /// caller registers it; `segment` stays readable until then.
    pub fn republish_with_vectors(&self, segment: &Segment, vectors: &VectorsWrite) -> Result<SegmentMeta> {
        writer::republish_with_vectors(&self.dir, segment, vectors)
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
    /// Empty = all labels except `File`/`Folder`/`Module` (and `Repository`),
    /// and except `Key`/`Document` unless `language` is a data language.
    pub labels: Vec<Label>,
    /// Glob on the file path (`src/**/*.py`).
    pub path_glob: Option<String>,
    pub language: Option<Language>,
    pub limit: usize,
    pub offset: usize,
    /// When the search must stop ([`SegmentError::DeadlineExceeded`]):
    /// checked before each tantivy segment is scored and before each
    /// over-fetch round, and per segment of a merged ranking.
    pub deadline: Option<Instant>,
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
    /// When the scan must stop ([`SegmentError::DeadlineExceeded`]):
    /// checked before verifying each candidate file.
    pub deadline: Option<Instant>,
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
            deadline: None,
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
    inner: segment::Inner,
}

impl Segment {
    /// Opens a segment file; `store` locates content packs.
    ///
    /// Validates magic, version (`SegmentError::Version`), every component
    /// checksum and all cross-table ids (`SegmentError::Corrupt`). `meta().bytes`
    /// is the actual file size.
    pub fn open(path: &Path, store: &Path) -> Result<Segment> {
        Ok(Segment { inner: segment::Inner::open(path, store)? })
    }
    pub fn meta(&self) -> &SegmentMeta {
        &self.inner.meta
    }
    pub fn snapshot(&self) -> &SnapshotInfo {
        &self.inner.meta.snapshot
    }
    pub fn nodes(&self) -> &[Node] {
        &self.inner.nodes
    }
    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes()[id as usize]
    }
    pub fn files(&self) -> &[FileEntry] {
        &self.inner.files
    }
    pub fn file(&self, id: FileId) -> &FileEntry {
        &self.files()[id as usize]
    }
    pub fn file_by_path(&self, path: &str) -> Option<FileId> {
        self.inner.file_by_path(path)
    }
    /// Module name of `file` as its language pack computed it (`""` for no
    /// module), or `None` when the segment predates the `files.modules`
    /// component or the file id is out of range.
    pub fn module_of(&self, file: FileId) -> Option<&str> {
        self.inner.modules.as_ref()?.get(file as usize).map(String::as_str)
    }
    /// The contract table (RFC 0006 phase 4): every contract hint of the
    /// snapshot on its node, in file then hint order
    /// (`ResolvedGraph::contracts`). Empty for segments written before the
    /// `contracts` component and for snapshots without contracts.
    pub fn contracts(&self) -> &[ContractRow] {
        &self.inner.contracts.rows
    }
    /// The rows of [`Segment::contracts`] placed on `node`, in table order.
    pub fn contracts_of(&self, node: NodeId) -> Vec<&ContractRow> {
        self.inner.contracts.of(node)
    }
    /// Outgoing edges of `node`, optionally filtered by kind, in stable order.
    ///
    /// Order: by kind (declaration order of `EdgeKind`), then target node id,
    /// then original edge index. Out-of-range nodes yield no edges.
    pub fn out_edges(&self, node: NodeId, kind: Option<EdgeKind>) -> Vec<EdgeRef> {
        self.inner.out_edges(node, kind)
    }
    /// Incoming edges of `node`, optionally filtered by kind, in stable order.
    ///
    /// Order: by kind, then source node id, then original edge index.
    pub fn in_edges(&self, node: NodeId, kind: Option<EdgeKind>) -> Vec<EdgeRef> {
        self.inner.in_edges(node, kind)
    }
    /// Exact match on `Node::qualified_name`. Ids ascending.
    pub fn nodes_by_qualified_name(&self, qualified_name: &str) -> Vec<NodeId> {
        self.inner.nodes_by_qualified_name(qualified_name)
    }
    /// Exact match on `Node::name`. Ids ascending.
    pub fn nodes_by_name(&self, name: &str) -> Vec<NodeId> {
        self.inner.nodes_by_name(name)
    }
    /// Symbol nodes defined in `file`, sorted by span start.
    ///
    /// Excludes `File`/`Folder`/`Repository` nodes; ties (and nodes without a
    /// span, which sort last) are ordered by node id.
    pub fn nodes_in_file(&self, file: FileId) -> Vec<NodeId> {
        self.inner.nodes_in_file(file)
    }
    /// BM25 over the code-aware tokenizer. `limit == 0` means 20. Results are
    /// ordered by score descending, then node id ascending.
    pub fn search_symbols(&self, query: &SymbolQuery) -> Result<Vec<SymbolHit>> {
        self.inner.search_symbols(query)
    }
    /// Matches are ordered by (path, line), one per matching line. Only text
    /// files (no NUL in the first 8 KiB, at most 2 MiB) are searchable.
    /// `max_matches == 0` means 100. `truncated` is set when `max_matches` or
    /// `scan_budget_bytes` stopped the search before every candidate was
    /// verified.
    pub fn search_content(&self, query: &ContentQuery) -> Result<ContentSearchResult> {
        self.inner.search_content(query)
    }
    /// Full content of a file, from the content packs.
    pub fn content(&self, file: FileId) -> Result<Vec<u8>> {
        self.inner.content(file)
    }
    /// The query-independent prior of every node (index = node id), the
    /// factor `search_symbols` multiplies BM25 by (crate docs, *Symbol
    /// index*).
    pub fn priors(&self) -> std::sync::Arc<[f32]> {
        self.inner.symbols().priors()
    }
    /// The symbol vectors (RFC 0007 §6), `None` for a lexical-only segment.
    /// The first call verifies the checksums of the bulk components, which
    /// opening the segment skips.
    ///
    /// # Errors
    ///
    /// [`SegmentError::Corrupt`] when a vectors component fails its checksum.
    pub fn vectors(&self) -> Result<Option<&Vectors>> {
        self.inner.vectors()
    }
    /// The metadata of the segment's vectors (preset, keys, width, rows),
    /// `None` for a lexical-only segment. Unlike [`Segment::vectors`] it
    /// verifies nothing (`rueca status`).
    pub fn vectors_meta(&self) -> Option<&VectorsMeta> {
        self.inner.vectors_meta()
    }
    /// The filter `search_symbols` applies for `query` without its text:
    /// labels (the default exclusions when `labels` is empty), language and
    /// `path_glob`. Dense retrieval applies it to its candidates.
    ///
    /// # Errors
    ///
    /// An invalid `path_glob`.
    pub fn symbol_filter(&self, query: &SymbolQuery) -> Result<impl Fn(NodeId) -> bool + '_> {
        let glob = query.path_glob.as_deref().map(symbols::compile_glob).transpose()?;
        let labels = symbols::label_filter(query);
        let language = query.language;
        Ok(move |n: NodeId| {
            let Some(node) = self.inner.nodes.get(n as usize) else { return false };
            labels(node.label)
                && language.is_none_or(|l| node.language == Some(l))
                && glob.as_ref().is_none_or(|g| g.is_match(self.inner.node_path(n)))
        })
    }
}

/// One ranking of `query` over the symbol indexes of several segments
/// (RFC 0006 §6, no rank fusion): every index is scored with BM25
/// statistics summed over all of them, so the merged order is the order one
/// index holding every document would give (the organization index computes
/// the same scores, see [`org`]). Returns (position in `segments`, hit),
/// ordered by score descending, then position, then node id; `offset` and
/// `limit` (`0` = 20) apply to the merged ranking.
pub fn search_symbols_merged(segments: &[&Segment], query: &SymbolQuery) -> Result<Vec<(usize, SymbolHit)>> {
    let limit = symbols::effective_limit(query);
    let want = query.offset.saturating_add(limit);
    let glob = query.path_glob.as_deref().map(symbols::compile_glob).transpose()?;
    let searchers: Vec<tantivy::Searcher> = segments.iter().map(|s| s.inner.symbols().searcher()).collect();
    let stats = symbols::SummedStats { searchers: &searchers };
    let mut all: Vec<(usize, SymbolHit)> = Vec::new();
    for (i, seg) in segments.iter().enumerate() {
        check_deadline(query.deadline)?;
        let index = seg.inner.symbols();
        let q = symbols::build_query(index.fields(), query);
        let priors: std::sync::Arc<symbols::PriorsBySegment> =
            std::sync::Arc::new(index.segment_id().map(|id| (id, index.priors())).into_iter().collect());
        let n = seg.nodes().len();
        let keep = |_: tantivy::index::SegmentId, doc: u32| {
            (doc as usize) < n && glob.as_ref().is_none_or(|g| g.is_match(seg.inner.node_path(doc)))
        };
        let top = symbols::TopK { want, keep: &keep, deadline: query.deadline };
        let hits = symbols::top_hits(&searchers[i], q.as_ref(), &stats, priors, &Default::default(), &top)?;
        all.extend(hits.into_iter().map(|h| (i, SymbolHit { node: h.doc, score: h.score })));
    }
    all.sort_by(|a, b| b.1.score.total_cmp(&a.1.score).then(a.0.cmp(&b.0)).then(a.1.node.cmp(&b.1.node)));
    Ok(all.into_iter().skip(query.offset).take(limit).collect())
}
