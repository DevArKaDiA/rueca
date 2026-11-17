//! The MCP server over in-process pipes, speaking raw JSON-RPC lines: the
//! legacy `initialize` handshake and the 2026-07-28 stateless flow.

mod common;

use common::Client;
use serde_json::{Value, json};

fn start(store: &std::path::Path) -> Client {
    let engine = rueca_query::Engine::new(store.to_path_buf(), None).unwrap();
    Client::start(rueca_mcp::RuecaServer::new(engine))
}

fn send(c: &mut Client, msg: Value) {
    c.send_line(&msg.to_string());
}

/// The response with `id`, skipping anything else.
fn response(c: &Client, id: u64) -> Value {
    loop {
        let line = c.next_line().expect("eof");
        let v: Value = serde_json::from_str(&line).unwrap();
        if v["id"] == id {
            return v;
        }
    }
}

fn request(c: &mut Client, id: u64, method: &str, params: Value) -> Value {
    send(c, json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
    response(c, id)
}

fn meta() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": {"name": "test", "version": "0"},
        "io.modelcontextprotocol/clientCapabilities": {}
    })
}

#[test]
fn legacy_initialize_list_and_call() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = start(dir.path());
    let init = request(
        &mut c,
        1,
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}),
    );
    let result = &init["result"];
    assert_eq!(result["protocolVersion"], "2025-06-18", "{init}");
    assert_eq!(result["serverInfo"]["name"], "rueca");
    assert_eq!(result["serverInfo"]["version"], env!("CARGO_PKG_VERSION"));
    assert!(result["instructions"].as_str().unwrap().contains("search_symbols"));
    assert!(result["capabilities"]["tools"].is_object());
    send(&mut c, json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));

    let list = request(&mut c, 2, "tools/list", json!({}));
    let tools = list["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, rueca_query::TOOL_NAMES);
    for t in tools {
        assert_eq!(t["inputSchema"]["type"], "object");
        assert!(t["description"].as_str().unwrap().len() > 40);
        assert_eq!(t["annotations"]["readOnlyHint"], true);
    }

    // Argument errors are tool errors, visible to the model.
    let call = request(&mut c, 3, "tools/call", json!({"name": "get_symbol", "arguments": {}}));
    assert_eq!(call["result"]["isError"], true, "{call}");
    assert!(call["result"]["content"][0]["text"].as_str().unwrap().contains("is required"));

    // An empty store is reported clearly.
    let call = request(&mut c, 4, "tools/call", json!({"name": "search_symbols", "arguments": {"query": "x"}}));
    assert_eq!(call["result"]["isError"], true, "{call}");
    assert!(call["result"]["content"][0]["text"].as_str().unwrap().contains("no repositories are indexed"), "{call}");

    // Unknown tool: protocol error.
    let call = request(&mut c, 5, "tools/call", json!({"name": "nope", "arguments": {}}));
    assert!(call["error"].is_object(), "{call}");

    let (outcome, rest) = c.close();
    outcome.unwrap();
    assert!(rest.is_empty(), "{rest:?}");
}

#[test]
fn stateless_discover_and_list() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = start(dir.path());
    let disc = request(&mut c, 1, "server/discover", json!({"_meta": meta()}));
    let result = &disc["result"];
    let versions: Vec<&str> =
        result["supportedVersions"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    assert!(versions.contains(&"2026-07-28") && versions.contains(&"2024-11-05"), "{disc}");
    let list = request(&mut c, 2, "tools/list", json!({"_meta": meta()}));
    assert_eq!(list["result"]["tools"].as_array().unwrap().len(), rueca_query::TOOL_NAMES.len(), "{list}");
    let call = request(&mut c, 3, "tools/call", json!({"_meta": meta(), "name": "trace", "arguments": {"bogus": 1}}));
    assert_eq!(call["result"]["isError"], true, "{call}");
    assert!(call["result"]["content"][0]["text"].as_str().unwrap().contains("unknown argument `bogus`"));
    let (outcome, _) = c.close();
    outcome.unwrap();
}

#[test]
fn eof_before_any_message_is_a_clean_exit() {
    let dir = tempfile::tempdir().unwrap();
    let (outcome, rest) = start(dir.path()).close();
    outcome.unwrap();
    assert!(rest.is_empty());
}
