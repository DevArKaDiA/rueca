//! The indexer role end to end, in process (RFC 0008 phase 2): `rueca
//! serve --role all` on `memory://` with a local bare repository as the
//! code host (`file://`, accepted by debug builds through the test-only
//! `[indexing].allow_file_urls`): configured → reconciled → leased →
//! fetched into a bare mirror → indexed in an `index-worker` child process
//! (the default `ChildRunner`) → published → generation served → a query
//! answers; then `/admin/*`, `/status` and the metrics; and the `indexer`
//! role's readiness.
//!
//! This binary has no libtest harness (`harness = false`): the runner
//! starts `<this executable> index-worker`, so `main` hands that
//! invocation to the worker before running the tests.

mod common;

use std::process::ExitCode;
use std::time::{Duration, Instant};

use common::{GitHost, Server, TOKEN_NO_RULES, TOKEN_OPS, delete, get, post, run_tests};
use rueca_serve::Role;
use serde_json::{Value, json};

const INVOICES: (&str, &str) =
    ("billing/invoices.py", "def issue_invoice(order):\n    return order\n\ndef void_invoice(i):\n    return None\n");

fn extra(host: &GitHost, dirs: &tempfile::TempDir) -> String {
    format!(
        r#"
[org]
name = "acme"

[[repo]]
url = "{url}"

[indexing]
allow_file_urls = true
discover = "100ms"
updater_tick = "100ms"
mirror_dir = "{mirrors}"
facts_dir = "{facts}"
"#,
        url = host.url(),
        mirrors = dirs.path().join("mirrors").display(),
        facts = dirs.path().join("facts").display(),
    )
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

/// Polls `GET /status` (ops token) until `pred` holds (at most 30 s).
fn wait_status(s: &Server, pred: impl Fn(&Value) -> bool) -> Value {
    let started = Instant::now();
    loop {
        let r = get(&format!("{}/status", s.base), &[("authorization", &bearer(TOKEN_OPS))]);
        let v = r.json();
        if pred(&v) {
            return v;
        }
        assert!(started.elapsed() < Duration::from_secs(30), "status never matched: {v}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn has_ref(status: &Value, name: &str) -> bool {
    status["repositories"].as_array().is_some_and(|rs| rs.iter().any(|r| r["refs"].to_string().contains(name)))
}

fn main() -> ExitCode {
    if std::env::args().nth(1).as_deref() == Some("index-worker") {
        return ExitCode::from(u8::try_from(rueca_service::worker::worker_main()).unwrap_or(1));
    }
    run_tests(&[
        ("role_all_indexes_publishes_and_serves_a_configured_repository", role_all_indexes_publishes_and_serves),
        ("the_indexer_role_is_ready_without_a_generation_and_serves_no_mcp", the_indexer_role_is_ready),
    ])
}

fn role_all_indexes_publishes_and_serves() {
    let host = GitHost::new("billing", &[INVOICES]);
    let dirs = tempfile::tempdir().unwrap();
    let started = Instant::now();
    let s = Server::start_as("memory://indexer-all-e2e", &extra(&host, &dirs), Role::All);
    let ready = s.wait_ready();
    eprintln!("role all: configured → generation {} served in {:?}", ready["generation"], started.elapsed());

    let r = s.call(TOKEN_OPS, "search_symbols", json!({"query": "issue invoice"}));
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(r.body.contains("issue_invoice"), "{}", r.body);
    assert!(r.body.contains("local/billing"), "{}", r.body);

    // Admin: index the tag on demand; it reaches the served generation.
    let auth = bearer(TOKEN_OPS);
    let admin = [("authorization", auth.as_str())];
    let body = json!({"repo": "local/billing", "ref": "v1"}).to_string();
    let r = post(&format!("{}/admin/index", s.base), &body, &admin);
    assert_eq!(r.status, 202, "{}", r.body);
    assert_eq!(r.json()["kind"], "tag");
    // Job counts are as of the last discovery: wait for both.
    let st = wait_status(&s, |v| has_ref(v, "v1") && v["jobs"]["index.done"] == 1);
    assert!(st["generation"].as_u64().unwrap() >= 2, "{st}");
    let r = s.call(TOKEN_OPS, "search_symbols", json!({"query": "void invoice", "repo": "local/billing", "ref": "v1"}));
    assert!(r.body.contains("void_invoice"), "{}", r.body);

    let jobs = get(&format!("{}/admin/jobs", s.base), &admin).json();
    let job = &jobs["jobs"][0];
    assert_eq!((job["repo"].as_str(), job["state"].as_str()), (Some("local/billing"), Some("done")), "{jobs}");
    assert_eq!(job["desired"]["v1"]["kind"], "tag");

    refusals(&s, &admin);
    removal(&s, &admin, &body);

    metrics_present(&s);
}

/// The indexer role's metrics after a run.
fn metrics_present(s: &Server) {
    let metrics = get(&format!("{}/metrics", s.ops), &[]).body;
    for needle in [
        r#"rueca_jobs{kind="index",state="done"} 1"#,
        r#"rueca_queue_depth{kind="index",priority="0"} 0"#,
        r#"rueca_publish_seconds_bucket{stage="fetch""#,
        r#"rueca_publish_seconds_bucket{stage="index""#,
        r#"rueca_publish_seconds_bucket{stage="generation""#,
        r#"rueca_job_seconds_bucket{kind="index",result="done""#,
        "rueca_generation ",
    ] {
        assert!(metrics.contains(needle), "{needle} not in\n{metrics}");
    }
}

/// Not an admin: 403. Unknown repository or ref: 404. A prefix: 400.
fn refusals(s: &Server, admin: &[(&str, &str)]) {
    let viewer = bearer(TOKEN_NO_RULES);
    assert_eq!(get(&format!("{}/admin/jobs", s.base), &[("authorization", &viewer)]).status, 403);
    for (repo, r, status) in
        [("local/nope", "v1", 404), ("local/billing", "v9", 404), ("local/billing", "abcdef1", 400)]
    {
        let body = json!({"repo": repo, "ref": r}).to_string();
        assert_eq!(post(&format!("{}/admin/index", s.base), &body, admin).status, status, "{repo}@{r}");
    }
}

/// Removes the tag (`body`); the default ref cannot be removed.
fn removal(s: &Server, admin: &[(&str, &str)], body: &str) {
    let main = json!({"repo": "local/billing", "ref": "main"}).to_string();
    assert_eq!(delete(&format!("{}/admin/refs", s.base), &main, admin).status, 400);
    assert_eq!(delete(&format!("{}/admin/refs", s.base), body, admin).status, 202);
    wait_status(s, |v| !has_ref(v, "v1") && v["generation"].as_u64() >= Some(3));
}

fn the_indexer_role_is_ready() {
    let host = GitHost::new("payments", &[INVOICES]);
    let dirs = tempfile::tempdir().unwrap();
    let s = Server::start_as("memory://indexer-only", &extra(&host, &dirs), Role::Indexer);
    let ready = s.wait_ready();
    assert_eq!(ready["store"], "ok", "{ready}");
    assert!(ready["git"].as_str().is_some(), "{ready}");
    let r = s.call(TOKEN_OPS, "search_symbols", json!({"query": "x"}));
    assert_eq!(r.status, 404, "no /mcp in the indexer role: {}", r.body);
    // It still indexes and publishes: the job finishes.
    let st = wait_status(&s, |v| v["jobs"]["index.done"] == 1);
    assert_eq!(st["role"], "indexer");
}
