//! Rust end to end: `rueca index` on the Cargo workspace fixture of
//! `rueca-lang-rust`, then the tools through `rueca query` — module-path
//! qualified names, symbol ids, `impl` items, trait `OVERRIDES`, `mod` and
//! `use` imports, macro arguments and env reads.

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

fn rust_project() -> Project {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../rueca-lang-rust/tests/fixtures/shop");
    copy_dir(&fixture, &root);
    let root = std::fs::canonicalize(root).unwrap();
    let store = dir.path().join("store");
    Project { dir, root, store }
}

#[test]
fn rust_index_then_every_tool() {
    let p = rust_project();
    index(&p);
    let total = "local/proj@worktree:crates/core/src/model.rs#Item.total";

    let r = query(&p, "search_symbols", json!({"query": "MemoryStore", "language": "rust"}));
    assert_eq!(r["results"][0]["qualified_name"], "shop_core::store::memory::MemoryStore", "{r}");

    let r = query(&p, "get_symbol", json!({"qualified_name": "Item.total"}));
    assert_eq!(r["symbol"]["id"], total, "{r}");
    assert_eq!(r["symbol"]["language"], "rust");
    assert_eq!(r["symbol"]["signature"], "pub fn total(&self, qty: u64) -> u64");

    // Callers: a method of the same impl through `self`, and `main` in the
    // binary crate (inside `traced!(…)`).
    let r = query(&p, "trace", json!({"id": total, "direction": "callers"}));
    let callers = qns(&r["nodes"]);
    for want in ["shop_app::main", "shop_core::model::Item.discounted"] {
        assert!(callers.contains(&want.to_string()), "{want}: {r}");
    }

    // A trait method's implementations through OVERRIDES.
    let r = query(
        &p,
        "trace",
        json!({"qualified_name": "shop_core::store::Store.save", "direction": "callers", "edge_types": ["OVERRIDES"]}),
    );
    assert_eq!(qns(&r["nodes"]), ["shop_core::store::memory::MemoryStore.save"], "{r}");

    // `mod cli;` and `use shop_core::…` → IMPORTS of the module files.
    let r = query(
        &p,
        "trace",
        json!({"qualified_name": "crates/app/src/main.rs", "direction": "callees", "edge_types": ["IMPORTS"]}),
    );
    let imported = qns(&r["nodes"]);
    for want in ["crates/app/src/cli.rs", "crates/core/src/lib.rs", "crates/core/src/model.rs"] {
        assert!(imported.contains(&want.to_string()), "{want}: {r}");
    }

    // The env var bridge: `std::env::var("SHOP_DB")`.
    let r =
        query(&p, "trace", json!({"qualified_name": "$env:SHOP_DB", "direction": "callers", "edge_types": ["READS"]}));
    assert_eq!(qns(&r["nodes"]), ["shop_core::util::check_env"], "{r}");

    let r = query(&p, "get_file_outline", json!({"path": "crates/core/src/model.rs"}));
    let outline = qns(&r["symbols"]);
    for want in ["shop_core::model::Item", "shop_core::model::Price", "shop_core::model::Item.total"] {
        assert!(outline.contains(&want.to_string()), "{want}: {r}");
    }

    let r = query(&p, "get_snippet", json!({"id": total}));
    assert!(r["text"].as_str().unwrap().trim_start().starts_with("pub fn total(&self, qty: u64) -> u64"), "{r}");
}
