//! CLI surface: help, version, and `query` error handling (exit code 2 with
//! `{"error": ...}` on stdout).

use assert_cmd::Command;
use serde_json::Value;

fn rueca(store: &std::path::Path) -> Command {
    let mut c = Command::cargo_bin("rueca").unwrap();
    c.arg("--store").arg(store).env_remove("RUECA_LOG");
    c
}

fn stdout_json(out: &std::process::Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)))
}

#[test]
fn help_lists_commands() {
    let out = Command::cargo_bin("rueca").unwrap().arg("--help").output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    for cmd in ["index", "query", "mcp", "status", "--store", "--root"] {
        assert!(text.contains(cmd), "missing {cmd} in:\n{text}");
    }
    let out = Command::cargo_bin("rueca").unwrap().args(["query", "--help"]).output().unwrap();
    assert!(String::from_utf8(out.stdout).unwrap().contains("get_file_outline"));
}

#[test]
fn version() {
    let out = Command::cargo_bin("rueca").unwrap().arg("--version").output().unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8(out.stdout).unwrap().trim(), format!("rueca {}", env!("CARGO_PKG_VERSION")));
}

#[test]
fn query_errors_exit_2_with_json() {
    let dir = tempfile::tempdir().unwrap();
    let cases: &[(&[&str], &str)] = &[
        (&["query", "nope", "{}"], "unknown tool `nope`"),
        (&["query", "search_symbols", "{not json"], "not valid JSON"),
        (&["query", "search_symbols", "{}"], "missing required argument `query`"),
        (&["query", "search_symbols", "[1]"], "must be a JSON object"),
        (&["query", "trace", r#"{"name": "x", "depth": "deep"}"#], "invalid argument `depth`"),
        (&["query", "search_symbols", r#"{"query": "x"}"#], "no repositories are indexed"),
    ];
    for (args, needle) in cases {
        let out = rueca(dir.path()).current_dir(dir.path()).args(*args).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        let v = stdout_json(&out);
        let msg = v["error"].as_str().unwrap_or_else(|| panic!("{v}"));
        assert!(msg.contains(needle), "{args:?}: {msg}");
        assert_eq!(v.as_object().unwrap().len(), 1);
    }
}

#[test]
fn status_on_empty_store() {
    let dir = tempfile::tempdir().unwrap();
    let out = rueca(dir.path()).args(["status", "--json"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(stdout_json(&out)["roots"], serde_json::json!([]));
    let out = rueca(dir.path()).arg("status").output().unwrap();
    assert!(String::from_utf8(out.stdout).unwrap().contains("no indexed roots"));
}
