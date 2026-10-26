//! `--org` end to end (RFC 0006 §7): `index --org`, `query --org` (file or
//! name), one member re-indexed, `status` listing organizations, and the MCP
//! server over an organization.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::process::{Command as StdCommand, Stdio};

use assert_cmd::Command;
use serde_json::{Value, json};

use common::write;

fn rueca(store: &std::path::Path) -> Command {
    let mut c = Command::cargo_bin("rueca").unwrap();
    c.arg("--store").arg(store).env_remove("RUECA_LOG");
    c
}

fn json_out(out: &std::process::Output) -> Value {
    assert!(out.status.success(), "{}\n{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)))
}

#[test]
fn index_query_and_status_of_an_organization() {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let store = base.join("store");
    write(&base.join("billing"), "billing/invoice.py", "def issue_invoice(order):\n    return order.total\n");
    write(&base.join("payments"), "payments/charge.py", "def charge_card(card, amount):\n    return card\n");
    let file = base.join("rueca-org.toml");
    std::fs::write(&file, "name = \"acme\"\n[[repo]]\npath = \"billing\"\n[[repo]]\npath = \"payments\"\n").unwrap();

    let v = json_out(&rueca(&store).args(["index", "--org"]).arg(&file).arg("--json").output().unwrap());
    assert_eq!(v["update"]["generation"], 1);
    assert_eq!(v["indexed"].as_array().unwrap().len(), 2);

    // `repo` omitted: the whole organization; by file or by name.
    for org in [file.to_string_lossy().into_owned(), "acme".to_string()] {
        let v = json_out(
            &rueca(&store)
                .args(["query", "search_symbols", r#"{"query": "charge card"}"#, "--org", &org])
                .output()
                .unwrap(),
        );
        assert_eq!(v["snapshots"].as_array().unwrap().len(), 1, "{v}");
        assert_eq!(v["snapshots"][0]["repo"], "local/payments");
    }
    let v = json_out(&rueca(&store).args(["query", "list_repositories", "{}", "--org", "acme"]).output().unwrap());
    assert_eq!(v["org"], "acme");
    assert_eq!(v["results"].as_array().unwrap().len(), 2);

    // One member re-indexed through the organization file …
    write(&base.join("payments"), "payments/refund.py", "def refund_card(card):\n    return card\n");
    let v = json_out(
        &rueca(&store).args(["index", "--org"]).arg(&file).arg(base.join("payments")).arg("--json").output().unwrap(),
    );
    assert_eq!(v["indexed"].as_array().unwrap().len(), 1);
    assert_eq!(v["update"]["replaced"], json!(["local/payments"]));
    assert_eq!(v["update"]["unchanged"], 1);
    // … or directly: the organizations holding it are updated.
    write(&base.join("billing"), "billing/tax.py", "def compute_tax(order):\n    return 0\n");
    let v = json_out(&rueca(&store).arg("index").arg(base.join("billing")).arg("--json").output().unwrap());
    assert_eq!(v["orgs"][0]["generation"], 3);
    assert_eq!(v["orgs"][0]["replaced"], json!(["local/billing"]));
    let v = json_out(
        &rueca(&store)
            .args(["query", "search_symbols", r#"{"query": "compute_tax"}"#, "--org", "acme"])
            .output()
            .unwrap(),
    );
    assert_eq!(v["results"][0]["symbols"][0]["qn"], "billing.tax.compute_tax", "{v}");

    let v = json_out(&rueca(&store).args(["status", "--json"]).output().unwrap());
    assert_eq!(v["orgs"][0]["name"], "acme");
    assert_eq!(v["orgs"][0]["generation"], 3);
    assert_eq!(v["orgs"][0]["repos"].as_array().unwrap().len(), 2);
    let out = rueca(&store).arg("status").output().unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("organization acme: generation 3, 2 repositories"), "{text}");

    // Errors: a name for `index --org`, an unknown organization, --repo.
    let out = rueca(&store).args(["index", "--org", "acme"]).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("needs the organization file"));
    let out = rueca(&store).args(["query", "search_symbols", r#"{"query": "x"}"#, "--org", "nope"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stdout).contains("organization `nope` is not built"));
    let out = rueca(&store).args(["index", "--repo", "x", "--org"]).arg(&file).output().unwrap();
    assert!(String::from_utf8_lossy(&out.stderr).contains("cannot be combined"));
}

/// `rueca mcp --org FILE` builds the organization when the store has none
/// and serves it.
#[test]
fn mcp_serves_an_organization() {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let store = base.join("store");
    write(&base.join("a"), "a.py", "def alpha_handler():\n    return 1\n");
    write(&base.join("b"), "b.py", "def beta_handler():\n    return 2\n");
    let file = base.join("org.toml");
    std::fs::write(&file, "name = \"two\"\n[[repo]]\npath = \"a\"\n[[repo]]\npath = \"b\"\n").unwrap();
    let mut child = StdCommand::new(assert_cmd::cargo::cargo_bin("rueca"))
        .arg("--store")
        .arg(&store)
        .arg("mcp")
        .arg("--org")
        .arg(&file)
        .env_remove("RUECA_LOG")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut send = |v: Value| {
        writeln!(stdin, "{v}").unwrap();
        stdin.flush().unwrap();
    };
    let mut recv = || {
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        serde_json::from_str::<Value>(&line).unwrap()
    };
    send(json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}}));
    let _ = recv();
    send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    send(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
        "name": "search_symbols", "arguments": {"query": "handler"}}}));
    let r = recv();
    let sc = &r["result"]["structuredContent"];
    let repos: Vec<&str> = sc["snapshots"].as_array().unwrap().iter().map(|s| s["repo"].as_str().unwrap()).collect();
    assert_eq!(repos, ["local/a", "local/b"], "{r}");
    drop(send);
    let _ = child.kill();
    let _ = child.wait();
}
