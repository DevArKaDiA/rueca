//! Tool behaviour for the RFC 0003 additions, on a hand-built segment:
//! canonical env vars (`$env` ids, `readers_count` / `writers_count`,
//! `READS` / `WRITES` traces), `Key` left out of default symbol search,
//! symbol ids of Go / Java / C++ / data symbols (module names from the
//! segment), optional overload suffixes and key-path suffix lookups.

mod common;

use rueca_model::*;
use rueca_query::Engine;
use serde_json::{Value, json};

use common::*;

const GO: &str = "package server\n\nfunc (s *Server) Handle() {}\n";
const JAVA: &str =
    "package com.acme;\nclass Parser {\n  void parse(String s) {}\n  void parse(String s, int n) {}\n}\n";
const CPP: &str = "namespace ns {\nvoid Foo::bar() {}\n}\n";
const YAML: &str = "spec:\n  env:\n    - name: DATABASE_URL\n      value: x\nserver:\n  port: 8080\n";
const PY: &str = "import os\n\ndef main():\n    return os.getenv('DATABASE_URL')\n";

struct Setup {
    _dir: tempfile::TempDir,
    engine: Engine,
}

fn setup() -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store");
    let root = dir.path().join("repo");
    std::fs::create_dir_all(&root).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    let r = root.to_str().unwrap();
    let mut b = Builder::new("local/repo", "c1", r);

    let (gf, gn) = b.file("server/handle.go", GO);
    let handle =
        b.sym(gn, gf, Label::Method, "Handle", "example.com/m/server.Server.Handle", span(GO, 2, 2), None, true, false);
    let (jf, jn) = b.file("src/com/acme/Parser.java", JAVA);
    let parser = b.sym(jn, jf, Label::Class, "Parser", "com.acme.Parser", span(JAVA, 1, 4), None, true, false);
    b.sym(parser, jf, Label::Method, "parse", "com.acme.Parser.parse(String)", span(JAVA, 2, 2), None, true, false);
    b.sym(
        parser,
        jf,
        Label::Method,
        "parse",
        "com.acme.Parser.parse(String, int)",
        span(JAVA, 3, 3),
        None,
        true,
        false,
    );
    let (cf, cn) = b.file("src/foo.cpp", CPP);
    b.sym(cn, cf, Label::Method, "bar", "ns::Foo::bar", span(CPP, 1, 1), None, true, false);

    let (yf, yn) = b.file("k8s/web.yaml", YAML);
    let spec = b.sym(yn, yf, Label::Key, "spec", "k8s/web.yaml:spec", span(YAML, 0, 3), None, true, false);
    let provider = b.sym(
        spec,
        yf,
        Label::EnvVar,
        "DATABASE_URL",
        "k8s/web.yaml:spec.env[\"DATABASE_URL\"]",
        span(YAML, 2, 3),
        Some("DATABASE_URL: •••"),
        true,
        false,
    );
    let server = b.sym(yn, yf, Label::Key, "server", "k8s/web.yaml:server", span(YAML, 4, 5), None, true, false);
    let port = b.sym(server, yf, Label::Key, "port", "k8s/web.yaml:server.port", span(YAML, 5, 5), None, true, false);

    let (pf, pn) = b.file("app.py", PY);
    let main = b.sym(pn, pf, Label::Function, "main", "app.main", span(PY, 2, 3), None, true, false);

    // Canonical env var: file-less, as the resolver creates it.
    let env = b.graph.nodes.len() as NodeId;
    b.graph.nodes.push(Node {
        label: Label::EnvVar,
        name: "DATABASE_URL".into(),
        qualified_name: env_var_qualified_name("DATABASE_URL"),
        file: None,
        span: None,
        language: None,
        signature: None,
        doc: None,
        exported: true,
        is_test: false,
    });
    b.edge(main, env, EdgeKind::Reads, 0.95, Strategy::Contract, Some((pf, line_span(PY, 3))));
    b.edge(provider, env, EdgeKind::Writes, 1.0, Strategy::Syntactic, Some((yf, line_span(YAML, 2))));
    b.edge(main, port, EdgeKind::Reads, 0.9, Strategy::Contract, Some((pf, line_span(PY, 3))));
    let _ = handle;

    b.graph.modules =
        vec!["example.com/m/server".into(), "com.acme".into(), String::new(), "k8s/web.yaml".into(), "app".into()];
    b.write(&store, r);
    Setup { engine: Engine::new(store, Some(root)).unwrap(), _dir: dir }
}

fn call(e: &Engine, tool: &str, args: Value) -> Value {
    e.call(tool, args.clone()).unwrap_or_else(|err| panic!("{tool} {args}: {err:#}"))
}

#[test]
fn canonical_env_var_ids_counts_and_trace() {
    let s = setup();
    let r = call(&s.engine, "get_symbol", json!({"qualified_name": "$env:DATABASE_URL"}));
    let sym = &r["symbol"];
    assert_eq!(sym["id"], "local/repo@c1:$env#DATABASE_URL");
    assert_eq!(sym["label"], "EnvVar");
    assert_eq!(sym["path"], Value::Null);
    assert_eq!((r["readers_count"].as_u64(), r["writers_count"].as_u64()), (Some(1), Some(1)));
    // The id round-trips.
    let again = call(&s.engine, "get_symbol", json!({"id": "local/repo@c1:$env#DATABASE_URL"}));
    assert_eq!(again["symbol"]["qualified_name"], "$env:DATABASE_URL");
    // A bare name prefers the canonical node (exported, shortest name) over the provider.
    let by_suffix = call(&s.engine, "get_symbol", json!({"qualified_name": "DATABASE_URL"}));
    assert_eq!(by_suffix["symbol"]["qualified_name"], "$env:DATABASE_URL");
    // Other labels do not carry the counts.
    let f = call(&s.engine, "get_symbol", json!({"qualified_name": "app.main"}));
    assert!(f.get("readers_count").is_none() && f.get("writers_count").is_none());
    let k = call(&s.engine, "get_symbol", json!({"qualified_name": "server.port"}));
    assert_eq!((k["readers_count"].as_u64(), k["writers_count"].as_u64()), (Some(1), Some(0)));
    // trace: who reads it and who sets it.
    let t = call(
        &s.engine,
        "trace",
        json!({"qualified_name": "$env:DATABASE_URL", "direction": "callers", "edge_types": ["READS", "WRITES"]}),
    );
    let mut kinds: Vec<(String, String, String)> = t["edges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["src"].as_str().unwrap().into(),
                e["kind"].as_str().unwrap().into(),
                e["strategy"].as_str().unwrap().into(),
            )
        })
        .collect();
    kinds.sort();
    assert_eq!(
        kinds,
        vec![
            ("local/repo@c1:app.py#main".to_string(), "READS".to_string(), "contract".to_string()),
            ("local/repo@c1:k8s/web.yaml#spec.env[\"DATABASE_URL\"]".into(), "WRITES".into(), "syntactic".into()),
        ]
    );
}

#[test]
fn keys_are_left_out_of_default_search() {
    let s = setup();
    let names = |v: &Value| -> Vec<String> {
        v["results"].as_array().unwrap().iter().map(|h| h["qualified_name"].as_str().unwrap().to_string()).collect()
    };
    let default = call(&s.engine, "search_symbols", json!({"query": "port"}));
    assert!(!names(&default).iter().any(|q| q.ends_with("server.port")), "{default}");
    let keys = call(&s.engine, "search_symbols", json!({"query": "port", "labels": ["Key"]}));
    assert_eq!(names(&keys)[0], "k8s/web.yaml:server.port");
    assert!(keys["results"].as_array().unwrap().iter().all(|h| h["label"] == "Key"));
    let yaml = call(&s.engine, "search_symbols", json!({"query": "port", "language": "yaml"}));
    assert_eq!(names(&yaml)[0], "k8s/web.yaml:server.port");
    // EnvVar stays in the default set.
    let env = call(&s.engine, "search_symbols", json!({"query": "DATABASE_URL"}));
    assert!(names(&env).contains(&"$env:DATABASE_URL".to_string()), "{env}");
    // Unknown-to-this-build languages are accepted and match nothing.
    let go = call(&s.engine, "search_symbols", json!({"query": "Handle", "language": "go"}));
    assert_eq!(names(&go), vec!["example.com/m/server.Server.Handle"]);
}

#[test]
fn symbol_ids_of_new_families() {
    let s = setup();
    let id = |qn: &str| call(&s.engine, "get_symbol", json!({"qualified_name": qn}))["symbol"]["id"].clone();
    // Go: the module (import path, dots and all) comes from the segment.
    assert_eq!(id("example.com/m/server.Server.Handle"), "local/repo@c1:server/handle.go#Server.Handle");
    // Java overloads keep their parameter suffix in the id.
    assert_eq!(
        id("com.acme.Parser.parse(String, int)"),
        "local/repo@c1:src/com/acme/Parser.java#Parser.parse(String, int)"
    );
    // C++: no module, the qualified name is the local name.
    assert_eq!(id("ns::Foo::bar"), "local/repo@c1:src/foo.cpp#ns::Foo::bar");
    // Data: the key path.
    assert_eq!(id("k8s/web.yaml:server.port"), "local/repo@c1:k8s/web.yaml#server.port");
    // Ids resolve back.
    for qn in ["example.com/m/server.Server.Handle", "com.acme.Parser.parse(String)", "ns::Foo::bar"] {
        let i = id(qn);
        let back = call(&s.engine, "get_symbol", json!({"id": i}));
        assert_eq!(back["symbol"]["qualified_name"], qn);
    }
}

#[test]
fn overload_suffix_is_optional_and_key_accessors_are_boundaries() {
    let s = setup();
    let r = call(&s.engine, "get_symbol", json!({"qualified_name": "Parser.parse"}));
    let mut all = vec![r["symbol"]["qualified_name"].as_str().unwrap().to_string()];
    all.extend(r["candidates"].as_array().unwrap().iter().map(|c| c["qualified_name"].as_str().unwrap().to_string()));
    all.sort();
    assert_eq!(all, vec!["com.acme.Parser.parse(String)", "com.acme.Parser.parse(String, int)"]);
    let r = call(&s.engine, "get_symbol", json!({"qualified_name": "Foo::bar"}));
    assert_eq!(r["symbol"]["qualified_name"], "ns::Foo::bar");
    let r = call(&s.engine, "get_symbol", json!({"qualified_name": "[\"DATABASE_URL\"]"}));
    assert_eq!(r["symbol"]["qualified_name"], "k8s/web.yaml:spec.env[\"DATABASE_URL\"]");
}
