//! End-to-end SQL resolution with the real `SqlPack`: migration order
//! (a table created again, a down migration, a column added later), the
//! nearest directory among same-named tables of several dialects, a
//! partially qualified name, an ambiguous name, CTEs and routines.

use rueca_extract::{Budget, LanguagePack, ProjectFiles, Registry};
use rueca_lang_sql::SqlPack;
use rueca_model::{EdgeKind, Language, ResolvedGraph, SnapshotInfo, Strategy, git_blob_sha};
use rueca_resolve::{FileInput, resolve};

fn build(files: &[(&str, &str)]) -> ResolvedGraph {
    let mut registry = Registry::new();
    registry.register(Box::new(SqlPack));
    let project = ProjectFiles { paths: files.iter().map(|(p, _)| p.to_string()).collect(), ..Default::default() };
    let inputs = files
        .iter()
        .map(|(path, src)| {
            let facts = SqlPack.extract(path, Language::Sql, src.as_bytes(), &Budget::default()).expect("extract");
            FileInput {
                path: path.to_string(),
                blob_sha: git_blob_sha(src.as_bytes()),
                language: Some(Language::Sql),
                size: src.len() as u64,
                status: "ok".into(),
                facts: Some(facts),
            }
        })
        .collect();
    let snapshot = SnapshotInfo {
        repo: "local/db".into(),
        commit: "worktree".into(),
        dirty: true,
        branch: None,
        root: None,
        indexed_at: 0,
        rueca_version: "test".into(),
    };
    let out = resolve(snapshot, inputs, &project, &registry);
    out.graph.validate().unwrap();
    out.graph
}

/// `(src qualified name @ path, kind, dst qualified name @ path, strategy, confidence)`.
fn edges(g: &ResolvedGraph) -> Vec<(String, EdgeKind, String, Strategy, f32)> {
    let name = |n: u32| {
        let node = &g.nodes[n as usize];
        let path = node.file.map_or(String::new(), |f| g.files[f as usize].path.clone());
        format!("{}@{path}", node.qualified_name)
    };
    g.edges
        .iter()
        .filter(|e| matches!(e.kind, EdgeKind::References | EdgeKind::Writes | EdgeKind::Calls))
        .map(|e| (name(e.src), e.kind, name(e.dst), e.meta.strategy, e.meta.confidence))
        .collect()
}

fn has(g: &ResolvedGraph, src: &str, kind: EdgeKind, dst: &str) -> bool {
    edges(g).iter().any(|(s, k, d, _, _)| s == src && *k == kind && d == dst)
}

#[test]
fn migrations_pick_the_effective_definition() {
    let g = build(&[
        ("db/V1__init.sql", "CREATE TABLE orders (id int);\nCREATE TABLE customers (id int);"),
        (
            "db/V2__items.sql",
            "CREATE TABLE items (order_id int REFERENCES orders (id));\nALTER TABLE customers ADD COLUMN email text;",
        ),
        ("db/V3__rebuild.sql", "DROP TABLE orders;\nCREATE TABLE orders (id bigint, note text);"),
        ("db/U3__undo.sql", "DROP TABLE orders;\nCREATE TABLE orders (id int);"),
        (
            "app/report.sql",
            "CREATE VIEW report AS SELECT o.note, c.email FROM orders o JOIN customers c ON c.id = o.id;",
        ),
    ]);
    // V2's foreign key: the `orders` of V1 (the only one before it).
    assert!(has(&g, "items@db/V2__items.sql", EdgeKind::References, "orders@db/V1__init.sql"), "{:#?}", edges(&g));
    // V3 drops the V1 table (a write from the file), then creates it again.
    assert!(
        has(&g, "db/V3__rebuild.sql@db/V3__rebuild.sql", EdgeKind::Writes, "orders@db/V1__init.sql"),
        "{:#?}",
        edges(&g)
    );
    // Outside the migrations: the latest `CREATE`, never the undo one.
    assert!(has(&g, "report@app/report.sql", EdgeKind::References, "orders@db/V3__rebuild.sql"));
    assert!(has(&g, "report@app/report.sql", EdgeKind::References, "orders.note@db/V3__rebuild.sql"));
    assert!(has(&g, "report@app/report.sql", EdgeKind::References, "customers.email@db/V2__items.sql"));
    assert!(!edges(&g).iter().any(|(_, _, d, _, _)| d.ends_with("@db/U3__undo.sql")));
}

#[test]
fn dialect_copies_resolve_to_the_nearest_directory() {
    let g = build(&[
        ("postgres/schema.sql", "CREATE TABLE actor (id int);\nCREATE VIEW actor_info AS SELECT id FROM actor;"),
        ("postgres/data.sql", "INSERT INTO actor (id) VALUES (1);"),
        ("mysql/schema.sql", "CREATE TABLE actor (id int);"),
        ("mysql/procs.sql", "CREATE VIEW names AS SELECT id FROM actor;"),
        ("shared/report.sql", "CREATE VIEW everyone AS SELECT id FROM actor;"),
    ]);
    assert!(has(&g, "actor_info@postgres/schema.sql", EdgeKind::References, "actor@postgres/schema.sql"));
    assert!(has(&g, "postgres/data.sql@postgres/data.sql", EdgeKind::Writes, "actor@postgres/schema.sql"));
    assert!(has(&g, "names@mysql/procs.sql", EdgeKind::References, "actor@mysql/schema.sql"));
    // Two copies at the same distance: no edge.
    assert!(!edges(&g).iter().any(|(s, _, _, _, _)| s.starts_with("everyone@")), "{:#?}", edges(&g));
}

#[test]
fn qualification_ctes_and_routines() {
    let g = build(&[
        (
            "schema.sql",
            "CREATE TABLE billing.invoices (id int);\nCREATE FUNCTION billing.total(x int) RETURNS int AS $$ SELECT count(*) FROM billing.invoices $$ LANGUAGE sql;",
        ),
        ("views.sql", "CREATE VIEW recent AS WITH last AS (SELECT id FROM invoices) SELECT total(id) FROM last;"),
    ]);
    // `invoices` matches `billing.invoices` only by its last segment.
    let e = edges(&g);
    let hit =
        e.iter().find(|(s, _, d, _, _)| s == "recent@views.sql" && d == "billing.invoices@schema.sql").expect("edge");
    assert_eq!((hit.3, hit.4), (Strategy::UniqueName, 0.6));
    assert!(has(&g, "recent@views.sql", EdgeKind::References, "recent.last@views.sql"));
    assert!(has(&g, "recent@views.sql", EdgeKind::Calls, "billing.total@schema.sql"));
    assert!(has(&g, "billing.total@schema.sql", EdgeKind::References, "billing.invoices@schema.sql"));
}
