//! C# end to end: `rueca index` on the solution fixture of
//! `rueca-lang-csharp`, then the tools through `rueca query` — namespace
//! qualified names, symbol ids, overloads, partial types, interface
//! `OVERRIDES`, `using` imports, tests, and the environment and
//! configuration bridge (`appsettings.json`).

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

fn csharp_project() -> Project {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../rueca-lang-csharp/tests/fixtures/shop");
    copy_dir(&fixture, &root);
    let root = std::fs::canonicalize(root).unwrap();
    let store = dir.path().join("store");
    Project { dir, root, store }
}

#[test]
fn csharp_index_then_every_tool() {
    let p = csharp_project();
    index(&p);
    let place = "local/proj@worktree:src/Shop.Core/Services/OrderService.cs#OrderService.Place";

    let r = query(&p, "search_symbols", json!({"query": "OrderService", "language": "csharp"}));
    assert_eq!(r["results"][0]["qualified_name"], "Acme.Shop.Services.OrderService", "{r}");

    let r = query(&p, "get_symbol", json!({"qualified_name": "OrderService.Place"}));
    assert_eq!(r["symbol"]["id"], place, "{r}");
    assert_eq!(r["symbol"]["language"], "csharp");
    assert_eq!(r["symbol"]["signature"], "public Order Place(int id)");

    // Callees: a constructor, an overload, an inherited method, an interface
    // method, `using static`, an extension method.
    let r = query(&p, "trace", json!({"id": place, "direction": "callees"}));
    let callees = qns(&r["nodes"]);
    for want in [
        "Acme.Shop.Models.Order.Order",
        "Acme.Shop.Models.Order.Add(string,int)",
        "Acme.Shop.Models.Entity.Validate",
        "Acme.Shop.Services.IOrderStore.Save",
        "Acme.Shop.Services.Pricing.Discount",
        "Acme.Shop.Services.OrderExtensions.Audit",
    ] {
        assert!(callees.contains(&want.to_string()), "{want}: {r}");
    }

    // An interface method's implementations through OVERRIDES.
    let r = query(
        &p,
        "trace",
        json!({"qualified_name": "Acme.Shop.Services.IOrderStore.Save", "direction": "callers", "edge_types": ["OVERRIDES"]}),
    );
    assert_eq!(qns(&r["nodes"]), ["Acme.Shop.App.MemoryStore.Save"], "{r}");

    // `using Acme.Shop.Services;` → IMPORTS of the namespace's directory.
    let r = query(
        &p,
        "trace",
        json!({"qualified_name": "src/Shop.App/Program.cs", "direction": "callees", "edge_types": ["IMPORTS"]}),
    );
    assert!(qns(&r["nodes"]).contains(&"src/Shop.Core/Services".to_string()), "{r}");

    // The bridge: an env var and an `appsettings.json` key.
    let r =
        query(&p, "trace", json!({"qualified_name": "$env:SHOP_DB", "direction": "callers", "edge_types": ["READS"]}));
    assert_eq!(qns(&r["nodes"]), ["Acme.Shop.Services.OrderService.Place"], "{r}");
    let r = query(
        &p,
        "trace",
        json!({"qualified_name": "src/Shop.App/appsettings.json:Shop.TaxRate", "direction": "callers", "edge_types": ["READS"]}),
    );
    assert_eq!(qns(&r["nodes"]), ["Acme.Shop.Services.OrderService.Place"], "{r}");

    let r = query(&p, "get_file_outline", json!({"path": "src/Shop.Core/Models/Order.cs"}));
    assert_eq!(qns(&r["symbols"]), ["Acme.Shop.Models.Order", "Acme.Shop.Models.Line"], "{r}");
    let members = qns(&r["symbols"][0]["children"]);
    for want in
        ["Acme.Shop.Models.Order.Order", "Acme.Shop.Models.Order.Add(Line)", "Acme.Shop.Models.Order.Add(string,int)"]
    {
        assert!(members.contains(&want.to_string()), "{want}: {r}");
    }

    let r = query(&p, "get_snippet", json!({"id": place}));
    assert!(r["text"].as_str().unwrap().trim_start().starts_with("public Order Place(int id)"), "{r}");
}
