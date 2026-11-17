//! The cooperative deadline (RFC 0008 §6.5) against the organization the
//! HTTP tests serve: in the engine (`Engine::respond_with`) and over HTTP
//! (`[http].query_deadline` → JSON-RPC -32001).

mod common;

use std::time::{Duration, Instant};

use common::{Server, TOKEN, get, populated};
use rueca_query::{DeadlineExceeded, RespondOptions};
use serde_json::{Value, json};

/// One call of every tool against the organization.
fn every_tool() -> Vec<(&'static str, Value)> {
    let issue = json!({"qualified_name": "billing.invoices.issue_invoice"});
    vec![
        ("search_symbols", json!({"query": "invoice order"})),
        ("search_code", json!({"pattern": "return"})),
        ("get_symbol", issue.clone()),
        ("get_snippet", issue.clone()),
        ("trace", issue),
        ("get_file_outline", json!({"path": "billing/invoices.py"})),
        ("list_repositories", json!({})),
        ("list_contracts", json!({})),
    ]
}

fn deadline(at: Instant) -> RespondOptions {
    RespondOptions { deadline: Some(at), visibility: None }
}

#[test]
fn every_tool_fails_fast_past_its_deadline() {
    let p = populated("memory://deadline-engine-past");
    let org = p.local_engine();
    for (tool, args) in every_tool() {
        let started = Instant::now();
        let err = org.respond_with(tool, args, deadline(Instant::now())).unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(1), "{tool} took {:?}", started.elapsed());
        assert!(err.downcast_ref::<DeadlineExceeded>().is_some(), "{tool}: {err:#}");
        assert_eq!(err.to_string(), "deadline of 0ms exceeded; narrow `repo` or lower `limit`", "{tool}");
    }
}

#[test]
fn a_generous_deadline_changes_nothing() {
    let p = populated("memory://deadline-engine-later");
    let org = p.local_engine();
    let later = Instant::now() + Duration::from_secs(600);
    for (tool, args) in every_tool() {
        let plain = org.respond(tool, args.clone()).unwrap();
        let timed = org.respond_with(tool, args, deadline(later)).unwrap();
        assert_eq!(timed.text, plain.text, "{tool}");
        assert_eq!(timed.json.to_string(), plain.json.to_string(), "{tool}");
    }
}

/// `query_deadline = "1ms"`: `search_symbols` over the organization is
/// stopped by the engine and answered with -32001 naming the deadline.
#[test]
fn a_short_query_deadline_is_a_json_rpc_error() {
    let _p = populated("memory://deadline-http");
    let s = Server::start("memory://deadline-http", "query_deadline = \"1ms\"");
    s.wait_ready();
    let reply = (0..20)
        .map(|_| s.call(TOKEN, "search_symbols", json!({"query": "invoice order", "limit": 50})))
        .find(|r| r.json()["error"]["code"] == -32001);
    let Some(r) = reply else { panic!("no call exceeded 1 ms") };
    assert_eq!(r.status, 200, "{}", r.body);
    let v = r.json();
    assert_eq!(v["id"], 7);
    assert_eq!(v["error"]["message"], "deadline of 1ms exceeded; narrow `repo` or lower `limit`", "{v}");
    let metrics = get(&format!("{}/metrics", s.ops), &[]).body;
    assert!(metrics.contains("rueca_mcp_requests_total{tool=\"search_symbols\",outcome=\"deadline\"}"), "{metrics}");
}
