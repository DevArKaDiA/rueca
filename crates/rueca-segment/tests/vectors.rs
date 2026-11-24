//! Symbol vectors in a segment (RFC 0007 §6): written with the bundle or
//! republished onto a lexical segment, opened lazily, scanned exactly.

use std::collections::HashMap;
use std::time::Instant;

use rueca_model::{FileEntry, Label, Language, Node, ResolvedGraph, SnapshotInfo, Span, git_blob_sha};
use rueca_segment::{
    ContentSource, Segment, SegmentError, Store, SymbolQuery, VectorsMeta, VectorsWrite, write_segment,
    write_segment_with,
};

struct Mem(HashMap<String, Vec<u8>>);

impl ContentSource for Mem {
    fn content(&self, file: &FileEntry) -> std::io::Result<Vec<u8>> {
        self.0.get(&file.path).cloned().ok_or_else(|| std::io::Error::other(file.path.clone()))
    }
}

const SRC: &str =
    "def parse_config(path):\n    return load(path)\n\ndef draw_widget():\n    pass\n\nclass Shape:\n    pass\n";
const TEST_SRC: &str = "def test_parse():\n    parse_config('x')\n";

fn span(first: u32, last: u32, start: usize, end: usize) -> Span {
    Span { start_byte: start as u32, end_byte: end as u32, start_line: first, start_col: 0, end_line: last, end_col: 0 }
}

fn node(label: Label, name: &str, qn: &str, file: u32, sp: Option<Span>) -> Node {
    Node {
        label,
        name: name.into(),
        qualified_name: qn.into(),
        file: Some(file),
        span: sp,
        language: Some(Language::Python),
        signature: None,
        doc: None,
        exported: true,
        is_test: false,
    }
}

/// Nodes: 0, 1 files; 2 parse_config; 3 draw_widget; 4 Shape; 5 test_parse.
fn graph() -> (ResolvedGraph, Mem) {
    let sources = [("app/config.py", SRC), ("tests/test_config.py", TEST_SRC)];
    let files = sources
        .iter()
        .enumerate()
        .map(|(i, (p, c))| FileEntry {
            path: (*p).into(),
            blob_sha: git_blob_sha(c.as_bytes()),
            language: Some(Language::Python),
            size: c.len() as u64,
            line_count: c.lines().count() as u32,
            node: i as u32,
            status: "ok".into(),
        })
        .collect();
    let mut nodes: Vec<Node> =
        sources.iter().enumerate().map(|(i, (p, _))| node(Label::File, p, p, i as u32, None)).collect();
    nodes.push(node(Label::Function, "parse_config", "app.config.parse_config", 0, Some(span(0, 1, 0, 46))));
    nodes.push(node(Label::Function, "draw_widget", "app.config.draw_widget", 0, Some(span(3, 4, 47, 75))));
    nodes.push(node(Label::Class, "Shape", "app.config.Shape", 0, Some(span(6, 7, 76, 98))));
    nodes.push(node(Label::Function, "test_parse", "tests.test_config.test_parse", 1, Some(span(0, 1, 0, 40))));
    let graph = ResolvedGraph {
        snapshot: SnapshotInfo {
            repo: "local/app".into(),
            commit: "worktree".into(),
            dirty: false,
            branch: None,
            root: None,
            indexed_at: 0,
            rueca_version: "test".into(),
        },
        nodes,
        edges: vec![],
        files,
        modules: vec![],
        contracts: vec![],
    };
    let mem = Mem(sources.iter().map(|(p, c)| (p.to_string(), c.as_bytes().to_vec())).collect());
    (graph, mem)
}

fn meta(dims: usize) -> VectorsMeta {
    VectorsMeta {
        preset: "hashing".into(),
        embed_key: "ab".repeat(32),
        model_key: "cd".repeat(32),
        model_sha256: "hashing-4".into(),
        dims,
        max_dims: 4,
        encoding: String::new(),
        rows: 0,
        text_builder_version: 1,
        token_budget: 512,
        extra: serde_json::Map::from_iter([("runtime".to_string(), serde_json::json!("hashing"))]),
    }
}

/// Vectors for nodes 2..=5, full width 4.
fn vectors(dims: usize) -> VectorsWrite {
    let mut v = VectorsWrite::new(meta(dims));
    let rows: [(u32, [f32; 4]); 4] =
        [(2, [1.0, 0.0, 0.0, 0.0]), (3, [0.0, 1.0, 0.0, 0.0]), (4, [0.6, 0.0, 0.8, 0.0]), (5, [0.8, 0.0, 0.0, 0.6])];
    for (n, r) in rows {
        v.push(n, [n as u8; 32], &r).unwrap();
    }
    v
}

fn search(seg: &Segment, text: &str) -> Vec<u32> {
    seg.search_symbols(&SymbolQuery { text: text.into(), limit: 10, ..Default::default() })
        .unwrap()
        .into_iter()
        .map(|h| h.node)
        .collect()
}

#[test]
fn vectors_round_trip_and_scan() {
    let (g, mem) = graph();
    let dir = tempfile::tempdir().unwrap();
    let m = write_segment_with(&g, &mem, dir.path(), Some(&vectors(4))).unwrap();
    let seg = Store::open(dir.path()).unwrap().open_segment(&m.id).unwrap();
    let v = seg.vectors().unwrap().expect("vectors");
    assert_eq!((v.len(), v.dims(), v.nodes()), (4, 4, &[2u32, 3, 4, 5][..]));
    assert_eq!(v.meta().encoding, "i8");
    assert_eq!(v.meta().extra["runtime"], "hashing");
    assert_eq!(v.key(1), [3u8; 32]);
    assert_eq!(v.row_of(4), Some(2));
    assert_eq!(v.row_of(1), None);
    let q = [1.0, 0.0, 0.0, 0.0];
    let top = v.top_n(&q, 3, |_| true);
    assert_eq!(top.iter().map(|t| t.0).collect::<Vec<_>>(), vec![2, 5, 4]);
    assert!((top[0].1 - 1.0).abs() < 0.01 && (top[1].1 - 0.8).abs() < 0.01);
    // The filter of a query applies (default labels; a glob).
    let filter = seg.symbol_filter(&SymbolQuery { path_glob: Some("app/**".into()), ..Default::default() }).unwrap();
    let top = v.top_n(&q, 10, &filter);
    assert_eq!(top.iter().map(|t| t.0).collect::<Vec<_>>(), vec![2, 4, 3]);
    let classes = seg.symbol_filter(&SymbolQuery { labels: vec![Label::Class], ..Default::default() }).unwrap();
    assert_eq!(v.top_n(&q, 10, classes).len(), 1);
    // A query of another width matches nothing.
    assert!(v.top_n(&[1.0, 0.0], 3, |_| true).is_empty());
    // Priors are exposed per node: the test function is demoted.
    let priors = seg.priors();
    assert!(priors[5] < priors[2]);
}

#[test]
fn matryoshka_dims_truncate_and_renormalize() {
    let (g, mem) = graph();
    let dir = tempfile::tempdir().unwrap();
    let m = write_segment_with(&g, &mem, dir.path(), Some(&vectors(2))).unwrap();
    let seg = Store::open(dir.path()).unwrap().open_segment(&m.id).unwrap();
    let v = seg.vectors().unwrap().unwrap();
    assert_eq!(v.dims(), 2);
    // Node 5's [0.8, 0, 0, 0.6] keeps [0.8, 0] → [1, 0].
    let row = v.row(v.row_of(5).unwrap());
    assert!((row[0] - 1.0).abs() < 0.01 && row[1].abs() < 0.01);
}

#[test]
fn lexical_segment_is_republished_with_vectors() {
    let (g, mem) = graph();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let lexical = write_segment(&g, &mem, dir.path()).unwrap();
    let seg = store.open_segment(&lexical.id).unwrap();
    assert!(seg.vectors().unwrap().is_none());
    let hybrid = store.republish_with_vectors(&seg, &vectors(4)).unwrap();
    assert_ne!(hybrid.id, lexical.id);
    assert_eq!(hybrid.node_count, lexical.node_count);
    let seg2 = store.open_segment(&hybrid.id).unwrap();
    assert_eq!(seg2.meta().id, hybrid.id);
    assert_eq!(seg2.vectors().unwrap().unwrap().len(), 4);
    // Same lexical answers; the source segment still opens.
    assert_eq!(search(&seg2, "parse config"), search(&seg, "parse config"));
    assert!(store.open_segment(&lexical.id).is_ok());
    // Replacing vectors again drops the old ones.
    let again = store.republish_with_vectors(&seg2, &vectors(2)).unwrap();
    assert_eq!(store.open_segment(&again.id).unwrap().vectors().unwrap().unwrap().dims(), 2);
    // Out-of-range nodes are refused.
    let mut bad = VectorsWrite::new(meta(4));
    bad.push(99, [0; 32], &[1.0, 0.0, 0.0, 0.0]).unwrap();
    assert!(store.republish_with_vectors(&seg, &bad).is_err());
    assert!(bad.push(3, [0; 32], &[1.0; 4]).is_err(), "nodes must ascend");
}

#[test]
fn vector_checksums_are_verified_lazily() {
    let (g, mem) = graph();
    let dir = tempfile::tempdir().unwrap();
    let m = write_segment_with(&g, &mem, dir.path(), Some(&vectors(4))).unwrap();
    let store = Store::open(dir.path()).unwrap();
    let path = store.segment_path(&m.id);
    // Flip one byte of `vectors.data` (the last component before the footer).
    let seg = store.open_segment(&m.id).unwrap();
    let v = seg.vectors().unwrap().unwrap();
    let probe = v.row(0)[0];
    drop(seg);
    let mut bytes = std::fs::read(&path).unwrap();
    let footer_offset = u64::from_le_bytes(bytes[bytes.len() - 56..bytes.len() - 48].try_into().unwrap()) as usize;
    let at = (footer_offset - 1..footer_offset).rev().find(|&i| bytes[i] != 0).unwrap();
    bytes[at] = bytes[at].wrapping_add(1);
    std::fs::write(&path, &bytes).unwrap();
    // Opens, and lexical search works: the vector bulk is not hashed at open.
    let seg = store.open_segment(&m.id).unwrap();
    assert!(!search(&seg, "parse config").is_empty());
    // The first dense use detects it, and keeps reporting it.
    assert!(matches!(seg.vectors(), Err(SegmentError::Corrupt(m)) if m.contains("vectors.")));
    assert!(seg.vectors().is_err());
    assert!(probe.is_finite());
}

/// `cargo test -p rueca-segment --test vectors -- --ignored --nocapture`:
/// ns per row of the exhaustive int8 scan at 1024 dimensions.
#[test]
#[ignore = "micro-benchmark"]
fn scan_speed() {
    let (rows, dims) = (100_000usize, 1024usize);
    let mut v = VectorsWrite::new(VectorsMeta { max_dims: dims, ..meta(dims) });
    let mut x = 0x9e37_79b9_u32;
    let mut row = vec![0.0f32; dims];
    for n in 0..rows {
        for c in row.iter_mut() {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            *c = (x as f32 / u32::MAX as f32) - 0.5;
        }
        v.push(n as u32, [0; 32], &row).unwrap();
    }
    let (g, mem) = synth_graph(rows);
    let dir = tempfile::tempdir().unwrap();
    let m = write_segment_with(&g, &mem, dir.path(), Some(&v)).unwrap();
    let t = Instant::now();
    let seg = Store::open(dir.path()).unwrap().open_segment(&m.id).unwrap();
    let open = t.elapsed();
    let t = Instant::now();
    let vecs = seg.vectors().unwrap().unwrap();
    let verify = t.elapsed();
    let q: Vec<f32> = row.iter().map(|x| x / 16.0).collect();
    let runs = 20;
    let t = Instant::now();
    for _ in 0..runs {
        std::hint::black_box(vecs.top_n(&q, 100, |_| true));
    }
    let per_row = t.elapsed().as_nanos() as f64 / (runs * rows) as f64;
    println!(
        "{rows} × {dims}: open {open:?}, first-use checksum {verify:?}, scan {per_row:.2} ns/row ({:.1} ms per query)",
        per_row * rows as f64 / 1e6
    );
}

/// A graph of `n` function nodes in one file.
fn synth_graph(n: usize) -> (ResolvedGraph, Mem) {
    let src = "x\n".to_string();
    let file = FileEntry {
        path: "a.py".into(),
        blob_sha: git_blob_sha(src.as_bytes()),
        language: Some(Language::Python),
        size: 2,
        line_count: 1,
        node: 0,
        status: "ok".into(),
    };
    let mut nodes = vec![node(Label::File, "a.py", "a.py", 0, None)];
    nodes.extend((1..n).map(|i| node(Label::Function, &format!("f{i}"), &format!("a.f{i}"), 0, None)));
    let (mut g, _) = graph();
    g.nodes = nodes;
    g.files = vec![file];
    (g, Mem(HashMap::from([("a.py".to_string(), src.into_bytes())])))
}
