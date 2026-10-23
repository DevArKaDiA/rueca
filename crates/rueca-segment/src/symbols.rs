//! The symbol index: a tantivy index built in a scratch directory, embedded
//! into the bundle as `tantivy/<file>` components, and served back through a
//! read-only [`BundleDirectory`] over slices of the mmapped bundle (the
//! Quickwit "bundle directory" pattern, without the hotcache layer).
//!
//! Invariant (checked at write and at open): the index has exactly one
//! tantivy segment (zero for an empty graph) and tantivy doc id `d` is node
//! `d`. tantivy breaks score ties by ascending doc address, so its own top-k
//! cut is already ordered by (score desc, node id asc) and paging is exact.

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use globset::GlobMatcher;
use rueca_model::{FileEntry, Label, Node};
use tantivy::collector::TopDocs;
use tantivy::directory::error::{DeleteError, LockError, OpenReadError, OpenWriteError};
use tantivy::directory::{DirectoryLock, FileHandle, Lock, OwnedBytes, WatchCallback, WatchHandle, WritePtr};
use tantivy::index::SegmentId;
use tantivy::indexer::NoMergePolicy;
use tantivy::query::{AllQuery, BooleanQuery, BoostQuery, ConstScoreQuery, Occur, Query, TermQuery};
use tantivy::schema::{Field, IndexRecordOption, NumericOptions, STRING, Schema, TextFieldIndexing, TextOptions};
use tantivy::{Directory, Index, IndexReader, ReloadPolicy, Searcher, TantivyDocument, Term};

use crate::bundle::{Bundle, BundleWriter, SharedMmap};
use crate::tokenizer::{CODE_TOKENIZER, CodeTokenizer, tokens};
use crate::{Result, SegmentError, SymbolHit, SymbolQuery};

pub(crate) const TANTIVY_PREFIX: &str = "tantivy/";
pub(crate) const DEFAULT_LIMIT: usize = 20;

/// Field boosts: name ≫ qualified_name > signature > path > doc.
const BOOSTS: [(&str, f32); 5] =
    [("name", 8.0), ("qualified_name", 3.0), ("signature", 1.5), ("path", 1.0), ("doc", 0.5)];
const NAME_EXACT_BOOST: f32 = 20.0;
const QNAME_EXACT_BOOST: f32 = 10.0;

/// Labels excluded when `SymbolQuery::labels` is empty.
pub(crate) const DEFAULT_EXCLUDED: [Label; 4] = [Label::File, Label::Folder, Label::Module, Label::Repository];

fn terr(e: tantivy::TantivyError) -> SegmentError {
    SegmentError::Other(format!("tantivy: {e}"))
}

struct Fields {
    text: Vec<(Field, f32)>,
    name_exact: Field,
    qname_exact: Field,
    label: Field,
    language: Field,
}

fn schema() -> Schema {
    let mut b = Schema::builder();
    let text = TextOptions::default().set_indexing_options(
        TextFieldIndexing::default().set_tokenizer(CODE_TOKENIZER).set_index_option(IndexRecordOption::WithFreqs),
    );
    for (name, _) in BOOSTS {
        b.add_text_field(name, text.clone());
    }
    b.add_text_field("name_exact", STRING);
    b.add_text_field("qname_exact", STRING);
    b.add_text_field("label", STRING);
    b.add_text_field("language", STRING);
    b.add_u64_field("node", NumericOptions::default().set_fast());
    b.build()
}

fn fields(schema: &Schema) -> Result<Fields> {
    let f = |n: &str| schema.get_field(n).map_err(terr);
    Ok(Fields {
        text: BOOSTS.iter().map(|(n, w)| Ok((f(n)?, *w))).collect::<Result<_>>()?,
        name_exact: f("name_exact")?,
        qname_exact: f("qname_exact")?,
        label: f("label")?,
        language: f("language")?,
    })
}

fn node_path<'a>(node: &'a Node, files: &'a [FileEntry]) -> &'a str {
    match node.file {
        Some(f) => files.get(f as usize).map_or("", |f| f.path.as_str()),
        None if matches!(node.label, Label::Folder | Label::File) => node.qualified_name.as_str(),
        None => "",
    }
}

/// Builds the tantivy index in `scratch` and embeds its files in the bundle.
pub(crate) fn write_symbols(w: &mut BundleWriter, nodes: &[Node], files: &[FileEntry], scratch: &Path) -> Result<()> {
    build_index(nodes, files, scratch, None)?;
    embed_index(w, scratch)
}

/// Builds the tantivy index in `scratch`. `commit_every` (tests only) forces
/// several tantivy segments so the ordered merge is exercised.
fn build_index(nodes: &[Node], files: &[FileEntry], scratch: &Path, commit_every: Option<usize>) -> Result<()> {
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
            doc.add_text(f.name_exact, node.name.to_lowercase());
            doc.add_text(f.qname_exact, node.qualified_name.to_lowercase());
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
}

impl SymbolIndex {
    /// Opens the embedded index and checks it against the node table.
    pub fn open(bundle: &Bundle, nodes: usize) -> Result<SymbolIndex> {
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
        Ok(SymbolIndex { reader, fields })
    }

    fn build_query(&self, q: &SymbolQuery) -> Box<dyn Query> {
        let f = &self.fields;
        let mut toks = tokens(&q.text);
        toks.sort();
        toks.dedup();
        let scoring: Box<dyn Query> = if toks.is_empty() {
            Box::new(ConstScoreQuery::new(Box::new(AllQuery), 1.0))
        } else {
            let mut should: Vec<(Occur, Box<dyn Query>)> = Vec::new();
            for t in &toks {
                for &(field, boost) in &f.text {
                    let tq = TermQuery::new(Term::from_field_text(field, t), IndexRecordOption::WithFreqs);
                    should.push((Occur::Should, Box::new(BoostQuery::new(Box::new(tq), boost))));
                }
            }
            let whole = q.text.trim().to_lowercase();
            for (field, boost) in [(f.name_exact, NAME_EXACT_BOOST), (f.qname_exact, QNAME_EXACT_BOOST)] {
                let tq = TermQuery::new(Term::from_field_text(field, &whole), IndexRecordOption::Basic);
                should.push((Occur::Should, Box::new(BoostQuery::new(Box::new(tq), boost))));
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
            clauses.push((Occur::MustNot, label_terms(&DEFAULT_EXCLUDED)));
        } else {
            clauses.push((Occur::Must, Box::new(ConstScoreQuery::new(label_terms(&q.labels), 0.0))));
        }
        if let Some(lang) = q.language {
            let tq = TermQuery::new(Term::from_field_text(f.language, lang.as_str()), IndexRecordOption::Basic);
            clauses.push((Occur::Must, Box::new(ConstScoreQuery::new(Box::new(tq), 0.0))));
        }
        Box::new(BooleanQuery::new(clauses))
    }

    /// Runs `q`; `path_ok(node)` post-filters (path glob). Over-fetches until
    /// `offset + limit` filtered hits are found or the index is exhausted.
    pub fn search(&self, q: &SymbolQuery, path_ok: &dyn Fn(u32) -> bool) -> Result<Vec<SymbolHit>> {
        let limit = if q.limit == 0 { DEFAULT_LIMIT } else { q.limit };
        let want = q.offset.saturating_add(limit);
        let query = self.build_query(q);
        let searcher = self.reader.searcher();
        let total_docs = searcher.num_docs() as usize;
        let mut fetch = want.saturating_mul(2).saturating_add(16).min(total_docs.max(1));
        loop {
            let top = searcher.search(&query, &TopDocs::with_limit(fetch).order_by_score()).map_err(terr)?;
            let exhausted = top.len() < fetch || fetch >= total_docs;
            // doc id == node id (checked at open).
            let mut hits: Vec<SymbolHit> = top
                .iter()
                .filter(|(_, addr)| path_ok(addr.doc_id))
                .map(|(score, addr)| SymbolHit { node: addr.doc_id, score: *score })
                .collect();
            if hits.len() >= want || exhausted {
                hits.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.node.cmp(&b.node)));
                return Ok(hits.into_iter().skip(q.offset).take(limit).collect());
            }
            fetch = fetch.saturating_mul(4).min(total_docs);
        }
    }
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

    /// Several tantivy segments are merged back into node order.
    #[test]
    fn multi_segment_build_keeps_doc_id_equal_node_id() {
        let nodes: Vec<Node> = (0..250).map(node).collect();
        let scratch = tempfile::tempdir().unwrap();
        build_index(&nodes, &[], scratch.path(), Some(40)).unwrap();
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
        let idx = SymbolIndex::open(&bundle, nodes.len()).unwrap();
        assert!(matches!(SymbolIndex::open(&bundle, nodes.len() + 1), Err(SegmentError::Corrupt(_))));
        // All 84 `handler0` nodes tie on score: ties come back in node order,
        // and paging across the tie is exact.
        let q = SymbolQuery { text: "handler0".into(), limit: 10, ..Default::default() };
        let page = |offset| idx.search(&SymbolQuery { offset, ..q.clone() }, &|_| true).unwrap();
        let expected: Vec<u32> = (0..250).filter(|i| i % 3 == 0).collect();
        assert_eq!(page(0).iter().map(|h| h.node).collect::<Vec<_>>(), expected[..10]);
        assert_eq!(page(70).iter().map(|h| h.node).collect::<Vec<_>>(), expected[70..80]);
    }
}
