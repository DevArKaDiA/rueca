use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use telar_model::{
    Edge, EdgeKind, EdgeMeta, FileEntry, Label, Language, Node, ResolvedGraph, SnapshotInfo, Span, Strategy,
    git_blob_sha,
};
use telar_segment::{
    ContentQuery, ContentSource, FORMAT_VERSION, Segment, SegmentError, Store, SymbolQuery, write_segment,
};

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

const SERVICE_PY: &str = "class UserService:
    def getUserById(self, user_id: int) -> User:
        return db.query(User).get(user_id)

def get_user(uid):
    \"\"\"Fetch a user\"\"\"
    return UserService().getUserById(uid)
";

const APP_TS: &str = "export function startHTTPServer() {
  const port = 8080;
  listen(port);
}
export const getUserById = (id: number) => getUser(id);
export function getUser(id: number) { return fetch(`/users/${id}`); }
";

const README: &str = "# Ñandú\r\nCafé con leche\r\nTODO: write docs\r\n";
const LOGO: &[u8] = b"\x89PNG\0\0\x01getUserById\n";

struct Mem {
    by_path: HashMap<String, Vec<u8>>,
    calls: AtomicUsize,
}

impl ContentSource for Mem {
    fn content(&self, file: &FileEntry) -> std::io::Result<Vec<u8>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.by_path
            .get(&file.path)
            .cloned()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, file.path.clone()))
    }
}

fn span(start: u32) -> Span {
    Span { start_byte: start, end_byte: start + 5, start_line: start / 40, start_col: 0, end_line: start / 40, end_col: 5 }
}

fn node(label: Label, name: &str, qn: &str, file: Option<u32>, start: Option<u32>, lang: Option<Language>) -> Node {
    Node {
        label,
        name: name.into(),
        qualified_name: qn.into(),
        file,
        span: start.map(span),
        language: lang,
        signature: None,
        doc: None,
        exported: true,
        is_test: false,
    }
}

fn meta(confidence: f32, strategy: Strategy, evidence: Option<(u32, Span)>) -> EdgeMeta {
    EdgeMeta { confidence, strategy, evidence }
}

fn edge(src: u32, dst: u32, kind: EdgeKind, m: EdgeMeta) -> Edge {
    Edge { src, dst, kind, meta: m }
}

fn fixture() -> (ResolvedGraph, Mem) {
    let contents: Vec<(&str, Vec<u8>, Option<Language>, u32)> = vec![
        ("src/users/service.py", SERVICE_PY.as_bytes().to_vec(), Some(Language::Python), 2),
        ("src/web/app.ts", APP_TS.as_bytes().to_vec(), Some(Language::TypeScript), 3),
        ("docs/ñandú/README.md", README.as_bytes().to_vec(), None, 4),
        ("assets/logo.bin", LOGO.to_vec(), None, 5),
        ("src/users/copy.py", SERVICE_PY.as_bytes().to_vec(), Some(Language::Python), 6),
    ];
    let files: Vec<FileEntry> = contents
        .iter()
        .map(|(p, c, l, n)| FileEntry {
            path: (*p).into(),
            blob_sha: git_blob_sha(c),
            language: *l,
            size: c.len() as u64,
            line_count: c.iter().filter(|&&b| b == b'\n').count() as u32,
            node: *n,
            status: "ok".into(),
        })
        .collect();
    let py = Some(Language::Python);
    let ts = Some(Language::TypeScript);
    let mut nodes = vec![
        node(Label::Repository, "demo", "local/demo", None, None, None), // 0
        node(Label::Folder, "src", "src", None, None, None),             // 1
        node(Label::File, "service.py", "src/users/service.py", Some(0), None, py), // 2
        node(Label::File, "app.ts", "src/web/app.ts", Some(1), None, ts), // 3
        node(Label::File, "README.md", "docs/ñandú/README.md", Some(2), None, None), // 4
        node(Label::File, "logo.bin", "assets/logo.bin", Some(3), None, None), // 5
        node(Label::File, "copy.py", "src/users/copy.py", Some(4), None, py), // 6
        node(Label::Class, "UserService", "src.users.service.UserService", Some(0), Some(10), py), // 7
        node(Label::Method, "getUserById", "src.users.service.UserService.getUserById", Some(0), Some(40), py), // 8
        node(Label::Function, "get_user", "src.users.service.get_user", Some(0), Some(200), py), // 9
        node(Label::Function, "startHTTPServer", "src/web/app:startHTTPServer", Some(1), Some(0), ts), // 10
        node(Label::Function, "getUser", "src/web/app:getUser", Some(1), Some(100), ts), // 11
        node(Label::Variable, "getUserById", "src/web/app:getUserById", Some(1), Some(50), ts), // 12
        node(Label::Module, "service", "src.users.service", Some(0), Some(0), py), // 13
        node(Label::Function, "ñandúHelper", "src.users.copy.ñandúHelper", Some(4), Some(7), py), // 14
    ];
    nodes[8].signature = Some("def getUserById(self, user_id: int) -> User".into());
    nodes[9].doc = Some("Fetch a user".into());
    let syn = meta(1.0, Strategy::Syntactic, None);
    let ev = Span { start_byte: 120, end_byte: 140, start_line: 6, start_col: 11, end_line: 6, end_col: 31 };
    let edges = vec![
        edge(0, 1, EdgeKind::Contains, syn),                                       // 0
        edge(1, 2, EdgeKind::Contains, syn),                                       // 1
        edge(1, 3, EdgeKind::Contains, syn),                                       // 2
        edge(2, 7, EdgeKind::Defines, syn),                                        // 3
        edge(2, 9, EdgeKind::Defines, syn),                                        // 4
        edge(7, 8, EdgeKind::Defines, syn),                                        // 5
        edge(9, 8, EdgeKind::Calls, meta(0.9, Strategy::ImportResolved, Some((0, ev)))), // 6
        edge(11, 9, EdgeKind::Calls, meta(0.3, Strategy::Heuristic, None)),        // 7
        edge(10, 9, EdgeKind::Calls, meta(0.5, Strategy::UniqueName, None)),       // 8
        edge(12, 11, EdgeKind::Calls, meta(0.95, Strategy::SameModule, None)),     // 9
        edge(3, 2, EdgeKind::Imports, meta(0.7, Strategy::ImportResolved, None)),  // 10
        edge(9, 7, EdgeKind::References, meta(0.8, Strategy::TypeResolved, None)), // 11
        edge(9, 8, EdgeKind::Calls, meta(0.4, Strategy::Heuristic, None)),         // 12 (duplicate, weaker)
        edge(8, 7, EdgeKind::Reads, meta(0.6, Strategy::Scip, None)),              // 13
    ];
    let graph = ResolvedGraph {
        snapshot: SnapshotInfo {
            repo: "local/demo".into(),
            commit: "0123456789abcdef0123".into(),
            dirty: false,
            root: Some("/tmp/demo".into()),
            indexed_at: 1_700_000_000,
            telar_version: "0.1.0-test".into(),
        },
        nodes,
        edges,
        files,
    };
    graph.validate().unwrap();
    let mem = Mem { by_path: contents.into_iter().map(|(p, c, _, _)| (p.to_string(), c)).collect(), calls: AtomicUsize::new(0) };
    (graph, mem)
}

fn open_fixture() -> (tempfile::TempDir, ResolvedGraph, Segment) {
    let dir = tempfile::tempdir().unwrap();
    let (graph, mem) = fixture();
    let meta = write_segment(&graph, &mem, dir.path()).unwrap();
    // 5 files, 4 distinct blobs.
    assert_eq!(mem.calls.load(Ordering::SeqCst), 4);
    assert!(meta.content_bytes_added > 0);
    let store = Store::open(dir.path()).unwrap();
    let seg = store.open_segment(&meta.id).unwrap();
    assert_eq!(seg.meta().bytes, std::fs::metadata(store.segment_path(&meta.id)).unwrap().len());
    assert_eq!(meta.bytes, seg.meta().bytes);
    (dir, graph, seg)
}

fn content_query(pattern: &str) -> ContentQuery {
    ContentQuery { pattern: pattern.into(), ..Default::default() }
}

fn sym(text: &str) -> SymbolQuery {
    SymbolQuery { text: text.into(), limit: 50, ..Default::default() }
}

// ---------------------------------------------------------------------------
// Tables, adjacency, names
// ---------------------------------------------------------------------------

#[test]
fn segment_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Segment>();
}

#[test]
fn tables_round_trip() {
    let (_d, graph, seg) = open_fixture();
    assert_eq!(seg.nodes(), &graph.nodes[..]);
    assert_eq!(seg.files(), &graph.files[..]);
    assert_eq!(seg.snapshot(), &graph.snapshot);
    let m = seg.meta();
    assert_eq!(m.format_version, FORMAT_VERSION);
    assert_eq!((m.node_count, m.edge_count, m.file_count), (15, 14, 5));
    assert!(m.id.starts_with("local-demo-0123456789ab-"), "{}", m.id);
    assert_eq!(seg.node(8).name, "getUserById");
    assert_eq!(seg.file(2).path, "docs/ñandú/README.md");
    assert_eq!(seg.file_by_path("docs/ñandú/README.md"), Some(2));
    assert_eq!(seg.file_by_path("src/web/app.ts"), Some(1));
    assert_eq!(seg.file_by_path("src/web/nope.ts"), None);
    assert_eq!(seg.file_by_path(""), None);
}

#[test]
fn adjacency_order_and_kind_filter() {
    let (_d, graph, seg) = open_fixture();
    let out9 = seg.out_edges(9, None);
    let summary: Vec<(u32, EdgeKind)> = out9.iter().map(|e| (e.other, e.kind)).collect();
    // Sorted by (kind, dst, edge index): Calls(8) x2 (edge 6 then 12), References(7).
    assert_eq!(summary, vec![(8, EdgeKind::Calls), (8, EdgeKind::Calls), (7, EdgeKind::References)]);
    assert_eq!(out9[0].meta, graph.edges[6].meta);
    assert_eq!(out9[1].meta, graph.edges[12].meta);
    assert_eq!(out9[0].meta.evidence.unwrap().1.start_col, 11);

    let calls = seg.out_edges(9, Some(EdgeKind::Calls));
    assert_eq!(calls.len(), 2);
    assert!(seg.out_edges(9, Some(EdgeKind::Imports)).is_empty());

    let into9: Vec<(u32, EdgeKind, Strategy)> =
        seg.in_edges(9, None).iter().map(|e| (e.other, e.kind, e.meta.strategy)).collect();
    assert_eq!(
        into9,
        vec![
            (2, EdgeKind::Defines, Strategy::Syntactic),
            (10, EdgeKind::Calls, Strategy::UniqueName),
            (11, EdgeKind::Calls, Strategy::Heuristic),
        ]
    );
    let into9_calls: Vec<u32> = seg.in_edges(9, Some(EdgeKind::Calls)).iter().map(|e| e.other).collect();
    assert_eq!(into9_calls, vec![10, 11]);
    let into8: Vec<u32> = seg.in_edges(8, Some(EdgeKind::Calls)).iter().map(|e| e.other).collect();
    assert_eq!(into8, vec![9, 9]);
    assert_eq!(seg.in_edges(2, Some(EdgeKind::Imports))[0].other, 3);

    // Every edge is visible exactly once from each side.
    let total_out: usize = (0..graph.nodes.len() as u32).map(|n| seg.out_edges(n, None).len()).sum();
    let total_in: usize = (0..graph.nodes.len() as u32).map(|n| seg.in_edges(n, None).len()).sum();
    assert_eq!((total_out, total_in), (14, 14));
    // Out-of-range ids do not panic.
    assert!(seg.out_edges(9999, None).is_empty());
    assert!(seg.in_edges(u32::MAX, Some(EdgeKind::Calls)).is_empty());
}

#[test]
fn name_lookups() {
    let (_d, _g, seg) = open_fixture();
    assert_eq!(seg.nodes_by_name("getUserById"), vec![8, 12]);
    assert_eq!(seg.nodes_by_name("getuserbyid"), Vec::<u32>::new());
    assert_eq!(seg.nodes_by_name("ñandúHelper"), vec![14]);
    assert_eq!(seg.nodes_by_qualified_name("src.users.service.UserService.getUserById"), vec![8]);
    assert_eq!(seg.nodes_by_qualified_name("src/web/app.ts"), vec![3]);
    assert!(seg.nodes_by_qualified_name("src.users").is_empty());
    assert!(seg.nodes_by_qualified_name("").is_empty());
}

#[test]
fn nodes_in_file_sorted_by_span() {
    let (_d, _g, seg) = open_fixture();
    assert_eq!(seg.nodes_in_file(0), vec![13, 7, 8, 9]);
    assert_eq!(seg.nodes_in_file(1), vec![10, 12, 11]);
    assert!(seg.nodes_in_file(2).is_empty());
    assert_eq!(seg.nodes_in_file(4), vec![14]);
    assert!(seg.nodes_in_file(99).is_empty());
}

// ---------------------------------------------------------------------------
// Symbol search
// ---------------------------------------------------------------------------

fn ids(hits: &[telar_segment::SymbolHit]) -> Vec<u32> {
    hits.iter().map(|h| h.node).collect()
}

#[test]
fn symbol_search_camel_case_and_exact_first() {
    let (_d, _g, seg) = open_fixture();
    let hits = seg.search_symbols(&sym("user by id")).unwrap();
    let top: Vec<u32> = ids(&hits).into_iter().take(2).collect();
    assert!(top.contains(&8) && top.contains(&12), "{hits:?}");

    let hits = seg.search_symbols(&sym("getUserById")).unwrap();
    let mut top2: Vec<u32> = ids(&hits).into_iter().take(2).collect();
    top2.sort();
    assert_eq!(top2, vec![8, 12], "{hits:?}");
    let pos_get_user = ids(&hits).iter().position(|&n| n == 11).expect("getUser also matches");
    assert!(pos_get_user >= 2);
    assert!(hits[0].score > hits[pos_get_user].score);

    let hits = seg.search_symbols(&sym("get_user")).unwrap();
    assert_eq!(hits[0].node, 9, "{hits:?}");

    let hits = seg.search_symbols(&sym("http server")).unwrap();
    assert_eq!(hits[0].node, 10);

    // Scores are non-increasing; ties ordered by node id.
    for w in hits.windows(2) {
        assert!(w[0].score > w[1].score || (w[0].score == w[1].score && w[0].node < w[1].node));
    }
    // Doc and signature fields are searchable.
    assert_eq!(seg.search_symbols(&sym("fetch")).unwrap()[0].node, 9);
    assert!(ids(&seg.search_symbols(&sym("int")).unwrap()).contains(&8));
    // Unicode identifiers.
    assert_eq!(ids(&seg.search_symbols(&sym("ñandú")).unwrap()), vec![14]);
    assert!(seg.search_symbols(&sym("zzzz")).unwrap().is_empty());
}

#[test]
fn symbol_search_filters() {
    let (_d, _g, seg) = open_fixture();
    // Default excludes File / Folder / Module / Repository.
    let hits = ids(&seg.search_symbols(&sym("service")).unwrap());
    assert!(hits.contains(&7));
    assert!(!hits.contains(&2) && !hits.contains(&13), "{hits:?}");
    let files = ids(&seg.search_symbols(&SymbolQuery { labels: vec![Label::File], ..sym("service") }).unwrap());
    assert_eq!(files, vec![2]);
    let modules = ids(&seg.search_symbols(&SymbolQuery { labels: vec![Label::Module], ..sym("service") }).unwrap());
    assert_eq!(modules, vec![13]);

    let funcs = seg.search_symbols(&SymbolQuery { labels: vec![Label::Function], ..sym("user") }).unwrap();
    assert!(!funcs.is_empty());
    assert!(funcs.iter().all(|h| seg.node(h.node).label == Label::Function));
    let two = seg
        .search_symbols(&SymbolQuery { labels: vec![Label::Function, Label::Variable], ..sym("getUserById") })
        .unwrap();
    assert!(two.iter().all(|h| matches!(seg.node(h.node).label, Label::Function | Label::Variable)));
    assert_eq!(two[0].node, 12);

    let py = seg.search_symbols(&SymbolQuery { language: Some(Language::Python), ..sym("user") }).unwrap();
    assert!(!py.is_empty() && py.iter().all(|h| seg.node(h.node).language == Some(Language::Python)));

    let web = seg.search_symbols(&SymbolQuery { path_glob: Some("src/web/**".into()), ..sym("user") }).unwrap();
    assert_eq!(ids(&web).len(), 2);
    assert!(web.iter().all(|h| seg.node(h.node).file == Some(1)));
    let py_glob = seg.search_symbols(&SymbolQuery { path_glob: Some("src/*/*.py".into()), ..sym("user") }).unwrap();
    assert!(py_glob.iter().all(|h| matches!(seg.node(h.node).file, Some(0) | Some(4))));
    assert!(seg.search_symbols(&SymbolQuery { path_glob: Some("src/[".into()), ..sym("user") }).is_err());

    // Empty text lists everything that passes the filters, in node order.
    let classes = seg.search_symbols(&SymbolQuery { labels: vec![Label::Class], ..sym("") }).unwrap();
    assert_eq!(ids(&classes), vec![7]);
}

#[test]
fn symbol_search_limit_offset() {
    let (_d, _g, seg) = open_fixture();
    let all = ids(&seg.search_symbols(&sym("user")).unwrap());
    assert!(all.len() >= 5, "{all:?}");
    let page = |offset, limit| ids(&seg.search_symbols(&SymbolQuery { offset, limit, ..sym("user") }).unwrap());
    assert_eq!(page(0, 2), all[..2].to_vec());
    assert_eq!(page(2, 2), all[2..4].to_vec());
    assert_eq!(page(all.len(), 5), Vec::<u32>::new());
    // limit 0 = default (20).
    assert_eq!(page(0, 0), all.iter().copied().take(20).collect::<Vec<_>>());
    // Deterministic across calls.
    assert_eq!(ids(&seg.search_symbols(&sym("user")).unwrap()), all);
    // Glob + offset over-fetch path.
    let glob = |offset, limit| {
        ids(&seg
            .search_symbols(&SymbolQuery { offset, limit, path_glob: Some("**/*.py".into()), ..sym("user") })
            .unwrap())
    };
    let g_all = glob(0, 50);
    assert_eq!(glob(1, 1), g_all[1..2].to_vec());
}

// ---------------------------------------------------------------------------
// Content search
// ---------------------------------------------------------------------------

fn lines(seg: &Segment, r: &telar_segment::ContentSearchResult) -> Vec<(String, u32)> {
    r.matches.iter().map(|m| (seg.file(m.file).path.clone(), m.line)).collect()
}

#[test]
fn content_literal_and_case() {
    let (_d, _g, seg) = open_fixture();
    let r = seg.search_content(&content_query("getUserById")).unwrap();
    let expected: Vec<(String, u32)> = vec![
        ("src/users/copy.py".into(), 2),
        ("src/users/copy.py".into(), 7),
        ("src/users/service.py".into(), 2),
        ("src/users/service.py".into(), 7),
        ("src/web/app.ts".into(), 5),
    ];
    assert_eq!(lines(&seg, &r), expected);
    assert!(!r.truncated);
    // The binary file contains the literal but is not indexed.
    assert_eq!(r.candidate_files, 3);
    assert_eq!(r.files_scanned, 3);
    assert_eq!(r.matches[0].text, "    def getUserById(self, user_id: int) -> User:");

    let r = seg.search_content(&content_query("GETUSERBYID")).unwrap();
    assert!(r.matches.is_empty());
    let r = seg.search_content(&ContentQuery { case_insensitive: true, ..content_query("GETUSERBYID") }).unwrap();
    assert_eq!(r.matches.len(), 5);

    // Trigram pruning: only app.ts contains "8080".
    let r = seg.search_content(&content_query("8080")).unwrap();
    assert_eq!(r.candidate_files, 1);
    assert_eq!(lines(&seg, &r), vec![("src/web/app.ts".into(), 2)]);

    // Regex metacharacters are literal in literal mode.
    let r = seg.search_content(&content_query("db.query(User)")).unwrap();
    assert_eq!(r.matches.len(), 2);
    let r = seg.search_content(&content_query("nothing-like-this")).unwrap();
    assert_eq!((r.candidate_files, r.matches.len()), (0, 0));
    assert!(matches!(seg.search_content(&content_query("")), Err(SegmentError::InvalidQuery(_))));
}

#[test]
fn content_unicode_and_crlf() {
    let (_d, _g, seg) = open_fixture();
    let r = seg.search_content(&ContentQuery { case_insensitive: true, ..content_query("ñandú") }).unwrap();
    assert_eq!(lines(&seg, &r), vec![("docs/ñandú/README.md".into(), 1)]);
    assert_eq!(r.matches[0].text, "# Ñandú"); // trailing \r stripped
    let r = seg.search_content(&content_query("ñandú")).unwrap();
    assert!(r.matches.is_empty());
    let r = seg.search_content(&content_query("Café")).unwrap();
    assert_eq!(lines(&seg, &r), vec![("docs/ñandú/README.md".into(), 2)]);
}

#[test]
fn content_regex() {
    let (_d, _g, seg) = open_fixture();
    let q = ContentQuery { regex: true, ..content_query(r"listen\(|fetch\(") };
    let r = seg.search_content(&q).unwrap();
    assert_eq!(lines(&seg, &r), vec![("src/web/app.ts".into(), 3), ("src/web/app.ts".into(), 6)]);
    assert_eq!(r.candidate_files, 1);

    let q = ContentQuery { regex: true, ..content_query(r"def (get_user|getUserById)\(") };
    let r = seg.search_content(&q).unwrap();
    assert_eq!(r.matches.len(), 4);

    let q = ContentQuery { regex: true, ..content_query(r"^export") };
    assert_eq!(seg.search_content(&q).unwrap().matches.len(), 3);

    // No usable literal: every text file is a candidate.
    let q = ContentQuery { regex: true, ..content_query(r"\d{4}") };
    let r = seg.search_content(&q).unwrap();
    assert_eq!(r.candidate_files, 4);
    assert_eq!(lines(&seg, &r), vec![("src/web/app.ts".into(), 2)]);

    // ... bounded by the scan budget.
    let q = ContentQuery { regex: true, scan_budget_bytes: 100, ..content_query(r"\w+") };
    let r = seg.search_content(&q).unwrap();
    assert!(r.truncated);
    assert!(r.files_scanned < 4);

    let q = ContentQuery { regex: true, ..content_query(r"(unclosed") };
    assert!(matches!(seg.search_content(&q), Err(SegmentError::InvalidQuery(_))));
}

#[test]
fn content_context_filters_and_limits() {
    let (_d, _g, seg) = open_fixture();
    let r = seg.search_content(&ContentQuery { context_lines: 1, ..content_query("port = 8080") }).unwrap();
    let m = &r.matches[0];
    assert_eq!(m.before, vec!["export function startHTTPServer() {"]);
    assert_eq!(m.after, vec!["  listen(port);"]);
    let r = seg.search_content(&ContentQuery { context_lines: 3, ..content_query("class UserService") }).unwrap();
    assert!(r.matches[0].before.is_empty());
    assert_eq!(r.matches[0].after.len(), 3);
    let r = seg.search_content(&ContentQuery { context_lines: 5, ..content_query("fetch(") }).unwrap();
    assert_eq!(r.matches[0].after, Vec::<String>::new());
    assert_eq!(r.matches[0].before.len(), 5);

    let r = seg
        .search_content(&ContentQuery { path_glob: Some("src/**/*.py".into()), ..content_query("getUserById") })
        .unwrap();
    assert_eq!(r.matches.len(), 4);
    assert!(r.matches.iter().all(|m| seg.file(m.file).path.ends_with(".py")));
    let r = seg
        .search_content(&ContentQuery { language: Some(Language::TypeScript), ..content_query("getUser") })
        .unwrap();
    assert_eq!(lines(&seg, &r), vec![("src/web/app.ts".into(), 5), ("src/web/app.ts".into(), 6)]);

    let r = seg.search_content(&ContentQuery { max_matches: 2, ..content_query("getUserById") }).unwrap();
    assert_eq!(r.matches.len(), 2);
    assert!(r.truncated);
    let r = seg.search_content(&ContentQuery { max_matches: 5, ..content_query("getUserById") }).unwrap();
    assert_eq!(r.matches.len(), 5);
    assert!(!r.truncated);
}

// ---------------------------------------------------------------------------
// Content packs
// ---------------------------------------------------------------------------

#[test]
fn content_round_trip() {
    let (_d, graph, seg) = open_fixture();
    let (_, mem) = fixture();
    for (i, f) in graph.files.iter().enumerate() {
        assert_eq!(seg.content(i as u32).unwrap(), mem.by_path[&f.path], "{}", f.path);
    }
    assert!(matches!(seg.content(42), Err(SegmentError::NotFound(_))));
}

#[test]
fn blob_dedup_across_segments_and_index_rebuild() {
    let dir = tempfile::tempdir().unwrap();
    let (graph, mem) = fixture();
    let first = write_segment(&graph, &mem, dir.path()).unwrap();
    assert!(first.content_bytes_added > 0);
    let (graph2, mem2) = fixture();
    let second = write_segment(&graph2, &mem2, dir.path()).unwrap();
    assert_eq!(second.content_bytes_added, 0);
    assert_eq!(mem2.calls.load(Ordering::SeqCst), 0);
    assert_ne!(first.id, second.id);
    let packs = std::fs::read_dir(dir.path().join("packs")).unwrap().count();
    assert_eq!(packs, 2, "one pack + index");

    // Losing the store-level blob index is recoverable from pack footers.
    std::fs::remove_file(dir.path().join("packs/index.tlpi")).unwrap();
    let seg = Segment::open(&dir.path().join(format!("segments/{}.tlseg", second.id)), dir.path()).unwrap();
    assert_eq!(seg.content(1).unwrap(), APP_TS.as_bytes());
    let (mut graph3, _) = fixture();
    // Change one file: only that blob is fetched.
    let new = b"export const x = 1;\n".to_vec();
    graph3.files[1].blob_sha = git_blob_sha(&new);
    graph3.files[1].size = new.len() as u64;
    let mut by_path = HashMap::new();
    by_path.insert("src/web/app.ts".to_string(), new.clone());
    let mem3 = Mem { by_path, calls: AtomicUsize::new(0) };
    let third = write_segment(&graph3, &mem3, dir.path()).unwrap();
    assert_eq!(mem3.calls.load(Ordering::SeqCst), 1);
    assert!(third.content_bytes_added > 0);
    assert!(dir.path().join("packs/index.tlpi").exists());
    let seg3 = Segment::open(&dir.path().join(format!("segments/{}.tlseg", third.id)), dir.path()).unwrap();
    assert_eq!(seg3.content(1).unwrap(), new);
    assert_eq!(seg3.content(0).unwrap(), SERVICE_PY.as_bytes());
    let r = seg3.search_content(&content_query("const x")).unwrap();
    assert_eq!(r.matches.len(), 1);
    assert!(seg3.search_content(&content_query("8080")).unwrap().matches.is_empty());
    // The old segment still serves its own content.
    assert_eq!(seg.content(1).unwrap(), APP_TS.as_bytes());
}

#[test]
fn blob_sha_mismatch_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let (graph, mut mem) = fixture();
    mem.by_path.insert("src/web/app.ts".into(), b"tampered".to_vec());
    let err = write_segment(&graph, &mem, dir.path()).unwrap_err();
    assert!(err.to_string().contains("blob sha mismatch"), "{err}");
    // Nothing was published.
    assert_eq!(std::fs::read_dir(dir.path().join("segments")).unwrap().count(), 0);
    assert_eq!(std::fs::read_dir(dir.path().join("tmp")).unwrap().count(), 0);
    let (graph, mut mem) = fixture();
    mem.by_path.remove("src/web/app.ts");
    assert!(matches!(write_segment(&graph, &mem, dir.path()), Err(SegmentError::Io(_))));
}

#[test]
fn empty_graph() {
    let dir = tempfile::tempdir().unwrap();
    let (mut graph, mem) = fixture();
    graph.nodes.clear();
    graph.edges.clear();
    graph.files.clear();
    let meta = write_segment(&graph, &mem, dir.path()).unwrap();
    assert_eq!(meta.content_bytes_added, 0);
    let seg = Store::open(dir.path()).unwrap().open_segment(&meta.id).unwrap();
    assert!(seg.nodes().is_empty());
    assert!(seg.search_symbols(&sym("x")).unwrap().is_empty());
    assert!(seg.search_content(&content_query("abc")).unwrap().matches.is_empty());
    assert!(seg.out_edges(0, None).is_empty());
}

// ---------------------------------------------------------------------------
// Catalog
// ---------------------------------------------------------------------------

#[test]
fn catalog_register_replace_and_lookup() {
    let dir = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let outer = work.path().join("mono");
    let inner = outer.join("services/api");
    std::fs::create_dir_all(inner.join("src")).unwrap();
    let store = Store::open(dir.path()).unwrap();
    for sub in ["segments", "packs", "tmp"] {
        assert!(dir.path().join(sub).is_dir());
    }
    assert!(store.entries().unwrap().is_empty());
    assert!(store.entry_for_path(&inner).unwrap().is_none());

    let (graph, mem) = fixture();
    let a = write_segment(&graph, &mem, dir.path()).unwrap();
    let b = write_segment(&graph, &mem, dir.path()).unwrap();
    let c = write_segment(&graph, &mem, dir.path()).unwrap();
    store.register(outer.to_str().unwrap(), &a).unwrap();
    store.register(inner.to_str().unwrap(), &b).unwrap();
    assert_eq!(store.entries().unwrap().len(), 2);

    let canon_inner = std::fs::canonicalize(&inner).unwrap();
    let e = store.entry_for_path(&inner.join("src")).unwrap().unwrap();
    assert_eq!(e.segment_id, b.id);
    assert_eq!(Path::new(&e.root), canon_inner);
    assert_eq!(e.repo, "local/demo");
    assert_eq!(store.entry_for_path(&inner).unwrap().unwrap().segment_id, b.id);
    // Non-existent paths resolve through their nearest existing ancestor.
    assert_eq!(store.entry_for_path(&inner.join("src/missing.py")).unwrap().unwrap().segment_id, b.id);
    assert_eq!(store.entry_for_path(&outer.join("services")).unwrap().unwrap().segment_id, a.id);
    // Sibling prefix is not an ancestor.
    std::fs::create_dir_all(work.path().join("mono2")).unwrap();
    assert!(store.entry_for_path(&work.path().join("mono2")).unwrap().is_none());

    // Replace outer: a's file is deleted.
    store.register(outer.to_str().unwrap(), &c).unwrap();
    assert!(!store.segment_path(&a.id).exists());
    assert!(store.segment_path(&c.id).exists());
    assert_eq!(store.entries().unwrap().len(), 2);
    assert_eq!(store.entry_for_path(&outer).unwrap().unwrap().segment_id, c.id);
    // Re-registering the same segment keeps its file.
    store.register(outer.to_str().unwrap(), &c).unwrap();
    assert!(store.segment_path(&c.id).exists());
    // A segment shared by two roots survives replacement of one of them.
    store.register(inner.to_str().unwrap(), &c).unwrap();
    assert!(!store.segment_path(&b.id).exists());
    let d = write_segment(&graph, &mem, dir.path()).unwrap();
    store.register(inner.to_str().unwrap(), &d).unwrap();
    assert!(store.segment_path(&c.id).exists());
    assert!(store.open_segment(&d.id).is_ok());
}

#[test]
fn default_dir_respects_telar_home() {
    // Only read the env; the test runner may run tests in parallel.
    let d = Store::default_dir();
    match std::env::var_os("TELAR_HOME").filter(|v| !v.is_empty()) {
        Some(h) => assert_eq!(d, Path::new(&h)),
        None => assert!(d.ends_with(".cache/telar")),
    }
}

// ---------------------------------------------------------------------------
// Corruption
// ---------------------------------------------------------------------------

#[test]
fn corruption_is_detected_without_panics() {
    let dir = tempfile::tempdir().unwrap();
    let (graph, mem) = fixture();
    let meta = write_segment(&graph, &mem, dir.path()).unwrap();
    let path = dir.path().join(format!("segments/{}.tlseg", meta.id));
    let good = std::fs::read(&path).unwrap();
    let bad = dir.path().join("bad.tlseg");
    let open = |bytes: &[u8]| {
        std::fs::write(&bad, bytes).unwrap();
        Segment::open(&bad, dir.path())
    };
    assert!(open(&good).is_ok());

    // Truncations.
    for cut in [0, 7, 16, 100, good.len() / 2, good.len() - 1] {
        assert!(matches!(open(&good[..cut]), Err(SegmentError::Corrupt(_))), "cut {cut}");
    }
    // Bit flips at many positions (header flips may also report Version).
    let step = (good.len() / 400).max(1);
    let mut checked = 0;
    for pos in (0..good.len()).step_by(step).chain([8, 12, good.len() - 1, good.len() - 50]) {
        let mut b = good.clone();
        b[pos] ^= 0x10;
        match open(&b) {
            Ok(_) => panic!("flip at {pos} not detected"),
            Err(SegmentError::Corrupt(_)) | Err(SegmentError::Version(_)) => {}
            Err(e) => panic!("unexpected error at {pos}: {e}"),
        }
        checked += 1;
    }
    assert!(checked > 300);

    // Version.
    let mut b = good.clone();
    b[8..12].copy_from_slice(&99u32.to_le_bytes());
    assert!(matches!(open(&b), Err(SegmentError::Version(99))));
    // Garbage.
    assert!(matches!(open(b"hello world, definitely not a segment file at all........................"), Err(SegmentError::Corrupt(_))));
    // Missing file.
    assert!(matches!(Segment::open(&dir.path().join("nope.tlseg"), dir.path()), Err(SegmentError::Io(_))));
}

#[test]
fn corrupt_pack_is_reported_not_panicking() {
    let dir = tempfile::tempdir().unwrap();
    let (graph, mem) = fixture();
    let meta = write_segment(&graph, &mem, dir.path()).unwrap();
    let pack = std::fs::read_dir(dir.path().join("packs"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|e| e == "tlpack"))
        .unwrap();
    let mut bytes = std::fs::read(&pack).unwrap();
    for b in &mut bytes[20..200] {
        *b ^= 0xff;
    }
    std::fs::write(&pack, &bytes).unwrap();
    let seg = Store::open(dir.path()).unwrap().open_segment(&meta.id).unwrap();
    for f in 0..5 {
        let _ = seg.content(f); // Err or Ok, never a panic
    }
    assert!((0..5).any(|f| seg.content(f).is_err()));
    let _ = seg.search_content(&content_query("getUserById"));
}

// ---------------------------------------------------------------------------
// Ranking, robustness, concurrency
// ---------------------------------------------------------------------------

/// One node per field carrying the query term: the field boosts order them
/// name ≫ qualified_name > signature > path > doc.
#[test]
fn symbol_field_boost_order() {
    let dir = tempfile::tempdir().unwrap();
    let files_src: Vec<(&str, &[u8])> = vec![("lib/io.py", b"a = 1\n"), ("lib/parse/io.py", b"b = 2\n")];
    let files: Vec<FileEntry> = files_src
        .iter()
        .enumerate()
        .map(|(i, (p, c))| FileEntry {
            path: (*p).into(),
            blob_sha: git_blob_sha(c),
            language: Some(Language::Python),
            size: c.len() as u64,
            line_count: 1,
            node: i as u32,
            status: "ok".into(),
        })
        .collect();
    let py = Some(Language::Python);
    let mut nodes = vec![
        node(Label::File, "io.py", "lib/io.py", Some(0), None, py),            // 0
        node(Label::File, "io.py", "lib/parse/io.py", Some(1), None, py),      // 1
        node(Label::Function, "w", "lib.io.w", Some(0), Some(0), py),          // 2: doc
        node(Label::Function, "v", "lib.io.v", Some(1), Some(10), py),         // 3: path
        node(Label::Function, "u", "lib.io.u", Some(0), Some(20), py),         // 4: signature
        node(Label::Function, "t", "lib.parse.t", Some(0), Some(30), py),      // 5: qualified name
        node(Label::Function, "parse", "lib.io.parse", Some(0), Some(40), py), // 6: name (+ qualified name)
    ];
    // Realistic density: every function has a signature and a docstring, so
    // BM25 length normalization compares like with like.
    for n in &mut nodes[2..] {
        n.signature = Some(format!("def {}(x)", n.name));
        n.doc = Some("Run the step on the input".into());
    }
    nodes[2].doc = Some("Run the parse step on the input".into());
    nodes[4].signature = Some("def u(parse)".into());
    let graph = ResolvedGraph {
        snapshot: fixture().0.snapshot,
        nodes,
        edges: vec![],
        files,
    };
    let mem = Mem {
        by_path: files_src.iter().map(|(p, c)| (p.to_string(), c.to_vec())).collect(),
        calls: AtomicUsize::new(0),
    };
    let meta = write_segment(&graph, &mem, dir.path()).unwrap();
    let seg = Store::open(dir.path()).unwrap().open_segment(&meta.id).unwrap();
    let hits = seg.search_symbols(&sym("parse")).unwrap();
    assert_eq!(ids(&hits), vec![6, 5, 4, 3, 2], "{hits:?}");
}

/// A blob whose pack frame is damaged is fetched and stored again instead of
/// failing the write; the blob index then points at the healthy copy.
#[test]
fn damaged_pack_blobs_are_refetched() {
    let dir = tempfile::tempdir().unwrap();
    let (graph, mem) = fixture();
    let first = write_segment(&graph, &mem, dir.path()).unwrap();
    let pack = std::fs::read_dir(dir.path().join("packs"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|e| e == "tlpack"))
        .unwrap();
    let mut bytes = std::fs::read(&pack).unwrap();
    for b in &mut bytes[20..60] {
        *b ^= 0xff; // inside the first frame; footer stays valid
    }
    std::fs::write(&pack, &bytes).unwrap();

    let (graph2, mem2) = fixture();
    let second = write_segment(&graph2, &mem2, dir.path()).unwrap();
    let refetched = mem2.calls.load(Ordering::SeqCst);
    assert!((1..4).contains(&refetched), "{refetched}");
    assert!(second.content_bytes_added > 0);
    let store = Store::open(dir.path()).unwrap();
    for id in [&first.id, &second.id] {
        let seg = store.open_segment(id).unwrap();
        for (i, f) in graph.files.iter().enumerate() {
            assert_eq!(seg.content(i as u32).unwrap(), mem.by_path[&f.path], "{}", f.path);
        }
    }
}

#[test]
fn concurrent_readers_share_one_segment() {
    let (_d, _g, seg) = open_fixture();
    let expected_sym = ids(&seg.search_symbols(&sym("user by id")).unwrap());
    let expected_content = seg.search_content(&content_query("getUserById")).unwrap();
    std::thread::scope(|s| {
        for _ in 0..8 {
            s.spawn(|| {
                for _ in 0..20 {
                    assert_eq!(ids(&seg.search_symbols(&sym("user by id")).unwrap()), expected_sym);
                    assert_eq!(seg.search_content(&content_query("getUserById")).unwrap(), expected_content);
                    assert_eq!(seg.content(1).unwrap(), APP_TS.as_bytes());
                    assert_eq!(seg.out_edges(9, Some(EdgeKind::Calls)).len(), 2);
                }
            });
        }
    });
}

#[test]
fn store_housekeeping() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    assert!(matches!(store.open_segment("does-not-exist"), Err(SegmentError::Io(_))));
    // Leftovers from a crashed writer: old ones are removed, fresh ones kept.
    let old = dir.path().join("tmp/crashed.tlseg.tmp");
    let fresh = dir.path().join("tmp/in-progress.tlpack.tmp");
    std::fs::write(&old, b"x").unwrap();
    std::fs::write(&fresh, b"y").unwrap();
    let two_days_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(48 * 3600);
    std::fs::File::options().write(true).open(&old).unwrap().set_modified(two_days_ago).unwrap();
    let (graph, mem) = fixture();
    write_segment(&graph, &mem, dir.path()).unwrap();
    assert!(!old.exists());
    assert!(fresh.exists());
    // A corrupt catalog is reported, not silently reset.
    std::fs::write(dir.path().join("catalog.json"), b"{not json").unwrap();
    assert!(matches!(store.entries(), Err(SegmentError::Corrupt(_))));
}
