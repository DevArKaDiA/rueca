//! The symbol index: a tantivy index built in a scratch directory, embedded
//! into the bundle as `tantivy/<file>` components, and served back through a
//! read-only [`BundleDirectory`] over slices of the mmapped bundle (the
//! Quickwit "bundle directory" pattern, without the hotcache layer).
//!
//! Invariant (checked at write and at open): the index has exactly one
//! tantivy segment (zero for an empty graph) and tantivy doc id `d` is node
//! `d`. tantivy breaks score ties by ascending doc address, so its own top-k
//! cut is already ordered by (score desc, node id asc) and paging is exact.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use globset::GlobMatcher;
use roaring::RoaringBitmap;
use rueca_model::{FileEntry, Label, LanguageFamily, Node};
use tantivy::collector::{Collector, SegmentCollector, TopDocs};
use tantivy::directory::error::{DeleteError, LockError, OpenReadError, OpenWriteError};
use tantivy::directory::{DirectoryLock, FileHandle, Lock, OwnedBytes, WatchCallback, WatchHandle, WritePtr};
use tantivy::index::SegmentId;
use tantivy::indexer::NoMergePolicy;
use tantivy::query::{
    AllQuery, Bm25StatisticsProvider, BooleanQuery, BoostQuery, ConstScoreQuery, Occur, Query, TermQuery, Weight,
};
use tantivy::schema::{Field, IndexRecordOption, NumericOptions, STRING, Schema, TextFieldIndexing, TextOptions};
use tantivy::{
    Directory, DocId, Index, IndexReader, ReloadPolicy, Score, Searcher, SegmentOrdinal, SegmentReader,
    TantivyDocument, Term,
};

use crate::bundle::{Bundle, BundleWriter, SharedMmap};
use crate::priors;
use crate::tokenizer::{CODE_TOKENIZER, CodeTokenizer, tokens};
use crate::{Result, SegmentError, SymbolHit, SymbolQuery};

pub(crate) const TANTIVY_PREFIX: &str = "tantivy/";
/// Optional component (added without a format bump): roaring bitmap of the
/// node ids that are C/C++ callables without a body ([`priors::is_bodiless`]).
pub(crate) const BODILESS_COMPONENT: &str = "symbols.bodiless";
pub(crate) const DEFAULT_LIMIT: usize = 20;

/// Field boosts. `body` (the symbol's source) carries the most weight for
/// descriptive queries; name and qualified name dominate through the exact
/// and suffix matches below.
const BOOSTS: [(&str, f32); 6] =
    [("name", 4.0), ("qualified_name", 3.0), ("signature", 1.5), ("path", 1.0), ("doc", 0.5), ("body", 8.0)];
/// Index of `body` in [`BOOSTS`].
const BODY_FIELD: usize = 5;
/// `body` boost for single-chunk (name lookup) queries.
const BODY_BOOST_NAME_LOOKUP: f32 = 0.25;
const NAME_EXACT_BOOST: f32 = 20.0;
const QNAME_EXACT_BOOST: f32 = 10.0;
/// A dotted query part (`Session.request`, `util:format`) equal to a suffix of
/// the qualified name on a segment boundary.
const QNAME_SUFFIX_BOOST: f32 = 20.0;
/// Two adjacent query words (`session request`) equal to a qualified-name
/// suffix (`Session.request`).
const QNAME_PAIR_BOOST: f32 = 10.0;
/// Most qualified-name segments considered for suffix terms.
const MAX_SUFFIX_SEGMENTS: usize = 16;
/// Bytes of a symbol's source kept for its `body` field.
pub(crate) const MAX_BODY_BYTES: usize = 32 * 1024;

/// Labels excluded when `SymbolQuery::labels` is empty.
pub(crate) const DEFAULT_EXCLUDED: [Label; 4] = [Label::File, Label::Folder, Label::Module, Label::Repository];
/// Also excluded by default unless `SymbolQuery::language` is a data
/// language: structured-data keys and documents (RFC 0003 §11), which would
/// otherwise crowd code out of localization queries.
pub(crate) const DATA_DETAIL: [Label; 2] = [Label::Key, Label::Document];

fn terr(e: tantivy::TantivyError) -> SegmentError {
    SegmentError::Other(format!("tantivy: {e}"))
}

pub(crate) struct Fields {
    text: Vec<(Field, f32)>,
    name_exact: Field,
    qname_exact: Field,
    qname_suffix: Field,
    label: Field,
    language: Field,
}

pub(crate) fn schema() -> Schema {
    let mut b = Schema::builder();
    let text = TextOptions::default().set_indexing_options(
        TextFieldIndexing::default().set_tokenizer(CODE_TOKENIZER).set_index_option(IndexRecordOption::WithFreqs),
    );
    for (name, _) in BOOSTS {
        b.add_text_field(name, text.clone());
    }
    b.add_text_field("name_exact", STRING);
    b.add_text_field("qname_exact", STRING);
    b.add_text_field("qname_suffix", STRING);
    b.add_text_field("label", STRING);
    b.add_text_field("language", STRING);
    b.add_u64_field("node", NumericOptions::default().set_fast());
    b.build()
}

/// Fields whose per-repository document frequencies route a query
/// (`org::route`): name, path and body.
pub(crate) fn routing_fields(f: &Fields) -> [Field; 3] {
    [f.text[0].0, f.text[3].0, f.text[BODY_FIELD].0]
}

pub(crate) fn fields(schema: &Schema) -> Result<Fields> {
    let f = |n: &str| schema.get_field(n).map_err(terr);
    Ok(Fields {
        text: BOOSTS.iter().map(|(n, w)| Ok((f(n)?, *w))).collect::<Result<_>>()?,
        name_exact: f("name_exact")?,
        qname_exact: f("qname_exact")?,
        qname_suffix: f("qname_suffix")?,
        label: f("label")?,
        language: f("language")?,
    })
}

/// Segments of a qualified name or of a dotted query part, lowercased, with
/// every member / module separator (`.`, `:`, `::`, `/`, `\\`, `#`) treated as
/// one boundary: `src/util:Fmt.run` → `src`, `util`, `fmt`, `run`.
fn qualified_segments(s: &str) -> Vec<String> {
    s.split(['.', ':', '/', '\\', '#'])
        .filter(|p| !p.is_empty())
        .map(|p| p.trim_end_matches("()").to_lowercase())
        .filter(|p| !p.is_empty())
        .collect()
}

/// `qname_suffix` terms of a qualified name: every suffix of at least two
/// segments, joined with `.` (`a.b.c` → `a.b.c`, `b.c`).
fn qualified_suffixes(qualified_name: &str) -> Vec<String> {
    let segs = qualified_segments(qualified_name);
    let first = segs.len().saturating_sub(MAX_SUFFIX_SEGMENTS);
    (first..segs.len().saturating_sub(1)).map(|k| segs[k..].join(".")).collect()
}

/// Query parts that name a qualified symbol: whitespace-separated chunks with
/// a separator between two identifier segments (`Session.request`,
/// `util:formatAmount`, `App\\User::save`), normalized like
/// [`qualified_suffixes`].
fn dotted_parts(text: &str) -> Vec<String> {
    let mut out: Vec<String> = text
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| !(is_ident_char(c) || matches!(c, '.' | ':' | '/' | '\\' | '#'))))
        .map(qualified_segments)
        .filter(|segs| {
            segs.len() >= 2 && segs.iter().all(|p| p.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '$'))
        })
        .map(|segs| segs.join("."))
        .collect();
    out.sort();
    out.dedup();
    out
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// `a.b` for every two adjacent identifier words `a b` of the query, so
/// `session request` also finds `Session.request`.
fn adjacent_pairs(text: &str) -> Vec<String> {
    let words: Vec<String> = text
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| !is_ident_char(c)))
        .filter(|w| !w.is_empty() && w.chars().all(is_ident_char))
        .map(str::to_lowercase)
        .collect();
    let mut out: Vec<String> = words.windows(2).map(|w| format!("{}.{}", w[0], w[1])).collect();
    out.sort();
    out.dedup();
    out
}

/// `body` field text of the symbol nodes of one file: the source text of each
/// node's span (nested definitions included, so an enclosing function or
/// class also matches the code of its closures and methods), lossily decoded
/// and cut to [`MAX_BODY_BYTES`]. `file_nodes` are node ids of the file;
/// containers (`File`, `Folder`, `Module`, …) and nodes without a valid span
/// get no body.
pub(crate) fn body_texts(file_nodes: &[u32], nodes: &[Node], content: &[u8]) -> Vec<(u32, String)> {
    let mut out = Vec::new();
    for &n in file_nodes {
        let node = &nodes[n as usize];
        if matches!(node.label, Label::File | Label::Folder | Label::Repository | Label::Module | Label::Package) {
            continue;
        }
        let Some(s) = node.span else { continue };
        let (start, end) = (s.start_byte as usize, (s.end_byte as usize).min(content.len()));
        if start >= end {
            continue;
        }
        let bytes = &content[start..end.min(start + MAX_BODY_BYTES)];
        let text = String::from_utf8_lossy(bytes);
        if !text.trim().is_empty() {
            out.push((n, text.into_owned()));
        }
    }
    out
}

fn node_path<'a>(node: &'a Node, files: &'a [FileEntry]) -> &'a str {
    match node.file {
        Some(f) => files.get(f as usize).map_or("", |f| f.path.as_str()),
        None if matches!(node.label, Label::Folder | Label::File) => node.qualified_name.as_str(),
        None => "",
    }
}

/// Builds the tantivy index in `scratch` and embeds its files in the bundle.
/// `bodies[i]` is the `body` text of node `i` (see [`body_texts`]).
pub(crate) fn write_symbols(
    w: &mut BundleWriter,
    nodes: &[Node],
    files: &[FileEntry],
    bodies: &[Option<String>],
    scratch: &Path,
) -> Result<()> {
    build_index(nodes, files, bodies, scratch, None)?;
    embed_index(w, scratch)
}

/// Builds the tantivy index in `scratch`. `commit_every` (tests only) forces
/// several tantivy segments so the ordered merge is exercised.
fn build_index(
    nodes: &[Node],
    files: &[FileEntry],
    bodies: &[Option<String>],
    scratch: &Path,
    commit_every: Option<usize>,
) -> Result<()> {
    let schema = schema();
    let f = fields(&schema)?;
    let node_field = schema.get_field("node").map_err(terr)?;
    let index = Index::create_in_dir(scratch, schema).map_err(terr)?;
    index.tokenizers().register(CODE_TOKENIZER, CodeTokenizer);
    {
        // One indexing thread keeps doc ids in insertion (= node) order inside
        // each tantivy segment; no background merges so the final merge below
        // controls segment order.
        let mut writer = index.writer_with_num_threads::<TantivyDocument>(1, 128 * 1024 * 1024).map_err(terr)?;
        writer.set_merge_policy(Box::new(NoMergePolicy));
        for (i, node) in nodes.iter().enumerate() {
            let mut doc = TantivyDocument::default();
            doc.add_text(f.text[0].0, &node.name);
            doc.add_text(f.text[1].0, &node.qualified_name);
            if let Some(s) = &node.signature {
                doc.add_text(f.text[2].0, s);
            }
            doc.add_text(f.text[3].0, node_path(node, files));
            if let Some(d) = &node.doc {
                doc.add_text(f.text[4].0, d);
            }
            if let Some(Some(b)) = bodies.get(i) {
                doc.add_text(f.text[5].0, b);
            }
            doc.add_text(f.name_exact, node.name.to_lowercase());
            doc.add_text(f.qname_exact, node.qualified_name.to_lowercase());
            if !matches!(node.label, Label::File | Label::Folder | Label::Repository) {
                for suffix in qualified_suffixes(&node.qualified_name) {
                    doc.add_text(f.qname_suffix, suffix);
                }
            }
            doc.add_text(f.label, node.label.as_str());
            if let Some(l) = node.language {
                doc.add_text(f.language, l.as_str());
            }
            doc.add_u64(node_field, i as u64);
            writer.add_document(doc).map_err(terr)?;
            if commit_every.is_some_and(|k| (i + 1) % k == 0) {
                writer.commit().map_err(terr)?;
            }
        }
        writer.commit().map_err(terr)?;
        let ids = segments_in_node_order(&index)?;
        if ids.len() > 1 {
            writer.merge(&ids).wait().map_err(terr)?;
        }
        writer.wait_merging_threads().map_err(terr)?;
    }
    let reader: IndexReader = index.reader_builder().reload_policy(ReloadPolicy::Manual).try_into().map_err(terr)?;
    check_doc_ids(&reader.searcher(), nodes.len())
        .map_err(|e| SegmentError::Other(format!("symbol index invariant violated at write: {e}")))?;
    Ok(())
}

/// Embeds `meta.json` plus exactly the files of the segments it references
/// (never stale or lock files) as `tantivy/<name>` components, sorted by name.
fn embed_index(w: &mut BundleWriter, scratch: &Path) -> Result<()> {
    let index = Index::open_in_dir(scratch).map_err(terr)?;
    let metas = index.load_metas().map_err(terr)?;
    let mut names: Vec<PathBuf> =
        metas.list_segment_files().into_iter().filter(|p| scratch.join(p).is_file()).collect();
    names.push(PathBuf::from("meta.json"));
    names.sort();
    for name in names {
        let bytes = std::fs::read(scratch.join(&name))?;
        let name = name.to_str().ok_or_else(|| SegmentError::Other("non-utf8 tantivy file name".into()))?;
        w.add(&format!("{TANTIVY_PREFIX}{name}"), &bytes)?;
    }
    Ok(())
}

/// Committed segment ids, ordered by the first node id they contain. With a
/// single indexing thread every segment holds a contiguous, ordered run of
/// nodes, so merging in this order yields doc id == node id.
fn segments_in_node_order(index: &Index) -> Result<Vec<SegmentId>> {
    let reader: IndexReader = index.reader_builder().reload_policy(ReloadPolicy::Manual).try_into().map_err(terr)?;
    let searcher = reader.searcher();
    let mut segs = Vec::new();
    for sr in searcher.segment_readers() {
        let col = sr.fast_fields().u64("node").map_err(terr)?;
        segs.push((col.first(0).unwrap_or(u64::MAX), sr.segment_id()));
    }
    segs.sort_by_key(|(first, _)| *first);
    Ok(segs.into_iter().map(|(_, id)| id).collect())
}

/// Verifies the doc id == node id invariant (see module docs).
fn check_doc_ids(searcher: &Searcher, nodes: usize) -> std::result::Result<(), String> {
    let readers = searcher.segment_readers();
    match readers {
        [] if nodes == 0 => Ok(()),
        [sr] => {
            if sr.max_doc() as usize != nodes || sr.num_docs() as usize != nodes {
                return Err(format!("{} docs for {nodes} nodes", sr.max_doc()));
            }
            let col = sr.fast_fields().u64("node").map_err(|e| e.to_string())?;
            for d in 0..sr.max_doc() {
                if col.first(d) != Some(d as u64) {
                    return Err(format!("doc {d} is not node {d}"));
                }
            }
            Ok(())
        }
        _ => Err(format!("{} tantivy segments for {nodes} nodes", readers.len())),
    }
}

/// Read-only tantivy `Directory` over bundle components.
#[derive(Clone)]
pub(crate) struct BundleDirectory {
    map: SharedMmap,
    files: Arc<HashMap<PathBuf, Range<usize>>>,
}

impl fmt::Debug for BundleDirectory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "BundleDirectory({} files)", self.files.len())
    }
}

impl BundleDirectory {
    pub fn new(bundle: &Bundle) -> BundleDirectory {
        let files = bundle
            .components
            .iter()
            .filter_map(|(k, r)| k.strip_prefix(TANTIVY_PREFIX).map(|n| (PathBuf::from(n), r.clone())))
            .collect();
        BundleDirectory { map: bundle.map.clone(), files: Arc::new(files) }
    }

    fn slice(&self, path: &Path) -> std::result::Result<OwnedBytes, OpenReadError> {
        let r = self.files.get(path).ok_or_else(|| OpenReadError::FileDoesNotExist(path.to_path_buf()))?;
        Ok(OwnedBytes::new(self.map.clone()).slice(r.clone()))
    }
}

fn read_only(path: &Path) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, format!("segment bundle is read-only: {}", path.display()))
}

impl Directory for BundleDirectory {
    fn get_file_handle(&self, path: &Path) -> std::result::Result<Arc<dyn FileHandle>, OpenReadError> {
        Ok(Arc::new(self.slice(path)?))
    }
    fn delete(&self, path: &Path) -> std::result::Result<(), DeleteError> {
        Err(DeleteError::IoError { io_error: Arc::new(read_only(path)), filepath: path.to_path_buf() })
    }
    fn exists(&self, path: &Path) -> std::result::Result<bool, OpenReadError> {
        Ok(self.files.contains_key(path))
    }
    fn open_write(&self, path: &Path) -> std::result::Result<WritePtr, OpenWriteError> {
        Err(OpenWriteError::wrap_io_error(read_only(path), path.to_path_buf()))
    }
    fn atomic_read(&self, path: &Path) -> std::result::Result<Vec<u8>, OpenReadError> {
        Ok(self.slice(path)?.as_slice().to_vec())
    }
    fn atomic_write(&self, path: &Path, _data: &[u8]) -> io::Result<()> {
        Err(read_only(path))
    }
    fn sync_directory(&self) -> io::Result<()> {
        Ok(())
    }
    /// Nothing ever writes a bundle, so locks are no-ops (readers take the
    /// meta lock while loading).
    fn acquire_lock(&self, _lock: &Lock) -> std::result::Result<DirectoryLock, LockError> {
        Ok(DirectoryLock::from(Box::new(())))
    }
    fn watch(&self, _cb: WatchCallback) -> tantivy::Result<WatchHandle> {
        Ok(WatchHandle::empty())
    }
}

pub(crate) struct SymbolIndex {
    reader: IndexReader,
    fields: Fields,
    /// Score multiplier per node (doc id), see [`crate::priors`].
    priors: Arc<[f32]>,
    /// The index's only tantivy segment (`None` for an empty graph).
    segment: Option<SegmentId>,
}

impl SymbolIndex {
    /// Opens the embedded index and checks it against the node table.
    /// `bodiless` is the `symbols.bodiless` component (empty when absent).
    pub fn open(
        bundle: &Bundle,
        node_table: &[Node],
        files: &[FileEntry],
        bodiless: &RoaringBitmap,
    ) -> Result<SymbolIndex> {
        let nodes = node_table.len();
        let dir = BundleDirectory::new(bundle);
        if !dir.files.contains_key(Path::new("meta.json")) {
            return Err(SegmentError::Corrupt("missing symbol index".into()));
        }
        let index = Index::open(dir).map_err(|e| SegmentError::Corrupt(format!("symbol index: {e}")))?;
        index.tokenizers().register(CODE_TOKENIZER, CodeTokenizer);
        let fields = fields(&index.schema()).map_err(|e| SegmentError::Corrupt(format!("symbol index schema: {e}")))?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .map_err(|e| SegmentError::Corrupt(format!("symbol index: {e}")))?;
        check_doc_ids(&reader.searcher(), nodes).map_err(|e| SegmentError::Corrupt(format!("symbol index: {e}")))?;
        let priors: Arc<[f32]> = priors::priors(node_table, files, bodiless).into();
        let segment = reader.searcher().segment_readers().first().map(|sr| sr.segment_id());
        Ok(SymbolIndex { reader, fields, priors, segment })
    }

    /// Runs `q`; `path_ok(node)` post-filters (path glob). Over-fetches until
    /// `offset + limit` filtered hits are found or the index is exhausted.
    pub fn search(&self, q: &SymbolQuery, path_ok: &dyn Fn(u32) -> bool) -> Result<Vec<SymbolHit>> {
        let limit = effective_limit(q);
        let searcher = self.reader.searcher();
        let query = build_query(&self.fields, q);
        let priors: Arc<PriorsBySegment> = Arc::new(self.segment.iter().map(|id| (*id, self.priors.clone())).collect());
        let hits = top_hits(
            &searcher,
            query.as_ref(),
            &searcher,
            priors,
            &HashMap::new(),
            q.offset.saturating_add(limit),
            &|_, doc| path_ok(doc),
        )?;
        // doc id == node id (checked at open).
        Ok(hits.into_iter().skip(q.offset).take(limit).map(|h| SymbolHit { node: h.doc, score: h.score }).collect())
    }

    pub fn searcher(&self) -> Searcher {
        self.reader.searcher()
    }

    pub fn fields(&self) -> &Fields {
        &self.fields
    }

    pub fn priors(&self) -> Arc<[f32]> {
        self.priors.clone()
    }

    pub fn segment_id(&self) -> Option<SegmentId> {
        self.segment
    }
}

/// `limit` of a query, `0` meaning [`DEFAULT_LIMIT`].
pub(crate) fn effective_limit(q: &SymbolQuery) -> usize {
    if q.limit == 0 { DEFAULT_LIMIT } else { q.limit }
}

/// Score multipliers per tantivy segment (indexed by doc id = node id). A
/// ranking reads exactly the segments listed here.
pub(crate) type PriorsBySegment = HashMap<SegmentId, Arc<[f32]>>;

/// One ranked document.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct RankedDoc {
    pub segment: SegmentId,
    pub segment_ord: SegmentOrdinal,
    pub doc: DocId,
    pub score: Score,
}

/// The best `want` documents for `query` over the segments of `searcher`
/// listed in `priors` (the others are skipped before any scoring, so they
/// never reach the top-k), each BM25 score multiplied by its prior. BM25
/// statistics come from `stats`: the searcher itself, a subset of its
/// segments ([`SubsetStats`]) or several indexes ([`SummedStats`]).
/// `keep(segment, doc)` post-filters (path globs) with over-fetching.
/// Ordered by (score desc, segment ordinal asc, doc id asc): tantivy breaks
/// score ties by doc address, so paging is exact.
///
/// `boost` multiplies every score of a segment (a repository's routing
/// prior in an organization index); missing segments keep their scores.
pub(crate) fn top_hits(
    searcher: &Searcher,
    query: &dyn Query,
    stats: &dyn Bm25StatisticsProvider,
    priors: Arc<PriorsBySegment>,
    boost: &HashMap<SegmentId, f32>,
    want: usize,
    keep: &dyn Fn(SegmentId, DocId) -> bool,
) -> Result<Vec<RankedDoc>> {
    let total: usize = searcher
        .segment_readers()
        .iter()
        .filter(|sr| priors.contains_key(&sr.segment_id()))
        .map(|sr| sr.max_doc() as usize)
        .sum();
    if total == 0 || want == 0 {
        return Ok(Vec::new());
    }
    let scope: Arc<HashSet<SegmentId>> = Arc::new(priors.keys().copied().collect());
    let boost: Arc<HashMap<SegmentId, f32>> = Arc::new(boost.clone());
    let mut fetch = want.saturating_mul(2).saturating_add(16).min(total);
    loop {
        let p = priors.clone();
        let b = boost.clone();
        let collector = InScope {
            inner: TopDocs::with_limit(fetch).tweak_score(move |sr: &SegmentReader| {
                let seg_priors = p.get(&sr.segment_id()).cloned();
                let seg_boost = b.get(&sr.segment_id()).copied().unwrap_or(1.0);
                move |doc: DocId, score: Score| {
                    score * seg_boost * seg_priors.as_ref().and_then(|x| x.get(doc as usize).copied()).unwrap_or(1.0)
                }
            }),
            scope: scope.clone(),
        };
        let top = searcher.search_with_statistics_provider(query, &collector, stats).map_err(terr)?;
        let exhausted = top.len() < fetch || fetch >= total;
        let mut hits: Vec<RankedDoc> = top
            .iter()
            .filter_map(|(score, addr)| {
                let segment = searcher.segment_reader(addr.segment_ord).segment_id();
                keep(segment, addr.doc_id).then_some(RankedDoc {
                    segment,
                    segment_ord: addr.segment_ord,
                    doc: addr.doc_id,
                    score: *score,
                })
            })
            .collect();
        if hits.len() >= want || exhausted {
            hits.sort_by(|a, b| {
                b.score.total_cmp(&a.score).then(a.segment_ord.cmp(&b.segment_ord)).then(a.doc.cmp(&b.doc))
            });
            hits.truncate(want);
            return Ok(hits);
        }
        fetch = fetch.saturating_mul(4).min(total);
    }
}

/// A collector that reads only the segments in `scope`: the others yield an
/// empty fruit without being scored (the visibility and repository filters
/// of an organization index, applied before top-k).
struct InScope<C> {
    inner: C,
    scope: Arc<HashSet<SegmentId>>,
}

impl<C: Collector> Collector for InScope<C> {
    type Fruit = C::Fruit;
    type Child = C::Child;

    fn check_schema(&self, schema: &Schema) -> tantivy::Result<()> {
        self.inner.check_schema(schema)
    }

    fn for_segment(&self, ord: SegmentOrdinal, reader: &SegmentReader) -> tantivy::Result<C::Child> {
        self.inner.for_segment(ord, reader)
    }

    fn requires_scoring(&self) -> bool {
        self.inner.requires_scoring()
    }

    fn merge_fruits(&self, fruits: Vec<<C::Child as SegmentCollector>::Fruit>) -> tantivy::Result<C::Fruit> {
        self.inner.merge_fruits(fruits)
    }

    fn collect_segment(
        &self,
        weight: &dyn Weight,
        ord: SegmentOrdinal,
        reader: &SegmentReader,
    ) -> tantivy::Result<<C::Child as SegmentCollector>::Fruit> {
        if !self.scope.contains(&reader.segment_id()) {
            return Ok(self.inner.for_segment(ord, reader)?.harvest());
        }
        self.inner.collect_segment(weight, ord, reader)
    }
}

/// BM25 statistics over a subset of the segments of one index (the
/// repositories a caller may read).
pub(crate) struct SubsetStats<'a> {
    pub searcher: &'a Searcher,
    pub segments: &'a HashSet<SegmentId>,
}

impl SubsetStats<'_> {
    fn readers(&self) -> impl Iterator<Item = &SegmentReader> {
        self.searcher.segment_readers().iter().filter(|sr| self.segments.contains(&sr.segment_id()))
    }
}

impl Bm25StatisticsProvider for SubsetStats<'_> {
    fn total_num_tokens(&self, field: Field) -> tantivy::Result<u64> {
        let mut n = 0u64;
        for sr in self.readers() {
            n += sr.inverted_index(field)?.total_num_tokens();
        }
        Ok(n)
    }

    fn total_num_docs(&self) -> tantivy::Result<u64> {
        Ok(self.readers().map(|sr| u64::from(sr.max_doc())).sum())
    }

    fn doc_freq(&self, term: &Term) -> tantivy::Result<u64> {
        let mut n = 0u64;
        for sr in self.readers() {
            n += u64::from(sr.inverted_index(term.field())?.doc_freq(term)?);
        }
        Ok(n)
    }
}

/// BM25 statistics summed over several indexes with the same schema (the
/// symbol indexes of several segments read together): a ranking with these
/// scores every document as if the indexes were one.
pub(crate) struct SummedStats<'a> {
    pub searchers: &'a [Searcher],
}

impl Bm25StatisticsProvider for SummedStats<'_> {
    fn total_num_tokens(&self, field: Field) -> tantivy::Result<u64> {
        let mut n = 0u64;
        for s in self.searchers {
            n += s.total_num_tokens(field)?;
        }
        Ok(n)
    }

    fn total_num_docs(&self) -> tantivy::Result<u64> {
        let mut n = 0u64;
        for s in self.searchers {
            n += Bm25StatisticsProvider::total_num_docs(s)?;
        }
        Ok(n)
    }

    fn doc_freq(&self, term: &Term) -> tantivy::Result<u64> {
        let mut n = 0u64;
        for s in self.searchers {
            n += s.doc_freq(term)?;
        }
        Ok(n)
    }
}

/// The scoring query of `q` (see the crate docs, *Symbol index*) plus its
/// label and language filters.
pub(crate) fn build_query(f: &Fields, q: &SymbolQuery) -> Box<dyn Query> {
    let mut counts: std::collections::BTreeMap<String, u32> = std::collections::BTreeMap::new();
    for t in tokens(&q.text) {
        *counts.entry(t).or_default() += 1;
    }
    let scoring: Box<dyn Query> = if counts.is_empty() {
        Box::new(ConstScoreQuery::new(Box::new(AllQuery), 1.0))
    } else {
        let mut should: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        // One chunk (`getUserById`, `Session.request`) is a name lookup:
        // the body only breaks ties. Several words describe behaviour.
        let name_lookup = q.text.split_whitespace().count() <= 1;
        for (t, &c) in &counts {
            // Sublinear query term frequency: issue text that keeps
            // naming a term is about it.
            let qtf = 1.0 + (c as f32).ln();
            for (i, &(field, boost)) in f.text.iter().enumerate() {
                let boost = if i == BODY_FIELD && name_lookup { BODY_BOOST_NAME_LOOKUP } else { boost };
                let tq = TermQuery::new(Term::from_field_text(field, t), IndexRecordOption::WithFreqs);
                should.push((Occur::Should, Box::new(BoostQuery::new(Box::new(tq), boost * qtf))));
            }
        }
        let whole = q.text.trim().to_lowercase();
        for (field, boost) in [(f.name_exact, NAME_EXACT_BOOST), (f.qname_exact, QNAME_EXACT_BOOST)] {
            let tq = TermQuery::new(Term::from_field_text(field, &whole), IndexRecordOption::Basic);
            should.push((Occur::Should, Box::new(BoostQuery::new(Box::new(tq), boost))));
        }
        for part in dotted_parts(&q.text) {
            let tq = TermQuery::new(Term::from_field_text(f.qname_suffix, &part), IndexRecordOption::Basic);
            should.push((Occur::Should, Box::new(BoostQuery::new(Box::new(tq), QNAME_SUFFIX_BOOST))));
        }
        for part in adjacent_pairs(&q.text) {
            let tq = TermQuery::new(Term::from_field_text(f.qname_suffix, &part), IndexRecordOption::Basic);
            should.push((Occur::Should, Box::new(BoostQuery::new(Box::new(tq), QNAME_PAIR_BOOST))));
        }
        Box::new(BooleanQuery::new(should))
    };
    let mut clauses: Vec<(Occur, Box<dyn Query>)> = vec![(Occur::Must, scoring)];
    let label_terms = |labels: &[Label]| -> Box<dyn Query> {
        let should = labels
            .iter()
            .map(|l| {
                let tq = TermQuery::new(Term::from_field_text(f.label, l.as_str()), IndexRecordOption::Basic);
                (Occur::Should, Box::new(tq) as Box<dyn Query>)
            })
            .collect();
        Box::new(BooleanQuery::new(should))
    };
    if q.labels.is_empty() {
        let mut excluded = DEFAULT_EXCLUDED.to_vec();
        if q.language.is_none_or(|l| l.family() != LanguageFamily::Data) {
            excluded.extend(DATA_DETAIL);
        }
        clauses.push((Occur::MustNot, label_terms(&excluded)));
    } else {
        clauses.push((Occur::Must, Box::new(ConstScoreQuery::new(label_terms(&q.labels), 0.0))));
    }
    if let Some(lang) = q.language {
        let tq = TermQuery::new(Term::from_field_text(f.language, lang.as_str()), IndexRecordOption::Basic);
        clauses.push((Occur::Must, Box::new(ConstScoreQuery::new(Box::new(tq), 0.0))));
    }
    Box::new(BooleanQuery::new(clauses))
}

pub(crate) fn compile_glob(glob: &str) -> Result<GlobMatcher> {
    globset::GlobBuilder::new(glob)
        .literal_separator(true)
        .build()
        .map(|g| g.compile_matcher())
        .map_err(|e| SegmentError::InvalidQuery(format!("bad path glob {glob:?}: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rueca_model::{Language, Span};

    fn node(i: usize) -> Node {
        Node {
            label: Label::Function,
            name: format!("handler{}", i % 3),
            qualified_name: format!("m.handler{i}"),
            file: None,
            span: Some(Span::default()),
            language: Some(Language::Python),
            signature: None,
            doc: None,
            exported: true,
            is_test: false,
        }
    }

    #[test]
    fn qualified_terms() {
        assert_eq!(
            qualified_suffixes("pkg.http.Session.send"),
            ["pkg.http.session.send", "http.session.send", "session.send"]
        );
        assert_eq!(
            qualified_suffixes("src/util/index:Fmt.run"),
            ["src.util.index.fmt.run", "util.index.fmt.run", "index.fmt.run", "fmt.run"]
        );
        assert!(qualified_suffixes("main").is_empty());
        assert_eq!(
            dotted_parts("see Session.request() and util:formatAmount, App\\User::save x"),
            ["app.user.save", "session.request", "util.formatamount"]
        );
        assert!(dotted_parts("plain words only").is_empty());
        assert_eq!(adjacent_pairs("session  request, retry() a.b"), ["request.retry", "session.request"]);
    }

    #[test]
    fn bodies_are_span_text() {
        let src = b"def f():\n    return g()\n";
        let mut nodes = vec![node(0), node(1)];
        nodes[0].span = Some(Span { start_byte: 0, end_byte: src.len() as u32, ..Span::default() });
        nodes[1].span = None;
        let bodies = body_texts(&[0, 1], &nodes, src);
        assert_eq!(bodies, vec![(0, "def f():\n    return g()\n".to_string())]);
        // Out-of-range spans are clamped, never a panic.
        nodes[0].span = Some(Span { start_byte: 4, end_byte: 9_999, ..Span::default() });
        assert_eq!(body_texts(&[0], &nodes, src)[0].1, "f():\n    return g()\n");
    }

    /// Several tantivy segments are merged back into node order.
    #[test]
    fn multi_segment_build_keeps_doc_id_equal_node_id() {
        let nodes: Vec<Node> = (0..250).map(node).collect();
        let scratch = tempfile::tempdir().unwrap();
        build_index(&nodes, &[], &[], scratch.path(), Some(40)).unwrap();
        let index = Index::open_in_dir(scratch.path()).unwrap();
        assert_eq!(index.searchable_segment_metas().unwrap().len(), 1);
        let bundle_path = scratch.path().join("x.rcseg");
        let mut w = BundleWriter::create(&bundle_path, 1).unwrap();
        embed_index(&mut w, scratch.path()).unwrap();
        w.finish().unwrap();
        let bundle = Bundle::open(&bundle_path, 1).unwrap();
        // Only live files were embedded.
        let live = index.load_metas().unwrap().list_segment_files().len();
        assert!(bundle.components.len() <= live + 1);
        let idx = SymbolIndex::open(&bundle, &nodes, &[], &RoaringBitmap::new()).unwrap();
        let mut more = nodes.clone();
        more.push(node(250));
        assert!(matches!(SymbolIndex::open(&bundle, &more, &[], &RoaringBitmap::new()), Err(SegmentError::Corrupt(_))));
        // All 84 `handler0` nodes tie on score: ties come back in node order,
        // and paging across the tie is exact.
        let q = SymbolQuery { text: "handler0".into(), limit: 10, ..Default::default() };
        let page = |offset| idx.search(&SymbolQuery { offset, ..q.clone() }, &|_| true).unwrap();
        let expected: Vec<u32> = (0..250).filter(|i| i % 3 == 0).collect();
        assert_eq!(page(0).iter().map(|h| h.node).collect::<Vec<_>>(), expected[..10]);
        assert_eq!(page(70).iter().map(|h| h.node).collect::<Vec<_>>(), expected[70..80]);
    }
}
