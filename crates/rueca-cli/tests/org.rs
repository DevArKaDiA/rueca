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

fn git(dir: &std::path::Path, args: &[&str]) -> bool {
    StdCommand::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false"])
        .args(args)
        .output()
        .is_ok_and(|o| o.status.success())
}

/// A git repository on `main` with branch `branch` adding `extra`; `main`
/// checked out. `false` without git.
fn repo_with_branch(root: &std::path::Path, file: (&str, &str), branch: &str, extra: (&str, &str)) -> bool {
    std::fs::create_dir_all(root).unwrap();
    if !git(root, &["init", "-q"]) {
        return false;
    }
    assert!(git(root, &["symbolic-ref", "HEAD", "refs/heads/main"]));
    write(root, file.0, file.1);
    assert!(git(root, &["add", "-A"]) && git(root, &["commit", "-q", "-m", "main"]));
    assert!(git(root, &["checkout", "-q", "-b", branch]));
    write(root, extra.0, extra.1);
    assert!(git(root, &["add", "-A"]) && git(root, &["commit", "-q", "-m", branch]));
    assert!(git(root, &["checkout", "-q", "main"]));
    true
}

/// Refs over an organization from the command line (RFC 0006 §9.3): the
/// file's `refs`, `index --org FILE PATH --ref NAME [--remove]`, a plain
/// `index PATH --ref NAME` propagated, `status` and `links --ref`.
#[test]
fn refs_of_an_organization() {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let store = base.join("store");
    let (a, b) = (base.join("alpha"), base.join("beta"));
    if !repo_with_branch(
        &a,
        ("alpha/core.py", "def alpha_core():\n    return 1\n"),
        "feature/x",
        ("alpha/x.py", "def x():\n    return 2\n"),
    ) {
        eprintln!("git not available; skipping");
        return;
    }
    assert!(repo_with_branch(
        &b,
        ("beta/core.py", "def beta_core():\n    return 1\n"),
        "next",
        ("beta/n.py", "def n():\n    return 3\n")
    ));
    let file = base.join("rueca-org.toml");
    std::fs::write(
        &file,
        "name = \"duo\"\n[[repo]]\npath = \"alpha\"\nrefs = [\"feature/x\"]\n[[repo]]\npath = \"beta\"\n",
    )
    .unwrap();

    let v = json_out(&rueca(&store).args(["index", "--org"]).arg(&file).arg("--json").output().unwrap());
    assert_eq!(v["update"]["refs"], 1);
    assert_eq!(v["update"]["refs_added"], json!(["local/alpha@feature/x"]));
    assert_eq!(v["refs"][0]["ref"], "feature/x");
    assert!(v["update"]["costs"].as_array().unwrap().iter().any(|c| c["entry"] == "local/alpha@feature/x"), "{v}");
    let text = String::from_utf8(rueca(&store).args(["index", "--org"]).arg(&file).output().unwrap().stdout).unwrap();
    assert!(text.contains("indexed ref feature/x of local/alpha") && text.contains("refs:       1"), "{text}");

    // One ref of one member through the file.
    let v = json_out(
        &rueca(&store).args(["index", "--org"]).arg(&file).arg(&b).args(["--ref", "next", "--json"]).output().unwrap(),
    );
    assert_eq!(v["repo"], "local/beta");
    assert_eq!(v["indexed"]["ref"], "next");
    assert_eq!(v["update"]["refs_added"], json!(["local/beta@next"]));
    assert_eq!(v["update"]["unchanged"], 2);
    // A plain `index PATH --ref NAME` updates the organizations holding it.
    let v = json_out(&rueca(&store).arg("index").arg(&a).args(["--ref", "HEAD", "--json"]).output().unwrap());
    let pinned = v["ref"].as_str().unwrap().to_string();
    assert_eq!(v["orgs"][0]["refs_added"], json!([format!("local/alpha@{pinned}")]));

    // `status` lists every repository's refs.
    let v = json_out(&rueca(&store).args(["status", "--json"]).output().unwrap());
    let repos = v["orgs"][0]["repos"].as_array().unwrap();
    let refs: Vec<&str> = repos[0]["refs"].as_array().unwrap().iter().map(|r| r["ref"].as_str().unwrap()).collect();
    assert_eq!((repos[0]["ref"].as_str(), refs.len()), (Some("main"), 2), "{v}");
    assert!(refs.contains(&"feature/x") && refs.contains(&pinned.as_str()));
    let text = String::from_utf8(rueca(&store).arg("status").output().unwrap().stdout).unwrap();
    assert!(text.contains("2 repositories + 3 refs"), "{text}");
    assert!(text.lines().any(|l| l.contains("local/beta  main@") && l.contains(" + next@")), "{text}");

    // Queries read refs through the organization: `list_repositories` lists
    // them with their commits, a `ref` list compares two refs of a member.
    let query = |tool: &str, args: &str| {
        let out = rueca(&store).args(["query", tool, args, "--org", "duo", "--text"]).output().unwrap();
        assert!(out.status.success(), "{out:?}");
        String::from_utf8(out.stdout).unwrap()
    };
    let text = query("list_repositories", "{}");
    assert!(text.lines().any(|l| l.contains("local/alpha  refs main@") && l.contains(", feature/x@")), "{text}");
    let text =
        query("search_symbols", r#"{"query": "alpha core x", "repo": "local/alpha", "ref": ["main", "feature/x"]}"#);
    assert!(text.contains("(main)\n[1] local/alpha@") && text.contains(" (feature/x)\n"), "{text}");
    assert!(text.lines().any(|l| l.contains("Function alpha_core") && l.ends_with(" [0,1]")), "{text}");
    assert!(text.lines().any(|l| l.contains("Function x L1-2") && l.ends_with(" changed")), "{text}");
    let text = query("search_symbols", r#"{"query": "n", "ref": "next"}"#);
    assert!(text.contains("local/beta@") && text.contains(" (next)\n"), "{text}");

    // `links --ref`: an empty link set prints its count; an unknown ref
    // lists the indexed ones.
    let out = rueca(&store).args(["links", "--org", "duo", "--repo", "local/beta", "--ref", "next"]).output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("0 cross-repository edges"), "{out:?}");
    let out = rueca(&store).args(["links", "--org", "duo", "--repo", "local/beta", "--ref", "nope"]).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("indexed refs: main@"), "{out:?}");
    let out = rueca(&store).args(["links", "--org", "duo", "--ref", "next"]).output().unwrap();
    assert!(!out.status.success(), "--ref needs --repo");

    // `--remove` through the file.
    let v = json_out(
        &rueca(&store)
            .args(["index", "--org"])
            .arg(&file)
            .arg(&b)
            .args(["--ref", "next", "--remove", "--json"])
            .output()
            .unwrap(),
    );
    assert_eq!(v["update"]["refs_removed"], json!(["local/beta@next"]));
    assert_eq!(v["removed"]["ref"], "next");
}
