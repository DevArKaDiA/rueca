//! `rueca serve` over HTTP against a `memory://` service store (RFC 0008
//! phase 1): the MCP flows, visibility, transport checks, readiness,
//! metrics, the generation watcher and rate limits.

mod common;

use std::time::{Duration, Instant};

use common::{BILLING, CORE, Publisher, Server, TOKEN, TOKEN_NO_RULES, get, populated, post};
use rueca_query::EngineOpens;
use serde_json::{Value, json};

const META_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
const META_CAPS: &str = "io.modelcontextprotocol/clientCapabilities";

fn initialize(version: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": version, "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}})
}

/// (a) The legacy flow (`initialize`, `tools/list`, `tools/call`) and the
/// 2026-07-28 stateless flow (`server/discover`, `_meta`) answer what the
/// engine answers locally.
#[test]
fn mcp_over_http_equals_the_local_engine() {
    let p = populated("memory://serve-a");
    let s = Server::start("memory://serve-a", "");
    s.wait_ready();
    legacy_handshake(&s);
    // The visible part of the organization, answered as the engine answers
    // it locally for the same visibility.
    let local = p.local_engine().with_visibility(rueca_query::Visibility::Repos(
        ["github.com/acme/billing", "github.com/acme/core"].iter().map(|s| s.to_string()).collect(),
    ));
    legacy_calls_match(&s, &local);
    stateless_flow_matches(&s, &local);
    // A notification is accepted without a body.
    let note = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    assert_eq!(s.mcp(Some(TOKEN), &note, &[]).status, 202);
}

/// `initialize` and `tools/list`: no session minted, every tool listed.
fn legacy_handshake(s: &Server) {
    let init = s.mcp(Some(TOKEN), &initialize("2025-06-18"), &[]);
    assert_eq!(init.status, 200, "{}", init.body);
    assert_eq!(init.header("content-type"), Some("application/json"));
    assert_eq!(init.json()["result"]["protocolVersion"], "2025-06-18");
    assert!(init.header("mcp-session-id").is_none(), "no session is minted");

    let list = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});
    let tools = s.mcp(Some(TOKEN), &list, &[("mcp-protocol-version", "2025-06-18"), ("mcp-session-id", "ignored")]);
    assert_eq!(tools.status, 200, "{}", tools.body);
    let listed = tools.json()["result"]["tools"].as_array().cloned().unwrap_or_default();
    let names: Vec<&str> = listed.iter().filter_map(|t| t["name"].as_str()).collect();
    assert_eq!(names, rueca_query::TOOL_NAMES);
}

/// Legacy `tools/call`s answer what `local` answers.
fn legacy_calls_match(s: &Server, local: &rueca_query::Engine) {
    for (tool, args) in [
        ("search_symbols", json!({"query": "invoice"})),
        ("list_repositories", json!({})),
        ("get_symbol", json!({"qualified_name": "billing.invoices.issue_invoice"})),
    ] {
        let r = s.call(TOKEN, tool, args.clone());
        assert_eq!(r.status, 200, "{}", r.body);
        let v = r.json();
        let want = local.respond(tool, args).unwrap();
        assert_eq!(v["result"]["structuredContent"], want.json, "{tool}");
        assert_eq!(v["result"]["content"][0]["text"], want.text, "{tool}");
        assert_eq!(v["result"]["isError"], false);
        assert!(!r.body.contains("hidden/secret"), "{tool}: {}", r.body);
    }
}

/// 2026-07-28: discover, then a call that carries `_meta` and the headers
/// matching it.
fn stateless_flow_matches(s: &Server, local: &rueca_query::Engine) {
    let meta = json!({META_VERSION: "2026-07-28", META_CAPS: {}});
    let discover = json!({"jsonrpc": "2.0", "id": 3, "method": "server/discover", "params": {"_meta": meta}});
    let d = s.mcp(Some(TOKEN), &discover, &[("mcp-protocol-version", "2026-07-28"), ("mcp-method", "server/discover")]);
    assert_eq!(d.status, 200, "{}", d.body);
    assert!(d.json()["result"]["supportedVersions"].as_array().unwrap().contains(&json!("2026-07-28")));
    let call = json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call",
        "params": {"name": "search_symbols", "arguments": {"query": "order"}, "_meta": meta}});
    let headers =
        [("mcp-protocol-version", "2026-07-28"), ("mcp-method", "tools/call"), ("mcp-name", "search_symbols")];
    let r = s.mcp(Some(TOKEN), &call, &headers);
    assert_eq!(r.status, 200, "{}", r.body);
    let v = r.json();
    assert_eq!(v["result"]["resultType"], "complete");
    assert_eq!(
        v["result"]["structuredContent"],
        local.respond("search_symbols", json!({"query": "order"})).unwrap().json
    );
}

/// (c) A token without `repos` sees an empty organization: no results, no
/// repository named.
#[test]
fn a_principal_without_rules_sees_nothing() {
    let _p = populated("memory://serve-c");
    let s = Server::start("memory://serve-c", "");
    s.wait_ready();
    organization_reads_as_empty(&s);
    nothing_hidden_is_named(&s);
    status_by_role(&s);
}

fn organization_reads_as_empty(s: &Server) {
    for (tool, args) in [("list_repositories", json!({})), ("search_symbols", json!({"query": "invoice"}))] {
        let r = s.call(TOKEN_NO_RULES, tool, args);
        assert_eq!(r.status, 200, "{}", r.body);
        let v = r.json();
        assert_eq!(v["result"]["isError"], false, "{tool}: {}", r.body);
        assert_eq!(v["result"]["structuredContent"]["results"], json!([]), "{tool}");
        assert_eq!(v["result"]["structuredContent"]["snapshots"], json!([]), "{tool}");
    }
}

/// A repository named by the caller is reported like one that does not
/// exist; a symbol is not found. Nothing else is named.
fn nothing_hidden_is_named(s: &Server) {
    for (tool, args) in [
        ("search_symbols", json!({"query": "invoice", "repo": "github.com/acme/billing"})),
        ("get_symbol", json!({"qualified_name": "billing.invoices.issue_invoice"})),
        ("search_code", json!({"query": "def"})),
    ] {
        let r = s.call(TOKEN_NO_RULES, tool, args);
        assert_eq!(r.status, 200, "{}", r.body);
        let named = ["acme/core", "hidden/secret", "billing/invoices.py", "create_order"]
            .into_iter()
            .find(|hidden| r.body.contains(hidden));
        assert_eq!(named, None, "{tool}: {}", r.body);
    }
}

/// `/status`: nothing visible and no cache detail for the principal without
/// rules; both repositories and the cache for the admin token.
fn status_by_role(s: &Server) {
    let st = get(&format!("{}/status", s.base), &[("authorization", &format!("Bearer {TOKEN_NO_RULES}"))]);
    assert_eq!(st.status, 200, "{}", st.body);
    assert_eq!(st.json()["repositories"], json!([]));
    assert!(st.json().get("cache").is_none(), "not an admin");
    let admin = get(&format!("{}/status", s.base), &[("authorization", &format!("Bearer {TOKEN}"))]).json();
    assert_eq!(admin["repositories"].as_array().map(Vec::len), Some(2));
    assert_eq!(admin["generation"], 1);
    assert!(admin["cache"]["bytes"]["segment"].as_u64().unwrap_or(0) > 0, "{admin}");
}

/// (d) Transport checks: Origin, methods, body size, header/body match,
/// tokens in the query string, unsupported versions.
#[test]
fn transport_checks() {
    let _p = populated("memory://serve-d");
    let s = Server::start("memory://serve-d", "max_body_bytes = \"2KiB\"");
    s.wait_ready();
    let auth = format!("Bearer {TOKEN}");
    let list = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});

    let r = s.mcp(Some(TOKEN), &list, &[("origin", "https://evil.example")]);
    assert_eq!(r.status, 403, "{}", r.body);
    let r = s.mcp(Some(TOKEN), &list, &[("origin", "http://localhost:5173")]);
    assert_eq!(r.status, 200, "loopback origin of a loopback deployment: {}", r.body);

    assert_eq!(get(&format!("{}/mcp", s.base), &[("authorization", &auth)]).status, 405);

    let big = json!({"jsonrpc": "2.0", "id": 3, "method": "tools/list", "params": {"pad": "x".repeat(4096)}});
    assert_eq!(s.mcp(Some(TOKEN), &big, &[]).status, 413);

    let call = json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "search_symbols", "arguments": {"query": "x"}}});
    let r = s.mcp(Some(TOKEN), &call, &[("mcp-method", "tools/call"), ("mcp-name", "trace")]);
    assert_eq!(r.status, 400, "{}", r.body);
    assert_eq!(r.json()["error"]["code"], -32020);
    assert_eq!(r.json()["id"], 4);
    let r = s.mcp(Some(TOKEN), &call, &[("mcp-method", "tools/list")]);
    assert_eq!((r.status, r.json()["error"]["code"].clone()), (400, json!(-32020)));

    let r = s.mcp(Some(TOKEN), &list, &[("mcp-protocol-version", "1999-01-01")]);
    assert_eq!((r.status, r.json()["error"]["code"].clone()), (400, json!(-32022)));

    let r = post(&format!("{}/mcp", s.base), "{not json", &[("authorization", &auth)]);
    assert_eq!((r.status, r.json()["error"]["code"].clone()), (400, json!(-32700)));

    let r = post(&format!("{}/mcp?access_token={TOKEN}", s.base), &list.to_string(), &[]);
    assert_eq!(r.status, 400);
    assert_eq!(r.json()["error"], "invalid_request");

    let r = get(&format!("{}/admin/jobs", s.base), &[("authorization", &auth)]);
    assert_eq!(r.status, 404, "admin endpoints belong to the indexer role");
}

/// (b, static part) No token: 401 with a challenge and no PRM without an
/// OIDC issuer.
#[test]
fn unauthenticated_requests_are_challenged() {
    let _p = populated("memory://serve-b0");
    let s = Server::start("memory://serve-b0", "");
    let list = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});
    let r = s.mcp(None, &list, &[]);
    assert_eq!(r.status, 401);
    assert_eq!(r.header("www-authenticate"), Some("Bearer scope=\"rueca:read\""));
    let r = s.mcp(Some("rueca_wrong"), &list, &[]);
    assert_eq!(r.status, 401);
    assert!(r.header("www-authenticate").unwrap().starts_with("Bearer error=\"invalid_token\""));
    assert_eq!(get(&format!("{}/.well-known/oauth-protected-resource/mcp", s.base), &[]).status, 404);
    let metrics = get(&format!("{}/metrics", s.ops), &[]).body;
    assert!(metrics.contains("rueca_auth_failures_total{reason=\"missing\"} 1"), "{metrics}");
}

/// (e, f) `/readyz` is 503 until the first generation is pulled; a new
/// generation is served within two watcher ticks; `/metrics` carries the
/// query-path metrics.
#[test]
fn readiness_metrics_and_new_generations() {
    let url = "memory://serve-e";
    let s = Server::start(url, "");
    std::thread::sleep(Duration::from_millis(300));
    let r = get(&format!("{}/readyz", s.ops), &[]);
    assert_eq!(r.status, 503, "{}", r.body);
    assert_eq!(get(&format!("{}/healthz", s.ops), &[]).json(), json!({"ok": true}));

    let mut p = Publisher::new(url);
    p.snapshot("github.com/acme/billing", &[BILLING]);
    assert_eq!(p.generation(&["github.com/acme/billing"]), 1);
    let ready = s.wait_ready();
    assert_eq!(ready, json!({"generation": 1, "warm": "1/1"}));

    let r = s.call(TOKEN, "list_repositories", json!({}));
    assert!(r.body.contains("github.com/acme/billing") && !r.body.contains("acme/core"), "{}", r.body);

    // Generation 2 adds a repository: served within two ticks.
    p.snapshot("github.com/acme/core", &[CORE]);
    assert_eq!(p.generation(&["github.com/acme/billing", "github.com/acme/core"]), 2);
    let published_at = s.state().ticks();
    let started = Instant::now();
    loop {
        let g = get(&format!("{}/readyz", s.ops), &[]).json();
        if g["generation"] == 2 {
            let ticks = s.state().ticks() - published_at;
            assert!(ticks <= 2, "served after {ticks} ticks");
            break;
        }
        assert!(started.elapsed() < Duration::from_secs(30), "generation 2 never served");
        std::thread::sleep(Duration::from_millis(10));
    }
    let r = s.call(TOKEN, "list_repositories", json!({}));
    assert!(r.body.contains("github.com/acme/core"), "{}", r.body);

    let metrics = get(&format!("{}/metrics", s.ops), &[]).body;
    for needle in [
        "rueca_mcp_requests_total{tool=\"list_repositories\",outcome=\"ok\"} 2",
        "rueca_mcp_request_seconds_bucket{tool=\"list_repositories\",scope=\"org\"",
        "rueca_generation 2",
        "rueca_generation_lag_seconds",
        "rueca_cache_requests_total{cache=\"segment\",result=\"miss\"}",
        "rueca_cache_bytes{cache=\"segment\"}",
        "rueca_store_requests_total{op=\"get\",result=\"not_modified\"}",
    ] {
        assert!(metrics.contains(needle), "{needle} not in\n{metrics}");
    }
}

/// Ready means warm: `/readyz` answers 200 only once the engine that serves
/// calls has opened the generation's organization index and every default
/// segment, so the first `search_symbols` after it opens nothing; a new
/// generation is warmed before `/readyz` names it.
#[test]
fn ready_means_warm() {
    let url = "memory://serve-warm";
    let s = Server::start(url, "");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(get(&format!("{}/readyz", s.ops), &[]).status, 503);
    let mut p = populated(url);
    assert_eq!(s.wait_ready()["generation"], 1);
    let serving = s.state().serving().unwrap();
    let warm = EngineOpens { org_index: 1, segments: 3 };
    assert_eq!(serving.engine.opens(), warm, "ready before the warm-up finished");
    let r = s.call(TOKEN, "search_symbols", json!({"query": "invoice"}));
    assert!(r.body.contains("issue_invoice"), "{}", r.body);
    assert_eq!(serving.engine.opens(), warm, "the first call after readiness opened something");
    let metrics = get(&format!("{}/metrics", s.ops), &[]).body;
    assert!(metrics.contains("rueca_generation_warm_seconds_count 1"), "{metrics}");

    // Generation 2 adds a repository: when `/readyz` names it, its index and
    // the new segment are open (the three unchanged segments stay open).
    p.snapshot("github.com/acme/shipping", &[("ship/labels.py", "def print_label(parcel):\n    return parcel\n")]);
    let repos =
        ["github.com/acme/billing", "github.com/acme/core", "github.com/hidden/secret", "github.com/acme/shipping"];
    assert_eq!(p.generation(&repos), 2);
    let started = Instant::now();
    while get(&format!("{}/readyz", s.ops), &[]).json()["generation"] != 2 {
        assert!(started.elapsed() < Duration::from_secs(30), "generation 2 never served");
        std::thread::sleep(Duration::from_millis(5));
    }
    let warm = EngineOpens { org_index: 2, segments: 4 };
    assert_eq!(serving.engine.opens(), warm, "generation 2 ready before its warm-up finished");
    let r = s.call(TOKEN, "search_symbols", json!({"query": "label"}));
    assert!(r.body.contains("print_label"), "{}", r.body);
    assert_eq!(serving.engine.opens(), warm);
    let metrics = get(&format!("{}/metrics", s.ops), &[]).body;
    assert!(metrics.contains("rueca_generation_warm_seconds_count 2"), "{metrics}");
}

/// (g) Over the principal's burst: 429 with `Retry-After`.
#[test]
fn rate_limits() {
    let _p = populated("memory://serve-g");
    let s = Server::start("memory://serve-g", "rate_limit = { per_second = 1, burst = 2 }");
    s.wait_ready();
    let list = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});
    let statuses: Vec<u16> = (0..3).map(|_| s.mcp(Some(TOKEN), &list, &[]).status).collect();
    assert_eq!(statuses, [200, 200, 429]);
    let r = s.mcp(Some(TOKEN), &list, &[]);
    assert_eq!(r.status, 429);
    assert!(r.header("retry-after").unwrap().parse::<u64>().unwrap() >= 1);
    assert_eq!(s.mcp(Some(TOKEN_NO_RULES), &list, &[]).status, 200, "limits are per principal");
    let metrics = get(&format!("{}/metrics", s.ops), &[]).body;
    assert!(metrics.contains("rueca_rate_limited_total{stage=\"principal\"} 2"), "{metrics}");
}
