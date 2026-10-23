//! End to end: `telar index` on a real working tree (real language packs,
//! resolver and segment writer), then every tool through `telar query`.

mod common;

use serde_json::{Value, json};

use common::*;

const ISSUE_ID: &str = "local/proj@worktree:pkg/service.py#InvoiceService.issue";

#[test]
fn python_index_then_every_tool() {
    let p = python_project();
    let report = index(&p);
    assert_eq!(report["root"], p.root.to_str().unwrap());
    assert_eq!(report["segment"]["snapshot"]["repo"], "local/proj");
    assert_eq!(report["files_total"], 4, "{report}");
    assert!(report["segment"]["node_count"].as_u64().unwrap() >= 8, "{report}");

    // search_symbols
    let r = query(&p, "search_symbols", json!({"query": "invoice issue"}));
    assert_eq!(r["results"][0]["id"], ISSUE_ID, "{r}");
    assert_eq!(r["results"][0]["start_line"], 8);
    assert_eq!(r["results"][0]["end_line"], 11);
    assert_eq!(r["results"][0]["signature"], "def issue(self, invoice)");
    let r = query(&p, "search_symbols", json!({"query": "validate", "labels": ["Class"]}));
    assert!(r["results"].as_array().unwrap().iter().all(|h| h["label"] == "Class"), "{r}");

    // get_symbol, by suffix, by name + path, and by id.
    let r = query(&p, "get_symbol", json!({"qualified_name": "InvoiceService.issue"}));
    assert_eq!(r["symbol"]["id"], ISSUE_ID);
    assert_eq!(r["symbol"]["language"], "python");
    assert_eq!(r["parent"]["qualified_name"], "pkg.service.InvoiceService");
    assert_eq!(r["callers_count"], 1, "{r}");
    assert_eq!(r["callees_count"], 2, "{r}");
    assert_eq!(query(&p, "get_symbol", json!({"id": ISSUE_ID}))["symbol"], r["symbol"]);
    assert_eq!(query(&p, "get_symbol", json!({"name": "issue", "path": "pkg/service.py"}))["symbol"], r["symbol"]);
    let r = query(&p, "get_symbol", json!({"qualified_name": "pkg.service.InvoiceService"}));
    assert_eq!(qns(&r["children"]), ["pkg.service.InvoiceService.issue"]);
    assert_eq!(qns(&r["bases"]), ["pkg.models.Base"], "{r}");
    assert_eq!(r["symbol"]["doc"], "Issues invoices.");
    let r = query(&p, "get_symbol", json!({"qualified_name": "models.Base"}));
    assert_eq!(qns(&r["subtypes"]), ["pkg.service.InvoiceService"], "{r}");

    // trace: callers before callees at each depth, edges with provenance.
    let r = query(&p, "trace", json!({"qualified_name": "InvoiceService.issue", "depth": 1}));
    assert_eq!(r["root"]["id"], ISSUE_ID);
    let nodes: Vec<(String, String)> = r["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| (n["direction"].as_str().unwrap().into(), n["qualified_name"].as_str().unwrap().into()))
        .collect();
    let expect = |d: &str, q: &str| (d.to_string(), q.to_string());
    assert_eq!(
        nodes,
        [
            expect("callers", "pkg.service.main"),
            expect("callees", "pkg.models.Base.save"),
            expect("callees", "pkg.models.validate"),
        ],
        "{r}"
    );
    let validate_edge = r["edges"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["dst"] == "local/proj@worktree:pkg/models.py#validate")
        .unwrap_or_else(|| panic!("{r}"));
    assert_eq!(validate_edge["src"], ISSUE_ID);
    assert_eq!(validate_edge["kind"], "CALLS");
    assert_eq!(validate_edge["call_site"], json!({"path": "pkg/service.py", "line": 9}));
    assert!(validate_edge["confidence"].as_f64().unwrap() > 0.0);
    assert!(validate_edge["strategy"].is_string());
    let r = query(&p, "trace", json!({"name": "main", "direction": "callees", "depth": 3}));
    let deep: Vec<String> = qns(&r["nodes"]);
    assert!(deep.contains(&"pkg.models.validate".to_string()), "{r}");
    assert!(r["nodes"].as_array().unwrap().iter().all(|n| n["direction"] == "callees"));

    // get_file_outline: nested, ordered by line.
    let r = query(&p, "get_file_outline", json!({"path": "pkg/service.py"}));
    assert_eq!(r["line_count"], 16);
    assert_eq!(r["language"], "python");
    assert_eq!(qns(&r["symbols"]), ["pkg.service.InvoiceService", "pkg.service.main"], "{r}");
    assert_eq!(qns(&r["symbols"][0]["children"]), ["pkg.service.InvoiceService.issue"]);

    // search_code: literal with context and enclosing symbol; regex.
    let r = query(&p, "search_code", json!({"pattern": "invalid invoice", "context_lines": 1}));
    assert_eq!(r["matches"].as_array().unwrap().len(), 1, "{r}");
    let m = &r["matches"][0];
    assert_eq!((m["path"].as_str(), m["line"].as_u64()), (Some("pkg/service.py"), Some(11)));
    assert_eq!(m["before"], json!(["            return self.save()"]));
    assert_eq!(m["symbol"]["id"], ISSUE_ID);
    let r = query(&p, "search_code", json!({"pattern": "^def \\w+", "regex": true}));
    let lines: Vec<(String, u64)> = r["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| (m["path"].as_str().unwrap().to_string(), m["line"].as_u64().unwrap()))
        .collect();
    assert_eq!(lines, [("pkg/models.py".to_string(), 6), ("pkg/service.py".to_string(), 14)], "{r}");
    let r = query(&p, "search_code", json!({"pattern": "demo", "path_glob": "*.md"}));
    assert_eq!(r["matches"][0]["path"], "README.md", "{r}");

    // get_snippet: by symbol and by range.
    let r = query(&p, "get_snippet", json!({"name": "validate"}));
    assert_eq!(r["text"], "def validate(obj):\n    return obj is not None");
    assert_eq!((r["start_line"].as_u64(), r["end_line"].as_u64()), (Some(6), Some(7)));
    assert_eq!(r["symbol"]["qualified_name"], "pkg.models.validate");
    let r = query(&p, "get_snippet", json!({"path": "pkg/service.py", "start_line": 1, "end_line": 2}));
    assert_eq!(r["text"], "\"\"\"Billing service.\"\"\"\nfrom pkg.models import Base, validate");
    assert!(r.get("symbol").is_none());
    // Absolute paths work, also through a non-canonical spelling of the root
    // (the temp dir is under the `/var` → `/private/var` symlink on macOS).
    let abs = p.dir.path().join("proj/pkg/models.py");
    let r = query(&p, "get_snippet", json!({"path": abs.to_str().unwrap(), "start_line": 6, "end_line": 6}));
    assert_eq!((r["path"].as_str(), r["text"].as_str()), (Some("pkg/models.py"), Some("def validate(obj):")));
    let r = query(&p, "get_file_outline", json!({"path": p.root.join("pkg/models.py").to_str().unwrap()}));
    assert_eq!(qns(&r["symbols"]), ["pkg.models.Base", "pkg.models.validate"], "{r}");
}

#[test]
fn reindex_and_status() {
    let p = python_project();
    index(&p);
    write(&p.root, "pkg/extra.py", "def brand_new_function():\n    return 1\n");
    let r = query(&p, "search_symbols", json!({"query": "brand_new_function"}));
    assert_eq!(r["results"], json!([]), "not indexed yet: {r}");
    index(&p);
    let r = query(&p, "search_symbols", json!({"query": "brand_new_function"}));
    assert_eq!(r["results"][0]["qualified_name"], "pkg.extra.brand_new_function", "{r}");

    let out = telar(&p.store).args(["status", "--json"]).output().unwrap();
    assert!(out.status.success());
    let s = stdout_json(&out);
    let roots = s["roots"].as_array().unwrap();
    assert_eq!(roots.len(), 1, "{s}");
    assert_eq!(roots[0]["root"], p.root.to_str().unwrap());
    assert_eq!(roots[0]["repo"], "local/proj");
    assert_eq!(roots[0]["files"], 5);
    let out = telar(&p.store).arg("status").output().unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.starts_with("ROOT") && text.contains("local/proj"), "{text}");

    // The current directory selects its indexed root when --root is absent.
    let out = telar(&p.store)
        .current_dir(p.root.join("pkg"))
        .args(["query", "get_symbol", r#"{"name": "validate"}"#])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    assert_eq!(stdout_json(&out)["symbol"]["path"], "pkg/models.py");

    // Unknown symbols and files are errors (exit 2, JSON on stdout).
    for (tool, args) in [
        ("get_symbol", json!({"name": "does_not_exist"})),
        ("get_file_outline", json!({"path": "nope.py"})),
        ("trace", json!({"qualified_name": "x.y", "depth": 2})),
    ] {
        let out = telar(&p.store).arg("--root").arg(&p.root).args(["query", tool, &args.to_string()]).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{tool}");
        assert!(stdout_json(&out)["error"].is_string());
    }
}

#[test]
fn index_summary_and_pretty() {
    let p = python_project();
    let out = telar(&p.store).arg("index").arg(&p.root).output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.starts_with("indexed ") && text.contains("local/proj"), "{text}");
    let out = telar(&p.store)
        .arg("--root")
        .arg(&p.root)
        .args(["query", "get_symbol", r#"{"name": "main"}"#, "--pretty"])
        .output()
        .unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.starts_with("{\n  \""), "{text}");
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["symbol"]["qualified_name"], "pkg.service.main");
    // Not a directory: exit 1 with a message on stderr.
    let out = telar(&p.store).args(["index"]).arg(p.root.join("pkg/models.py")).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("not a directory"));
}

/// TypeScript end to end, in a mixed Python + TypeScript tree.
#[test]
fn typescript_index_then_every_tool() {
    let p = python_project();
    add_typescript(&p);
    index(&p);

    let r = query(&p, "search_symbols", json!({"query": "format amount", "language": "typescript"}));
    assert_eq!(r["results"][0]["qualified_name"], "web/src/util:formatAmount", "{r}");
    assert_eq!(r["results"][0]["id"], "local/proj@worktree:web/src/util.ts#formatAmount");

    let r = query(&p, "get_symbol", json!({"qualified_name": "Session.request"}));
    assert_eq!(r["symbol"]["id"], "local/proj@worktree:web/src/util.ts#Session.request", "{r}");
    assert_eq!(r["symbol"]["language"], "typescript");
    assert_eq!(r["parent"]["qualified_name"], "web/src/util:Session");
    assert_eq!(r["callees_count"], 1, "{r}");
    assert_eq!(r["callers_count"], 1, "{r}");

    let r = query(&p, "trace", json!({"qualified_name": "util:formatAmount", "direction": "callers", "depth": 2}));
    let callers = qns(&r["nodes"]);
    assert!(callers.contains(&"web/src/util:Session.request".to_string()), "{r}");
    assert!(callers.contains(&"web/src/app:run".to_string()), "{r}");

    let r = query(&p, "get_file_outline", json!({"path": "web/src/util.ts"}));
    assert_eq!(qns(&r["symbols"]), ["web/src/util:formatAmount", "web/src/util:Session"], "{r}");
    assert_eq!(qns(&r["symbols"][1]["children"]), ["web/src/util:Session.request"]);

    let r = query(&p, "search_code", json!({"pattern": "toFixed", "language": "typescript"}));
    assert_eq!(r["matches"][0]["symbol"]["qualified_name"], "web/src/util:formatAmount", "{r}");

    let r = query(&p, "get_snippet", json!({"qualified_name": "app:run"}));
    assert!(r["text"].as_str().unwrap().starts_with("export function run(): string {"), "{r}");
    assert_eq!((r["start_line"].as_u64(), r["end_line"].as_u64()), (Some(3), Some(6)));
}
