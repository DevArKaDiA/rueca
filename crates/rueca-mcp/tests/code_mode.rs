//! Code mode (RFC 0009, experimental): `execute` and `describe_api` against
//! the golden fixture (`tests/fixtures/golden-repo`, indexed into a fresh
//! store): the tool list with code mode off (byte-identical to the golden
//! transcripts) and on (`tests/code_mode/transcript.json`), the response
//! envelope, scope defaults, budget, every limit and error kind in each
//! language this build has, determinism and the generated API.
//!
//! `RUECA_BLESS_CODE_MODE=1` rewrites `tests/code_mode/transcript.json`
//! from what the layer writes (review the diff); `RUECA_PRINT_API=1` prints
//! both `describe_api` references (`--nocapture`).

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::Client;
use rueca_mcp::codemode::{self, CodeMode};
use rueca_query::{Engine, RespondOptions, Response};
use serde_json::{Value, json};

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/golden-repo");
const GOLDEN_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden");
const TRANSCRIPT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/code_mode/transcript.json");

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

/// A copy of the golden fixture indexed into a fresh store.
struct Fixture {
    _tmp: tempfile::TempDir,
    store: PathBuf,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("golden-repo");
        copy_dir(Path::new(FIXTURE), &root);
        let store = tmp.path().join("store");
        std::fs::create_dir_all(&store).unwrap();
        rueca_mcp::index_root(&root, &store).unwrap();
        Fixture { _tmp: tmp, store, root }
    }

    fn engine(&self) -> Engine {
        Engine::new(self.store.clone(), Some(self.root.clone())).unwrap()
    }

    fn options(&self, code_mode: bool) -> rueca_mcp::ServerOptions {
        rueca_mcp::ServerOptions {
            store_dir: self.store.clone(),
            root: Some(self.root.clone()),
            auto_index: false,
            code_mode,
            ..Default::default()
        }
    }
}

/// Runs `execute` with `args`.
fn execute(fx: &Fixture, args: Value) -> anyhow::Result<Response> {
    CodeMode::new().with_timing(false).respond(&fx.engine(), "execute", args, &RespondOptions::default())
}

fn ok(fx: &Fixture, args: Value) -> Value {
    match execute(fx, args.clone()) {
        Ok(r) => r.json,
        Err(e) => panic!("{args}: {e:#}"),
    }
}

fn fail(fx: &Fixture, args: Value) -> String {
    match execute(fx, args.clone()) {
        Ok(r) => panic!("{args}: expected an error, got {}", r.json),
        Err(e) => format!("{e:#}"),
    }
}

/// The languages this build runs.
fn languages() -> Vec<&'static str> {
    if CodeMode::new().has_starlark() { vec!["js", "starlark"] } else { vec!["js"] }
}

/// `js` or `starlark` version of a script.
fn script(lang: &str, js: &str, starlark: &str) -> Value {
    json!({"code": if lang == "js" { js } else { starlark }, "language": lang})
}

fn with(mut args: Value, extra: Value) -> Value {
    for (k, v) in extra.as_object().unwrap() {
        args[k] = v.clone();
    }
    args
}

// --- the tool list -----------------------------------------------------

/// The `tools` array of the recorded `tools/list` answer, as written.
fn recorded_tools() -> String {
    let doc: Value =
        serde_json::from_str(&std::fs::read_to_string(Path::new(GOLDEN_DIR).join("legacy-2025-11-25.json")).unwrap())
            .unwrap();
    let steps = doc["sessions"][0]["steps"].as_array().unwrap();
    let line = steps
        .iter()
        .flat_map(|s| s["recv_raw"].as_array().unwrap())
        .filter_map(Value::as_str)
        .find(|l| l.contains("\"tools\":["))
        .unwrap();
    let start = line.find("\"tools\":").unwrap() + "\"tools\":".len();
    line[start..line.len() - 2].to_string()
}

#[test]
fn disabled_tool_list_is_byte_identical_to_the_golden() {
    let fx = Fixture::new();
    let server = rueca_mcp::RuecaServer::from_options(&fx.options(false)).unwrap();
    assert!(!server.code_mode());
    assert_eq!(serde_json::to_string(server.tool_list()).unwrap(), recorded_tools());
    // `with_code_mode(false)` is the same list.
    let off = server.clone().with_code_mode(false);
    assert_eq!(serde_json::to_string(off.tool_list()).unwrap(), recorded_tools());
}

#[test]
fn enabled_tool_list_appends_the_two_tools() {
    let fx = Fixture::new();
    let server = rueca_mcp::RuecaServer::from_options(&fx.options(true)).unwrap();
    let names: Vec<&str> = server.tool_list().iter().map(|t| t.name.as_str()).collect();
    let mut want: Vec<&str> = rueca_query::TOOL_NAMES.to_vec();
    want.extend(["execute", "describe_api"]);
    assert_eq!(names, want);
    let eight = serde_json::to_string(&server.tool_list()[..8]).unwrap();
    assert_eq!(eight, recorded_tools(), "the eight tools are unchanged");
}

#[test]
fn disabled_server_does_not_know_the_tools() {
    let fx = Fixture::new();
    let mut client = Client::start(rueca_mcp::RuecaServer::from_options(&fx.options(false)).unwrap());
    client.send_line(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#);
    client.next_line().unwrap();
    client.send_line(
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"execute","arguments":{"code":"return 1"}}}"#,
    );
    let line = client.next_line().unwrap();
    assert!(line.contains("unknown tool `execute`"), "{line}");
    let (outcome, _) = client.close();
    outcome.unwrap();
}

/// The steps of the code-mode transcript (one session, code mode on).
fn transcript_requests() -> Vec<String> {
    let ex = |id: u32, args: Value| {
        json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": "execute", "arguments": args}})
            .to_string()
    };
    vec![
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"golden","version":"0"}}}"#.to_string(),
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#.to_string(),
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#.to_string(),
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"describe_api","arguments":{}}}"#.to_string(),
        ex(4, json!({"code": "const g = await rueca.searchSymbols({query: 'total_due'});\nconst t = await rueca.trace({id: g[0].symbols[0].id});\nconsole.log(t.nodes.length);\nreturn t.nodes.map(n => [n.local, n.repo, n.dir]);"})),
        ex(5, json!({"code": "const g = await rueca.searchSymbols({query: 'invoice', limit: 1});\nreturn g;", "budget_tokens": 100})),
        ex(6, json!({"code": "return [...Array(300).keys()];", "budget_tokens": 100})),
        ex(7, json!({"code": "const x = 1;\nreturn x(;"})),
        ex(8, json!({"code": "await rueca.getSymbol({name: 'no_such_symbol'});"})),
    ]
}

fn run_transcript() -> Vec<Value> {
    let fx = Fixture::new();
    let mut client = Client::start(rueca_mcp::RuecaServer::from_options(&fx.options(true)).unwrap());
    let mut steps = Vec::new();
    for req in transcript_requests() {
        client.send_line(&req);
        let notification = !req.contains("\"id\"");
        let recv: Vec<String> = if notification { Vec::new() } else { client.next_line().into_iter().collect() };
        steps.push(json!({"send_raw": req, "recv_raw": recv}));
    }
    let (outcome, trailing) = client.close();
    outcome.unwrap();
    assert!(trailing.is_empty(), "{trailing:?}");
    steps
}

#[test]
fn enabled_transcript_matches_the_golden() {
    let steps = run_transcript();
    let version = env!("CARGO_PKG_VERSION");
    let normalise = |s: String| s.replace(&format!("\"version\":\"{version}\""), "\"version\":\"VERSION\"");
    let steps: Vec<Value> = steps
        .into_iter()
        .map(|mut s| {
            let recv: Vec<String> =
                s["recv_raw"].as_array().unwrap().iter().map(|l| normalise(l.as_str().unwrap().into())).collect();
            s["recv_raw"] = json!(recv);
            s
        })
        .collect();
    let doc = json!({"description": "rueca mcp --experimental-code-mode on a copy of tests/fixtures/golden-repo (JavaScript only); the server version is written as VERSION", "steps": steps});
    if codemode::truthy(std::env::var("RUECA_BLESS_CODE_MODE").ok().as_deref()) {
        std::fs::create_dir_all(Path::new(TRANSCRIPT).parent().unwrap()).unwrap();
        std::fs::write(TRANSCRIPT, serde_json::to_string_pretty(&doc).unwrap() + "\n").unwrap();
    }
    let want: Value = serde_json::from_str(&std::fs::read_to_string(TRANSCRIPT).unwrap()).unwrap();
    let want_steps = want["steps"].as_array().unwrap();
    assert_eq!(want_steps.len(), steps.len());
    for (i, (got, want)) in steps.iter().zip(want_steps).enumerate() {
        assert_eq!(got["send_raw"], want["send_raw"], "step {i}");
        assert_eq!(got["recv_raw"], want["recv_raw"], "step {i}: {}", got["send_raw"]);
    }
}

// --- execute: the envelope ----------------------------------------------

#[test]
fn multi_step_script_end_to_end() {
    let fx = Fixture::new();
    for lang in languages() {
        let args = script(
            lang,
            "const hits = await rueca.searchSymbols({query: 'total_due', labels: ['Function']});
             const sym = hits[0].symbols[0];
             const callers = await rueca.trace({id: sym.id, direction: 'callers'});
             const callees = await rueca.trace({id: sym.id, direction: 'callees'});
             console.log('root', sym.qn);
             return {root: sym.id, s: hits[0].s,
                     callers: callers.nodes.filter(n => n.dir === 'in').map(n => n.qn),
                     callees: callees.nodes.filter(n => n.dir === 'out').map(n => n.qn).sort()};",
            "hits = rueca.search_symbols(query = 'total_due', labels = ['Function'])
sym = hits[0]['symbols'][0]
callers = rueca.trace(id = sym['id'], direction = 'callers')
callees = rueca.trace(id = sym['id'], direction = 'callees')
print('root', sym['qn'])
result = {'root': sym['id'], 's': hits[0]['s'],
          'callers': [n['qn'] for n in callers['nodes'] if n.get('dir') == 'in'],
          'callees': sorted([n['qn'] for n in callees['nodes'] if n.get('dir') == 'out'])}",
        );
        let out = ok(&fx, args);
        let r = &out["result"];
        assert_eq!(r["root"], "local/golden-repo@worktree:billing/invoices.py#total_due", "{lang}");
        assert_eq!(r["callers"], json!(["billing.invoices.InvoiceService.issue"]), "{lang}");
        assert_eq!(r["callees"], json!(["billing.invoices.apply_discount", "billing.invoices.compute_tax"]), "{lang}");
        assert_eq!(out["calls"], 3, "{lang}");
        assert_eq!(out["logs"], json!(["root billing.invoices.total_due"]), "{lang}");
        assert_eq!(out["truncated"], false);
        // One snapshot, read by every call; `s` indexes it.
        let snaps = out["snapshots"].as_array().unwrap();
        assert_eq!(snaps.len(), 1, "{lang}");
        assert_eq!(snaps[0]["s"], 0);
        assert_eq!(snaps[0]["repo"], "local/golden-repo");
        assert_eq!(snaps[0]["commit"], "worktree");
        assert_eq!(r["s"], 0);
        let keys: Vec<&str> = out.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, ["result", "logs", "calls", "snapshots", "truncated"], "{lang}");
    }
}

#[test]
fn text_rendering_is_compact() {
    let fx = Fixture::new();
    let r =
        execute(&fx, json!({"code": "await rueca.listRepositories(); console.log('a'); return {x: [1, 2]};"})).unwrap();
    assert_eq!(r.text, "local/golden-repo@worktree (worktree)\n{\"x\":[1,2]}\nlog: a\n");
    let r = execute(&fx, json!({"code": "return 1;"})).unwrap();
    assert_eq!(r.text, "1\n", "no snapshot read, no header");
}

#[test]
fn deterministic() {
    let fx = Fixture::new();
    for lang in languages() {
        let args = script(
            lang,
            "const g = await rueca.searchSymbols({query: 'invoice'}); console.log(g.length); return g;",
            "g = rueca.search_symbols(query = 'invoice')\nprint(len(g))\nresult = g",
        );
        let a = execute(&fx, args.clone()).unwrap();
        let b = execute(&fx, args).unwrap();
        assert_eq!(serde_json::to_string(&a.json).unwrap(), serde_json::to_string(&b.json).unwrap(), "{lang}");
        assert_eq!(a.text, b.text, "{lang}");
        assert!(a.json.get("timing").is_none());
    }
}

#[test]
fn scope_defaults_apply_to_calls_without_repo_ref_or_id() {
    let fx = Fixture::new();
    for lang in languages() {
        // `repo` reaches a call that names nothing...
        let e = fail(
            &fx,
            with(
                script(
                    lang,
                    "return await rueca.searchSymbols({query: 'x'});",
                    "result = rueca.search_symbols(query = 'x')",
                ),
                json!({"repo": "github.com/no/such"}),
            ),
        );
        assert!(e.contains("repository `github.com/no/such` is not indexed"), "{lang}: {e}");
        // ... not one with its own `repo`, nor one naming an `id`.
        let id = "local/golden-repo@worktree:billing/invoices.py#total_due";
        let out = ok(
            &fx,
            with(
                script(
                    lang,
                    &format!(
                        "const a = await rueca.searchSymbols({{query: 'tax', repo: 'local/golden-repo', ref: 'worktree'}}); const b = await rueca.getSymbol({{id: '{id}'}}); return [a.length, b.symbol.qn];"
                    ),
                    &format!(
                        "a = rueca.search_symbols(query = 'tax', repo = 'local/golden-repo', ref = 'worktree')\nb = rueca.get_symbol(id = '{id}')\nresult = [len(a), b['symbol']['qn']]"
                    ),
                ),
                json!({"repo": "github.com/no/such", "ref": "no-such-ref"}),
            ),
        );
        assert_eq!(out["result"], json!([1, "billing.invoices.total_due"]), "{lang}");
        // `ref` reaches a call that names nothing.
        let e = fail(
            &fx,
            with(
                script(lang, "return await rueca.listRepositories();", "result = rueca.list_repositories()"),
                json!({"ref": "no-such-ref"}),
            ),
        );
        assert!(e.contains("ref `no-such-ref` is not indexed"), "{lang}: {e}");
        let out = ok(
            &fx,
            with(
                script(
                    lang,
                    "return (await rueca.listRepositories()).length;",
                    "result = len(rueca.list_repositories())",
                ),
                json!({"ref": "worktree"}),
            ),
        );
        assert_eq!(out["result"], 1);
    }
}

#[test]
fn budget_cuts_the_result() {
    let fx = Fixture::new();
    for lang in languages() {
        let args = with(
            script(
                lang,
                "return [...Array(1000).keys()].map(i => 'item-' + i);",
                "result = ['item-%d' % i for i in range(1000)]",
            ),
            json!({"budget_tokens": 100}),
        );
        let r = execute(&fx, args).unwrap();
        assert_eq!(r.json["truncated"], true, "{lang}");
        let kept = r.json["result"].as_array().unwrap();
        assert!(!kept.is_empty() && kept.len() < 1000, "{lang}");
        assert_eq!(kept[0], "item-0");
        assert!(rueca_query::compact::count_tokens(&r.json["result"].to_string()) <= 100);
        assert!(r.text.contains("(truncated: the result was cut to budget_tokens 100)"), "{}", r.text);
    }
    // A string answer is cut too.
    let r = execute(&fx, json!({"code": "return 'z'.repeat(5000);", "budget_tokens": 100})).unwrap();
    assert_eq!(r.json["truncated"], true);
    assert!(r.json["result"].as_str().unwrap().ends_with('…'));
}

#[test]
fn partial_calls_are_listed() {
    let fx = Fixture::new();
    for lang in languages() {
        let out = ok(
            &fx,
            script(
                lang,
                "return await rueca.searchSymbols({query: 'invoice', limit: 1});",
                "result = rueca.search_symbols(query = 'invoice', limit = 1)",
            ),
        );
        assert_eq!(out["partial"], json!([{"call": 1, "tool": "search_symbols", "next_cursor": "1"}]), "{lang}");
    }
}

#[test]
fn timing_is_opt_in() {
    let fx = Fixture::new();
    let cm = CodeMode::new().with_timing(true);
    let r = cm
        .respond(
            &fx.engine(),
            "execute",
            json!({"code": "await rueca.listRepositories(); return 1;"}),
            &RespondOptions::default(),
        )
        .unwrap();
    let t = &r.json["timing"];
    let (total, engine) = (t["total_us"].as_u64().unwrap(), t["engine_us"].as_u64().unwrap());
    assert!(total >= engine && engine > 0, "{t}");
    assert!(!r.text.contains("total_us"), "timing is not in the text");
}

// --- limits and errors ---------------------------------------------------

#[test]
fn wall_time_limit_and_clamp() {
    let fx = Fixture::new();
    for lang in languages() {
        let started = Instant::now();
        let e = fail(
            &fx,
            with(script(lang, "while (true) {}", "for i in range(2147483647):\n    pass"), json!({"timeout_ms": 100})),
        );
        assert!(started.elapsed() < Duration::from_secs(2), "{lang}");
        assert!(e.contains("timeout error") && e.contains("timeout_ms 100; the maximum is 10000"), "{lang}: {e}");
    }
    let e = fail(&fx, json!({"code": "while (true) {}", "timeout_ms": 0}));
    assert!(e.contains("timeout_ms 1;"), "clamped up: {e}");
}

#[test]
fn memory_limit() {
    let fx = Fixture::new();
    for lang in languages() {
        let e = fail(
            &fx,
            script(
                lang,
                "const a = []; while (true) a.push('x'.repeat(10000) + a.length);",
                "a = []\nfor i in range(100000000):\n    a.append('x' * 10000 + str(i))",
            ),
        );
        assert!(e.contains("memory error") && e.contains("memory limit 64 MiB"), "{lang}: {e}");
    }
}

#[test]
fn call_limit() {
    let fx = Fixture::new();
    for lang in languages() {
        let e = fail(
            &fx,
            script(
                lang,
                "for (let i = 0; i < 201; i++) await rueca.listRepositories();",
                "for i in range(201):\n    rueca.list_repositories()",
            ),
        );
        assert!(e.contains("call_limit error") && e.contains("200"), "{lang}: {e}");
    }
}

#[test]
fn script_size_limit() {
    let fx = Fixture::new();
    for lang in languages() {
        let big = format!("{}\n", " ".repeat(16 * 1024));
        let e = fail(&fx, json!({"code": big, "language": lang}));
        assert!(e.contains("script_too_large error"), "{lang}: {e}");
    }
}

#[test]
fn log_cap() {
    let fx = Fixture::new();
    for lang in languages() {
        let out = ok(
            &fx,
            script(
                lang,
                "for (let i = 0; i < 100; i++) console.log('x'.repeat(100)); return 1;",
                "for i in range(100):\n    print('x' * 100)\nresult = 1",
            ),
        );
        assert_eq!(out["logs_truncated"], true, "{lang}");
        let bytes: usize = out["logs"].as_array().unwrap().iter().map(|l| l.as_str().unwrap().len()).sum();
        assert!(bytes <= 4096, "{lang}");
    }
}

#[test]
fn errors_carry_kind_and_line() {
    let fx = Fixture::new();
    for lang in languages() {
        let e = fail(&fx, script(lang, "const a = 1;\nreturn a(;", "a = 1\nresult = a(("));
        assert!(e.contains("syntax error at line 2"), "{lang}: {e}");
        let e = fail(&fx, script(lang, "const a = 1;\nnull.x;", "a = 1\nresult = None['x']"));
        assert!(e.contains("runtime error at line 2"), "{lang}: {e}");
        // An unknown function.
        let e = fail(&fx, script(lang, "\nawait rueca.noSuchTool({});", "\nrueca.no_such_tool()"));
        assert!(e.contains("error at line 2") && e.contains("describe_api"), "{lang}: {e}");
    }
}

#[test]
fn engine_errors_name_the_call() {
    let fx = Fixture::new();
    // JavaScript: catchable, with the call's name and the engine's message.
    let out = ok(&fx, json!({"code": "try { await rueca.searchSymbols({}); } catch (e) { return e.message; }"}));
    let msg = out["result"].as_str().unwrap();
    assert!(msg.starts_with("rueca.searchSymbols: ") && msg.contains("missing required argument `query`"), "{msg}");
    // Uncaught: the script fails at the call's line.
    let e = fail(&fx, json!({"code": "const x = 1;\nawait rueca.getSymbol({name: 'no_such_symbol', bogus: 1});"}));
    assert!(e.contains("runtime error at line 2") && e.contains("rueca.getSymbol: unknown argument `bogus`"), "{e}");
    if languages().contains(&"starlark") {
        let e = fail(&fx, json!({"code": "x = 1\nresult = rueca.search_symbols(limit = 3)", "language": "starlark"}));
        assert!(
            e.contains("runtime error at line 2")
                && e.contains("rueca.search_symbols: missing required argument `query`"),
            "{e}"
        );
    }
}

#[test]
fn bad_execute_arguments() {
    let fx = Fixture::new();
    assert!(fail(&fx, json!({})).contains("missing required argument `code`"));
    assert!(fail(&fx, json!({"code": "1", "extra": true})).contains("unknown argument `extra`"));
    assert!(fail(&fx, json!({"code": "1", "language": "python"})).contains("unknown language `python`"));
    let e = fail(&fx, json!({"code": "return await rueca.listRepositories({format: 'verbose'});"}));
    assert!(e.contains("`format` is not available in code mode"), "{e}");
}

#[cfg(not(feature = "code-mode-starlark"))]
#[test]
fn starlark_needs_the_feature() {
    let fx = Fixture::new();
    let e = fail(&fx, json!({"code": "result = 1", "language": "starlark"}));
    assert!(e.contains("built without Starlark") && e.contains("--features code-mode-starlark"), "{e}");
    let e = CodeMode::new()
        .respond(&fx.engine(), "describe_api", json!({"language": "starlark"}), &RespondOptions::default())
        .unwrap_err()
        .to_string();
    assert!(e.contains("--features code-mode-starlark"), "{e}");
}

// --- the generated API ---------------------------------------------------

fn describe(fx: &Fixture, lang: &str) -> String {
    let r = CodeMode::new()
        .respond(&fx.engine(), "describe_api", json!({"language": lang}), &RespondOptions::default())
        .unwrap();
    assert_eq!(r.json["reference"], r.text.as_str());
    r.text
}

#[test]
fn every_tool_is_in_both_references() {
    let js = codemode::reference(rueca_codemode::Language::Js);
    let star = codemode::reference(rueca_codemode::Language::Starlark);
    for d in rueca_query::tool_definitions() {
        let camel = rueca_codemode::to_camel_case(d.name);
        assert!(js.contains(&format!("  {camel}(args")), "js: {}", d.name);
        assert!(star.contains(&format!("\nrueca.{}(", d.name)), "starlark: {}", d.name);
        // Every argument but `format` is documented.
        for arg in d.input_schema["properties"].as_object().unwrap().keys().filter(|k| *k != "format") {
            assert!(js.contains(&format!("  {arg}")), "js: {}.{arg}", d.name);
            assert!(star.contains(&format!("  {arg}: ")), "starlark: {}.{arg}", d.name);
        }
    }
    if codemode::truthy(std::env::var("RUECA_PRINT_API").ok().as_deref()) {
        eprintln!("{js}\n----\n{star}");
    }
    eprintln!(
        "describe_api sizes: js {} bytes ~{} tokens; starlark {} bytes ~{} tokens",
        js.len(),
        rueca_query::compact::count_tokens(js),
        star.len(),
        rueca_query::compact::count_tokens(star)
    );
}

#[test]
fn every_tool_is_callable() {
    let fx = Fixture::new();
    for lang in languages() {
        for d in rueca_query::tool_definitions() {
            let camel = rueca_codemode::to_camel_case(d.name);
            let args = script(
                lang,
                &format!("try {{ await rueca.{camel}({{}}); return 'ok'; }} catch (e) {{ return e.message; }}"),
                &format!("rueca.{}()\nresult = 'ok'", d.name),
            );
            // A tool with required arguments fails in the engine, naming
            // the call: it exists.
            let outcome = match execute(&fx, args) {
                Ok(r) => r.json["result"].as_str().unwrap_or_default().to_string(),
                Err(e) => format!("{e:#}"),
            };
            let named = if lang == "js" { format!("rueca.{camel}: ") } else { format!("rueca.{}: ", d.name) };
            assert!(outcome == "ok" || outcome.contains(&named), "{lang} {}: {outcome}", d.name);
        }
    }
}

#[test]
fn reference_examples_run() {
    let fx = Fixture::new();
    let js = describe(&fx, "js");
    assert!(js.contains("declare const rueca: {") && js.contains("interface CommonArgs"));
    for lang in languages() {
        let reference = describe(&fx, lang);
        let blocks: Vec<&str> = reference.split("Example: ").skip(1).collect();
        assert_eq!(blocks.len(), 2, "{lang}");
        for block in blocks {
            let code = block.split_once('\n').unwrap().1;
            let out = ok(&fx, json!({"code": code, "language": lang}));
            assert!(!out["result"].is_null(), "{lang}: {code}");
        }
    }
}

#[cfg(feature = "code-mode-starlark")]
#[test]
fn starlark_dialect_is_as_documented() {
    let fx = Fixture::new();
    let star = |code: &str| execute(&fx, json!({"code": code, "language": "starlark"}));
    for refused in [
        "x = 0\nwhile x < 3:\n    x += 1",
        "class A:\n    pass",
        "try:\n    x = 1\nexcept:\n    x = 2",
        "load('a.star', 'b')",
        "result = 2 ** 3",
        "result = None is None",
        "result = {1, 2}",
    ] {
        assert!(star(refused).is_err(), "should fail: {refused}");
    }
    let allowed = "x = 1\ndef f():\n    x = 2\n    return x\ndef g(n):\n    return g(n - 1) + 1 if n else 0\nresult = [f(), x, sorted(set([2, 1])), f'{x}', json.encode({'a': 1}), struct(a = 1).a, [i for i in range(3) if i], (lambda y: y + 1)(1), g(3)]";
    assert_eq!(star(allowed).unwrap().json["result"], json!([2, 1, [1, 2], "1", "{\"a\":1}", 1, [1, 2], 2, 3]));
    // Recursion is bounded by the call depth.
    let deep = star("def g(n):\n    return g(n - 1) + 1 if n else 0\nresult = g(1000)").unwrap_err().to_string();
    assert!(deep.contains("runtime error"), "{deep}");
}
