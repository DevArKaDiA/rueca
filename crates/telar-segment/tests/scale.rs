//! Scale smoke test. Run with:
//! `cargo test -p telar-segment --release --test scale -- --ignored --nocapture`

use std::collections::HashMap;
use std::time::{Duration, Instant};

use telar_model::{
    Edge, EdgeKind, EdgeMeta, FileEntry, Label, Language, Node, ResolvedGraph, SnapshotInfo, Span, Strategy,
    git_blob_sha,
};
use telar_segment::{ContentQuery, ContentSource, Segment, SymbolQuery, write_segment};

const WORDS: &[&str] = &[
    "user", "account", "invoice", "payment", "order", "item", "cart", "price", "tax", "report", "cache", "client",
    "server", "request", "response", "token", "session", "auth", "loan", "score", "model", "feature", "batch",
    "queue", "worker", "event", "stream", "record", "parser", "builder", "handler", "service", "repo", "store",
    "index", "query", "filter", "mapper", "config", "logger", "metric", "trace", "retry", "limit", "window",
    "graph", "node", "edge", "path", "file",
];
const VERBS: &[&str] = &["get", "set", "load", "save", "build", "parse", "fetch", "update", "delete", "compute", "validate", "emit"];

struct Mem(HashMap<String, Vec<u8>>);
impl ContentSource for Mem {
    fn content(&self, file: &FileEntry) -> std::io::Result<Vec<u8>> {
        Ok(self.0[&file.path].clone())
    }
}

fn p50(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[v.len() / 2]
}

fn synth(files_n: usize, syms_per_file: usize, edges_n: usize) -> (ResolvedGraph, Mem) {
    let mut rng = fastrand::Rng::with_seed(42);
    let mut nodes = Vec::new();
    let mut files = Vec::new();
    let mut contents = HashMap::new();
    let mut edges = Vec::new();
    let mut sym_ids = Vec::new();
    for f in 0..files_n {
        let path = format!("src/{}/{}_{f}.py", WORDS[f % WORDS.len()], WORDS[(f / 7) % WORDS.len()]);
        let file_node = nodes.len() as u32;
        nodes.push(Node {
            label: Label::File,
            name: path.rsplit('/').next().unwrap().into(),
            qualified_name: path.clone(),
            file: Some(f as u32),
            span: None,
            language: Some(Language::Python),
            signature: None,
            doc: None,
            exported: true,
            is_test: false,
        });
        let mut text = String::new();
        for s in 0..syms_per_file {
            let verb = VERBS[rng.usize(..VERBS.len())];
            let a = WORDS[rng.usize(..WORDS.len())];
            let b = WORDS[rng.usize(..WORDS.len())];
            let name = format!("{verb}{}{}{s}", cap(a), cap(b));
            let start = text.len() as u32;
            text.push_str(&format!(
                "def {name}({a}_id, {b}=None):\n    \"\"\"{verb} the {a} for {b}.\"\"\"\n    value = {}({a}_id) * {}\n    return value\n\n",
                VERBS[rng.usize(..VERBS.len())],
                rng.u32(..100_000)
            ));
            let id = nodes.len() as u32;
            nodes.push(Node {
                label: if s % 5 == 0 { Label::Class } else { Label::Function },
                name: name.clone(),
                qualified_name: format!("src.{a}.mod{f}.{name}"),
                file: Some(f as u32),
                span: Some(Span { start_byte: start, end_byte: text.len() as u32, start_line: 0, start_col: 0, end_line: 0, end_col: 0 }),
                language: Some(Language::Python),
                signature: Some(format!("def {name}({a}_id, {b}=None)")),
                doc: Some(format!("{verb} the {a} for {b}.")),
                exported: true,
                is_test: false,
            });
            edges.push(Edge {
                src: file_node,
                dst: id,
                kind: EdgeKind::Defines,
                meta: EdgeMeta { confidence: 1.0, strategy: Strategy::Syntactic, evidence: None },
            });
            sym_ids.push(id);
        }
        let bytes = text.into_bytes();
        files.push(FileEntry {
            path: path.clone(),
            blob_sha: git_blob_sha(&bytes),
            language: Some(Language::Python),
            size: bytes.len() as u64,
            line_count: 0,
            node: file_node,
            status: "ok".into(),
        });
        contents.insert(path, bytes);
    }
    let kinds = [EdgeKind::Calls, EdgeKind::References, EdgeKind::Imports, EdgeKind::Inherits];
    while edges.len() < edges_n {
        let src = sym_ids[rng.usize(..sym_ids.len())];
        let dst = sym_ids[rng.usize(..sym_ids.len())];
        edges.push(Edge {
            src,
            dst,
            kind: kinds[rng.usize(..kinds.len())],
            meta: EdgeMeta {
                confidence: rng.f32(),
                strategy: Strategy::Heuristic,
                evidence: Some((nodes[src as usize].file.unwrap(), Span::default())),
            },
        });
    }
    let graph = ResolvedGraph {
        snapshot: SnapshotInfo {
            repo: "github.com/acme/scale".into(),
            commit: "deadbeefdeadbeefdeadbeef".into(),
            dirty: false,
            root: None,
            indexed_at: 0,
            telar_version: "test".into(),
        },
        nodes,
        edges,
        files,
    };
    (graph, Mem(contents))
}

fn cap(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

#[test]
#[ignore]
fn scale_50k_nodes_200k_edges_2k_files() {
    let (graph, mem) = synth(2_000, 24, 200_000);
    let content_bytes: usize = mem.0.values().map(|v| v.len()).sum();
    println!(
        "graph: {} nodes, {} edges, {} files, {:.1} MiB content",
        graph.nodes.len(),
        graph.edges.len(),
        graph.files.len(),
        content_bytes as f64 / 1048576.0
    );
    let dir = tempfile::tempdir().unwrap();
    let t = Instant::now();
    let meta = write_segment(&graph, &mem, dir.path()).unwrap();
    let write = t.elapsed();
    let t = Instant::now();
    let meta2 = write_segment(&graph, &mem, dir.path()).unwrap();
    let rewrite = t.elapsed();
    assert_eq!(meta2.content_bytes_added, 0);
    let path = dir.path().join(format!("segments/{}.tlseg", meta.id));
    if let Some(keep) = std::env::var_os("TELAR_SCALE_KEEP") {
        std::fs::copy(&path, keep).unwrap();
    }
    let t = Instant::now();
    let seg = Segment::open(&path, dir.path()).unwrap();
    let open = t.elapsed();
    println!(
        "write {:?} (rewrite, all blobs known: {:?}); open {:?}; bundle {:.2} MiB; packs +{:.2} MiB",
        write,
        rewrite,
        open,
        meta.bytes as f64 / 1048576.0,
        meta.content_bytes_added as f64 / 1048576.0
    );

    let sym_queries = ["user by id", "fetchUserAccount", "parse token", "invoice payment", "build cache", "validate session"];
    let mut lat = Vec::new();
    for _ in 0..20 {
        for q in sym_queries {
            let t = Instant::now();
            let hits = seg.search_symbols(&SymbolQuery { text: q.into(), limit: 20, ..Default::default() }).unwrap();
            lat.push(t.elapsed());
            assert!(!hits.is_empty());
        }
    }
    println!("symbol search p50 {:?} ({} queries)", p50(lat), sym_queries.len() * 20);

    let glob_t = Instant::now();
    let hits = seg
        .search_symbols(&SymbolQuery { text: "user".into(), path_glob: Some("src/loan/**".into()), limit: 20, ..Default::default() })
        .unwrap();
    println!("symbol search with path_glob: {:?} ({} hits)", glob_t.elapsed(), hits.len());

    let content_queries: [(&str, bool); 4] =
        [("getUserAccount", false), ("value = fetch", false), (r"def (load|save)Cache\w*", true), (r"\* 9999\d", true)];
    let mut lat = Vec::new();
    for _ in 0..5 {
        for (q, regex) in content_queries {
            let t = Instant::now();
            let r = seg.search_content(&ContentQuery { pattern: q.into(), regex, ..Default::default() }).unwrap();
            lat.push(t.elapsed());
            let _ = r;
        }
    }
    println!("content search p50 {:?} ({} queries)", p50(lat), content_queries.len() * 5);
    for (q, regex) in content_queries {
        let t = Instant::now();
        let r = seg.search_content(&ContentQuery { pattern: q.into(), regex, ..Default::default() }).unwrap();
        println!(
            "  {q:?}: {:?}, candidates {}, scanned {}, matches {}{}",
            t.elapsed(),
            r.candidate_files,
            r.files_scanned,
            r.matches.len(),
            if r.truncated { " (truncated)" } else { "" }
        );
    }
    let t = Instant::now();
    let r = seg.search_content(&ContentQuery { pattern: r"\d+".into(), regex: true, max_matches: 1_000_000, ..Default::default() }).unwrap();
    println!("  no-literal full scan: {:?}, scanned {} files, {} matches", t.elapsed(), r.files_scanned, r.matches.len());

    let t = Instant::now();
    let mut n = 0;
    for id in (0..seg.nodes().len() as u32).step_by(7) {
        n += seg.out_edges(id, Some(EdgeKind::Calls)).len() + seg.in_edges(id, None).len();
    }
    println!("adjacency: {} edge reads over {} nodes in {:?}", n, seg.nodes().len() / 7, t.elapsed());
}
