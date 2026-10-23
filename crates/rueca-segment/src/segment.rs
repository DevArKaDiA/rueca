//! Opening a bundle and the read paths behind [`crate::Segment`].

use std::ops::Range;
use std::path::Path;

use roaring::RoaringBitmap;
use rueca_model::{EdgeKind, FileEntry, FileId, Node, NodeId};

use crate::bundle::Bundle;
use crate::graph::{self, EdgeMetaRec};
use crate::ngram::{self, NgramIndex};
use crate::packs::PackReader;
use crate::symbols::{self, SymbolIndex};
use crate::{
    ContentMatch, ContentQuery, ContentSearchResult, EdgeRef, FORMAT_VERSION, Result, SegmentError, SegmentMeta,
    SymbolHit, SymbolQuery,
};

pub(crate) const DEFAULT_MAX_MATCHES: usize = 100;

struct Adj {
    offsets: Range<usize>,
    targets: Range<usize>,
    kinds: Range<usize>,
    edges: Range<usize>,
}

pub(crate) struct Inner {
    bundle: Bundle,
    pub meta: SegmentMeta,
    pub nodes: Vec<Node>,
    pub files: Vec<FileEntry>,
    out: Adj,
    inc: Adj,
    edge_meta: Range<usize>,
    names_q: Range<usize>,
    names_s: Range<usize>,
    files_by_path: Range<usize>,
    file_nodes_off: Range<usize>,
    file_nodes_ids: Range<usize>,
    ng_keys: Range<usize>,
    ng_offsets: Range<usize>,
    ng_postings: Range<usize>,
    text_files: RoaringBitmap,
    symbols: SymbolIndex,
    packs: PackReader,
}

fn corrupt(msg: impl Into<String>) -> SegmentError {
    SegmentError::Corrupt(msg.into())
}

impl Inner {
    /// Zero-copy view of a validated array component.
    fn arr<T: bytemuck::Pod>(&self, r: &Range<usize>) -> &[T] {
        bytemuck::cast_slice(&self.bundle.map[r.clone()])
    }

    pub fn open(path: &Path, store: &Path) -> Result<Inner> {
        let bundle = Bundle::open(path, FORMAT_VERSION)?;
        let mut meta: SegmentMeta =
            serde_json::from_slice(bundle.bytes("meta")?).map_err(|e| corrupt(format!("meta: {e}")))?;
        if meta.format_version != FORMAT_VERSION {
            return Err(SegmentError::Version(meta.format_version));
        }
        meta.bytes = bundle.map.len() as u64;
        let nodes: Vec<Node> =
            postcard::from_bytes(bundle.bytes("nodes")?).map_err(|e| corrupt(format!("nodes: {e}")))?;
        let files: Vec<FileEntry> =
            postcard::from_bytes(bundle.bytes("files")?).map_err(|e| corrupt(format!("files: {e}")))?;
        graph::validate_tables(&nodes, &files)?;
        let (n, f) = (nodes.len(), files.len());
        if meta.node_count as usize != n || meta.file_count as usize != f {
            return Err(corrupt("meta counts disagree with tables"));
        }
        let e = meta.edge_count as usize;

        let edge_meta = bundle.pod_range::<EdgeMetaRec>("edge_meta")?;
        let em: &[EdgeMetaRec] = bytemuck::cast_slice(&bundle.map[edge_meta.clone()]);
        if em.len() != e {
            return Err(corrupt("edge_meta length"));
        }
        graph::validate_edge_meta(em, f)?;

        let adj = |dir: &str| -> Result<Adj> {
            let a = Adj {
                offsets: bundle.pod_range::<u32>(&format!("adj.{dir}.offsets"))?,
                targets: bundle.pod_range::<u32>(&format!("adj.{dir}.targets"))?,
                kinds: bundle.range(&format!("adj.{dir}.kinds"))?,
                edges: bundle.pod_range::<u32>(&format!("adj.{dir}.edges"))?,
            };
            let m = &bundle.map;
            graph::validate_adjacency(
                bytemuck::cast_slice(&m[a.offsets.clone()]),
                bytemuck::cast_slice(&m[a.targets.clone()]),
                &m[a.kinds.clone()],
                bytemuck::cast_slice(&m[a.edges.clone()]),
                n,
                e,
                dir,
            )?;
            Ok(a)
        };
        let out = adj("out")?;
        let inc = adj("in")?;

        let ids = |name: &str, bound: usize, len: Option<usize>| -> Result<Range<usize>> {
            let r = bundle.pod_range::<u32>(name)?;
            graph::validate_ids(bytemuck::cast_slice(&bundle.map[r.clone()]), bound, len, name)?;
            Ok(r)
        };
        let names_q = ids("names.qualified", n, Some(n))?;
        let names_s = ids("names.simple", n, Some(n))?;
        let files_by_path = ids("files.by_path", f, Some(f))?;
        let file_nodes_ids = ids("file_nodes.ids", n, None)?;
        let file_nodes_off = bundle.pod_range::<u32>("file_nodes.offsets")?;
        graph::validate_csr(
            bytemuck::cast_slice(&bundle.map[file_nodes_off.clone()]),
            file_nodes_ids.len() / 4,
            f,
            "file_nodes",
        )?;

        let ng_keys = bundle.pod_range::<u32>("ngrams.keys")?;
        let ng_offsets = bundle.pod_range::<u64>("ngrams.offsets")?;
        let ng_postings = bundle.range("ngrams.postings")?;
        {
            let offs: &[u64] = bytemuck::cast_slice(&bundle.map[ng_offsets.clone()]);
            if offs.len() != ng_keys.len() / 4 + 1
                || offs[0] != 0
                || offs.windows(2).any(|w| w[0] > w[1])
                || offs[offs.len() - 1] != ng_postings.len() as u64
            {
                return Err(corrupt("ngram offsets"));
            }
        }
        let text_files = RoaringBitmap::deserialize_from(bundle.bytes("ngrams.text_files")?)
            .map_err(|e| corrupt(format!("ngrams.text_files: {e}")))?;
        if text_files.max().is_some_and(|m| m as usize >= f) {
            return Err(corrupt("ngrams.text_files out of range"));
        }

        let symbols = SymbolIndex::open(&bundle, n)?;
        Ok(Inner {
            meta,
            nodes,
            files,
            out,
            inc,
            edge_meta,
            names_q,
            names_s,
            files_by_path,
            file_nodes_off,
            file_nodes_ids,
            ng_keys,
            ng_offsets,
            ng_postings,
            text_files,
            symbols,
            packs: PackReader::new(store),
            bundle,
        })
    }

    pub fn file_by_path(&self, path: &str) -> Option<FileId> {
        let ids: &[u32] = self.arr(&self.files_by_path);
        let i = ids.partition_point(|&id| self.files[id as usize].path.as_str() < path);
        ids.get(i).copied().filter(|&id| self.files[id as usize].path == path)
    }

    fn edges(&self, adj: &Adj, node: NodeId, kind: Option<EdgeKind>) -> Vec<EdgeRef> {
        let offsets: &[u32] = self.arr(&adj.offsets);
        let (Some(&a), Some(&b)) = (offsets.get(node as usize), offsets.get(node as usize + 1)) else {
            return Vec::new();
        };
        let (mut a, mut b) = (a as usize, b as usize);
        let kinds = &self.bundle.map[adj.kinds.clone()];
        if let Some(k) = kind {
            let k = k.index() as u8;
            let row = &kinds[a..b];
            let lo = row.partition_point(|&x| x < k);
            let hi = row.partition_point(|&x| x <= k);
            b = a + hi;
            a += lo;
        }
        let targets: &[u32] = self.arr(&adj.targets);
        let edges: &[u32] = self.arr(&adj.edges);
        let metas: &[EdgeMetaRec] = self.arr(&self.edge_meta);
        (a..b)
            .map(|i| EdgeRef {
                other: targets[i],
                kind: EdgeKind::ALL[kinds[i] as usize],
                meta: metas[edges[i] as usize].decode(),
            })
            .collect()
    }

    pub fn out_edges(&self, node: NodeId, kind: Option<EdgeKind>) -> Vec<EdgeRef> {
        self.edges(&self.out, node, kind)
    }

    pub fn in_edges(&self, node: NodeId, kind: Option<EdgeKind>) -> Vec<EdgeRef> {
        self.edges(&self.inc, node, kind)
    }

    fn lookup(&self, r: &Range<usize>, key: &str, get: impl Fn(&Node) -> &str) -> Vec<NodeId> {
        let ids: &[u32] = self.arr(r);
        let lo = ids.partition_point(|&id| get(&self.nodes[id as usize]) < key);
        let hi = lo + ids[lo..].partition_point(|&id| get(&self.nodes[id as usize]) <= key);
        ids[lo..hi].to_vec()
    }

    pub fn nodes_by_qualified_name(&self, q: &str) -> Vec<NodeId> {
        self.lookup(&self.names_q, q, |n| n.qualified_name.as_str())
    }

    pub fn nodes_by_name(&self, name: &str) -> Vec<NodeId> {
        self.lookup(&self.names_s, name, |n| n.name.as_str())
    }

    pub fn nodes_in_file(&self, file: FileId) -> Vec<NodeId> {
        let offsets: &[u32] = self.arr(&self.file_nodes_off);
        let (Some(&a), Some(&b)) = (offsets.get(file as usize), offsets.get(file as usize + 1)) else {
            return Vec::new();
        };
        let ids: &[u32] = self.arr(&self.file_nodes_ids);
        ids[a as usize..b as usize].to_vec()
    }

    fn node_path(&self, node: NodeId) -> &str {
        let nd = &self.nodes[node as usize];
        match nd.file {
            Some(f) => &self.files[f as usize].path,
            None => &nd.qualified_name,
        }
    }

    pub fn search_symbols(&self, q: &SymbolQuery) -> Result<Vec<SymbolHit>> {
        let glob = q.path_glob.as_deref().map(symbols::compile_glob).transpose()?;
        let n = self.nodes.len();
        let path_ok = |node: u32| -> bool {
            (node as usize) < n && glob.as_ref().is_none_or(|g| g.is_match(self.node_path(node)))
        };
        self.symbols.search(q, &path_ok)
    }

    pub fn content(&self, file: FileId) -> Result<Vec<u8>> {
        let f = self.files.get(file as usize).ok_or_else(|| SegmentError::NotFound(format!("file id {file}")))?;
        self.packs.read(&f.blob_sha)
    }

    pub fn search_content(&self, q: &ContentQuery) -> Result<ContentSearchResult> {
        if q.pattern.is_empty() {
            return Err(SegmentError::InvalidQuery("empty pattern".into()));
        }
        let pattern = if q.regex { q.pattern.clone() } else { regex::escape(&q.pattern) };
        let plan = ngram::plan(&pattern, q.case_insensitive)?;
        let re = regex::bytes::RegexBuilder::new(&pattern)
            .case_insensitive(q.case_insensitive)
            .multi_line(true)
            .size_limit(64 << 20)
            .build()
            .map_err(|e| SegmentError::InvalidQuery(e.to_string()))?;
        let glob = q.path_glob.as_deref().map(symbols::compile_glob).transpose()?;
        let index = NgramIndex {
            keys: self.arr(&self.ng_keys),
            offsets: self.arr(&self.ng_offsets),
            postings: &self.bundle.map[self.ng_postings.clone()],
        };
        let selected = index.eval(&plan, &self.text_files)? & &self.text_files;
        let by_path: &[u32] = self.arr(&self.files_by_path);
        let candidates: Vec<u32> = by_path
            .iter()
            .copied()
            .filter(|&f| selected.contains(f))
            .filter(|&f| {
                let fe = &self.files[f as usize];
                q.language.is_none_or(|l| fe.language == Some(l)) && glob.as_ref().is_none_or(|g| g.is_match(&fe.path))
            })
            .collect();
        let max = if q.max_matches == 0 { DEFAULT_MAX_MATCHES } else { q.max_matches };
        let mut res = ContentSearchResult { candidate_files: candidates.len() as u32, ..Default::default() };
        let mut scanned = 0u64;
        'files: for f in candidates {
            let size = self.files[f as usize].size;
            if scanned.saturating_add(size) > q.scan_budget_bytes {
                res.truncated = true;
                break;
            }
            let content = self.content(f)?;
            scanned += content.len() as u64;
            res.files_scanned += 1;
            let mut cursor = (0usize, 1u32); // (byte offset of a line start, its 1-based line number)
            let mut pos = 0usize;
            while pos <= content.len() {
                let Some(m) = re.find_at(&content, pos) else { break };
                if m.start() == content.len() && (content.is_empty() || content.ends_with(b"\n")) {
                    break; // empty match after the final newline: no such line
                }
                let (ls, le) = line_bounds(&content, m.start());
                if res.matches.len() >= max {
                    res.truncated = true;
                    break 'files;
                }
                let line = cursor.1 + count_newlines(&content[cursor.0..ls]);
                cursor = (ls, line);
                let ctx = q.context_lines as usize;
                res.matches.push(ContentMatch {
                    file: f,
                    line,
                    text: line_text(&content[ls..le]),
                    before: lines_before(&content, ls, ctx),
                    after: lines_after(&content, le, ctx),
                });
                pos = le + 1;
            }
        }
        Ok(res)
    }
}

fn count_newlines(b: &[u8]) -> u32 {
    b.iter().filter(|&&c| c == b'\n').count() as u32
}

/// `[start, end)` of the line containing `at` (end excludes the `\n`).
fn line_bounds(c: &[u8], at: usize) -> (usize, usize) {
    let at = at.min(c.len());
    let start = c[..at].iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    let end = c[at..].iter().position(|&b| b == b'\n').map_or(c.len(), |i| at + i);
    (start, end)
}

fn line_text(b: &[u8]) -> String {
    let b = b.strip_suffix(b"\r").unwrap_or(b);
    String::from_utf8_lossy(b).into_owned()
}

fn lines_before(c: &[u8], line_start: usize, n: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut end = line_start;
    while out.len() < n && end > 0 {
        let (s, e) = line_bounds(c, end - 1);
        out.push(line_text(&c[s..e]));
        end = s;
    }
    out.reverse();
    out
}

fn lines_after(c: &[u8], line_end: usize, n: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut start = line_end + 1;
    while out.len() < n && start < c.len() {
        let (s, e) = line_bounds(c, start);
        out.push(line_text(&c[s..e]));
        start = e + 1;
    }
    out
}
