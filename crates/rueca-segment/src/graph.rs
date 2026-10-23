//! Graph components: CSR adjacency (both directions), the edge-meta table,
//! name indexes and per-file node lists. Built at write time, validated at
//! open time, read zero-copy from the mmapped bundle.

use bytemuck::{Pod, Zeroable};
use rueca_model::{EdgeKind, EdgeMeta, FileEntry, Label, Node, ResolvedGraph, Span, Strategy};

use crate::bundle::BundleWriter;
use crate::{Result, SegmentError};

/// `Strategy` ↔ `u8` (declaration order).
pub(crate) const STRATEGIES: [Strategy; 7] = [
    Strategy::Syntactic,
    Strategy::Scip,
    Strategy::TypeResolved,
    Strategy::ImportResolved,
    Strategy::SameModule,
    Strategy::UniqueName,
    Strategy::Heuristic,
];

pub(crate) fn strategy_code(s: Strategy) -> u8 {
    match s {
        Strategy::Syntactic => 0,
        Strategy::Scip => 1,
        Strategy::TypeResolved => 2,
        Strategy::ImportResolved => 3,
        Strategy::SameModule => 4,
        Strategy::UniqueName => 5,
        Strategy::Heuristic => 6,
    }
}

/// One row of the `edge_meta` component (36 bytes, little-endian, no padding).
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub(crate) struct EdgeMetaRec {
    pub confidence: f32,
    pub strategy: u8,
    /// 1 if `evidence_*` is meaningful.
    pub has_evidence: u8,
    pub _pad: [u8; 2],
    pub evidence_file: u32,
    /// start_byte, end_byte, start_line, start_col, end_line, end_col.
    pub evidence_span: [u32; 6],
}

impl EdgeMetaRec {
    pub fn encode(m: &EdgeMeta) -> EdgeMetaRec {
        let (has, file, span) = match m.evidence {
            Some((f, s)) => (1, f, [s.start_byte, s.end_byte, s.start_line, s.start_col, s.end_line, s.end_col]),
            None => (0, 0, [0; 6]),
        };
        EdgeMetaRec {
            confidence: m.confidence,
            strategy: strategy_code(m.strategy),
            has_evidence: has,
            _pad: [0; 2],
            evidence_file: file,
            evidence_span: span,
        }
    }

    /// Only called on validated records.
    pub fn decode(&self) -> EdgeMeta {
        let s = self.evidence_span;
        EdgeMeta {
            confidence: self.confidence,
            strategy: STRATEGIES[self.strategy as usize],
            evidence: (self.has_evidence == 1).then_some((
                self.evidence_file,
                Span {
                    start_byte: s[0],
                    end_byte: s[1],
                    start_line: s[2],
                    start_col: s[3],
                    end_line: s[4],
                    end_col: s[5],
                },
            )),
        }
    }
}

/// Labels never returned by `nodes_in_file`.
fn is_container(label: Label) -> bool {
    matches!(label, Label::File | Label::Folder | Label::Repository)
}

/// Writes every graph component of `graph` into the bundle.
pub(crate) fn write_graph(w: &mut BundleWriter, graph: &ResolvedGraph) -> Result<()> {
    let n = graph.nodes.len();
    let e = graph.edges.len();
    if n >= u32::MAX as usize || e >= u32::MAX as usize || graph.files.len() >= u32::MAX as usize {
        return Err(SegmentError::Other("graph too large for format v1 (u32 ids)".into()));
    }
    w.add("nodes", &postcard::to_stdvec(&graph.nodes).map_err(|e| SegmentError::Other(e.to_string()))?)?;
    w.add("files", &postcard::to_stdvec(&graph.files).map_err(|e| SegmentError::Other(e.to_string()))?)?;

    let metas: Vec<EdgeMetaRec> = graph.edges.iter().map(|e| EdgeMetaRec::encode(&e.meta)).collect();
    w.add_pod("edge_meta", &metas)?;

    for dir in ["out", "in"] {
        let mut order: Vec<u32> = (0..e as u32).collect();
        let key = |i: u32| {
            let ed = &graph.edges[i as usize];
            let (a, b) = if dir == "out" { (ed.src, ed.dst) } else { (ed.dst, ed.src) };
            (a, ed.kind.index() as u8, b, i)
        };
        order.sort_unstable_by_key(|&i| key(i));
        let mut offsets = vec![0u32; n + 1];
        let mut targets = Vec::with_capacity(e);
        let mut kinds = Vec::with_capacity(e);
        for &i in &order {
            let (a, k, b, _) = key(i);
            offsets[a as usize + 1] += 1;
            targets.push(b);
            kinds.push(k);
        }
        for j in 0..n {
            offsets[j + 1] += offsets[j];
        }
        w.add_pod(&format!("adj.{dir}.offsets"), &offsets)?;
        w.add_pod(&format!("adj.{dir}.targets"), &targets)?;
        w.add_pod(&format!("adj.{dir}.kinds"), &kinds)?;
        w.add_pod(&format!("adj.{dir}.edges"), &order)?;
    }

    let mut by_q: Vec<u32> = (0..n as u32).collect();
    by_q.sort_by(|&a, &b| {
        graph.nodes[a as usize].qualified_name.cmp(&graph.nodes[b as usize].qualified_name).then(a.cmp(&b))
    });
    w.add_pod("names.qualified", &by_q)?;
    let mut by_n: Vec<u32> = (0..n as u32).collect();
    by_n.sort_by(|&a, &b| graph.nodes[a as usize].name.cmp(&graph.nodes[b as usize].name).then(a.cmp(&b)));
    w.add_pod("names.simple", &by_n)?;
    let mut by_path: Vec<u32> = (0..graph.files.len() as u32).collect();
    by_path.sort_by(|&a, &b| graph.files[a as usize].path.cmp(&graph.files[b as usize].path).then(a.cmp(&b)));
    w.add_pod("files.by_path", &by_path)?;

    // Per-file symbol lists, sorted by (span start, node id).
    let mut per_file: Vec<(u32, u32, u32)> = graph
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, nd)| !is_container(nd.label))
        .filter_map(|(i, nd)| nd.file.map(|f| (f, nd.span.map_or(u32::MAX, |s| s.start_byte), i as u32)))
        .collect();
    per_file.sort_unstable();
    let mut offsets = vec![0u32; graph.files.len() + 1];
    for &(f, _, _) in &per_file {
        offsets[f as usize + 1] += 1;
    }
    for j in 0..graph.files.len() {
        offsets[j + 1] += offsets[j];
    }
    let ids: Vec<u32> = per_file.iter().map(|&(_, _, i)| i).collect();
    w.add_pod("file_nodes.offsets", &offsets)?;
    w.add_pod("file_nodes.ids", &ids)?;
    Ok(())
}

fn corrupt(msg: impl Into<String>) -> SegmentError {
    SegmentError::Corrupt(msg.into())
}

/// Validates decoded tables against each other (ids in range).
pub(crate) fn validate_tables(nodes: &[Node], files: &[FileEntry]) -> Result<()> {
    for (i, nd) in nodes.iter().enumerate() {
        if nd.file.is_some_and(|f| f as usize >= files.len()) {
            return Err(corrupt(format!("node {i} file out of range")));
        }
    }
    for (i, f) in files.iter().enumerate() {
        if f.node as usize >= nodes.len() {
            return Err(corrupt(format!("file {i} node out of range")));
        }
    }
    Ok(())
}

pub(crate) fn validate_csr(offsets: &[u32], targets_len: usize, rows: usize, name: &str) -> Result<()> {
    if offsets.len() != rows + 1
        || offsets.first() != Some(&0)
        || offsets.last().map(|&l| l as usize) != Some(targets_len)
    {
        return Err(corrupt(format!("{name}: bad offsets")));
    }
    if offsets.windows(2).any(|w| w[0] > w[1]) {
        return Err(corrupt(format!("{name}: offsets not monotonic")));
    }
    Ok(())
}

pub(crate) fn validate_ids(ids: &[u32], bound: usize, len: Option<usize>, name: &str) -> Result<()> {
    if len.is_some_and(|l| l != ids.len()) {
        return Err(corrupt(format!("{name}: wrong length")));
    }
    if ids.iter().any(|&i| i as usize >= bound) {
        return Err(corrupt(format!("{name}: id out of range")));
    }
    Ok(())
}

pub(crate) fn validate_adjacency(
    offsets: &[u32],
    targets: &[u32],
    kinds: &[u8],
    edges: &[u32],
    n: usize,
    e: usize,
    name: &str,
) -> Result<()> {
    validate_csr(offsets, e, n, name)?;
    validate_ids(targets, n, Some(e), name)?;
    validate_ids(edges, e, Some(e), name)?;
    if kinds.len() != e || kinds.iter().any(|&k| k as usize >= EdgeKind::ALL.len()) {
        return Err(corrupt(format!("{name}: bad edge kinds")));
    }
    Ok(())
}

pub(crate) fn validate_edge_meta(metas: &[EdgeMetaRec], files: usize) -> Result<()> {
    for m in metas {
        if m.strategy as usize >= STRATEGIES.len()
            || m.has_evidence > 1
            || (m.has_evidence == 1 && m.evidence_file as usize >= files)
        {
            return Err(corrupt("bad edge meta"));
        }
    }
    Ok(())
}
