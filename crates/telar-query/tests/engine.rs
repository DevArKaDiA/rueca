//! Every tool through `Engine::call` against a hand-built segment, asserting
//! the JSON shapes of `docs/tools.md`.

mod common;

use serde_json::{Value, json};
use telar_query::Engine;

use common::*;

const ISSUE_ID: &str = "local/demo@abc123:pkg/service.py#InvoiceService.issue";

fn engine(f: &Fixture) -> Engine {
    Engine::new(f.store.clone(), Some(f.root.clone())).unwrap()
}

fn call(e: &Engine, tool: &str, args: Value) -> Value {
    e.call(tool, args.clone()).unwrap_or_else(|err| panic!("{tool} {args}: {err:#}"))
}

fn err(e: &Engine, tool: &str, args: Value) -> String {
    format!("{:#}", e.call(tool, args).expect_err("expected an error"))
}

fn brief(id: &str, name: &str, qn: &str, label: &str, path: &str, s: u32, e: u32) -> Value {
    json!({"id": id, "name": name, "qualified_name": qn, "label": label, "path": path, "start_line": s, "end_line": e})
}

fn issue_brief() -> Value {
    brief(ISSUE_ID, "issue", "pkg.service.InvoiceService.issue", "Method", "pkg/service.py", 9, 11)
}
fn helper_brief() -> Value {
    brief("local/demo@abc123:pkg/service.py#helper", "helper", "pkg.service.helper", "Function", "pkg/service.py", 13, 14)
}
fn run_brief() -> Value {
    brief("local/demo@abc123:pkg/service.py#Base.run", "run", "pkg.service.Base.run", "Method", "pkg/service.py", 4, 5)
}
fn base_brief() -> Value {
    brief("local/demo@abc123:pkg/service.py#Base", "Base", "pkg.service.Base", "Class", "pkg/service.py", 3, 5)
}
fn service_brief() -> Value {
    brief(
        "local/demo@abc123:pkg/service.py#InvoiceService",
        "InvoiceService",
        "pkg.service.InvoiceService",
        "Class",
        "pkg/service.py",
        7,
        11,
    )
}
fn test_issue_brief() -> Value {
    brief(
        "local/demo@abc123:pkg/service.py#test_issue",
        "test_issue",
        "pkg.service.test_issue",
        "Function",
        "pkg/service.py",
        16,
        17,
    )
}

fn with(mut v: Value, extra: Value) -> Value {
    for (k, x) in extra.as_object().unwrap() {
        v[k] = x.clone();
    }
    v
}

#[test]
fn get_symbol_full_shape() {
    let f = fixture();
    let e = engine(&f);
    let r = call(&e, "get_symbol", json!({"qualified_name": "InvoiceService.issue"}));
    let expected = json!({
        "symbol": with(issue_brief(), json!({
            "language": "python",
            "signature": "def issue(self, invoice)",
            "doc": null,
            "exported": true,
            "is_test": false
        })),
        "parent": service_brief(),
        "children": [],
        "bases": [],
        "subtypes": [],
        "callers_count": 1,
        "callees_count": 2,
        "candidates": []
    });
    assert_eq!(r, expected);

    // Same symbol by id, by exact global name, and by name + path.
    assert_eq!(call(&e, "get_symbol", json!({"id": ISSUE_ID})), expected);
    assert_eq!(call(&e, "get_symbol", json!({"qualified_name": "pkg.service.InvoiceService.issue"})), expected);
    assert_eq!(call(&e, "get_symbol", json!({"name": "issue", "path": "pkg/service.py"})), expected);
}

#[test]
fn get_symbol_relations_and_candidates() {
    let f = fixture();
    let e = engine(&f);
    let r = call(&e, "get_symbol", json!({"name": "InvoiceService"}));
    assert_eq!(r["bases"], json!([base_brief()]));
    assert_eq!(r["children"], json!([issue_brief()]));
    assert_eq!(r["parent"]["label"], "File");
    assert_eq!(r["parent"]["id"], "local/demo@abc123:pkg/service.py");
    assert_eq!(r["callers_count"], 1);
    assert_eq!(r["symbol"]["doc"], "Issues invoices.");

    let r = call(&e, "get_symbol", json!({"qualified_name": "Base"}));
    assert_eq!(r["subtypes"], json!([service_brief()]));
    assert_eq!(r["children"], json!([run_brief()]));

    // Ambiguous `helper`: the exported Python one wins, the TS one is a candidate.
    let r = call(&e, "get_symbol", json!({"name": "helper"}));
    assert_eq!(r["symbol"]["qualified_name"], "pkg.service.helper");
    assert_eq!(
        r["candidates"],
        json!([brief(
            "local/demo@abc123:src/util/index.ts#helper",
            "helper",
            "src/util/index:helper",
            "Function",
            "src/util/index.ts",
            7,
            7
        )])
    );
    // `path` narrows.
    let r = call(&e, "get_symbol", json!({"name": "helper", "path": "src/util"}));
    assert_eq!(r["symbol"]["qualified_name"], "src/util/index:helper");
    assert_eq!(r["candidates"], json!([]));

    // ECMAScript ids use the local name after the module colon.
    let r = call(&e, "get_symbol", json!({"qualified_name": "Session.request"}));
    assert_eq!(r["symbol"]["id"], "local/demo@abc123:src/util/index.ts#Session.request");
    assert_eq!(r["symbol"]["language"], "typescript");
    assert_eq!(r["callees_count"], 1);
    // Suffixes may drop leading module directories (`/` boundary) but not
    // cut through a name.
    let by_module = call(&e, "get_symbol", json!({"qualified_name": "index:Session.request"}));
    assert_eq!(by_module["symbol"], r["symbol"]);
    assert!(err(&e, "get_symbol", json!({"qualified_name": "dex:Session.request"})).contains("no symbol found"));
    let id = r["symbol"]["id"].as_str().unwrap().to_string();
    assert_eq!(call(&e, "get_symbol", json!({"id": id}))["symbol"]["qualified_name"], "src/util/index:Session.request");
}

#[test]
fn get_symbol_errors() {
    let f = fixture();
    let e = engine(&f);
    assert!(err(&e, "get_symbol", json!({})).contains("one of `id`, `qualified_name` or `name` is required"));
    assert!(err(&e, "get_symbol", json!({"name": "nope"})).contains("no symbol found"));
    assert!(err(&e, "get_symbol", json!({"id": "garbage"})).contains("malformed symbol id"));
    assert!(err(&e, "get_symbol", json!({"name": "x", "colour": 1})).contains("unknown argument `colour`"));
    assert!(err(&e, "get_symbol", json!({"name": 3})).contains("invalid argument `name`"));
}

#[test]
fn search_symbols_shape_and_paging() {
    let f = fixture();
    let e = engine(&f);
    let r = call(&e, "search_symbols", json!({"query": "InvoiceService"}));
    let results = r["results"].as_array().unwrap();
    assert!(!results.is_empty(), "{r}");
    assert_eq!(r["truncated"], false);
    assert_eq!(r["next_offset"], Value::Null);
    let first = &results[0];
    assert_eq!(first["qualified_name"], "pkg.service.InvoiceService");
    let mut keys: Vec<&str> = first.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["end_line", "id", "label", "name", "path", "qualified_name", "score", "signature", "start_line"]
    );
    assert!(first["score"].as_f64().unwrap() > 0.0);

    let all = call(&e, "search_symbols", json!({"query": "issue"}));
    let n = all["results"].as_array().unwrap().len();
    assert!(n >= 2, "{all}");
    let page = call(&e, "search_symbols", json!({"query": "issue", "limit": 1}));
    assert_eq!(page["results"].as_array().unwrap().len(), 1);
    assert_eq!(page["truncated"], true);
    assert_eq!(page["next_offset"], 1);
    let page2 = call(&e, "search_symbols", json!({"query": "issue", "limit": 1, "offset": 1}));
    assert_eq!(page2["results"][0], all["results"][1]);

    // Label and language filters.
    let r = call(&e, "search_symbols", json!({"query": "helper", "labels": ["Function"], "language": "typescript"}));
    let results = r["results"].as_array().unwrap();
    assert!(results.iter().all(|h| h["path"] == "src/util/index.ts"), "{r}");
    assert!(err(&e, "search_symbols", json!({"query": "x", "labels": ["Gadget"]})).contains("unknown label `Gadget`"));
    assert!(err(&e, "search_symbols", json!({"query": "x", "language": "cobol"})).contains("invalid argument `language`"));
    assert!(err(&e, "search_symbols", json!({})).contains("missing required argument `query`"));
}

#[test]
fn search_code_matches_with_enclosing_symbol() {
    let f = fixture();
    let e = engine(&f);
    let r = call(&e, "search_code", json!({"pattern": "getcwd"}));
    assert_eq!(
        r,
        json!({
            "matches": [{"path": "pkg/service.py", "line": 14, "text": "    return os.getcwd()", "symbol": helper_brief()}],
            "truncated": false,
            "files_scanned": r["files_scanned"].clone()
        })
    );
    assert!(r["files_scanned"].as_u64().unwrap() >= 1);

    let r = call(&e, "search_code", json!({"pattern": "getcwd", "context_lines": 1}));
    let m = &r["matches"][0];
    assert_eq!(m["before"], json!(["def helper():"]));
    assert_eq!(m["after"], json!([""]));

    // Module-level match: no enclosing symbol.
    let r = call(&e, "search_code", json!({"pattern": "import os"}));
    assert!(r["matches"][0].get("symbol").is_none(), "{r}");

    // Regex, case-insensitivity, truncation.
    let r = call(&e, "search_code", json!({"pattern": "RETURN", "case_insensitive": true, "limit": 2}));
    assert_eq!(r["matches"].as_array().unwrap().len(), 2);
    assert_eq!(r["truncated"], true);
    let r = call(&e, "search_code", json!({"pattern": "format[A-Z]\\w+", "regex": true, "path_glob": "src/**"}));
    let lines: Vec<u64> = r["matches"].as_array().unwrap().iter().map(|m| m["line"].as_u64().unwrap()).collect();
    assert_eq!(lines, [1, 5]);
    assert_eq!(r["matches"][1]["symbol"]["qualified_name"], "src/util/index:Session.request");
    assert!(!r["matches"][0]["text"].as_str().unwrap().ends_with('\r'));
}

#[test]
fn get_snippet_symbol_and_range() {
    let f = fixture();
    let e = engine(&f);
    let r = call(&e, "get_snippet", json!({"qualified_name": "pkg.service.helper"}));
    assert_eq!(
        r,
        json!({
            "path": "pkg/service.py",
            "language": "python",
            "start_line": 13,
            "end_line": 14,
            "text": "def helper():\n    return os.getcwd()",
            "truncated": false,
            "symbol": helper_brief()
        })
    );
    let r = call(&e, "get_snippet", json!({"name": "helper", "path": "pkg/service.py", "context_lines": 1}));
    assert_eq!((r["start_line"].as_u64(), r["end_line"].as_u64()), (Some(12), Some(15)));
    let r = call(&e, "get_snippet", json!({"id": ISSUE_ID, "max_lines": 1}));
    assert_eq!(r["text"], "    def issue(self, invoice):");
    assert_eq!(r["truncated"], true);
    assert_eq!((r["start_line"].as_u64(), r["end_line"].as_u64()), (Some(9), Some(9)));

    // Line range on a CRLF file.
    let r = call(&e, "get_snippet", json!({"path": "src/util/index.ts", "start_line": 2, "end_line": 3}));
    assert_eq!(
        r,
        json!({
            "path": "src/util/index.ts",
            "language": "typescript",
            "start_line": 2,
            "end_line": 3,
            "text": "  return n.toFixed(2);\n}",
            "truncated": false
        })
    );
    // Open-ended range is bounded by max_lines.
    let r = call(&e, "get_snippet", json!({"path": "src/util/index.ts", "start_line": 6, "max_lines": 1}));
    assert_eq!(r["text"], "}");
    assert_eq!(r["truncated"], true);
    // Unique suffix paths resolve.
    let r = call(&e, "get_snippet", json!({"path": "index.ts", "start_line": 7}));
    assert_eq!(r["text"], "function helper() {}");
    assert_eq!(r["truncated"], false);

    assert!(err(&e, "get_snippet", json!({"path": "src/util/index.ts", "start_line": 99})).contains("past the end"));
    assert!(err(&e, "get_snippet", json!({"path": "nope.py", "start_line": 1})).contains("file not indexed"));
    assert!(err(&e, "get_snippet", json!({"path": "src/util/index.ts", "start_line": 3, "end_line": 2})).contains("before"));
    assert!(err(&e, "get_snippet", json!({})).contains("give a symbol"));
}

#[test]
fn trace_both_directions() {
    let f = fixture();
    let e = engine(&f);
    let r = call(&e, "trace", json!({"qualified_name": "InvoiceService.issue"}));
    let node = |b: Value, d: u32, dir: &str| with(b, json!({"depth": d, "direction": dir}));
    let edge = |src: &Value, dst: &Value, conf: f64, strategy: &str, line: u32| {
        json!({
            "src": src["id"], "dst": dst["id"], "kind": "CALLS", "confidence": conf, "strategy": strategy,
            "call_site": {"path": "pkg/service.py", "line": line}
        })
    };
    assert_eq!(
        r,
        json!({
            "root": issue_brief(),
            "nodes": [
                node(test_issue_brief(), 1, "callers"),
                node(run_brief(), 1, "callees"),
                node(helper_brief(), 1, "callees"),
            ],
            "edges": [
                edge(&issue_brief(), &run_brief(), 0.6, "heuristic", 11),
                edge(&issue_brief(), &helper_brief(), 0.95, "same_module", 10),
                edge(&test_issue_brief(), &issue_brief(), 0.5, "unique_name", 17),
            ],
            "truncated": false
        })
    );

    // Confidence filter drops the weak edges.
    let r = call(&e, "trace", json!({"id": ISSUE_ID, "min_confidence": 0.7}));
    assert_eq!(r["nodes"], json!([node(helper_brief(), 1, "callees")]));
    assert_eq!(r["edges"].as_array().unwrap().len(), 1);

    // Depth 2 callees from the test.
    let r = call(&e, "trace", json!({"name": "test_issue", "direction": "callees", "depth": 2}));
    let got: Vec<(u64, String)> = r["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| (n["depth"].as_u64().unwrap(), n["qualified_name"].as_str().unwrap().to_string()))
        .collect();
    assert_eq!(
        got,
        [
            (1, "pkg.service.InvoiceService".to_string()),
            (1, "pkg.service.InvoiceService.issue".to_string()),
            (2, "pkg.service.Base.run".to_string()),
            (2, "pkg.service.helper".to_string()),
        ]
    );
    let r1 = call(&e, "trace", json!({"name": "test_issue", "direction": "callees", "depth": 1}));
    assert_eq!(r1["nodes"].as_array().unwrap().len(), 2);

    // Limit truncates.
    let r = call(&e, "trace", json!({"name": "test_issue", "direction": "callees", "limit": 1}));
    assert_eq!(r["nodes"].as_array().unwrap().len(), 1);
    assert_eq!(r["truncated"], true);

    // Other edge kinds.
    let r = call(&e, "trace", json!({"name": "InvoiceService", "direction": "callees", "edge_types": ["INHERITS"]}));
    assert_eq!(r["nodes"], json!([node(base_brief(), 1, "callees")]));
    assert_eq!(r["edges"][0]["kind"], "INHERITS");

    assert!(err(&e, "trace", json!({"name": "issue", "edge_types": ["CALLZ"]})).contains("unknown edge type `CALLZ`"));
    assert!(err(&e, "trace", json!({"name": "issue", "direction": "up"})).contains("invalid argument `direction`"));
}

#[test]
fn file_outline_nests_by_containment() {
    let f = fixture();
    let e = engine(&f);
    let r = call(&e, "get_file_outline", json!({"path": "pkg/service.py"}));
    let sym = |b: Value, sig: &str, children: Value| with(b, json!({"signature": sig, "children": children}));
    assert_eq!(
        r,
        json!({
            "path": "pkg/service.py",
            "language": "python",
            "line_count": 17,
            "symbols": [
                sym(base_brief(), "class Base:", json!([sym(run_brief(), "def run(self)", json!([]))])),
                sym(service_brief(), "class InvoiceService(Base):",
                    json!([sym(issue_brief(), "def issue(self, invoice)", json!([]))])),
                sym(helper_brief(), "def helper()", json!([])),
                sym(test_issue_brief(), "def test_issue()", json!([])),
            ]
        })
    );
    let r = call(&e, "get_file_outline", json!({"path": "./src/util/index.ts"}));
    let names: Vec<&str> = r["symbols"].as_array().unwrap().iter().map(|s| s["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["formatAmount", "Session", "helper"]);
    assert_eq!(r["symbols"][1]["children"][0]["name"], "request");
    assert!(err(&e, "get_file_outline", json!({})).contains("missing required argument `path`"));
}

#[test]
fn repository_selection() {
    let f = fixture();
    // A second repository in the same store.
    let other_root = f.store.parent().unwrap().join("other");
    std::fs::create_dir_all(&other_root).unwrap();
    let other_root = std::fs::canonicalize(other_root).unwrap();
    let (mut b, _) = demo("github.com/acme/other", other_root.to_str().unwrap());
    b.graph.snapshot.commit = "def456".into();
    b.write(&f.store, other_root.to_str().unwrap());

    // No default root and two repositories: an error listing both.
    let e = Engine::new(f.store.clone(), None).unwrap();
    let msg = err(&e, "get_symbol", json!({"name": "helper"}));
    assert!(msg.contains("several repositories") && msg.contains("github.com/acme/other"), "{msg}");

    // By repo id, by root path, and implied by a symbol id.
    let r = call(&e, "get_symbol", json!({"name": "helper", "repo": "github.com/acme/other"}));
    assert_eq!(r["symbol"]["id"], "github.com/acme/other@def456:pkg/service.py#helper");
    let r = call(&e, "get_symbol", json!({"name": "helper", "repo": f.root.to_str().unwrap()}));
    assert_eq!(r["symbol"]["id"], "local/demo@abc123:pkg/service.py#helper");
    let r = call(&e, "get_symbol", json!({"id": "github.com/acme/other@def456:pkg/service.py#helper"}));
    assert_eq!(r["symbol"]["qualified_name"], "pkg.service.helper");
    let msg = err(&e, "get_symbol", json!({"name": "helper", "repo": "github.com/nope/nope"}));
    assert!(msg.contains("not indexed"), "{msg}");

    // A default root picks its repository; a subdirectory of it works too.
    let sub = f.root.join("pkg");
    std::fs::create_dir_all(&sub).unwrap();
    let e = Engine::new(f.store.clone(), Some(sub)).unwrap();
    assert_eq!(call(&e, "get_symbol", json!({"name": "helper"}))["symbol"]["id"], "local/demo@abc123:pkg/service.py#helper");
}

#[test]
fn reindex_is_picked_up() {
    let f = fixture();
    let e = engine(&f);
    assert_eq!(call(&e, "get_symbol", json!({"name": "helper"}))["symbol"]["id"], "local/demo@abc123:pkg/service.py#helper");
    let (mut b, _) = demo("local/demo", f.root.to_str().unwrap());
    b.graph.snapshot.commit = "fff999".into();
    b.write(&f.store, f.root.to_str().unwrap());
    assert_eq!(call(&e, "get_symbol", json!({"name": "helper"}))["symbol"]["id"], "local/demo@fff999:pkg/service.py#helper");
}

#[test]
fn empty_store_is_a_clear_error() {
    let dir = tempfile::tempdir().unwrap();
    let e = Engine::new(dir.path().join("store"), None).unwrap();
    let msg = err(&e, "search_symbols", json!({"query": "x"}));
    assert!(msg.contains("no repositories are indexed"), "{msg}");
}
