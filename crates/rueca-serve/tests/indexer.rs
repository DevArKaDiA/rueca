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
        ("webhooks_verify_dedupe_confirm_and_reach_queries", webhooks_end_to_end),
        ("a_webhook_route_without_its_secret_refuses_to_start", webhook_without_secret),
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

const REFUNDS: (&str, &str) = ("billing/refunds.py", "def refund_payment(p):\n    return p\n");

fn webhook_config(host: &GitHost, dirs: &tempfile::TempDir) -> String {
    extra(host, dirs).replace(
        &format!("url = \"{}\"\n", host.url()),
        &format!("url = \"{}\"\nbranches = [\"release/*\"]\ntags = \"v*\"\nprs = true\n", host.url()),
    ) + r#"webhooks = { github = "env:RUECA_TEST_GITHUB_SECRET", gitlab = "env:RUECA_TEST_GITLAB_SECRET", bitbucket = "env:RUECA_TEST_BITBUCKET_SECRET" }
"#
}

fn fixture(name: &str, url: &str) -> String {
    let path = format!("{}/tests/fixtures/webhooks/{name}.json", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap().replace("REPO_URL", url)
}

fn hmac_hex(key: &str, body: &str) -> String {
    use hmac::{KeyInit, Mac};
    let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(key.as_bytes()).unwrap();
    mac.update(body.as_bytes());
    mac.finalize().into_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

/// Posts `body` to `/webhooks/<host>` signed as that host signs it
/// (`sign = false`: a wrong signature), with delivery id `id`.
fn deliver(s: &Server, host: &str, event: &str, id: &str, body: &str, sign: bool) -> common::Reply {
    let secret = if sign { common::webhook_secret(host) } else { "wrong".to_string() };
    let sig = format!("sha256={}", hmac_hex(&secret, body));
    let headers: Vec<(&str, &str)> = match host {
        "github" => vec![("x-github-event", event), ("x-github-delivery", id), ("x-hub-signature-256", &sig)],
        "gitlab" => vec![("x-gitlab-event", event), ("idempotency-key", id), ("x-gitlab-token", &secret)],
        _ => vec![("x-event-key", event), ("x-request-uuid", id), ("x-hub-signature", &sig)],
    };
    post(&format!("{}/webhooks/{host}", s.base), body, &headers)
}

/// Polls `GET /admin/jobs` until the job's `desired` satisfies `pred`.
fn wait_desired(s: &Server, pred: impl Fn(&Value) -> bool) -> Value {
    let auth = bearer(TOKEN_OPS);
    let started = Instant::now();
    loop {
        let jobs = get(&format!("{}/admin/jobs", s.base), &[("authorization", &auth)]).json();
        let desired = jobs["jobs"][0]["desired"].clone();
        if pred(&desired) {
            return desired;
        }
        assert!(started.elapsed() < Duration::from_secs(30), "desired never matched: {desired}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn webhooks_end_to_end() {
    let host = GitHost::new("billing", &[INVOICES]);
    host.set_ref("refs/heads/release/1");
    let dirs = tempfile::tempdir().unwrap();
    let s = Server::start_as("memory://indexer-webhooks", &webhook_config(&host, &dirs), Role::All);
    s.wait_ready();
    let url = host.url();
    refusals_and_ignored(&s, &url);

    // A push: 202, a duplicate is 200, and the commit becomes queryable.
    let moved = host.push(&[REFUNDS]);
    let push = fixture("github-push", &url);
    let started = Instant::now();
    assert_eq!(deliver(&s, "github", "push", "d-1", &push, true).json()["result"], "accepted");
    let dup = deliver(&s, "github", "push", "d-1", &push, true);
    assert_eq!((dup.status, dup.json()["result"].as_str()), (200, Some("duplicate")));
    wait_desired(&s, |d| d["main"]["commit"] == moved.as_str());
    wait_status(&s, |v| v.to_string().contains(&moved));
    let r = s.call(TOKEN_OPS, "search_symbols", json!({"query": "refund payment"}));
    assert!(r.body.contains("refund_payment"), "{}", r.body);
    eprintln!("webhooks: push → queryable in {:?}", started.elapsed());

    pull_requests(&s, &host, &url);
    other_hosts(&s, &host, &url);
    let metrics = get(&format!("{}/metrics", s.ops), &[]).body;
    for needle in [
        r#"rueca_webhook_deliveries_total{host="github",result="accepted"}"#,
        r#"rueca_webhook_deliveries_total{host="github",result="duplicate"} 1"#,
        r#"rueca_webhook_deliveries_total{host="github",result="unauthorized"} 2"#,
        r#"rueca_webhook_deliveries_total{host="github",result="too_large"} 1"#,
        r#"rueca_webhook_confirmations_total{host="gitlab",result="changed"}"#,
    ] {
        assert!(metrics.contains(needle), "{needle} not in\n{metrics}");
    }
}

/// Bad or missing signatures, an oversized body, a ping, an unknown
/// repository.
fn refusals_and_ignored(s: &Server, url: &str) {
    let push = fixture("github-push", url);
    assert_eq!(deliver(s, "github", "push", "x-1", &push, false).status, 401);
    let unsigned = post(&format!("{}/webhooks/github", s.base), &push, &[("x-github-event", "push")]);
    assert_eq!(unsigned.status, 401);
    assert!(oversized(s).starts_with("HTTP/1.1 413"), "a declared body over 25 MB is refused unread");
    let ping = deliver(s, "github", "ping", "x-3", &fixture("github-ping", url), true);
    assert_eq!((ping.status, ping.json()["result"].as_str()), (202, Some("ignored")));
    let foreign = fixture("github-push", "https://github.com/acme/elsewhere.git");
    let r = deliver(s, "github", "push", "x-4", &foreign, true);
    assert_eq!((r.status, r.json()["result"].as_str()), (202, Some("ignored")));
    assert_eq!(deliver(s, "github", "push", "x-5", "{nope", true).status, 400);
}

/// Declares a 25 MB + 1 body and sends none: the status line.
fn oversized(s: &Server) -> String {
    use std::io::{Read, Write};
    let addr = s.base.trim_start_matches("http://");
    let mut conn = std::net::TcpStream::connect(addr).unwrap();
    let head = format!(
        "POST /webhooks/github HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        25 * 1024 * 1024 + 1
    );
    conn.write_all(head.as_bytes()).unwrap();
    let mut buf = [0u8; 64];
    let n = conn.read(&mut buf).unwrap();
    String::from_utf8_lossy(&buf[..n]).to_string()
}

/// Opened → `pr/7` desired and served; closed → out of the desired state,
/// `closed_at` in the catalog.
fn pull_requests(s: &Server, host: &GitHost, url: &str) {
    host.set_ref("refs/pull/7/head");
    let opened = fixture("github-pull_request-opened", url);
    assert_eq!(deliver(s, "github", "pull_request", "pr-1", &opened, true).status, 202);
    wait_desired(s, |d| d["pr/7"]["kind"] == "pr");
    wait_status(s, |v| has_ref(v, "pr/7"));
    let closed = fixture("github-pull_request-closed", url);
    assert_eq!(deliver(s, "github", "pull_request", "pr-2", &closed, true).status, 202);
    wait_desired(s, |d| d.get("pr/7").is_none());
    let idx = s.state().indexer.clone().unwrap();
    let catalog =
        rueca_service::reconcile::read_catalog(idx.staging.storage.as_ref(), "local/billing").unwrap().unwrap();
    assert!(catalog.refs["pr/7"].closed_at.is_some(), "{:?}", catalog.refs["pr/7"]);
}

/// GitLab (legacy token) and Bitbucket deliveries.
fn other_hosts(s: &Server, host: &GitHost, url: &str) {
    host.set_ref("refs/tags/v2");
    let tag = fixture("gitlab-tag_push", url);
    assert_eq!(deliver(s, "gitlab", "Tag Push Hook", "gl-1", &tag, false).status, 401);
    assert_eq!(deliver(s, "gitlab", "Tag Push Hook", "gl-1", &tag, true).status, 202);
    wait_desired(s, |d| d["v2"]["kind"] == "tag");
    host.delete_ref("refs/heads/release/1");
    let push = fixture("bitbucket-repo_push", url);
    assert_eq!(deliver(s, "bitbucket", "repo:push", "bb-1", &push, true).status, 202);
    wait_desired(s, |d| d.get("release/1").is_none() && d["main"].is_object());
}

fn webhook_without_secret() {
    let host = GitHost::new("ledger", &[INVOICES]);
    let dirs = tempfile::tempdir().unwrap();
    let text = webhook_config(&host, &dirs).replace("RUECA_TEST_GITHUB_SECRET", "RUECA_UNSET_VARIABLE");
    let cache = tempfile::tempdir().unwrap();
    let loaded = common::config_as("memory://indexer-webhook-unset", cache.path(), &text, Role::All);
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let err = match rt.block_on(rueca_serve::start(loaded)) {
        Ok(_) => panic!("started without the webhook secret"),
        Err(e) => format!("{e:#}"),
    };
    assert!(err.contains("indexing.webhooks.github") && err.contains("refuses to start"), "{err}");
}
