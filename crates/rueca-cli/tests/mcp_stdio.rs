//! `rueca mcp` as a child process speaking newline-delimited JSON-RPC on
//! stdio: legacy `initialize` handshake, the 2026-07-28 stateless flow,
//! automatic indexing of `--root`, and a clean exit when stdin closes.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::Duration;

use serde_json::{Value, json};

use common::*;

const TIMEOUT: Duration = Duration::from_secs(60);

struct Server {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
}

impl Server {
    fn spawn(p: &Project, extra: &[&str]) -> Server {
        let mut cmd = std::process::Command::new(assert_cmd::cargo::cargo_bin("rueca"));
        cmd.arg("--store")
            .arg(&p.store)
            .arg("--root")
            .arg(&p.root)
            .arg("mcp")
            .args(extra)
            .env_remove("RUECA_LOG")
            .env_remove("RUECA_HOME")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = cmd.spawn().unwrap();
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Server { child, stdin, lines: rx }
    }

    fn send(&mut self, msg: Value) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{msg}").unwrap();
        stdin.flush().unwrap();
    }

    /// The response with `id`. Every stdout line must be a JSON-RPC message.
    fn response(&mut self, id: u64) -> Value {
        loop {
            let line = match self.lines.recv_timeout(TIMEOUT) {
                Ok(l) => l,
                Err(RecvTimeoutError::Timeout) => panic!("no response to request {id}"),
                Err(RecvTimeoutError::Disconnected) => panic!("server exited before answering {id}"),
            };
            let v: Value = serde_json::from_str(&line).unwrap_or_else(|e| panic!("non-JSON on stdout ({e}): {line}"));
            assert_eq!(v["jsonrpc"], "2.0", "{line}");
            if v["id"] == id {
                return v;
            }
        }
    }

    fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        self.response(id)
    }

    fn initialize(&mut self, version: &str) -> Value {
        let init = self.request(
            0,
            "initialize",
            json!({"protocolVersion": version, "capabilities": {}, "clientInfo": {"name": "cli-test", "version": "0"}}),
        );
        self.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        init
    }

    /// Closes stdin and waits for a clean exit.
    fn shutdown(mut self) {
        drop(self.stdin.take());
        for _ in 0..200 {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "rueca mcp exited with {status}");
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        panic!("rueca mcp did not exit after stdin closed");
    }
}

/// Structured result of a successful `tools/call`, checked against its text content.
fn structured(resp: &Value) -> Value {
    let result = &resp["result"];
    assert_eq!(result["isError"], false, "{resp}");
    let text = result["content"][0]["text"].as_str().unwrap_or_else(|| panic!("{resp}"));
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed, result["structuredContent"], "text and structuredContent differ");
    parsed
}

#[test]
fn legacy_handshake_list_and_call() {
    let p = python_project();
    index(&p);
    let mut s = Server::spawn(&p, &[]);
    let init = s.initialize("2025-06-18");
    let r = &init["result"];
    assert_eq!(r["protocolVersion"], "2025-06-18", "{init}");
    assert_eq!(r["serverInfo"]["name"], "rueca");
    assert_eq!(r["serverInfo"]["version"], env!("CARGO_PKG_VERSION"));
    let instructions = r["instructions"].as_str().unwrap();
    assert!(instructions.contains("search_symbols") && instructions.contains("trace"));

    let list = s.request(1, "tools/list", json!({}));
    let tools = list["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        [
            "search_symbols",
            "search_code",
            "get_symbol",
            "get_snippet",
            "trace",
            "get_file_outline",
            "list_repositories"
        ]
    );
    let trace_tool = tools.iter().find(|t| t["name"] == "trace").unwrap();
    assert_eq!(trace_tool["inputSchema"]["additionalProperties"], false);
    assert!(trace_tool["inputSchema"]["properties"]["min_confidence"].is_object());

    let call = s.request(
        2,
        "tools/call",
        json!({"name": "get_symbol", "arguments": {"qualified_name": "InvoiceService.issue", "format": "verbose"}}),
    );
    let v = structured(&call);
    assert_eq!(v["symbol"]["id"], "local/proj@worktree:pkg/service.py#InvoiceService.issue");
    assert_eq!(v["callees_count"], 2);

    let call = s.request(
        3,
        "tools/call",
        json!({"name": "trace", "arguments": {"name": "validate", "direction": "callers", "format": "verbose"}}),
    );
    let v = structured(&call);
    let callers = qns(&v["nodes"]);
    assert!(callers.contains(&"pkg.service.InvoiceService.issue".to_string()), "{v}");

    let call = s.request(
        4,
        "tools/call",
        json!({"name": "get_snippet", "arguments": {"name": "validate", "format": "verbose"}}),
    );
    assert_eq!(structured(&call)["text"], "def validate(obj):\n    return obj is not None");

    // Tool errors are results with isError, not protocol errors.
    let call = s.request(5, "tools/call", json!({"name": "get_file_outline", "arguments": {"path": "nope.py"}}));
    assert_eq!(call["result"]["isError"], true, "{call}");
    assert!(call["result"]["content"][0]["text"].as_str().unwrap().contains("file not indexed"));
    let call = s.request(6, "tools/call", json!({"name": "search_code", "arguments": {"pattern": "x", "limt": 3}}));
    assert_eq!(call["result"]["isError"], true, "{call}");
    assert!(call["result"]["content"][0]["text"].as_str().unwrap().contains("unknown argument `limt`"));

    // Default format: the envelope as structuredContent, the compact text rendering as text.
    let call = s.request(
        7,
        "tools/call",
        json!({"name": "get_symbol", "arguments": {"qualified_name": "InvoiceService.issue"}}),
    );
    let result = &call["result"];
    assert_eq!(result["isError"], false, "{call}");
    let structured = &result["structuredContent"];
    assert_eq!(structured["snapshots"][0]["repo"], "local/proj");
    assert_eq!(structured["results"]["symbol"]["id"], "local/proj@worktree:pkg/service.py#InvoiceService.issue");
    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(
        text.starts_with("local/proj@worktree (worktree)\nMethod InvoiceService.issue  pkg/service.py L8-11  python"),
        "{text}"
    );
    assert!(text.contains("callers 1, callees 2"), "{text}");
    s.shutdown();
}

#[test]
fn version_negotiation_falls_back_to_newest_handshake() {
    let p = python_project();
    index(&p);
    for (asked, expected) in [("2024-11-05", "2024-11-05"), ("2026-07-28", "2025-11-25"), ("2099-01-01", "2025-11-25")]
    {
        let mut s = Server::spawn(&p, &["--no-auto-index"]);
        let init = s.initialize(asked);
        assert_eq!(init["result"]["protocolVersion"], expected, "asked {asked}: {init}");
        s.shutdown();
    }
}

#[test]
fn stateless_flow_with_auto_index() {
    // Not indexed beforehand: `rueca mcp --root` indexes it in the background.
    let p = python_project();
    let mut s = Server::spawn(&p, &[]);
    let meta = json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": {"name": "cli-test", "version": "0"},
        "io.modelcontextprotocol/clientCapabilities": {}
    });
    let disc = s.request(1, "server/discover", json!({"_meta": meta}));
    let versions: Vec<&str> =
        disc["result"]["supportedVersions"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(versions, ["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25", "2026-07-28"], "{disc}");
    // 2026-07-28 carries server info in `_meta`.
    assert_eq!(disc["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"], "rueca", "{disc}");
    assert!(disc["result"]["instructions"].as_str().unwrap().contains("Recommended flow"));

    let list = s.request(2, "tools/list", json!({"_meta": meta}));
    assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 7, "{list}");
    let call = s.request(
        3,
        "tools/call",
        json!({"_meta": meta, "name": "search_symbols", "arguments": {"query": "validate", "format": "verbose"}}),
    );
    let v = structured(&call);
    assert_eq!(v["results"][0]["qualified_name"], "pkg.models.validate", "{v}");
    s.shutdown();

    // The index persisted in the store.
    let out = rueca(&p.store).args(["status", "--json"]).output().unwrap();
    assert_eq!(stdout_json(&out)["roots"][0]["root"], p.root.to_str().unwrap());
}

#[test]
fn no_auto_index_reports_missing_root() {
    let p = python_project();
    let mut s = Server::spawn(&p, &["--no-auto-index"]);
    s.initialize("2025-11-25");
    let call = s.request(1, "tools/call", json!({"name": "search_symbols", "arguments": {"query": "x"}}));
    assert_eq!(call["result"]["isError"], true, "{call}");
    assert!(call["result"]["content"][0]["text"].as_str().unwrap().contains("no repositories are indexed"), "{call}");
    s.shutdown();
}
