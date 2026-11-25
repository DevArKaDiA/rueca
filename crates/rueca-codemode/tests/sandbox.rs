//! The same test matrix through the [`Sandbox`] trait for every engine this
//! build has: each test gives one script per language and expects the same
//! outcome.

use std::time::{Duration, Instant};

use rueca_codemode::{ErrorKind, Host, Language, Limits, Outcome, Sandbox, ScriptError, sandbox};
use serde_json::{Value, json};

/// The two tools of the spike, answered from memory.
const TOOLS: &[&str] = &["search_symbols", "get_symbol"];

/// A fake engine: `search_symbols` returns two hits, `get_symbol` echoes the
/// id; `{"fail": true}` makes either fail. It records every call.
#[derive(Default)]
struct FakeHost {
    calls: Vec<(String, Value)>,
}

impl Host for FakeHost {
    fn call(&mut self, tool: &str, args: Value) -> Result<Value, String> {
        self.calls.push((tool.to_string(), args.clone()));
        if args.get("fail") == Some(&Value::Bool(true)) {
            return Err(format!("{tool}: asked to fail"));
        }
        match tool {
            "search_symbols" => Ok(json!([
                {"id": "r@1:a.py#parse", "name": "parse", "kind": "Function"},
                {"id": "r@1:b.py#parse_all", "name": "parse_all", "kind": "Function"}
            ])),
            "get_symbol" => Ok(json!({"id": args["id"], "name": "parse", "callers": 3})),
            other => Err(format!("unknown tool `{other}`")),
        }
    }
}

fn engine(language: Language) -> Box<dyn Sandbox + Send + Sync> {
    sandbox(language, TOOLS).expect("language built in")
}

fn pick<'a>(language: Language, js: &'a str, starlark: &'a str) -> &'a str {
    match language {
        Language::Js => js,
        Language::Starlark => starlark,
    }
}

fn run_with(language: Language, js: &str, starlark: &str, limits: &Limits) -> (Result<Outcome, ScriptError>, FakeHost) {
    let mut host = FakeHost::default();
    let out = engine(language).run(pick(language, js, starlark), &mut host, limits);
    (out, host)
}

fn ok(language: Language, js: &str, starlark: &str) -> Outcome {
    match run_with(language, js, starlark, &Limits::default()).0 {
        Ok(o) => o,
        Err(e) => panic!("{language}: {e}"),
    }
}

fn err(language: Language, js: &str, starlark: &str, limits: &Limits) -> ScriptError {
    match run_with(language, js, starlark, limits).0 {
        Ok(o) => panic!("{language}: expected an error, got {o:?}"),
        Err(e) => e,
    }
}

#[test]
fn returns_a_value() {
    for &lang in Language::available() {
        let o = ok(lang, r#"return {a: 1, b: [true, null, "x"]};"#, r#"result = {"a": 1, "b": [True, None, "x"]}"#);
        assert_eq!(o.result, json!({"a": 1, "b": [true, null, "x"]}), "{lang}");
        assert_eq!(o.calls, 0);
        let o = ok(lang, "const x = 1;", "x = 1");
        assert_eq!(o.result, Value::Null, "{lang}: no answer is null");
    }
}

#[test]
fn host_call_round_trip() {
    for &lang in Language::available() {
        let (out, host) = run_with(
            lang,
            r#"const hits = await rueca.searchSymbols({query: "parse", limit: 2});
const sym = await rueca.getSymbol({id: hits[1].id});
return [hits.length, sym.id, sym.callers];"#,
            r#"hits = rueca.search_symbols(query = "parse", limit = 2)
sym = rueca.get_symbol(id = hits[1]["id"])
result = [len(hits), sym["id"], sym["callers"]]"#,
            &Limits::default(),
        );
        let o = out.unwrap_or_else(|e| panic!("{lang}: {e}"));
        assert_eq!(o.result, json!([2, "r@1:b.py#parse_all", 3]), "{lang}");
        assert_eq!(o.calls, 2);
        assert_eq!(host.calls[0], ("search_symbols".to_string(), json!({"query": "parse", "limit": 2})));
        assert_eq!(host.calls[1], ("get_symbol".to_string(), json!({"id": "r@1:b.py#parse_all"})));
    }
}

#[test]
fn host_errors_reach_the_script() {
    for &lang in Language::available() {
        let e = err(lang, "await rueca.getSymbol({fail: true});", "rueca.get_symbol(fail = True)", &Limits::default());
        assert_eq!(e.kind, ErrorKind::Runtime, "{lang}: {e}");
        assert!(e.message.contains("get_symbol: asked to fail"), "{lang}: {e}");
        assert_eq!(e.line, Some(1), "{lang}: {e}");
    }
    // JavaScript can handle them.
    if Language::available().contains(&Language::Js) {
        let o = ok(Language::Js, "try { await rueca.getSymbol({fail: true}); } catch (e) { return e.message; }", "");
        assert_eq!(o.result, json!("get_symbol: asked to fail"));
    }
}

#[test]
fn starlark_takes_keyword_arguments_only() {
    if !Language::available().contains(&Language::Starlark) {
        return;
    }
    let e = err(Language::Starlark, "", r#"rueca.get_symbol("x")"#, &Limits::default());
    assert_eq!(e.kind, ErrorKind::Runtime, "{e}");
    let o = ok(Language::Starlark, "", "args = {\"id\": \"x\"}\nresult = rueca.get_symbol(**args)[\"id\"]");
    assert_eq!(o.result, json!("x"));
}

#[test]
fn deadline_stops_an_endless_loop() {
    let wall = Duration::from_millis(200);
    let limits = Limits { wall_time: wall, ..Limits::default() };
    for &lang in Language::available() {
        let started = Instant::now();
        let e = err(lang, "while (true) {}", "for x in range(2147483647):\n    pass", &limits);
        let took = started.elapsed();
        assert_eq!(e.kind, ErrorKind::Timeout, "{lang}: {e}");
        assert!(took >= wall && took < wall + Duration::from_millis(300), "{lang}: stopped after {took:?}");
    }
}

#[test]
fn deadline_stops_a_script_that_keeps_awaiting() {
    if !Language::available().contains(&Language::Js) {
        return;
    }
    let limits = Limits { wall_time: Duration::from_millis(100), ..Limits::default() };
    let e = err(Language::Js, "while (true) { await null; }", "", &limits);
    assert_eq!(e.kind, ErrorKind::Timeout, "{e}");
}

#[test]
fn memory_limit_stops_growth() {
    let limits = Limits { memory_bytes: 16 << 20, ..Limits::default() };
    for &lang in Language::available() {
        let e = err(
            lang,
            r#"const a = []; while (true) a.push("x".repeat(10000) + a.length);"#,
            "a = []\nfor i in range(2147483647):\n    a.append(\"x\" * 10000 + str(i))",
            &limits,
        );
        assert_eq!(e.kind, ErrorKind::Memory, "{lang}: {e}");
    }
}

#[test]
fn memory_limit_catches_one_large_allocation() {
    // 128 MiB in one built-in call against a 32 MiB limit: QuickJS refuses
    // the allocation; Starlark makes it and fails at its next check (here,
    // the check after the script).
    let limits = Limits { memory_bytes: 32 << 20, wall_time: Duration::from_secs(30), ..Limits::default() };
    for &lang in Language::available() {
        let e = err(
            lang,
            r#"const s = "x".repeat(128 << 20); return s.length;"#,
            "s = \"x\" * (128 << 20)\nresult = len(s)",
            &limits,
        );
        assert_eq!(e.kind, ErrorKind::Memory, "{lang}: {e}");
    }
}

#[test]
fn call_limit() {
    let limits = Limits { max_calls: 3, ..Limits::default() };
    for &lang in Language::available() {
        let (out, host) = run_with(
            lang,
            r#"for (let i = 0; i < 5; i++) { try { await rueca.searchSymbols({query: "q"}); } catch (e) {} } return 1;"#,
            "for i in range(5):\n    rueca.search_symbols(query = \"q\")\nresult = 1",
            &limits,
        );
        let e = out.expect_err(lang.as_str());
        assert_eq!(e.kind, ErrorKind::CallLimit, "{lang}: {e}");
        assert_eq!(host.calls.len(), 3, "{lang}: the host saw only the admitted calls");
    }
}

#[test]
fn log_cap() {
    let limits = Limits { max_log_bytes: 12, ..Limits::default() };
    for &lang in Language::available() {
        let (out, _) = run_with(
            lang,
            r#"for (let i = 0; i < 5; i++) console.log("hello", i); console.log({a: 1}); return 0;"#,
            "for i in range(5):\n    print(\"hello\", i)\nresult = 0",
            &limits,
        );
        let o = out.unwrap_or_else(|e| panic!("{lang}: {e}"));
        assert_eq!(o.logs, vec!["hello 0".to_string(), "hello".to_string()], "{lang}");
        assert!(o.logs_truncated, "{lang}");
    }
    if Language::available().contains(&Language::Js) {
        let o = ok(Language::Js, r#"console.log("n", 1, {a: [1]}, null); return 0;"#, "");
        assert_eq!(o.logs, vec![r#"n 1 {"a":[1]} null"#.to_string()]);
    }
}

#[test]
fn syntax_error_has_a_line() {
    for &lang in Language::available() {
        let e = err(lang, "const x = 1;\nconst = ;\n", "x = 1\ny = = 2\n", &Limits::default());
        assert_eq!(e.kind, ErrorKind::Syntax, "{lang}: {e}");
        assert_eq!(e.line, Some(2), "{lang}: {e}");
    }
}

#[test]
fn runtime_error_has_a_line() {
    for &lang in Language::available() {
        let e = err(lang, "const a = 1;\nnull.x;\n", "a = 1\nb = None.x\n", &Limits::default());
        assert_eq!(e.kind, ErrorKind::Runtime, "{lang}: {e}");
        assert_eq!(e.line, Some(2), "{lang}: {e}");
    }
}

#[test]
fn script_size_limit() {
    let limits = Limits { max_script_bytes: 8, ..Limits::default() };
    for &lang in Language::available() {
        let e = err(lang, "return 123456789;", "result = 123456789", &limits);
        assert_eq!(e.kind, ErrorKind::ScriptTooLarge, "{lang}: {e}");
    }
}

#[test]
fn no_ambient_capabilities_js() {
    if !Language::available().contains(&Language::Js) {
        return;
    }
    let o = ok(
        Language::Js,
        r#"const names = ["require", "process", "fetch", "setTimeout", "setInterval", "Date", "performance",
  "eval", "WeakRef", "FinalizationRegistry", "std", "os", "__rueca_call", "__rueca_log", "SharedArrayBuffer"];
const types = {};
for (const n of names) types[n] = typeof globalThis[n];
const throws = (f) => { try { f(); return false; } catch (e) { return e instanceof TypeError; } };
return {
  types,
  random: throws(() => Math.random()),
  fn: throws(() => Function("return 1")),
  newFn: throws(() => new Function("return 1")),
  asyncFn: throws(() => (async function () {}).constructor("return 1")),
  genFn: throws(() => (function* () {}).constructor("return 1")),
  instance: (() => {}) instanceof Function,
};"#,
        "",
    );
    let undefined: Value = o.result["types"].as_object().unwrap().values().all(|t| t == "undefined").into();
    assert_eq!(undefined, json!(true), "{}", o.result);
    for key in ["random", "fn", "newFn", "asyncFn", "genFn", "instance"] {
        assert_eq!(o.result[key], json!(true), "{key}: {}", o.result);
    }
    let e = err(Language::Js, r#"await import("os");"#, "", &Limits::default());
    assert_ne!(e.kind, ErrorKind::Internal, "{e}");
}

#[test]
fn no_ambient_capabilities_starlark() {
    if !Language::available().contains(&Language::Starlark) {
        return;
    }
    let e = err(Language::Starlark, "", "load(\"x.star\", \"y\")\nresult = 1", &Limits::default());
    assert_eq!(e.kind, ErrorKind::Syntax, "{e}");
    for name in ["time", "random", "open", "exec", "eval", "getattr(rueca, \"__dict__\")"] {
        let e = err(Language::Starlark, "", &format!("result = {name}"), &Limits::default());
        assert_ne!(e.kind, ErrorKind::Internal, "{name}: {e}");
    }
}

#[test]
fn fresh_interpreter_per_run() {
    for &lang in Language::available() {
        let sb = engine(lang);
        let mut host = FakeHost::default();
        let first = pick(lang, "globalThis.leak = 1; return 1;", "leak = 1\nresult = 1");
        sb.run(first, &mut host, &Limits::default()).unwrap();
        let second = pick(lang, "return typeof leak;", "result = leak");
        match (lang, sb.run(second, &mut host, &Limits::default())) {
            (Language::Js, Ok(o)) => assert_eq!(o.result, json!("undefined")),
            (Language::Starlark, Err(e)) => assert_eq!(e.kind, ErrorKind::Syntax, "{e}"),
            (_, other) => panic!("{lang}: {other:?}"),
        }
    }
}

#[test]
fn deterministic() {
    for &lang in Language::available() {
        let js = r#"const hits = await rueca.searchSymbols({query: "parse"});
const out = {};
for (const h of hits) { out[h.name] = (await rueca.getSymbol({id: h.id})).callers; console.log(h.id); }
return {out, keys: Object.keys(out), s: JSON.stringify(out)};"#;
        let star = r#"hits = rueca.search_symbols(query = "parse")
out = {}
for h in hits:
    out[h["name"]] = rueca.get_symbol(id = h["id"])["callers"]
    print(h["id"])
result = {"out": out, "keys": list(out.keys()), "s": json.encode(out)}"#;
        let a = ok(lang, js, star);
        let b = ok(lang, js, star);
        assert_eq!(a, b, "{lang}");
        assert_eq!(a.calls, 3);
    }
}

#[test]
fn never_settling_promise_is_an_error() {
    if !Language::available().contains(&Language::Js) {
        return;
    }
    let e = err(Language::Js, "await new Promise(() => {}); return 1;", "", &Limits::default());
    assert_eq!(e.kind, ErrorKind::Runtime, "{e}");
}

#[test]
fn concurrent_calls_resolve_in_order() {
    if !Language::available().contains(&Language::Js) {
        return;
    }
    let (out, host) = run_with(
        Language::Js,
        r#"const [a, b] = await Promise.all([rueca.getSymbol({id: "a"}), rueca.getSymbol({id: "b"})]); return [a.id, b.id];"#,
        "",
        &Limits::default(),
    );
    assert_eq!(out.unwrap().result, json!(["a", "b"]));
    assert_eq!(host.calls.len(), 2);
}
