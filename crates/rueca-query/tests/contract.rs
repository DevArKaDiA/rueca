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
            "local": "InvoiceService.issue",
            "qn": "pkg.service.InvoiceService.issue",
            "label": "Method",
            "lines": [9, 11],
            "sig": "def issue(self, invoice)",
            "repo": BILLING,
            "ref": "main"
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
    // The candidate's snapshot is not listed: the object names its own.
    let cand = &r["results"]["candidates"][0];
    assert_eq!((cand["repo"].as_str(), cand["local"].as_str()), (Some(WEB), Some("InvoiceService.issue")), "{r}");
    assert!(cand["ref"].is_string(), "{r}");
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
        json!({"id": billing_id("pkg/service.py#InvoiceService.issue"), "local": "InvoiceService.issue",
               "qn": "pkg.service.InvoiceService.issue", "label": "Method", "path": "pkg/service.py", "lines": [9, 11],
               "repo": BILLING, "ref": "main", "d": 0, "dir": "root"})
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
    // The root's `dir` is not printed: `0 Method …`.
    assert!(txt.contains("\n0 Method InvoiceService.issue  pkg/service.py L9-11\n"), "{txt}");
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
               "local": "InvoiceService", "qn": "pkg.service.InvoiceService", "label": "Class",
               "path": "pkg/service.py", "lines": [7, 11], "lang": "python", "sig": "class InvoiceService(Base):",
               "doc": "Issues invoices.", "exported": true, "test": false, "repo": BILLING, "ref": "main"})
    );
    assert_eq!(res["parent"]["id"], format!("{BILLING}@7696364aef89+:pkg/service.py"));
    // A file has no `#` part: its `local` is its path.
    assert_eq!(res["parent"]["local"], "pkg/service.py");
    assert_eq!(res["children"][0]["qn"], "pkg.service.InvoiceService.issue");
    assert_eq!(
        res["bases"][0],
        json!({"id": billing_id("pkg/service.py#Base"), "local": "Base", "qn": "pkg.service.Base", "label": "Class",
               "lines": [3, 5], "repo": BILLING, "ref": "main"})
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

/// `pkg/service.py` on branch `feature/x`: `issue` also calls a new `audit`,
/// `test_issue` is gone; `src/util/index.ts` is unchanged.
const PY_FEATURE: &str = "import os

class Base:
    def run(self):
        return 1

class InvoiceService(Base):
    \"\"\"Issues invoices.\"\"\"
    def issue(self, invoice):
        helper()
        audit(invoice)
        return self.run()

def helper():
    return os.getcwd()

def audit(invoice):
    return invoice
";
const C3: &str = "c0ffee00c0ffee00c0ffee00c0ffee00c0ffee00";

/// The billing snapshot of `feature/x`, exported under the store and
/// registered as a ref snapshot of billing.
fn feature_ref(store: &std::path::Path) -> String {
    let base = std::fs::canonicalize(store).unwrap();
    let root = base.join("refs").join("github-com-acme-billing").join("feature-x");
    std::fs::create_dir_all(&root).unwrap();
    let root = root.to_string_lossy().into_owned();
    let mut b = Builder::new(BILLING, C3, &root);
    b.graph.snapshot.branch = Some("feature/x".into());
    let (pf, pn) = b.file("pkg/service.py", PY_FEATURE);
    let (tf, tn) = b.file("src/util/index.ts", TS);
    let py = |b: &mut Builder, parent, label, name: &str, qn: &str, l0, l1, sig: &str| {
        b.sym(parent, pf, label, name, qn, span(PY_FEATURE, l0, l1), Some(sig), true, false)
    };
    let base_c = py(&mut b, pn, Label::Class, "Base", "pkg.service.Base", 2, 4, "class Base:");
    let run = py(&mut b, base_c, Label::Method, "run", "pkg.service.Base.run", 3, 4, "def run(self)");
    let svc = py(
        &mut b,
        pn,
        Label::Class,
        "InvoiceService",
        "pkg.service.InvoiceService",
        6,
        11,
        "class InvoiceService(Base):",
    );
    let issue =
        py(&mut b, svc, Label::Method, "issue", "pkg.service.InvoiceService.issue", 8, 11, "def issue(self, invoice)");
    let helper = py(&mut b, pn, Label::Function, "helper", "pkg.service.helper", 13, 14, "def helper()");
    let audit = py(&mut b, pn, Label::Function, "audit", "pkg.service.audit", 16, 17, "def audit(invoice)");
    let ts = |b: &mut Builder, parent, label, name: &str, qn: &str, l0, l1, sig: &str, exported| {
        b.sym(parent, tf, label, name, qn, span(TS, l0, l1), Some(sig), exported, false)
    };
    let fmt = ts(
        &mut b,
        tn,
        Label::Function,
        "formatAmount",
        "src/util/index:formatAmount",
        0,
        2,
        "export function formatAmount(n: number): string",
        true,
    );
    let session = ts(&mut b, tn, Label::Class, "Session", "src/util/index:Session", 3, 5, "export class Session", true);
    let request =
        ts(&mut b, session, Label::Method, "request", "src/util/index:Session.request", 4, 4, "request()", true);
    ts(&mut b, tn, Label::Function, "helper", "src/util/index:helper", 6, 6, "function helper()", false);
    use rueca_model::{EdgeKind as K, Strategy as S};
    b.edge(svc, base_c, K::Inherits, 0.95, S::SameModule, Some((pf, line_span(PY_FEATURE, 6))));
    b.edge(issue, helper, K::Calls, 0.95, S::SameModule, Some((pf, line_span(PY_FEATURE, 9))));
    b.edge(issue, audit, K::Calls, 0.95, S::SameModule, Some((pf, line_span(PY_FEATURE, 10))));
    b.edge(issue, run, K::Calls, 0.6, S::Heuristic, Some((pf, line_span(PY_FEATURE, 11))));
    b.edge(request, fmt, K::Calls, 0.95, S::SameModule, Some((tf, line_span(TS, 4))));
    b.graph.validate().expect("valid graph");
    let st = rueca_segment::Store::open(store).unwrap();
    let meta = rueca_segment::write_segment(&b.graph, &Sources(b.sources.clone()), store).unwrap();
    st.register_ref(&root, &meta, "feature/x").unwrap();
    root
}

/// [`two`] plus billing's `feature/x` ref snapshot.
fn two_refs() -> Two {
    let t = two();
    feature_ref(&t.engine.store_dir);
    t
}

const BOTH: [&str; 2] = ["main", "feature/x"];

/// `(qn, in, changed)` of every item of a compact `search_symbols` answer.
fn marks(r: &Value) -> Vec<(String, Value, bool)> {
    let mut out = Vec::new();
    for g in r["results"].as_array().unwrap() {
        for s in g["symbols"].as_array().unwrap() {
            out.push((s["qn"].as_str().unwrap().to_string(), s["in"].clone(), s["changed"] == true));
        }
    }
    out
}

#[test]
fn ref_lists_collapse_identical_symbols_and_flag_changes() {
    let t = two_refs();
    // One ref: the ref snapshot alone, no marks.
    let r = call(&t.engine, "search_symbols", json!({"query": "audit", "ref": "feature/x"}));
    assert_eq!(
        r["snapshots"],
        json!([{"s": 0, "repo": BILLING, "ref": "feature/x", "commit": C3, "overlay": false,
        "url": format!("https://github.com/acme/billing/blob/{C3}/{{path}}#L{{start}}-L{{end}}")}])
    );
    assert_eq!(marks(&r)[0], ("pkg.service.audit".to_string(), Value::Null, false));

    // Identical in both refs (same blob): one item in [0, 1], not changed.
    let r = call(&t.engine, "search_symbols", json!({"query": "formatAmount", "ref": BOTH}));
    let refs: Vec<&str> = r["snapshots"].as_array().unwrap().iter().map(|s| s["ref"].as_str().unwrap()).collect();
    assert_eq!(refs, BOTH);
    assert_eq!(marks(&r)[0], ("src/util/index:formatAmount".to_string(), json!([0, 1]), false), "{r}");
    assert_eq!(marks(&r).iter().filter(|m| m.0 == "src/util/index:formatAmount").count(), 1);
    // A changed file: one item per version, each `changed`.
    let r = call(&t.engine, "search_symbols", json!({"query": "issue", "ref": BOTH, "labels": ["Method"]}));
    let issue: Vec<_> = marks(&r).into_iter().filter(|m| m.0 == "pkg.service.InvoiceService.issue").collect();
    assert_eq!(
        issue,
        [
            ("pkg.service.InvoiceService.issue".to_string(), json!([0]), true),
            ("pkg.service.InvoiceService.issue".to_string(), json!([1]), true)
        ],
        "{r}"
    );
    // Deleted on the branch, added on the branch.
    let r = call(&t.engine, "search_symbols", json!({"query": "test_issue", "ref": BOTH}));
    assert_eq!(marks(&r)[0], ("pkg.service.test_issue".to_string(), json!([0]), true));
    let r = call(&t.engine, "search_symbols", json!({"query": "audit", "ref": BOTH}));
    assert_eq!(marks(&r)[0], ("pkg.service.audit".to_string(), json!([1]), true));
    assert_eq!(r["snapshots"].as_array().unwrap().len(), 2, "the snapshots of every item's `in`");

    // Collapse happens before the limit: an identical symbol counts once.
    let all = marks(&call(&t.engine, "search_symbols", json!({"query": "helper", "ref": BOTH, "limit": 50})));
    // 3 symbols of the changed file in both versions, the TS helper once.
    assert_eq!(all.len(), 7, "{all:?}");
    assert_eq!(all.iter().filter(|m| m.2).count(), 6, "{all:?}");
    let page1 = call(&t.engine, "search_symbols", json!({"query": "helper", "ref": BOTH, "limit": 4}));
    assert_eq!((marks(&page1).len(), page1["next_cursor"].as_str()), (4, Some("4")));
    let page2 = call(&t.engine, "search_symbols", json!({"query": "helper", "ref": BOTH, "limit": 4, "cursor": "4"}));
    // The same items (pages group by file on their own).
    let sorted = |mut v: Vec<(String, Value, bool)>| {
        v.sort_by(|a, b| (&a.0, a.1.to_string()).cmp(&(&b.0, b.1.to_string())));
        v
    };
    assert_eq!(sorted([marks(&page1), marks(&page2)].concat()), sorted(all));
    assert_eq!(page2["next_cursor"], Value::Null);

    // Text marks: `[0,1]` for an item in several snapshots, `changed`.
    let txt = text(&t.engine, "search_symbols", json!({"query": "helper", "ref": BOTH}));
    assert!(
        txt.starts_with(
            "[0] github.com/acme/billing@7696364aef89+ (main)\n[1] github.com/acme/billing@c0ffee00c0ff (feature/x)\n"
        ),
        "{txt}"
    );
    assert!(txt.contains("  Function helper L7  function helper() [0,1]\n"), "{txt}");
    assert!(txt.contains("  Function helper L13-14  def helper() changed\n"), "{txt}");
    // Verbose keeps the alpha shapes: no collapse, no marks.
    let v = call(&t.engine, "search_symbols", json!({"query": "formatAmount", "ref": BOTH, "format": "verbose"}));
    assert_eq!(v["results"].as_array().unwrap().iter().filter(|h| h["name"] == "formatAmount").count(), 2);
}

#[test]
fn ref_lists_in_get_symbol_trace_and_outline() {
    let t = two_refs();
    // get_symbol: one item per distinct version; the other version is not a
    // candidate.
    let r = call(&t.engine, "get_symbol", json!({"qualified_name": "InvoiceService.issue", "ref": BOTH}));
    let res = &r["results"];
    assert_eq!((res["s"].clone(), res["in"].clone(), res["changed"].clone()), (json!(0), json!([0]), json!(true)));
    let v = &res["versions"][0];
    assert_eq!((v["s"].clone(), v["in"].clone(), v["changed"].clone()), (json!(1), json!([1]), json!(true)), "{r}");
    assert_eq!(v["symbol"]["id"], format!("{BILLING}@c0ffee00c0ff:pkg/service.py#InvoiceService.issue"));
    assert_eq!((v["callers"].as_u64(), v["callees"].as_u64()), (Some(0), Some(3)));
    assert!(res["candidates"].as_array().unwrap().is_empty(), "{r}");
    let txt = text(&t.engine, "get_symbol", json!({"qualified_name": "InvoiceService.issue", "ref": BOTH}));
    assert!(txt.contains("Method InvoiceService.issue  pkg/service.py L9-11  python exported changed\n"), "{txt}");
    assert!(
        txt.contains(
            "  version [1] changed: Method InvoiceService.issue L9-12  def issue(self, invoice)  callers 0, callees 3\n"
        ),
        "{txt}"
    );
    let r = call(&t.engine, "get_symbol", json!({"qualified_name": "formatAmount", "ref": BOTH}));
    assert_eq!(
        (r["results"]["in"].clone(), r["results"].get("changed"), r["results"].get("versions")),
        (json!([0, 1]), None, None)
    );

    // trace: the union graph of both refs.
    let args = json!({"qualified_name": "InvoiceService.issue", "direction": "callees", "ref": BOTH});
    let r = call(&t.engine, "trace", args.clone());
    let nodes = r["results"]["nodes"].as_array().unwrap();
    let node = |qn: &str| nodes.iter().position(|n| n["qn"] == qn).unwrap_or_else(|| panic!("{qn}: {r}"));
    assert_eq!(nodes.len(), 4, "root, run, helper, audit: {r}");
    assert_eq!((nodes[0]["in"].clone(), nodes[0]["changed"].clone()), (json!([0, 1]), json!(true)));
    let (helper, audit) = (node("pkg.service.helper"), node("pkg.service.audit"));
    assert_eq!((nodes[audit]["s"].clone(), nodes[audit]["in"].clone()), (json!(1), json!([1])));
    let edges = r["results"]["edges"].as_array().unwrap();
    let edge = |dst: usize| edges.iter().find(|e| e[1] == dst).unwrap_or_else(|| panic!("{dst}: {r}"));
    assert_eq!(edge(helper)[6], json!({"in": [0, 1]}), "in both refs: {r}");
    assert_eq!(edge(audit)[6], json!({"in": [1], "changed": true}), "only on the branch: {r}");
    let txt = text(&t.engine, "trace", args);
    assert!(txt.contains(&format!("{audit} [1] out d1 Function audit  pkg/service.py L17-18 changed  ←0 CALLS 0.95 same_module @11 [1] changed\n")), "{txt}");
    assert!(txt.contains(" [0,1] changed  ←0 CALLS 0.95 same_module @10\n"), "{txt}");
    // Callers on main only: the test that the branch deleted.
    let r = call(
        &t.engine,
        "trace",
        json!({"qualified_name": "InvoiceService.issue", "direction": "callers", "ref": BOTH, "include_tests": true}),
    );
    let t_node = &r["results"]["nodes"][1];
    assert_eq!(
        (t_node["qn"].as_str(), t_node["in"].clone(), t_node["changed"].clone()),
        (Some("pkg.service.test_issue"), json!([0]), json!(true))
    );

    // get_file_outline: per-ref groups, one per distinct version of the file.
    let r = call(&t.engine, "get_file_outline", json!({"path": "src/util/index.ts", "ref": BOTH}));
    assert_eq!((r["results"]["in"].clone(), r["results"].get("versions")), (json!([0, 1]), None));
    let r = call(&t.engine, "get_file_outline", json!({"path": "pkg/service.py", "ref": BOTH}));
    let res = &r["results"];
    assert_eq!(
        (res["in"].clone(), res["changed"].clone(), res["versions"][0]["in"].clone()),
        (json!([0]), json!(true), json!([1]))
    );
    let names = |v: &Value| {
        v["symbols"].as_array().unwrap().iter().map(|s| s["name"].as_str().unwrap().to_string()).collect::<Vec<_>>()
    };
    assert!(
        names(res).contains(&"test_issue".to_string()) && names(&res["versions"][0]).contains(&"audit".to_string()),
        "{r}"
    );
    let txt = text(&t.engine, "get_file_outline", json!({"path": "pkg/service.py", "ref": BOTH}));
    assert!(
        txt.contains("pkg/service.py (python, 17 lines) [0] changed\n")
            && txt.contains("version [1] changed: pkg/service.py (python, 18 lines)\n"),
        "{txt}"
    );
    // The outline cursor runs over every version.
    let r1 = call(&t.engine, "get_file_outline", json!({"path": "pkg/service.py", "ref": BOTH, "budget_tokens": 100}));
    assert_eq!(r1["truncated"], true, "{r1}");

    // An unknown ref in a list errors listing the indexed refs.
    let e = err(&t.engine, "search_symbols", json!({"query": "x", "ref": ["main", "nope"]}));
    assert!(
        e.contains("ref `nope` is not indexed for repository `github.com/acme/billing`")
            && e.contains("`feature/x` (c0ffee00c0ff, ref snapshot)")
            && e.contains("`main` (7696364aef89+"),
        "{e}"
    );
    let e = err(&t.engine, "search_symbols", json!({"query": "x", "ref": []}));
    assert!(e.contains("must not be an empty list"), "{e}");

    // list_repositories lists every snapshot of a repository, the default first.
    let r = call(&t.engine, "list_repositories", json!({}));
    let billing = &r["results"][0];
    assert_eq!(
        billing["refs"],
        json!([{"ref": "main", "commit": "7696364aef89"}, {"ref": "feature/x", "commit": "c0ffee00c0ff"}])
    );
    let txt = text(&t.engine, "list_repositories", json!({}));
    assert!(txt.contains("github.com/acme/billing  refs main@7696364aef89, feature/x@c0ffee00c0ff  2 files"), "{txt}");
}

/// Registers the demo graph of `repo` as its ref snapshot `name` at
/// `commit`, exported under the store.
fn demo_ref(store: &std::path::Path, repo: &str, name: &str, commit: &str) {
    let base = std::fs::canonicalize(store).unwrap();
    let root = base.join("refs").join(repo.replace(['/', '.'], "-")).join(name.replace('/', "-"));
    std::fs::create_dir_all(&root).unwrap();
    let root = root.to_string_lossy().into_owned();
    let (mut b, _) = demo(repo, &root);
    b.graph.snapshot.commit = commit.into();
    b.graph.snapshot.branch = Some(name.into());
    b.graph.validate().expect("valid graph");
    let meta = rueca_segment::write_segment(&b.graph, &Sources(b.sources.clone()), store).unwrap();
    rueca_segment::Store::open(store).unwrap().register_ref(&root, &meta, name).unwrap();
}

#[test]
fn a_ref_snapshot_wins_over_the_working_tree_of_its_name() {
    // billing's working tree is on `main` with uncommitted changes;
    // `rueca index . --ref main` exported the commit `main` points to.
    let t = two();
    demo_ref(&t.engine.store_dir, BILLING, "main", C3);
    let r = call(&t.engine, "search_symbols", json!({"query": "helper", "ref": "main"}));
    let snap = &r["snapshots"][0];
    assert_eq!(
        (snap["ref"].as_str(), snap["commit"].as_str(), snap["overlay"].as_bool()),
        (Some("main"), Some(C3), Some(false)),
        "{r}"
    );
    // The working tree is still the default, and `HEAD` / its commit name it.
    let r = call(&t.engine, "search_symbols", json!({"query": "helper"}));
    assert_eq!(r["snapshots"][0]["overlay"], true, "{r}");
    let r = call(&t.engine, "search_symbols", json!({"query": "helper", "ref": "HEAD"}));
    assert_eq!(r["snapshots"][0]["overlay"], true, "{r}");
    let r = call(&t.engine, "search_symbols", json!({"query": "helper", "ref": &C1[..7]}));
    assert_eq!(r["snapshots"][0]["overlay"], true, "{r}");
}

#[test]
fn ref_snapshots_are_not_roots_of_their_own() {
    // One working tree and one ref snapshot, no default root: a call without
    // `repo` reads the working tree.
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store");
    let root = dir.path().join("billing");
    std::fs::create_dir_all(&root).unwrap();
    let root = std::fs::canonicalize(&root).unwrap().to_string_lossy().into_owned();
    demo(BILLING, &root).0.write(&store, &root);
    feature_ref(&store);
    let e = Engine::new(store, None).unwrap();
    let r = call(&e, "search_symbols", json!({"query": "helper"}));
    assert_eq!(r["snapshots"].as_array().unwrap().len(), 1, "{r}");
    assert_eq!(
        (r["snapshots"][0]["repo"].as_str(), r["snapshots"][0]["overlay"].as_bool()),
        (Some(BILLING), Some(false))
    );
    assert_ne!(r["snapshots"][0]["ref"], "feature/x", "{r}");
    let r = call(&e, "search_symbols", json!({"query": "audit", "ref": "feature/x"}));
    assert_eq!(r["snapshots"][0]["ref"], "feature/x", "{r}");
}

#[test]
fn a_pull_request_without_repo_names_one_repository() {
    let t = two();
    let store = t.engine.store_dir.clone();
    demo_ref(&store, BILLING, "pr/7", C3);
    // One repository has `pr/7`: read there, the others at their default.
    let r = call(&t.engine, "search_symbols", json!({"query": "helper", "repo": "*", "ref": "pr/7"}));
    let refs: Vec<&str> = r["snapshots"].as_array().unwrap().iter().map(|s| s["ref"].as_str().unwrap()).collect();
    assert!(refs.contains(&"pr/7"), "{r}");
    // Two have one: pull request numbers are per repository.
    demo_ref(&store, WEB, "pr/7", "dead00dead00dead00dead00dead00dead00dead");
    let e = err(&t.engine, "search_symbols", json!({"query": "x", "repo": "*", "ref": "pr/7"}));
    assert!(
        e.contains("`pr/7` exists in several repositories (github.com/acme/billing, github.com/acme/web): name `repo`"),
        "{e}"
    );
    let r = call(&t.engine, "search_symbols", json!({"query": "helper", "repo": WEB, "ref": "pr/7"}));
    assert_eq!((r["snapshots"][0]["repo"].as_str(), r["snapshots"][0]["ref"].as_str()), (Some(WEB), Some("pr/7")));
    // Not a pull request: every repository that has it.
    demo_ref(&store, BILLING, "release", "1111111111111111111111111111111111111111");
    demo_ref(&store, WEB, "release", "2222222222222222222222222222222222222222");
    let r = call(&t.engine, "search_symbols", json!({"query": "helper", "repo": "*", "ref": "release"}));
    assert_eq!(r["snapshots"].as_array().unwrap().len(), 2, "{r}");
}

#[test]
fn huge_cursors_do_not_overflow() {
    let t = two_refs();
    let r = call(&t.engine, "search_symbols", json!({"query": "helper", "ref": BOTH, "cursor": "4294967295"}));
    assert_eq!((r["results"].as_array().map(Vec::len), r["next_cursor"].clone()), (Some(0), Value::Null), "{r}");
    let e = err(&t.engine, "search_symbols", json!({"query": "helper", "ref": BOTH, "cursor": "18446744073709551615"}));
    assert!(e.contains("invalid `cursor`"), "{e}");
}
