//! SQL end to end: `rueca index` on the fixture of `rueca-lang-sql`
//! (PostgreSQL Flyway migrations with a table dropped and created again,
//! a view, sqlc named queries, T-SQL procedures in `GO` batches, a MySQL
//! `DELIMITER` procedure, dbt models and a macro, a psql script including
//! another), then the tools through `rueca query`.

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

fn sql_project() -> Project {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../rueca-lang-sql/tests/fixtures/shop");
    copy_dir(&fixture, &root);
    let root = std::fs::canonicalize(root).unwrap();
    let store = dir.path().join("store");
    Project { dir, root, store }
}

const DEPS: [&str; 3] = ["REFERENCES", "WRITES", "CALLS"];

#[test]
fn sql_index_then_every_tool() {
    let p = sql_project();
    index(&p);
    let view = "local/proj@worktree:db/views.sql#customer_orders";

    let r = query(&p, "search_symbols", json!({"query": "customer_orders", "language": "sql"}));
    assert_eq!(r["results"][0]["qualified_name"], "customer_orders", "{r}");

    let r = query(&p, "get_symbol", json!({"qualified_name": "customer_orders"}));
    assert_eq!(r["symbol"]["id"], view, "{r}");
    assert_eq!(r["symbol"]["language"], "sql");
    assert_eq!(r["symbol"]["label"], "Struct");
    assert_eq!(r["symbol"]["signature"], "CREATE VIEW customer_orders AS -- ansi");

    // The view reads the effective `orders` (created again in V3), the
    // column V2 added to `customers`, and calls `order_total`.
    let r = query(&p, "trace", json!({"id": view, "depth": 1, "direction": "callees", "edge_types": DEPS}));
    let deps = ids(&r["nodes"]);
    for want in [
        "db/migrations/V3__rebuild_orders.sql#orders",
        "db/migrations/V1__init.sql#customers",
        "db/migrations/V2__items.sql#customers.email",
        "db/migrations/V2__items.sql#order_total",
    ] {
        assert!(deps.iter().any(|d| d.ends_with(want)), "{want}: {r}");
    }
    assert!(!deps.iter().any(|d| d.ends_with("V1__init.sql#orders")), "{r}");

    // Who uses the first `orders`: the V2 foreign key and the V3 `DROP`
    // (a write from the migration file); the V3 one: the queries, the
    // T-SQL procedure, the view.
    let r = query(
        &p,
        "trace",
        json!({"id": "local/proj@worktree:db/migrations/V1__init.sql#orders", "depth": 1, "direction": "callers", "edge_types": DEPS}),
    );
    assert_eq!(qns(&r["nodes"]), ["db/migrations/V3__rebuild_orders.sql", "order_items"], "{r}");
    let r = query(
        &p,
        "trace",
        json!({"id": "local/proj@worktree:db/migrations/V3__rebuild_orders.sql#orders", "depth": 1, "direction": "callers", "edge_types": DEPS}),
    );
    let users = qns(&r["nodes"]);
    for want in ["ListOrders", "reportorders", "customer_orders"] {
        assert!(users.contains(&want.to_string()), "{want}: {r}");
    }

    // T-SQL: a procedure calling another; MySQL: a DELIMITER procedure.
    let r = query(&p, "trace", json!({"qualified_name": "reportorders", "depth": 1, "direction": "callees"}));
    assert_eq!(qns(&r["nodes"]), ["audit"], "{r}");
    let r = query(
        &p,
        "trace",
        json!({"qualified_name": "purge_legacy", "depth": 1, "direction": "callees", "edge_types": ["WRITES"]}),
    );
    assert_eq!(qns(&r["nodes"]), ["legacy_users"], "{r}");

    // dbt: ref() to a model, a macro call, the CTE.
    let r = query(
        &p,
        "trace",
        json!({"qualified_name": "orders_summary", "depth": 1, "direction": "callees", "edge_types": DEPS}),
    );
    let deps = qns(&r["nodes"]);
    for want in ["stg_orders", "cents_to_dollars", "orders_summary.orders"] {
        assert!(deps.contains(&want.to_string()), "{want}: {r}");
    }

    // dbt `var('start_date')` → the key of `dbt_project.yml`.
    let r = query(
        &p,
        "trace",
        json!({"qualified_name": "stg_orders", "depth": 1, "direction": "callees", "edge_types": ["READS"]}),
    );
    assert_eq!(qns(&r["nodes"]), ["dbt/dbt_project.yml:vars.start_date"], "{r}");

    // psql `\ir` → IMPORTS.
    let r = query(
        &p,
        "trace",
        json!({"qualified_name": "scripts/init.sql", "depth": 1, "direction": "callees", "edge_types": ["IMPORTS"]}),
    );
    assert_eq!(qns(&r["nodes"]), ["db/views.sql"], "{r}");

    let r = query(&p, "get_file_outline", json!({"path": "db/migrations/V1__init.sql"}));
    let outline = qns(&r["symbols"]);
    for want in ["customers", "orders"] {
        assert!(outline.contains(&want.to_string()), "{want}: {r}");
    }

    let r = query(&p, "get_snippet", json!({"id": view}));
    assert!(r["text"].as_str().unwrap().trim_start().starts_with("CREATE VIEW customer_orders AS"), "{r}");
}

/// The ids of a trace's nodes.
fn ids(nodes: &serde_json::Value) -> Vec<String> {
    nodes.as_array().unwrap().iter().map(|n| n["id"].as_str().unwrap_or_default().to_string()).collect()
}
