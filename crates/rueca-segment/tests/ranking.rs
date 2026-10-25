//! Ranking of `search_symbols` on realistic sources: body tokens, qualified
//! (dotted) queries, the test prior, query term frequency. The label prior
//! is covered in `segment.rs` (`symbol_search_camel_case_and_exact_first`).

use std::collections::HashMap;

use rueca_model::{FileEntry, Label, Language, Node, ResolvedGraph, SnapshotInfo, Span, git_blob_sha};
use rueca_segment::{ContentSource, Segment, Store, SymbolQuery, write_segment};

struct Mem(HashMap<String, Vec<u8>>);

impl ContentSource for Mem {
    fn content(&self, file: &FileEntry) -> std::io::Result<Vec<u8>> {
        self.0.get(&file.path).cloned().ok_or_else(|| std::io::Error::other(file.path.clone()))
    }
}

const SESSIONS_PY: &str = r#"class Session:
    """A requests session."""

    def request(self, method, url):
        prep = self.prepare_request(method)
        return self.send(prep)

    def prepare_request(self, request):
        return request

    def send(self, request):
        return request


def session():
    return Session()
"#;

const API_PY: &str = r#"def request(method, url):
    s = session()
    return s.request(method, url)


def follow(resp):
    location = resp.headers["location"]
    while location:
        location = redirect_target(location)
    return location


REQUEST_TIMEOUT = 30
"#;

const TEST_PY: &str = r#"def test_follow():
    location = follow(response)
    assert location
"#;

/// Span of lines `first..=last` (1-based) of `src`.
fn lines(src: &str, first: u32, last: u32) -> Span {
    let starts: Vec<usize> = std::iter::once(0).chain(src.match_indices('\n').map(|(i, _)| i + 1)).collect();
    let start = starts[first as usize - 1];
    let end = starts.get(last as usize).copied().unwrap_or(src.len());
    Span {
        start_byte: start as u32,
        end_byte: end as u32,
        start_line: first - 1,
        start_col: 0,
        end_line: last - 1,
        end_col: 0,
    }
}

struct Def {
    label: Label,
    name: &'static str,
    qn: &'static str,
    file: u32,
    lines: (u32, u32),
    is_test: bool,
}

fn def(label: Label, name: &'static str, qn: &'static str, file: u32, lines: (u32, u32)) -> Def {
    Def { label, name, qn, file, lines, is_test: false }
}

fn open() -> (tempfile::TempDir, Segment) {
    let sources = [("pkg/sessions.py", SESSIONS_PY), ("pkg/api.py", API_PY), ("tests/test_api.py", TEST_PY)];
    let defs = vec![
        def(Label::Class, "Session", "pkg.sessions.Session", 0, (1, 12)), // 3
        def(Label::Method, "request", "pkg.sessions.Session.request", 0, (4, 6)), // 4
        def(Label::Method, "prepare_request", "pkg.sessions.Session.prepare_request", 0, (8, 9)), // 5
        def(Label::Method, "send", "pkg.sessions.Session.send", 0, (11, 12)), // 6
        def(Label::Function, "session", "pkg.sessions.session", 0, (15, 16)), // 7
        def(Label::Function, "request", "pkg.api.request", 1, (1, 3)),    // 8
        def(Label::Function, "follow", "pkg.api.follow", 1, (6, 10)),     // 9
        def(Label::Constant, "REQUEST_TIMEOUT", "pkg.api.REQUEST_TIMEOUT", 1, (13, 13)), // 10
        Def { is_test: true, ..def(Label::Function, "test_follow", "tests.test_api.test_follow", 2, (1, 3)) }, // 11
    ];
    build(&sources, defs)
}

/// Writes and opens a segment with one `File` node per source, then `defs`.
fn build(sources: &[(&str, &str)], defs: Vec<Def>) -> (tempfile::TempDir, Segment) {
    let py = Some(Language::Python);
    let files: Vec<FileEntry> = sources
        .iter()
        .enumerate()
        .map(|(i, (p, c))| FileEntry {
            path: (*p).into(),
            blob_sha: git_blob_sha(c.as_bytes()),
            language: py,
            size: c.len() as u64,
            line_count: c.lines().count() as u32,
            node: i as u32,
            status: "ok".into(),
        })
        .collect();
    let mut nodes: Vec<Node> = sources
        .iter()
        .enumerate()
        .map(|(i, (p, _))| Node {
            label: Label::File,
            name: p.rsplit('/').next().unwrap().into(),
            qualified_name: (*p).into(),
            file: Some(i as u32),
            span: None,
            language: py,
            signature: None,
            doc: None,
            exported: true,
            is_test: false,
        })
        .collect();
    for d in defs {
        let src = sources[d.file as usize].1;
        nodes.push(Node {
            label: d.label,
            name: d.name.into(),
            qualified_name: d.qn.into(),
            file: Some(d.file),
            span: Some(lines(src, d.lines.0, d.lines.1)),
            language: py,
            signature: None,
            doc: None,
            exported: true,
            is_test: d.is_test,
        });
    }
    let graph = ResolvedGraph {
        snapshot: SnapshotInfo {
            repo: "local/pkg".into(),
            commit: "worktree".into(),
            dirty: false,
            root: None,
            indexed_at: 0,
            rueca_version: "test".into(),
        },
        nodes,
        edges: vec![],
        files,
    };
    let mem = Mem(sources.iter().map(|(p, c)| (p.to_string(), c.as_bytes().to_vec())).collect());
    let dir = tempfile::tempdir().unwrap();
    let meta = write_segment(&graph, &mem, dir.path()).unwrap();
    let seg = Store::open(dir.path()).unwrap().open_segment(&meta.id).unwrap();
    (dir, seg)
}

/// Definitions whose text features are identical (same name, same source
/// line, qualified names of equal length) tie on BM25; the prior decides. The
/// demoted one has the lower node id, so without priors it would come first.
#[test]
fn priors_prefer_code_callables() {
    let src = "retry_policy = 3\n";
    let sources = [("a/x.py", src), ("b/x.py", src), ("c/x.py", src), ("tests/x.py", src)];
    let one = (1, 1);
    let defs = vec![
        def(Label::Variable, "retry_policy", "a.x.retry_policy", 0, one), // 4
        Def { is_test: true, ..def(Label::Function, "retry_policy", "c.x.retry_policy", 2, one) }, // 5
        def(Label::Function, "retry_policy", "t.x.retry_policy", 3, one), // 6: test path
        def(Label::Function, "retry_policy", "b.x.retry_policy", 1, one), // 7
    ];
    let (_d, seg) = build(&sources, defs);
    for q in ["retry_policy", "retry policy"] {
        let hits = search(&seg, q);
        assert_eq!(hits[0], "b.x.retry_policy", "{q}: {hits:?}");
        assert_eq!(hits.len(), 4, "{hits:?}");
    }
}

fn search(seg: &Segment, text: &str) -> Vec<String> {
    let q = SymbolQuery { text: text.into(), limit: 20, ..Default::default() };
    seg.search_symbols(&q).unwrap().iter().map(|h| seg.node(h.node).qualified_name.clone()).collect()
}

#[test]
fn dotted_query_ranks_the_qualified_member_first() {
    let (_d, seg) = open();
    for q in ["Session.request", "sessions.Session.request", "pkg.sessions.Session.request", "session request"] {
        assert_eq!(search(&seg, q)[0], "pkg.sessions.Session.request", "{q}: {:?}", search(&seg, q));
    }
    // A bare name still prefers definitions with that exact name.
    let bare = search(&seg, "request");
    assert!(bare[..2].iter().all(|q| q.ends_with(".request")), "{bare:?}");
}

#[test]
fn body_tokens_localize_behaviour() {
    let (_d, seg) = open();
    // Only `follow`'s code mentions redirects and locations.
    let hits = search(&seg, "redirect location header");
    assert_eq!(hits[0], "pkg.api.follow", "{hits:?}");
    // The test that exercises it is found too, but ranked below the code.
    let test = hits.iter().position(|q| q == "tests.test_api.test_follow").expect("test is listed");
    assert!(test > 0);
}

#[test]
fn repeated_query_terms_weigh_more() {
    let (_d, seg) = open();
    // Issue text that keeps coming back to `send` favours `Session.send`
    // over `Session.prepare_request`.
    let hits = search(&seg, "prepare send send send the request");
    let send = hits.iter().position(|q| q == "pkg.sessions.Session.send").unwrap();
    let prepare = hits.iter().position(|q| q == "pkg.sessions.Session.prepare_request").unwrap();
    assert!(send < prepare, "{hits:?}");
}

#[test]
fn ranking_is_deterministic_and_pages_exactly() {
    let (_d, seg) = open();
    let all = search(&seg, "request session location");
    assert_eq!(search(&seg, "request session location"), all);
    let page = |offset| {
        let q = SymbolQuery { text: "request session location".into(), limit: 2, offset, ..Default::default() };
        seg.search_symbols(&q).unwrap().iter().map(|h| seg.node(h.node).qualified_name.clone()).collect::<Vec<_>>()
    };
    assert_eq!(page(0), all[..2].to_vec());
    assert_eq!(page(2), all[2..4].to_vec());
}
