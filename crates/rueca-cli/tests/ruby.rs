//! Ruby end to end: `rueca index` on the gem fixture of `rueca-lang-ruby`,
//! then the tools through `rueca query` — constant-path qualified names,
//! symbol ids, a reopened module, mixins, `require` imports and env reads.

mod common;

use std::path::Path;

use serde_json::json;

use common::*;

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            copy_dir(&p, &to.join(e.file_name()));
        } else {
            std::fs::copy(&p, to.join(e.file_name())).unwrap();
        }
    }
}

fn ruby_project() -> Project {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../rueca-lang-ruby/tests/fixtures/shop");
    copy_dir(&fixture, &root);
    let root = std::fs::canonicalize(root).unwrap();
    let store = dir.path().join("store");
    Project { dir, root, store }
}

#[test]
fn ruby_index_then_every_tool() {
    let p = ruby_project();
    index(&p);
    let sell = "local/proj@worktree:lib/shop/store.rb#Shop::Store#sell";

    let r = query(&p, "search_symbols", json!({"query": "Inventory", "language": "ruby"}));
    assert_eq!(r["results"][0]["qualified_name"], "Shop::Inventory", "{r}");

    let r = query(&p, "get_symbol", json!({"qualified_name": "Shop::Store#sell"}));
    assert_eq!(r["symbol"]["id"], sell, "{r}");
    assert_eq!(r["symbol"]["language"], "ruby");
    assert_eq!(r["symbol"]["signature"], "def sell(sku)");

    // Callees: a method of the class typed in `initialize`.
    let r = query(&p, "trace", json!({"id": sell, "direction": "callees"}));
    assert!(qns(&r["nodes"]).contains(&"Shop::Audit#record".to_string()), "{r}");

    // A mixin method's callers through `include Countable`.
    let r = query(&p, "trace", json!({"qualified_name": "Shop::Countable#total", "direction": "callers"}));
    assert_eq!(qns(&r["nodes"]), ["Shop::Inventory#count"], "{r}");

    // `require` / `require_relative` → IMPORTS of the files.
    let r =
        query(&p, "trace", json!({"qualified_name": "lib/shop.rb", "direction": "callees", "edge_types": ["IMPORTS"]}));
    let imported = qns(&r["nodes"]);
    for want in ["lib/shop/store.rb", "lib/shop/version.rb"] {
        assert!(imported.contains(&want.to_string()), "{want}: {r}");
    }

    // The env var bridge: `ENV.fetch("SHOP_CURRENCY", "USD")`.
    let r = query(
        &p,
        "trace",
        json!({"qualified_name": "$env:SHOP_CURRENCY", "direction": "callers", "edge_types": ["READS"]}),
    );
    assert_eq!(qns(&r["nodes"]), ["Shop::Store#sell"], "{r}");

    let r = query(&p, "get_file_outline", json!({"path": "lib/shop/store.rb"}));
    assert_eq!(qns(&r["symbols"]), ["Shop::Store"], "{r}");
    let outline = qns(&r["symbols"][0]["children"]);
    for want in ["Shop::Store.open", "Shop::Store#inventory", "Shop::Store#sell"] {
        assert!(outline.contains(&want.to_string()), "{want}: {r}");
    }

    let r = query(&p, "get_snippet", json!({"id": sell}));
    assert!(r["text"].as_str().unwrap().trim_start().starts_with("def sell(sku)"), "{r}");
}
