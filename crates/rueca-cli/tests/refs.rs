//! `index --ref` end to end (RFC 0006 §9.3): a branch indexed as a snapshot
//! of its own, listed by `status` under its repository, read with `ref` by
//! `query`, and removed with `--remove`. Skips where git is missing.

mod common;

use std::path::Path;

use serde_json::Value;

use common::{rueca, stdout_json, write};

fn git(dir: &Path, args: &[&str]) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false"])
        .args(args)
        .output()
        .is_ok_and(|o| o.status.success())
}

fn commit_all(dir: &Path, msg: &str) {
    assert!(git(dir, &["add", "-A"]));
    assert!(git(dir, &["commit", "-q", "-m", msg]));
}

fn ok_json(out: &std::process::Output) -> Value {
    assert!(out.status.success(), "{}\n{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    stdout_json(out)
}

#[test]
fn index_a_branch_query_it_and_remove_it() {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let (root, store) = (base.join("shop"), base.join("store"));
    std::fs::create_dir_all(&root).unwrap();
    if !git(&root, &["init", "-q"]) {
        eprintln!("git not available; skipping");
        return;
    }
    assert!(git(&root, &["symbolic-ref", "HEAD", "refs/heads/main"]));
    write(&root, "shop/orders.py", "def place_order(cart):\n    return cart\n");
    commit_all(&root, "main");
    assert!(git(&root, &["checkout", "-q", "-b", "feature/x"]));
    write(&root, "shop/refunds.py", "def refund_only_on_branch(order):\n    return order\n");
    commit_all(&root, "feature");
    assert!(git(&root, &["checkout", "-q", "main"]));

    ok_json(&rueca(&store).arg("index").arg(&root).arg("--json").output().unwrap());
    let v = ok_json(&rueca(&store).arg("index").arg(&root).args(["--ref", "feature/x", "--json"]).output().unwrap());
    assert_eq!(
        (v["ref"].as_str(), v["kind"].as_str(), v["unchanged"].as_bool()),
        (Some("feature/x"), Some("branch"), Some(false))
    );
    assert_eq!(v["commit"].as_str().unwrap().len(), 40);
    assert_eq!(v["repo"], "local/shop");
    for k in ["facts_cache_hits", "files_extracted", "duration_ms", "index_ms", "export_ms", "resolve_ms"] {
        assert!(v[k].is_u64(), "{k}: {v}");
    }
    assert!(v["facts_cache_hits"].as_u64().unwrap() >= 1, "orders.py comes from the facts cache: {v}");
    let again =
        ok_json(&rueca(&store).arg("index").arg(&root).args(["--ref", "feature/x", "--json"]).output().unwrap());
    assert_eq!(again["unchanged"], true);

    // `status` lists the ref under its repository.
    let st = ok_json(&rueca(&store).args(["status", "--json"]).output().unwrap());
    let roots = st["roots"].as_array().unwrap();
    assert_eq!(roots.len(), 2);
    let r = roots.iter().find(|r| r["ref_snapshot"] == true).unwrap();
    assert_eq!(
        (r["ref"].as_str(), r["repo"].as_str(), r["dirty"].as_bool()),
        (Some("feature/x"), Some("local/shop"), Some(false))
    );
    let text = rueca(&store).arg("status").output().unwrap();
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(text.contains("  ref feature/x @") && text.contains("(export)"), "{text}");
    assert!(text.lines().nth(1).unwrap().contains(" main "), "the working tree row shows its branch: {text}");

    // `ref` reads the branch's snapshot; the default stays the working tree.
    let query = |args: &str| {
        rueca(&store).arg("--root").arg(&root).args(["query", "search_symbols", args, "--text"]).output().unwrap()
    };
    let out = query(r#"{"query": "refund only on branch", "ref": "feature/x"}"#);
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(out.status.success() && text.contains("refund_only_on_branch") && text.contains("(feature/x)"), "{text}");
    let text = String::from_utf8(query(r#"{"query": "refund only on branch"}"#).stdout).unwrap();
    assert!(!text.contains("refund_only_on_branch"), "{text}");
    // A list of refs compares them: identical symbols once, in both
    // snapshots (`[0,1]`); the branch's own symbol `changed`.
    let out = query(r#"{"query": "place order refund", "ref": ["main", "feature/x"]}"#);
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(out.status.success() && text.starts_with("[0] local/shop@"), "{text}");
    assert!(text.lines().any(|l| l.contains("Function place_order") && l.ends_with(" [0,1]")), "{text}");
    assert!(text.lines().any(|l| l.contains("Function refund_only_on_branch") && l.ends_with(" changed")), "{text}");
    assert_eq!(text.matches("Function place_order").count(), 1, "{text}");
    // `list_repositories` lists every snapshot of the repository.
    let out =
        rueca(&store).arg("--root").arg(&root).args(["query", "list_repositories", "{}", "--text"]).output().unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("local/shop  refs main@") && text.contains(", feature/x@"), "{text}");
    let out = query(r#"{"query": "refund", "ref": "nope"}"#);
    assert_eq!(out.status.code(), Some(2));
    let err = stdout_json(&out)["error"].as_str().unwrap().to_string();
    assert!(
        err.contains("ref `nope` is not indexed") && err.contains("`feature/x`") && err.contains("`main`"),
        "{err}"
    );

    // `--remove` drops it.
    let gone = ok_json(
        &rueca(&store).arg("index").arg(&root).args(["--ref", "feature/x", "--remove", "--json"]).output().unwrap(),
    );
    assert_eq!(gone["export_removed"], true);
    let st = ok_json(&rueca(&store).args(["status", "--json"]).output().unwrap());
    assert_eq!(st["roots"].as_array().unwrap().len(), 1);
    let out = rueca(&store).args(["index", "--remove"]).arg(&root).output().unwrap();
    assert!(!out.status.success(), "--remove needs --ref");
}
