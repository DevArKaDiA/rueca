//! The RFC 0005 response contract: pinned ids, the snapshot envelope, `ref`,
//! several repositories in one ranking (RFC 0006 §6), grouping and short fields, `trace`
//! edges by index, the token budget with cursors, and the text rendering.

mod common;

use rueca_model::{Label, NodeId};
use rueca_query::Engine;
use serde_json::{Value, json};

use common::*;

const BILLING: &str = "github.com/acme/billing";
const WEB: &str = "github.com/acme/web";
const C1: &str = "7696364aef89a22e4a2fe84f6fd23e186bef9aa8";
const C2: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f9012345678";

/// Two repositories: billing at `main` with uncommitted changes (the
/// default root), web at a detached clean `HEAD`. Both hold the demo graph.
struct Two {
    _dir: tempfile::TempDir,
    engine: Engine,
    billing_root: String,
}

fn two() -> Two {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store");
    let mut roots = Vec::new();
    for (name, repo, commit, dirty, branch) in
        [("billing", BILLING, C1, true, Some("main")), ("web", WEB, C2, false, None)]
    {
        let root = dir.path().join(name);
        std::fs::create_dir_all(&root).unwrap();
        let root = std::fs::canonicalize(&root).unwrap().to_string_lossy().into_owned();
        let (mut b, _) = demo(repo, &root);
        b.graph.snapshot.commit = commit.into();
        b.graph.snapshot.dirty = dirty;
        b.graph.snapshot.branch = branch.map(str::to_string);
        b.write(&store, &root);
        roots.push(root);
    }
    let engine = Engine::new(store, Some(roots[0].clone().into())).unwrap();
    Two { _dir: dir, engine, billing_root: roots[0].clone() }
}

fn call(e: &Engine, tool: &str, args: Value) -> Value {
    e.call(tool, args.clone()).unwrap_or_else(|err| panic!("{tool} {args}: {err:#}"))
}

fn text(e: &Engine, tool: &str, args: Value) -> String {
    e.respond(tool, args.clone()).unwrap_or_else(|err| panic!("{tool} {args}: {err:#}")).text
}

fn err(e: &Engine, tool: &str, args: Value) -> String {
    format!("{:#}", e.call(tool, args).expect_err("expected an error"))
}

fn billing_id(local: &str) -> String {
    format!("{BILLING}@7696364aef89+:{local}")
}

#[test]
fn envelope_pinned_ids_and_grouping() {
    let t = two();
    let r = call(&t.engine, "search_symbols", json!({"query": "issue"}));
    assert_eq!(
        r["snapshots"],
        json!([{
            "s": 0, "repo": BILLING, "ref": "main", "commit": C1, "overlay": true,
            "url": format!("https://github.com/acme/billing/blob/{C1}/{{path}}#L{{start}}-L{{end}}")
        }])
    );
    assert_eq!(r["truncated"], false);
    assert_eq!(r["next_cursor"], Value::Null);
    assert_eq!(r["budget"]["limit_tokens"], 2000);
    let g = &r["results"][0];
    assert_eq!((g["s"].as_u64(), g["path"].as_str()), (Some(0), Some("pkg/service.py")), "{r}");
    assert_eq!(
        g["symbols"][0],
        json!({
            "id": billing_id("pkg/service.py#InvoiceService.issue"),
            "name": "issue",
            "qn": "pkg.service.InvoiceService.issue",
            "label": "Method",
            "lines": [9, 11],
            "sig": "def issue(self, invoice)"
        })
    );
    // Every item of a group is in that group's file.
    for g in r["results"].as_array().unwrap() {
        for s in g["symbols"].as_array().unwrap() {
            assert!(s["id"].as_str().unwrap().contains(&format!(":{}#", g["path"].as_str().unwrap())), "{g}");
        }
    }
    let resp = t.engine.respond("search_symbols", json!({"query": "issue"})).unwrap();
    assert_eq!(resp.json["budget"]["used_tokens"].as_u64().unwrap() as usize, resp.text.len().div_ceil(4));
    assert!(resp.text.starts_with("github.com/acme/billing@7696364aef89+ (main)\npkg/service.py\n"), "{}", resp.text);
    assert!(resp.text.contains("\n  Method InvoiceService.issue L9-11  def issue(self, invoice)\n"), "{}", resp.text);

    // Scores on request, 3 significant digits.
    let r = call(&t.engine, "search_symbols", json!({"query": "issue", "scores": true}));
    let score = r["results"][0]["symbols"][0]["score"].as_f64().unwrap();
    assert!(score > 0.0 && format!("{score}").trim_start_matches("0.").trim_matches('.').len() <= 4, "{score}");
}

#[test]
fn ids_accept_full_commits_prefixes_and_ref_names() {
    let t = two();
    let want = billing_id("pkg/service.py#InvoiceService.issue");
    for commit in [C1, "7696364", "7696364aef89", "7696364aef89+", "main", "stale0000000"] {
        let id = format!("{BILLING}@{commit}:pkg/service.py#InvoiceService.issue");
        let r = call(&t.engine, "get_symbol", json!({"id": id}));
        assert_eq!(r["results"]["symbol"]["id"], want, "{commit}");
    }
    // An id of the other repository selects it without `repo`.
    let r = call(&t.engine, "get_symbol", json!({"id": format!("{WEB}@a1b2c3d:pkg/service.py#helper")}));
    assert_eq!(r["results"]["symbol"]["id"], format!("{WEB}@a1b2c3d4e5f6:pkg/service.py#helper"));
    assert_eq!(r["snapshots"][0]["ref"], "HEAD");
    assert_eq!(r["snapshots"][0]["overlay"], false);
    // Verbose results carry the same pinned ids.
    let v = call(&t.engine, "get_symbol", json!({"qualified_name": "InvoiceService.issue", "format": "verbose"}));
    assert_eq!(v["symbol"]["id"], want);
    assert_eq!(v["symbol"]["qualified_name"], "pkg.service.InvoiceService.issue");
}

#[test]
fn ref_argument() {
    let t = two();
    for r in ["main", "worktree", "HEAD", "7696364", C1] {
        let v = call(&t.engine, "get_file_outline", json!({"path": "pkg/service.py", "ref": r}));
        assert_eq!(v["snapshots"][0]["ref"], "main", "{r}");
    }
    let e = err(&t.engine, "search_symbols", json!({"query": "x", "ref": "pr/12"}));
    assert!(e.contains("ref `pr/12` is not indexed for repository `github.com/acme/billing`"), "{e}");
    assert!(e.contains(&format!("`main` (7696364aef89+, working tree of {})", t.billing_root)), "{e}");
    // With "*", snapshots that do not have the ref are skipped.
    let v = call(&t.engine, "search_symbols", json!({"query": "helper", "repo": "*", "ref": "main"}));
    assert_eq!(v["snapshots"].as_array().unwrap().len(), 1);
    assert_eq!(v["snapshots"][0]["repo"], BILLING);
    let e = err(&t.engine, "search_symbols", json!({"query": "x", "repo": "*", "ref": "release"}));
    assert!(e.contains("indexed refs"), "{e}");
}

#[test]
fn several_repositories_in_one_ranking() {
    let t = two();
    for repo in [json!("*"), json!([BILLING, WEB]), json!([WEB, "*"])] {
        let r = call(&t.engine, "search_symbols", json!({"query": "helper", "repo": repo, "scores": true}));
        let snaps: Vec<&str> = r["snapshots"].as_array().unwrap().iter().map(|s| s["repo"].as_str().unwrap()).collect();
        assert_eq!(snaps.len(), 2, "{r}");
        let groups = r["results"].as_array().unwrap();
        // One ranking with statistics over both snapshots: the same
        // definition in two identical repositories has the same BM25 score
        // (not a fused rank), so the two first groups come from different
        // snapshots, in snapshot order.
        let top: Vec<(u64, f64)> = groups
            .iter()
            .take(2)
            .map(|g| (g["s"].as_u64().unwrap(), g["symbols"][0]["score"].as_f64().unwrap()))
            .collect();
        assert!(top[0].1 > 1.0 && top[0].1 == top[1].1, "{r}");
        assert_eq!((top[0].0, top[1].0), (0, 1), "{r}");
        // Ids are pinned to their own snapshot.
        for g in groups {
            let repo = snaps[g["s"].as_u64().unwrap() as usize];
            for s in g["symbols"].as_array().unwrap() {
                assert!(s["id"].as_str().unwrap().starts_with(&format!("{repo}@")), "{g}");
            }
        }
    }
    let txt = text(&t.engine, "search_symbols", json!({"query": "helper", "repo": "*"}));
    assert!(
        txt.starts_with(
            "[0] github.com/acme/billing@7696364aef89+ (main)\n[1] github.com/acme/web@a1b2c3d4e5f6 (HEAD)\n"
        ),
        "{txt}"
    );
    assert!(txt.contains("\n[1] pkg/service.py\n"), "{txt}");

    // Name lookups: the best match wins, the other snapshot's is a candidate.
    let r = call(&t.engine, "get_symbol", json!({"qualified_name": "InvoiceService.issue", "repo": "*"}));
    assert_eq!(r["snapshots"].as_array().unwrap().len(), 1);
    assert_eq!(r["results"]["symbol"]["id"], billing_id("pkg/service.py#InvoiceService.issue"));
    assert_eq!(r["results"]["candidates"][0]["id"], format!("{WEB}@a1b2c3d4e5f6:pkg/service.py#InvoiceService.issue"));
    // A path present in both is ambiguous.
    let e = err(&t.engine, "get_file_outline", json!({"path": "pkg/service.py", "repo": "*"}));
    assert!(e.contains("indexed in several repositories"), "{e}");
    // A list naming one repository reads only it.
    let r = call(&t.engine, "get_file_outline", json!({"path": "pkg/service.py", "repo": [WEB]}));
    assert_eq!(r["snapshots"][0]["repo"], WEB);
    let e = err(&t.engine, "search_symbols", json!({"query": "x", "repo": []}));
    assert!(e.contains("must not be an empty list"), "{e}");
}

#[test]
fn trace_nodes_and_edges_by_index() {
    let t = two();
    let r = call(&t.engine, "trace", json!({"qualified_name": "InvoiceService.issue"}));
    let res = &r["results"];
    let nodes = res["nodes"].as_array().unwrap();
    assert_eq!(
        nodes[0],
        json!({"id": billing_id("pkg/service.py#InvoiceService.issue"), "qn": "pkg.service.InvoiceService.issue",
               "label": "Method", "path": "pkg/service.py", "lines": [9, 11], "d": 0})
    );
    let qns: Vec<&str> = nodes.iter().map(|n| n["qn"].as_str().unwrap()).collect();
    assert_eq!(qns, ["pkg.service.InvoiceService.issue", "pkg.service.Base.run", "pkg.service.helper"]);
    assert_eq!(nodes[1]["dir"], "out");
    assert_eq!(nodes[1]["d"], 1);
    assert_eq!(res["tests_excluded"], 1);
    assert_eq!(res["edges"], json!([[0, 1, "CALLS", 0.6, "heuristic", 11], [0, 2, "CALLS", 0.95, "same_module", 10]]));
    let txt = text(&t.engine, "trace", json!({"qualified_name": "InvoiceService.issue"}));
    assert!(
        txt.contains("\n2 out d1 Function helper  pkg/service.py L13-14  ←0 CALLS 0.95 same_module @10\n"),
        "{txt}"
    );
    assert!(txt.contains("(1 tests excluded"), "{txt}");

    // A node reachable both ways is listed once.
    let r = call(&t.engine, "trace", json!({"qualified_name": "InvoiceService.issue", "include_tests": true}));
    let nodes = r["results"]["nodes"].as_array().unwrap();
    let test = nodes.iter().position(|n| n["qn"] == "pkg.service.test_issue").unwrap();
    assert_eq!(nodes[test]["dir"], "in");
    assert!(r["results"]["edges"].as_array().unwrap().contains(&json!([test, 0, "CALLS", 0.5, "unique_name", 17])));
}

#[test]
fn get_symbol_snippet_outline_and_code_shapes() {
    let t = two();
    let r = call(&t.engine, "get_symbol", json!({"qualified_name": "InvoiceService"}));
    let res = &r["results"];
    assert_eq!(
        res["symbol"],
        json!({"id": billing_id("pkg/service.py#InvoiceService"), "name": "InvoiceService",
               "qn": "pkg.service.InvoiceService", "label": "Class", "path": "pkg/service.py", "lines": [7, 11],
               "lang": "python", "sig": "class InvoiceService(Base):", "doc": "Issues invoices.",
               "exported": true, "test": false})
    );
    assert_eq!(res["parent"]["id"], format!("{BILLING}@7696364aef89+:pkg/service.py"));
    assert_eq!(res["children"][0]["qn"], "pkg.service.InvoiceService.issue");
    assert_eq!(
        res["bases"][0],
        json!({"id": billing_id("pkg/service.py#Base"), "qn": "pkg.service.Base", "label": "Class", "lines": [3, 5]})
    );
    assert_eq!((res["callers"].as_u64(), res["callees"].as_u64()), (Some(1), Some(0)));
    let txt = text(&t.engine, "get_symbol", json!({"qualified_name": "InvoiceService"}));
    assert!(txt.contains("Class InvoiceService  pkg/service.py L7-11  python exported\n"), "{txt}");
    assert!(txt.contains("  bases:\n    Class Base L3-5\n"), "{txt}");

    let r = call(&t.engine, "get_snippet", json!({"qualified_name": "helper", "path": "pkg"}));
    assert_eq!(r["results"]["lines"], json!([13, 14]));
    assert_eq!(r["results"]["text"], "def helper():\n    return os.getcwd()");
    assert_eq!(r["results"]["symbol"]["qn"], "pkg.service.helper");

    let r = call(&t.engine, "get_file_outline", json!({"path": "pkg/service.py"}));
    let syms = r["results"]["symbols"].as_array().unwrap();
    assert_eq!(syms[0]["qn"], "pkg.service.Base");
    assert_eq!(syms[0]["children"][0]["name"], "run");
    assert!(syms.iter().find(|s| s["name"] == "helper").unwrap().get("children").is_none());
    let txt = text(&t.engine, "get_file_outline", json!({"path": "pkg/service.py"}));
    assert!(txt.contains("pkg/service.py (python, 17 lines)\n  Class Base L3-5  class Base:\n    Method Base.run L4-5  def run(self)\n"), "{txt}");

    let r = call(&t.engine, "search_code", json!({"pattern": "helper", "context_lines": 1}));
    assert_eq!(r["files_scanned"], 2);
    let g = &r["results"][0];
    assert_eq!(g["path"], "pkg/service.py");
    assert_eq!(g["matches"][0]["line"], 10);
    assert_eq!(g["matches"][0]["symbol"]["qn"], "pkg.service.InvoiceService.issue");
    assert_eq!(g["matches"][0]["before"], json!(["    def issue(self, invoice):"]));
    let txt = text(&t.engine, "search_code", json!({"pattern": "helper"}));
    assert!(txt.contains("pkg/service.py\n  [Method InvoiceService.issue L9-11]\n  10:         helper()\n"), "{txt}");
}

/// A snapshot with one large file, to exercise the budget.
fn big() -> (tempfile::TempDir, Engine) {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store");
    let root = dir.path().join("big");
    std::fs::create_dir_all(&root).unwrap();
    let root = std::fs::canonicalize(&root).unwrap().to_string_lossy().into_owned();
    let src: String = (0..120).map(|i| format!("def handler_{i:03}(request, response):\n    return {i}\n")).collect();
    let mut b = Builder::new("local/big", "worktree", &root);
    let (f, fnode) = b.file("app/handlers.py", &src);
    let mut prev: Option<NodeId> = None;
    let first = fnode + 1;
    for i in 0..120u32 {
        let name = format!("handler_{i:03}");
        let n = b.sym(
            fnode,
            f,
            Label::Function,
            &name,
            &format!("app.handlers.{name}"),
            span(&src, 2 * i, 2 * i + 1),
            Some(&format!("def {name}(request, response)")),
            true,
            false,
        );
        // Every handler calls the previous one and the first one.
        let call_site = Some((f, line_span(&src, 2 * i + 1)));
        if let Some(p) = prev {
            b.edge(n, p, rueca_model::EdgeKind::Calls, 0.9, rueca_model::Strategy::SameModule, call_site);
            if p != first {
                b.edge(n, first, rueca_model::EdgeKind::Calls, 0.8, rueca_model::Strategy::SameModule, call_site);
            }
        }
        prev = Some(n);
    }
    b.write(&store, &root);
    (dir, Engine::new(store, Some(root.into())).unwrap())
}

fn ids_of(r: &Value, list: &str) -> Vec<String> {
    let mut out = Vec::new();
    for g in r["results"].as_array().unwrap() {
        for s in g[list].as_array().unwrap() {
            out.push(s["id"].as_str().map_or_else(|| s["line"].to_string(), str::to_string));
        }
    }
    out
}

#[test]
fn budget_cuts_and_cursors_continue() {
    let (_d, e) = big();
    // search_symbols: the budget cuts before the limit; the cursor continues.
    let q = json!({"query": "handler request response", "limit": 60, "budget_tokens": 200});
    let r1 = e.respond("search_symbols", q.clone()).unwrap();
    assert_eq!(r1.json["truncated"], true);
    assert!(r1.json["budget"]["used_tokens"].as_u64().unwrap() <= 200);
    assert!(r1.text.ends_with(&format!(
        "(truncated; continue with \"cursor\": \"{}\")\n",
        r1.json["next_cursor"].as_str().unwrap()
    )));
    let first = ids_of(&r1.json, "symbols");
    assert!(!first.is_empty() && first.len() < 60, "{}", first.len());
    assert_eq!(r1.json["next_cursor"], first.len().to_string());
    let mut q2 = q.clone();
    q2["cursor"] = r1.json["next_cursor"].clone();
    let second = ids_of(&call(&e, "search_symbols", q2), "symbols");
    let all = ids_of(
        &call(
            &e,
            "search_symbols",
            json!({"query": "handler request response", "limit": 120, "budget_tokens": 100000}),
        ),
        "symbols",
    );
    assert_eq!(all.len(), 120);
    assert_eq!([first.clone(), second.clone()].concat(), all[..first.len() + second.len()].to_vec());

    // The limit alone also yields a cursor.
    let r = call(&e, "search_symbols", json!({"query": "handler"}));
    assert_eq!(ids_of(&r, "symbols").len(), 10);
    assert_eq!((r["truncated"].clone(), r["next_cursor"].clone()), (json!(true), json!("10")));

    // get_file_outline pages through the symbols.
    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..100 {
        let mut a = json!({"path": "app/handlers.py", "budget_tokens": 300});
        if let Some(c) = &cursor {
            a["cursor"] = json!(c);
        }
        let r = call(&e, "get_file_outline", a);
        seen.extend(
            r["results"]["symbols"].as_array().unwrap().iter().map(|s| s["name"].as_str().unwrap().to_string()),
        );
        match r["next_cursor"].as_str() {
            Some(c) => cursor = Some(c.to_string()),
            None => break,
        }
    }
    let want: Vec<String> = (0..120).map(|i| format!("handler_{i:03}")).collect();
    assert_eq!(seen, want);

    // get_snippet continues at the next line.
    let r = call(&e, "get_snippet", json!({"path": "app/handlers.py", "budget_tokens": 100}));
    let lines = r["results"]["lines"].as_array().unwrap().clone();
    let next = r["next_cursor"].as_str().unwrap().to_string();
    assert_eq!(next, (lines[1].as_u64().unwrap() + 1).to_string());
    let r = call(&e, "get_snippet", json!({"path": "app/handlers.py", "budget_tokens": 100, "cursor": next}));
    assert_eq!(r["results"]["lines"][0].as_u64().unwrap(), lines[1].as_u64().unwrap() + 1);

    // trace pages through nodes; edges refer to nodes of the same response.
    let r =
        call(&e, "trace", json!({"name": "handler_000", "direction": "callers", "depth": 10, "budget_tokens": 150}));
    assert_eq!(r["truncated"], true);
    let c = r["next_cursor"].as_str().unwrap().to_string();
    let r2 = call(
        &e,
        "trace",
        json!({"name": "handler_000", "direction": "callers", "depth": 10, "budget_tokens": 150, "cursor": c}),
    );
    let nodes = r2["results"]["nodes"].as_array().unwrap();
    assert_eq!(nodes[0]["qn"], "app.handlers.handler_000", "the root stays node 0");
    for edge in r2["results"]["edges"].as_array().unwrap() {
        assert!(
            (edge[0].as_u64().unwrap() as usize) < nodes.len() && (edge[1].as_u64().unwrap() as usize) < nodes.len()
        );
    }
    assert!(!r2["results"]["edges"].as_array().unwrap().is_empty());

    // Invalid cursors are errors.
    let e2 = err(&e, "search_symbols", json!({"query": "x", "cursor": "abc"}));
    assert!(e2.contains("invalid `cursor`"), "{e2}");
}

#[test]
fn defaults_differ_between_compact_and_verbose() {
    let (_d, e) = big();
    let r = call(&e, "search_code", json!({"pattern": "return", "budget_tokens": 100000}));
    assert_eq!(ids_of(&r, "matches").len(), 20);
    let r = call(&e, "search_code", json!({"pattern": "return", "format": "verbose"}));
    assert_eq!(r["matches"].as_array().unwrap().len(), 50);
    assert!(r.get("snapshots").is_none());
    let r = call(&e, "search_symbols", json!({"query": "handler", "format": "verbose"}));
    assert_eq!(r["results"].as_array().unwrap().len(), 20);
    assert_eq!(r["next_offset"], 20);
    let r = call(&e, "trace", json!({"name": "handler_005", "direction": "callees", "budget_tokens": 100000}));
    assert_eq!(r["results"]["nodes"].as_array().unwrap().len(), 3, "root + depth 1 by default");
    let r = call(&e, "trace", json!({"name": "handler_005", "direction": "callees", "format": "verbose"}));
    assert_eq!(r["nodes"].as_array().unwrap().len(), 3, "verbose keeps depth 2");
    let r = call(&e, "trace", json!({"name": "handler_000", "direction": "callers", "budget_tokens": 100000}));
    assert_eq!(r["results"]["nodes"].as_array().unwrap().len(), 51, "root + 50 nodes by default");
    assert_eq!((r["truncated"].clone(), r["next_cursor"].clone()), (json!(true), json!("50")));
    // Outside git there is no overlay marker and no URL.
    let r = call(&e, "get_symbol", json!({"name": "handler_001"}));
    assert_eq!(r["results"]["symbol"]["id"], "local/big@worktree:app/handlers.py#handler_001");
    assert_eq!(
        r["snapshots"][0],
        json!({"s": 0, "repo": "local/big", "ref": "worktree", "commit": "worktree", "overlay": false})
    );
}
