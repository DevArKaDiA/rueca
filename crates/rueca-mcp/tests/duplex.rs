//! The MCP server over an in-memory pipe, speaking raw JSON-RPC lines: the
//! legacy `initialize` handshake and the 2026-07-28 stateless flow.

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines, ReadHalf, WriteHalf};

struct Client {
    lines: Lines<BufReader<ReadHalf<DuplexStream>>>,
    writer: WriteHalf<DuplexStream>,
}

impl Client {
    async fn start(store: &std::path::Path) -> Client {
        let (server_io, client_io) = tokio::io::duplex(1 << 20);
        let engine = rueca_query::Engine::new(store.to_path_buf(), None).unwrap();
        let server = rueca_mcp::RuecaServer::new(engine);
        tokio::spawn(async move {
            let _ = rueca_mcp::serve(server, server_io).await;
        });
        let (r, w) = tokio::io::split(client_io);
        Client { lines: BufReader::new(r).lines(), writer: w }
    }

    async fn send(&mut self, msg: Value) {
        let mut line = msg.to_string();
        line.push('\n');
        self.writer.write_all(line.as_bytes()).await.unwrap();
    }

    async fn response(&mut self, id: u64) -> Value {
        loop {
            let line = tokio::time::timeout(std::time::Duration::from_secs(10), self.lines.next_line())
                .await
                .expect("timeout")
                .unwrap()
                .expect("eof");
            let v: Value = serde_json::from_str(&line).unwrap();
            if v["id"] == id {
                return v;
            }
        }
    }

    async fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})).await;
        self.response(id).await
    }
}

fn meta() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": {"name": "test", "version": "0"},
        "io.modelcontextprotocol/clientCapabilities": {}
    })
}

#[tokio::test]
async fn legacy_initialize_list_and_call() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = Client::start(dir.path()).await;
    let init = c
        .request(
            1,
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}),
        )
        .await;
    let result = &init["result"];
    assert_eq!(result["protocolVersion"], "2025-06-18", "{init}");
    assert_eq!(result["serverInfo"]["name"], "rueca");
    assert_eq!(result["serverInfo"]["version"], env!("CARGO_PKG_VERSION"));
    assert!(result["instructions"].as_str().unwrap().contains("search_symbols"));
    assert!(result["capabilities"]["tools"].is_object());
    c.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).await;

    let list = c.request(2, "tools/list", json!({})).await;
    let tools = list["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, rueca_query::TOOL_NAMES);
    for t in tools {
        assert_eq!(t["inputSchema"]["type"], "object");
        assert!(t["description"].as_str().unwrap().len() > 40);
        assert_eq!(t["annotations"]["readOnlyHint"], true);
    }

    // Argument errors are tool errors, visible to the model.
    let call = c.request(3, "tools/call", json!({"name": "get_symbol", "arguments": {}})).await;
    assert_eq!(call["result"]["isError"], true, "{call}");
    assert!(call["result"]["content"][0]["text"].as_str().unwrap().contains("is required"));

    // An empty store is reported clearly.
    let call = c.request(4, "tools/call", json!({"name": "search_symbols", "arguments": {"query": "x"}})).await;
    assert_eq!(call["result"]["isError"], true, "{call}");
    assert!(call["result"]["content"][0]["text"].as_str().unwrap().contains("no repositories are indexed"), "{call}");

    // Unknown tool: protocol error.
    let call = c.request(5, "tools/call", json!({"name": "nope", "arguments": {}})).await;
    assert!(call["error"].is_object(), "{call}");
}

#[tokio::test]
async fn stateless_discover_and_list() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = Client::start(dir.path()).await;
    let disc = c.request(1, "server/discover", json!({"_meta": meta()})).await;
    let result = &disc["result"];
    let versions: Vec<&str> =
        result["supportedVersions"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    assert!(versions.contains(&"2026-07-28") && versions.contains(&"2024-11-05"), "{disc}");
    let list = c.request(2, "tools/list", json!({"_meta": meta()})).await;
    assert_eq!(list["result"]["tools"].as_array().unwrap().len(), rueca_query::TOOL_NAMES.len(), "{list}");
    let call = c.request(3, "tools/call", json!({"_meta": meta(), "name": "trace", "arguments": {"bogus": 1}})).await;
    assert_eq!(call["result"]["isError"], true, "{call}");
    assert!(call["result"]["content"][0]["text"].as_str().unwrap().contains("unknown argument `bogus`"));
}
