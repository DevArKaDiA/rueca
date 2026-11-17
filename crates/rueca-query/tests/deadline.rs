//! `Engine::respond_with` in local mode: the cooperative deadline (RFC 0008
//! §6.5) and the caller's visibility over indexed roots.

mod common;

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use rueca_query::{DeadlineExceeded, Engine, RespondOptions, Visibility};
use serde_json::{Value, json};

use common::*;

const OTHER: &str = "github.com/acme/other";

/// One call of every tool against the demo snapshot.
fn every_tool() -> Vec<(&'static str, Value)> {
    vec![
        ("search_symbols", json!({"query": "issue invoice"})),
        ("search_code", json!({"pattern": "def"})),
        ("get_symbol", json!({"name": "helper"})),
        ("get_snippet", json!({"name": "helper"})),
        ("trace", json!({"name": "issue", "depth": 3})),
        ("get_file_outline", json!({"path": "pkg/service.py"})),
        ("list_repositories", json!({})),
        ("list_contracts", json!({})),
    ]
}

fn with_deadline(at: Instant) -> RespondOptions {
    RespondOptions { deadline: Some(at), visibility: None }
}

fn only(repo: &str) -> RespondOptions {
    RespondOptions { deadline: None, visibility: Some(Visibility::Repos(BTreeSet::from([repo.to_string()]))) }
}

#[test]
fn every_tool_fails_fast_past_its_deadline() {
    let f = fixture();
    let e = Engine::new(f.store.clone(), Some(f.root.clone())).unwrap();
    for (tool, args) in every_tool() {
        let started = Instant::now();
        let err = e.respond_with(tool, args, with_deadline(Instant::now())).unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(1), "{tool} took {:?}", started.elapsed());
        assert_eq!(err.downcast_ref::<DeadlineExceeded>(), Some(&DeadlineExceeded { budget: Duration::ZERO }));
        assert_eq!(err.to_string(), "deadline of 0ms exceeded; narrow `repo` or lower `limit`", "{tool}");
    }
    // Invalid arguments are still reported as such.
    let err = e.respond_with("search_symbols", json!({}), with_deadline(Instant::now())).unwrap_err();
    assert!(err.to_string().contains("query"), "{err}");
}

#[test]
fn a_generous_deadline_changes_nothing() {
    let f = fixture();
    let e = Engine::new(f.store.clone(), Some(f.root.clone())).unwrap();
    let later = Instant::now() + Duration::from_secs(600);
    for (tool, args) in every_tool() {
        let plain = e.respond(tool, args.clone()).unwrap();
        let timed = e.respond_with(tool, args.clone(), with_deadline(later)).unwrap();
        assert_eq!(timed.text, plain.text, "{tool}");
        assert_eq!(timed.json.to_string(), plain.json.to_string(), "{tool}");
        let all = RespondOptions { deadline: Some(later), visibility: Some(Visibility::All) };
        assert_eq!(e.respond_with(tool, args, all).unwrap(), plain, "{tool}");
    }
}

/// The demo store plus a second root of repository [`OTHER`].
fn two_roots(f: &Fixture) -> PathBuf {
    let other_root = f.store.parent().unwrap().join("other-root");
    std::fs::create_dir_all(&other_root).unwrap();
    let other_root = std::fs::canonicalize(other_root).unwrap();
    let (mut b, _) = demo(OTHER, other_root.to_str().unwrap());
    b.graph.snapshot.commit = "def456".into();
    b.write(&f.store, other_root.to_str().unwrap());
    other_root
}

fn text_of(e: &Engine, tool: &str, args: Value, opts: RespondOptions) -> String {
    match e.respond_with(tool, args, opts) {
        Ok(r) => format!("{}\n{}", r.text, r.json),
        Err(err) => format!("{err:#}"),
    }
}

#[test]
fn local_visibility_hides_a_root() {
    let f = fixture();
    let other_root = two_roots(&f);
    let hidden = other_root.to_str().unwrap();
    let e = Engine::new(f.store.clone(), None).unwrap();

    // Without a visibility, two roots: `repo` is required.
    assert!(e.respond("get_symbol", json!({"name": "helper"})).is_err());
    // With one visible root, it is the only root: every tool reads it, and
    // nothing names the hidden one.
    for (tool, args) in every_tool() {
        let r = e.respond_with(tool, args.clone(), only("local/demo")).unwrap();
        let text = format!("{}\n{}", r.text, r.json);
        assert!(!text.contains(OTHER) && !text.contains(hidden), "{tool}: {text}");
        let star = text_of(&e, tool, with_repo(args, json!("*")), only("local/demo"));
        assert!(!star.contains(OTHER) && !star.contains(hidden), "{tool} *: {star}");
    }

    // Naming the hidden root, by repository id or by path, reads like a
    // root that is not indexed; the visible roots are listed.
    for name in [OTHER, hidden] {
        let msg = text_of(&e, "search_symbols", json!({"query": "helper", "repo": name}), only("local/demo"));
        assert!(msg.contains("is not indexed; indexed:") && msg.contains("local/demo"), "{msg}");
        assert!(!msg.contains(&format!("({OTHER})")), "{msg}");
    }
    let id = format!("{OTHER}@def456:pkg/service.py#helper");
    let msg = text_of(&e, "get_symbol", json!({"id": id}), only("local/demo"));
    assert!(!msg.contains(hidden), "{msg}");

    // A default root the caller may not read is ignored, as if unset.
    let e = Engine::new(f.store.clone(), Some(other_root.clone())).unwrap();
    let r = e.respond_with("get_symbol", json!({"name": "helper"}), only("local/demo")).unwrap();
    assert!(r.text.contains("local/demo"), "{}", r.text);
    assert!(!r.text.contains(OTHER) && !r.text.contains(hidden), "{}", r.text);
    let theirs = e.respond_with("get_symbol", json!({"name": "helper"}), only(OTHER)).unwrap();
    assert!(theirs.text.contains(OTHER), "{}", theirs.text);
}

fn with_repo(mut args: Value, repo: Value) -> Value {
    args["repo"] = repo;
    args
}
