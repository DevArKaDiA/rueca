//! Replays the golden transcripts (`tests/golden/*.json`, recorded from the
//! rmcp-based server, see `tests/golden/README.md`) against Rueca's own MCP
//! layer, in process, and compares every stdout line byte for byte.
//!
//! Each session runs as `capture.py` ran `rueca mcp --store <tmp> --root
//! <tmp>/golden-repo`: a copy of `tests/fixtures/golden-repo` outside any git
//! repository (so the snapshot is `local/golden-repo@worktree`), a fresh
//! store, auto-index on. The only normalisation is the server version
//! (`CARGO_PKG_VERSION`), so a release bump does not invalidate the files.
//!
//! Where Rueca differs from rmcp on purpose (`docs/mcp-protocol.md` §9), the
//! new expectation is spelled out in [`OVERRIDES`] and [`EXIT_1_IN_RMCP`];
//! every entry must be used.

mod common;

use std::cell::Cell;
use std::path::Path;

use common::Client;
use serde_json::Value;

const GOLDEN_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden");
const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/golden-repo");

/// A step whose expected output differs from the golden on purpose.
struct Override {
    file: &'static str,
    session: &'static str,
    step: usize,
    /// The lines Rueca writes instead.
    lines: &'static [&'static str],
    /// Which accepted difference of `docs/mcp-protocol.md` §9.
    why: &'static str,
    used: Cell<bool>,
}

const fn over(
    file: &'static str,
    session: &'static str,
    step: usize,
    lines: &'static [&'static str],
    why: &'static str,
) -> Override {
    Override { file, session, step, lines, why, used: Cell::new(false) }
}

const NOTIFICATION_FIRST: &str =
    "§9.1: a notification or a client response before any lifecycle is ignored (rmcp exited 1)";
const ALIVE_AFTER_IT: &str = "§9.1: the server is still alive, so the ping rmcp never received is answered";
const TOOLS_CALL_PARAMS: &str = "§9.2: malformed `tools/call` params get -32602 (rmcp: -32601 \"tools/call\")";

fn overrides() -> Vec<Override> {
    const PING_1: &[&str] = &[r#"{"jsonrpc":"2.0","id":1,"result":{}}"#];
    vec![
        over(
            "legacy-2025-06-18",
            "legacy 2025-06-18",
            24,
            &[
                r#"{"jsonrpc":"2.0","id":35,"error":{"code":-32602,"message":"invalid params for `tools/call`: missing `name`"}}"#,
            ],
            TOOLS_CALL_PARAMS,
        ),
        over(
            "legacy-2025-06-18",
            "legacy 2025-06-18",
            25,
            &[
                r#"{"jsonrpc":"2.0","id":36,"error":{"code":-32602,"message":"invalid params for `tools/call`: `name` must be a string"}}"#,
            ],
            TOOLS_CALL_PARAMS,
        ),
        over(
            "legacy-2025-06-18",
            "legacy 2025-06-18",
            26,
            &[
                r#"{"jsonrpc":"2.0","id":37,"error":{"code":-32602,"message":"invalid params for `tools/call`: `arguments` must be an object"}}"#,
            ],
            TOOLS_CALL_PARAMS,
        ),
        over("pre-init", "notifications/initialized as the first message", 0, &[], NOTIFICATION_FIRST),
        over("pre-init", "notifications/initialized as the first message", 1, PING_1, ALIVE_AFTER_IT),
        over("pre-init", "another notification as the first message", 0, &[], NOTIFICATION_FIRST),
        over("pre-init", "another notification as the first message", 1, PING_1, ALIVE_AFTER_IT),
        over("pre-init", "a response-shaped message as the first message", 0, &[], NOTIFICATION_FIRST),
        over("pre-init", "a response-shaped message as the first message", 1, PING_1, ALIVE_AFTER_IT),
    ]
}

/// Sessions where rmcp exited 1 because no lifecycle had been chosen when
/// the input ended or a notification arrived; Rueca always returns cleanly
/// (exit 0) at end of input (§9.1).
const EXIT_1_IN_RMCP: &[(&str, &str)] = &[
    ("pre-init", "notifications/initialized as the first message"),
    ("pre-init", "another notification as the first message"),
    ("pre-init", "a response-shaped message as the first message"),
    ("pre-init", "a malformed line as the first message"),
    ("pre-init", "invalid requests as the first messages"),
    ("shutdown", "after server/discover (stateless)"),
    ("shutdown", "before any message"),
    ("shutdown", "after a ping only (no initialize)"),
];

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
    overrides: &'a [Override],
}

impl Replay<'_> {
    fn new<'a>(file: &'a str, doc: &Value, overrides: &'a [Override]) -> Replay<'a> {
        let info = |v: &str| format!(r#""name":"rueca","title":"Rueca code intelligence","version":"{v}""#);
        let recorded = doc["server"].as_str().and_then(|s| s.strip_prefix("rueca ")).expect("server version");
        Replay { file, recorded_info: info(recorded), current_info: info(env!("CARGO_PKG_VERSION")), overrides }
    }

    fn normalise(&self, line: String) -> String {
        line.replace(&self.current_info, &self.recorded_info)
    }

    fn expected(&self, label: &str, index: usize, step: &Value) -> Vec<String> {
        let overridden = self.overrides.iter().find(|o| o.file == self.file && o.session == label && o.step == index);
        if let Some(o) = overridden {
            o.used.set(true);
            return o.lines.iter().map(|l| l.to_string()).collect();
        }
        let ended = step.get("eof").is_some() || step.get("skipped").is_some();
        assert!(!ended, "{} / {label} / step {index}: rmcp exited here; add an override", self.file);
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
        let rmcp_code = i64::from(EXIT_1_IN_RMCP.contains(&(self.file, label)));
        assert_eq!(session["exit"]["code"].as_i64(), Some(rmcp_code), "{} / {label}: rmcp's exit code", self.file);
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
    let overrides = overrides();
    let replay = Replay::new(file, &doc, &overrides);
    for session in doc["sessions"].as_array().unwrap() {
        replay.session(session);
    }
    for o in overrides.iter().filter(|o| o.file == file) {
        assert!(o.used.get(), "unused override {} / {} / step {} ({})", o.file, o.session, o.step, o.why);
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
