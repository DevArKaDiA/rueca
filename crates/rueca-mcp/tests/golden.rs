//! Replays the golden transcripts (`tests/golden/*.json`, recorded from
//! Rueca's own MCP layer with `tests/golden/capture.py`, see
//! `tests/golden/README.md`) against the layer, in process, and compares
//! every stdout line byte for byte.
//!
//! Each session runs as `capture.py` ran `rueca mcp --store <tmp> --root
//! <tmp>/golden-repo`: a copy of `tests/fixtures/golden-repo` outside any git
//! repository (so the snapshot is `local/golden-repo@worktree`), a fresh
//! store, auto-index on. The only normalisation is the server version
//! (`CARGO_PKG_VERSION`), so a release bump does not invalidate the files.
//!
//! The files were first recorded from the rmcp-based server and replayed
//! with the differences of `docs/mcp-protocol.md` §9 spelled out here; since
//! the capture of the own layer (tool list with `list_contracts`) they are
//! what the layer writes, with no override.

mod common;

use std::path::Path;

use common::Client;
use serde_json::Value;

const GOLDEN_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden");
const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/golden-repo");

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let to = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &to);
        } else {
            std::fs::copy(entry.path(), &to).unwrap();
        }
    }
}

/// A server as `rueca mcp --store <store> --root <copy of the fixture>` builds it.
fn start(tmp: &Path) -> Client {
    let root = tmp.join("golden-repo");
    copy_dir(Path::new(FIXTURE), &root);
    let store = tmp.join("store");
    std::fs::create_dir_all(&store).unwrap();
    let opts = rueca_mcp::ServerOptions { store_dir: store, root: Some(root), auto_index: true, ..Default::default() };
    Client::start(rueca_mcp::RuecaServer::from_options(&opts).unwrap())
}

/// Lines read for one step: up to the response to `wait` (an id), one line
/// (`"line"`), or exactly `expected` lines (`null`: no answer awaited; any
/// extra line shows up in the next step or at the end).
fn collect(client: &Client, wait: &Value, expected: usize) -> Vec<String> {
    match wait {
        Value::Null => (0..expected).map_while(|_| client.next_line()).collect(),
        Value::String(s) if s == "line" => client.next_line().into_iter().collect(),
        id => {
            let mut got = Vec::new();
            while let Some(line) = client.next_line() {
                let done = serde_json::from_str::<Value>(&line).is_ok_and(|v| v.get("id") == Some(id));
                got.push(line);
                if done {
                    break;
                }
            }
            got
        }
    }
}

struct Replay<'a> {
    file: &'a str,
    /// `serverInfo` as recorded, and as this build writes it.
    recorded_info: String,
    current_info: String,
}

impl Replay<'_> {
    fn new<'a>(file: &'a str, doc: &Value) -> Replay<'a> {
        let info = |v: &str| format!(r#""name":"rueca","title":"Rueca code intelligence","version":"{v}""#);
        let recorded = doc["server"].as_str().and_then(|s| s.strip_prefix("rueca ")).expect("server version");
        Replay { file, recorded_info: info(recorded), current_info: info(env!("CARGO_PKG_VERSION")) }
    }

    fn normalise(&self, line: String) -> String {
        line.replace(&self.current_info, &self.recorded_info)
    }

    fn expected(&self, label: &str, index: usize, step: &Value) -> Vec<String> {
        let ended = step.get("eof").is_some() || step.get("skipped").is_some();
        assert!(!ended, "{} / {label} / step {index}: the server exited here when captured", self.file);
        step["recv_raw"].as_array().unwrap().iter().map(|l| l.as_str().unwrap().to_string()).collect()
    }

    fn session(&self, session: &Value) {
        let label = session["label"].as_str().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let mut client = start(tmp.path());
        for (index, step) in session["steps"].as_array().unwrap().iter().enumerate() {
            let expected = self.expected(label, index, step);
            client.send_line(step["send_raw"].as_str().unwrap());
            let got: Vec<String> =
                collect(&client, &step["wait"], expected.len()).into_iter().map(|l| self.normalise(l)).collect();
            let at = format!("{} / {label} / step {index} ({})", self.file, step["note"].as_str().unwrap_or(""));
            assert_same(&got, &expected, &at);
        }
        // End of input: `serve` returns `Ok` (exit status 0) in every session.
        let (outcome, trailing) = client.close();
        outcome.unwrap_or_else(|e| panic!("{} / {label}: serve failed: {e:#}", self.file));
        assert_eq!(session["exit"]["code"].as_i64(), Some(0), "{} / {label}: recorded exit code", self.file);
        let trailing: Vec<String> = trailing.into_iter().map(|l| self.normalise(l)).collect();
        assert_same(&trailing, &[], &format!("{} / {label} / after EOF", self.file));
    }
}

/// Byte-for-byte equality, and the same JSON once parsed.
fn assert_same(got: &[String], expected: &[String], at: &str) {
    assert_eq!(got, expected, "{at}");
    let parse = |lines: &[String]| -> Vec<Value> { lines.iter().map(|l| serde_json::from_str(l).unwrap()).collect() };
    assert_eq!(parse(got), parse(expected), "{at}");
}

fn replay(file: &str) {
    let path = Path::new(GOLDEN_DIR).join(format!("{file}.json"));
    let doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let replay = Replay::new(file, &doc);
    for session in doc["sessions"].as_array().unwrap() {
        replay.session(session);
    }
}

const FILES: &[&str] = &[
    "framing",
    "initialize-unknown-version",
    "legacy-2024-11-05",
    "legacy-2025-03-26",
    "legacy-2025-06-18",
    "legacy-2025-11-25",
    "pre-init",
    "shutdown",
    "stateless-2026-07-28",
];

#[test]
fn every_transcript_is_replayed() {
    let mut on_disk: Vec<String> = std::fs::read_dir(GOLDEN_DIR)
        .unwrap()
        .filter_map(|e| e.unwrap().file_name().to_str()?.strip_suffix(".json").map(str::to_string))
        .collect();
    on_disk.sort();
    assert_eq!(on_disk, FILES);
}

#[test]
fn framing() {
    replay("framing");
}

#[test]
fn initialize_unknown_version() {
    replay("initialize-unknown-version");
}

#[test]
fn legacy_2024_11_05() {
    replay("legacy-2024-11-05");
}

#[test]
fn legacy_2025_03_26() {
    replay("legacy-2025-03-26");
}

#[test]
fn legacy_2025_06_18() {
    replay("legacy-2025-06-18");
}

#[test]
fn legacy_2025_11_25() {
    replay("legacy-2025-11-25");
}

#[test]
fn pre_init() {
    replay("pre-init");
}

#[test]
fn shutdown() {
    replay("shutdown");
}

#[test]
fn stateless_2026_07_28() {
    replay("stateless-2026-07-28");
}
