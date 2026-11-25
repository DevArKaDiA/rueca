//! `execute`: arguments, the engine-backed [`Host`], and the response.
//!
//! # Response
//!
//! ```json
//! {"result": <the script's answer>, "logs": ["…"], "calls": 3,
//!  "snapshots": [<RFC 0005 envelope entries>], "truncated": false}
//! ```
//!
//! followed, only when they apply, by `partial` (the calls whose own answer
//! was cut, `[{"call": n, "tool": "…", "next_cursor": "…"}]`: a `limit` or a
//! `max_lines` reached; raise it in the script or continue with `cursor`),
//! `logs_truncated: true` (output past the 4 KiB log cap was dropped) and,
//! with [`super::ENV_TIMING`], `timing: {"total_us", "engine_us"}`.
//!
//! - Each `rueca.*` call resolves to its envelope's `results`. `snapshots`
//!   lists every snapshot the calls read, once each (same entry: same `repo`,
//!   `ref`, `commit`, `overlay`, `url`), numbered in the order the calls
//!   first read them; every `s` and `in` inside a call's results is
//!   renumbered to that list before the script sees it. Ids are the pinned
//!   ids of the tools and stay valid on their own.
//! - Other envelope fields (`budget`, `retrieval`, `files_scanned`, `org`,
//!   `omitted`) are not passed to scripts; `truncated` / `next_cursor` become
//!   `partial`.
//! - A call's `budget_tokens` defaults to [`SCRIPT_CALL_BUDGET_TOKENS`] (the
//!   tools' maximum) instead of 2000: a call's answer reaches the script, not
//!   the agent, so it is bounded by the tool's `limit` alone. `format` other
//!   than `compact` is refused. The `repo` / `ref` of `execute` are added to
//!   a call that names neither the argument nor an `id` (an id names its own
//!   repository and commit).
//! - `result` is cut to `budget_tokens` (`budget.rs`).
//!
//! The text content (what a model reads) is the snapshot header lines of
//! the tools (`repo@commit12[+] (ref)`, `[s] ` first when several), the
//! result as compact JSON on one line, `log: <line>` per log line, then
//! `(truncated: …)`, `(partial: …)` and `(logs truncated)` notes when they
//! apply.

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use rueca_codemode::{ErrorKind, Host, Language, Limits, Outcome, Sandbox, ScriptError};
use rueca_query::{DeadlineExceeded, Engine, RespondOptions, Response, Visibility};
use serde_json::{Map, Value, json};

use super::budget;

/// Default of `execute`'s `budget_tokens`.
pub const DEFAULT_BUDGET_TOKENS: usize = 2000;

/// Default of `execute`'s `timeout_ms`.
pub const DEFAULT_TIMEOUT_MS: u64 = 2000;

/// Largest `timeout_ms`; larger values are clamped.
pub const MAX_TIMEOUT_MS: u64 = 10_000;

/// `budget_tokens` of an engine call that does not pass its own.
pub const SCRIPT_CALL_BUDGET_TOKENS: u64 = 1_000_000;

const ARGS: &[&str] = &["code", "language", "repo", "ref", "budget_tokens", "timeout_ms"];

/// `execute`'s validated arguments.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ExecuteArgs {
    pub code: String,
    pub language: Language,
    pub scope: Scope,
    pub budget_tokens: usize,
    pub timeout: Duration,
}

/// `repo` / `ref` applied to the script's calls.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Scope {
    pub repo: Option<Value>,
    pub ref_: Option<Value>,
}

impl ExecuteArgs {
    pub(crate) fn parse(args: Value) -> anyhow::Result<ExecuteArgs> {
        let mut obj = match args {
            Value::Object(m) => m,
            Value::Null => Map::new(),
            _ => anyhow::bail!("arguments of `execute` must be a JSON object"),
        };
        obj.retain(|_, v| !v.is_null());
        if let Some(k) = obj.keys().find(|k| !ARGS.contains(&k.as_str())) {
            anyhow::bail!("unknown argument `{k}` for `execute` (valid arguments: {})", ARGS.join(", "));
        }
        let code = match obj.remove("code") {
            Some(Value::String(s)) => s,
            None => anyhow::bail!("missing required argument `code` for `execute`"),
            Some(_) => anyhow::bail!("invalid argument `code` for `execute`: expected a string"),
        };
        let budget = int_arg(&obj, "budget_tokens")?.map_or(DEFAULT_BUDGET_TOKENS as i64, |n| n.clamp(100, 1_000_000));
        let timeout =
            int_arg(&obj, "timeout_ms")?.map_or(DEFAULT_TIMEOUT_MS as i64, |n| n.clamp(1, MAX_TIMEOUT_MS as i64));
        Ok(ExecuteArgs {
            code,
            language: parse_language(obj.get("language"), super::EXECUTE)?,
            scope: Scope { repo: scope_arg(obj.remove("repo"), "repo")?, ref_: scope_arg(obj.remove("ref"), "ref")? },
            budget_tokens: usize::try_from(budget).unwrap_or(DEFAULT_BUDGET_TOKENS),
            timeout: Duration::from_millis(u64::try_from(timeout).unwrap_or(DEFAULT_TIMEOUT_MS)),
        })
    }
}

fn int_arg(obj: &Map<String, Value>, name: &str) -> anyhow::Result<Option<i64>> {
    match obj.get(name) {
        None => Ok(None),
        Some(v) => v
            .as_i64()
            .map(Some)
            .ok_or_else(|| anyhow::anyhow!("invalid argument `{name}` for `execute`: expected an integer")),
    }
}

/// A `repo` / `ref`: a string or a list of strings.
fn scope_arg(v: Option<Value>, name: &str) -> anyhow::Result<Option<Value>> {
    match v {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(Value::String(s))),
        Some(Value::Array(a)) if a.iter().all(Value::is_string) => Ok(Some(Value::Array(a))),
        Some(_) => anyhow::bail!("invalid argument `{name}` for `execute`: expected a string or a list of strings"),
    }
}

/// `language` (`js` default, `starlark`).
pub(crate) fn parse_language(v: Option<&Value>, tool: &str) -> anyhow::Result<Language> {
    match v {
        None | Some(Value::Null) => Ok(Language::Js),
        Some(Value::String(s)) => Language::parse(s.trim()).ok_or_else(|| {
            anyhow::anyhow!(
                "invalid argument `language` for `{tool}`: unknown language `{s}` (expected `js` or `starlark`)"
            )
        }),
        Some(_) => anyhow::bail!("invalid argument `language` for `{tool}`: expected a string"),
    }
}

/// `args` of a call with `scope` and the script defaults applied (see the
/// module docs), or why the call is refused.
pub(crate) fn scope_args_for_call(args: Value, repo: Option<&Value>, ref_: Option<&Value>) -> Result<Value, String> {
    let mut obj = match args {
        Value::Object(m) => m,
        Value::Null => Map::new(),
        _ => return Err("arguments must be an object".into()),
    };
    if obj.get("format").is_some_and(|f| !f.is_null() && f != "compact") {
        return Err("`format` is not available in code mode (results are always the compact shape)".into());
    }
    let absent = |o: &Map<String, Value>, k: &str| o.get(k).is_none_or(Value::is_null);
    let pinned = !absent(&obj, "id");
    if absent(&obj, "budget_tokens") {
        obj.insert("budget_tokens".into(), json!(SCRIPT_CALL_BUDGET_TOKENS));
    }
    for (key, value) in [("repo", repo), ("ref", ref_)] {
        if let Some(v) = value.filter(|_| !pinned && absent(&obj, key)) {
            obj.insert(key.into(), v.clone());
        }
    }
    Ok(Value::Object(obj))
}

/// The snapshots read by a run, once each, in first-read order.
#[derive(Debug, Default)]
struct Snapshots {
    entries: Vec<Value>,
}

impl Snapshots {
    /// The index of `entry` (an envelope snapshot without its `s`).
    fn intern(&mut self, mut entry: Value) -> usize {
        if let Value::Object(m) = &mut entry {
            m.shift_remove("s");
        }
        if let Some(i) = self.entries.iter().position(|e| *e == entry) {
            return i;
        }
        self.entries.push(entry);
        self.entries.len() - 1
    }

    fn to_json(&self) -> Value {
        let list = self.entries.iter().enumerate().map(|(i, e)| {
            let mut m = Map::new();
            m.insert("s".into(), json!(i));
            if let Value::Object(fields) = e {
                m.extend(fields.clone());
            }
            Value::Object(m)
        });
        Value::Array(list.collect())
    }
}

/// Renumbers every `s` and `in` of `v` from a call's snapshot indexes to
/// the run's (`in` stays sorted, as the tools write it).
fn renumber(v: &mut Value, map: &[usize]) {
    let fix = |n: &Value| n.as_u64().and_then(|i| map.get(i as usize)).map_or_else(|| n.clone(), |g| json!(g));
    match v {
        Value::Object(m) => {
            for (k, x) in m.iter_mut() {
                match (k.as_str(), &mut *x) {
                    ("s", Value::Number(_)) => *x = fix(x),
                    ("in", Value::Array(list)) => {
                        list.iter_mut().for_each(|n| *n = fix(n));
                        list.sort_by_key(|n| n.as_u64());
                    }
                    _ => renumber(x, map),
                }
            }
        }
        Value::Array(list) => list.iter_mut().for_each(|x| renumber(x, map)),
        _ => {}
    }
}

/// The [`Host`] of a run: the engine, the scope, and what the calls read.
struct EngineHost<'a> {
    engine: &'a Engine,
    language: Language,
    scope: &'a Scope,
    deadline: Instant,
    visibility: Option<Visibility>,
    snapshots: Snapshots,
    partial: Vec<Value>,
    calls: u32,
    engine_time: Duration,
    deadline_hit: bool,
}

impl EngineHost<'_> {
    /// The name the script used: `rueca.searchSymbols` / `rueca.search_symbols`.
    fn name(&self, tool: &str) -> String {
        match self.language {
            Language::Js => format!("rueca.{}", rueca_codemode::to_camel_case(tool)),
            Language::Starlark => format!("rueca.{tool}"),
        }
    }

    /// The call's `results`, after recording its snapshots and paging.
    fn absorb(&mut self, tool: &str, mut json: Value) -> Value {
        let local: Vec<Value> = match json.get_mut("snapshots").map(Value::take) {
            Some(Value::Array(list)) => list,
            _ => Vec::new(),
        };
        let map: Vec<usize> = local.into_iter().map(|e| self.snapshots.intern(e)).collect();
        if json.get("truncated") == Some(&Value::Bool(true)) {
            let cursor = json.get("next_cursor").cloned().unwrap_or(Value::Null);
            self.partial.push(json!({"call": self.calls, "tool": tool, "next_cursor": cursor}));
        }
        let mut results = json.get_mut("results").map(Value::take).unwrap_or(Value::Null);
        renumber(&mut results, &map);
        results
    }
}

impl Host for EngineHost<'_> {
    fn call(&mut self, tool: &str, args: Value) -> Result<Value, String> {
        self.calls += 1;
        let name = self.name(tool);
        let args = scope_args_for_call(args, self.scope.repo.as_ref(), self.scope.ref_.as_ref())
            .map_err(|e| format!("{name}: {e}"))?;
        let opts = RespondOptions { deadline: Some(self.deadline), visibility: self.visibility.clone() };
        let started = Instant::now();
        let answer = self.engine.respond_with(tool, args, opts);
        self.engine_time += started.elapsed();
        match answer {
            Ok(response) => Ok(self.absorb(tool, response.json)),
            Err(err) => {
                self.deadline_hit |= err.downcast_ref::<DeadlineExceeded>().is_some();
                Err(format!("{name}: {}", script_error(&format!("{err:#}"))))
            }
        }
    }
}

/// A tool's error as a script reads it: `format` is not part of the script
/// API (it is refused), so it is dropped from the "valid arguments" list the
/// tools' argument errors print.
fn script_error(msg: &str) -> String {
    msg.replace("(valid arguments: format, ", "(valid arguments: ").replace(", format,", ",").replace(", format)", ")")
}

/// Runs `args.code` in `sandbox` against `engine` and builds the response.
pub(crate) fn run(
    sandbox: &(dyn Sandbox + Send + Sync),
    engine: &Engine,
    args: ExecuteArgs,
    opts: &RespondOptions,
    timing: bool,
) -> anyhow::Result<Response> {
    let started = Instant::now();
    let mut deadline = started + args.timeout;
    if let Some(d) = opts.deadline {
        deadline = deadline.min(d);
    }
    let mut host = EngineHost {
        engine,
        language: args.language,
        scope: &args.scope,
        deadline,
        visibility: opts.visibility.clone(),
        snapshots: Snapshots::default(),
        partial: Vec::new(),
        calls: 0,
        engine_time: Duration::ZERO,
        deadline_hit: false,
    };
    let limits = Limits { wall_time: deadline.saturating_duration_since(started), ..Limits::default() };
    let outcome = sandbox.run(&args.code, &mut host, &limits);
    let outcome = outcome.map_err(|e| script_failure(e, &host, &args, &limits))?;
    let mut json = assemble(outcome, &host, args.budget_tokens);
    if timing {
        json["timing"] = json!({"total_us": micros(started.elapsed()), "engine_us": micros(host.engine_time)});
    }
    let text = render(&json, args.budget_tokens);
    Ok(Response { json, text })
}

fn micros(d: Duration) -> u64 {
    u64::try_from(d.as_micros()).unwrap_or(u64::MAX)
}

/// The response of a successful run (without `timing`).
fn assemble(outcome: Outcome, host: &EngineHost<'_>, budget_tokens: usize) -> Value {
    let Outcome { mut result, logs, logs_truncated, calls } = outcome;
    let truncated = budget::fit(&mut result, budget_tokens);
    let mut m = Map::new();
    m.insert("result".into(), result);
    m.insert("logs".into(), json!(logs));
    m.insert("calls".into(), json!(calls));
    m.insert("snapshots".into(), host.snapshots.to_json());
    m.insert("truncated".into(), Value::Bool(truncated));
    if !host.partial.is_empty() {
        m.insert("partial".into(), Value::Array(host.partial.clone()));
    }
    if logs_truncated {
        m.insert("logs_truncated".into(), Value::Bool(true));
    }
    Value::Object(m)
}

/// The error a failed run reports: kind, line and message, plus what the
/// limit was for a limit error. A run that failed after an engine call
/// ran out of the script's time is a timeout.
fn script_failure(e: ScriptError, host: &EngineHost<'_>, args: &ExecuteArgs, limits: &Limits) -> anyhow::Error {
    let e = if host.deadline_hit && e.kind == ErrorKind::Runtime {
        ScriptError::new(
            ErrorKind::Timeout,
            format!("script exceeded its wall-time limit in an engine call ({})", e.message),
        )
        .with_line(e.line)
    } else {
        e
    };
    let hint = match e.kind {
        ErrorKind::Timeout => format!(" (timeout_ms {}; the maximum is {MAX_TIMEOUT_MS})", args.timeout.as_millis()),
        ErrorKind::Memory => format!(" (memory limit {} MiB)", limits.memory_bytes >> 20),
        ErrorKind::Syntax | ErrorKind::Runtime if args.language == Language::Starlark => {
            " (describe_api {\"language\": \"starlark\"} lists the API and the dialect)".to_string()
        }
        ErrorKind::Syntax | ErrorKind::Runtime => " (describe_api lists the API)".to_string(),
        _ => String::new(),
    };
    anyhow::anyhow!("{} {e}{hint}", args.language)
}

/// The text content of a response (see the module docs).
fn render(json: &Value, budget_tokens: usize) -> String {
    let mut out = String::new();
    let snaps = json["snapshots"].as_array().map(Vec::as_slice).unwrap_or_default();
    for s in snaps {
        if snaps.len() > 1 {
            let _ = write!(out, "[{}] ", s["s"]);
        }
        out.push_str(&header(s));
        out.push('\n');
    }
    out.push_str(&json["result"].to_string());
    out.push('\n');
    for line in json["logs"].as_array().map(Vec::as_slice).unwrap_or_default() {
        let _ = writeln!(out, "log: {}", line.as_str().unwrap_or_default().replace('\n', "\n     "));
    }
    if json["truncated"] == true {
        let _ = writeln!(out, "(truncated: the result was cut to budget_tokens {budget_tokens})");
    }
    if let Some(partial) = json.get("partial").and_then(Value::as_array) {
        let parts: Vec<String> = partial.iter().map(partial_text).collect();
        let _ = writeln!(out, "(partial: {})", parts.join("; "));
    }
    if json.get("logs_truncated") == Some(&Value::Bool(true)) {
        out.push_str("(logs truncated)\n");
    }
    out
}

fn partial_text(p: &Value) -> String {
    let base = format!("call {} {} was cut", p["call"], p["tool"].as_str().unwrap_or_default());
    match p["next_cursor"].as_str() {
        Some(c) => format!("{base}, next cursor \"{c}\""),
        None => base,
    }
}

/// `repo@commit12[+] (ref)`, as the tools' text.
fn header(s: &Value) -> String {
    let commit = s["commit"].as_str().unwrap_or_default();
    let plus = if s["overlay"] == true { "+" } else { "" };
    format!(
        "{}@{}{plus} ({})",
        s["repo"].as_str().unwrap_or_default(),
        rueca_query::snapshot::short_commit(commit),
        s["ref"].as_str().unwrap_or_default()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments() {
        let a = ExecuteArgs::parse(json!({"code": "return 1"})).unwrap();
        assert_eq!(a.language, Language::Js);
        assert_eq!((a.budget_tokens, a.timeout), (2000, Duration::from_millis(2000)));
        let a =
            ExecuteArgs::parse(json!({"code": "", "timeout_ms": 99999, "budget_tokens": 1, "language": "starlark"}))
                .unwrap();
        assert_eq!((a.budget_tokens, a.timeout, a.language), (100, Duration::from_secs(10), Language::Starlark));
        let e = ExecuteArgs::parse(json!({"code": "", "nope": 1})).unwrap_err().to_string();
        assert!(e.contains("unknown argument `nope` for `execute`"), "{e}");
        let e = ExecuteArgs::parse(json!({})).unwrap_err().to_string();
        assert!(e.contains("missing required argument `code`"), "{e}");
        let e = ExecuteArgs::parse(json!({"code": "", "language": "ruby"})).unwrap_err().to_string();
        assert!(e.contains("unknown language `ruby`"), "{e}");
        let e = ExecuteArgs::parse(json!({"code": "", "repo": 3})).unwrap_err().to_string();
        assert!(e.contains("`repo`"), "{e}");
    }

    #[test]
    fn call_arguments() {
        let repo = json!("github.com/a/b");
        let r = json!(["main", "pr/1"]);
        let got = scope_args_for_call(json!({"query": "x"}), Some(&repo), Some(&r)).unwrap();
        assert_eq!(got, json!({"query": "x", "budget_tokens": 1_000_000, "repo": repo, "ref": r}));
        // An id names its repository and commit; an explicit value wins.
        let got = scope_args_for_call(json!({"id": "x@1:a#b"}), Some(&repo), Some(&r)).unwrap();
        assert_eq!(got, json!({"id": "x@1:a#b", "budget_tokens": 1_000_000}));
        let got = scope_args_for_call(json!({"repo": "*", "budget_tokens": 300}), Some(&repo), None).unwrap();
        assert_eq!(got, json!({"repo": "*", "budget_tokens": 300}));
        assert!(scope_args_for_call(json!({"format": "verbose"}), None, None).is_err());
        assert!(scope_args_for_call(json!({"format": "compact"}), None, None).is_ok());
    }

    #[test]
    fn argument_errors_do_not_offer_format() {
        let msg =
            "unknown argument `nme` for `trace` (valid arguments: id, name, repo, ref, format, budget_tokens, cursor)";
        assert_eq!(
            script_error(msg),
            "unknown argument `nme` for `trace` (valid arguments: id, name, repo, ref, budget_tokens, cursor)"
        );
        assert_eq!(script_error("(valid arguments: format, cursor)"), "(valid arguments: cursor)");
        assert_eq!(script_error("(valid arguments: cursor, format)"), "(valid arguments: cursor)");
        assert_eq!(script_error("no symbol found for name `format`"), "no symbol found for name `format`");
    }

    #[test]
    fn snapshots_are_merged_and_renumbered() {
        let mut snaps = Snapshots::default();
        let a = json!({"s": 0, "repo": "a", "ref": "main", "commit": "1", "overlay": false});
        let b = json!({"s": 1, "repo": "b", "ref": "main", "commit": "2", "overlay": false});
        assert_eq!(snaps.intern(b.clone()), 0);
        let map = vec![snaps.intern(a), snaps.intern(b)];
        assert_eq!(map, vec![1, 0]);
        let mut r = json!([{"s": 0, "x": [{"s": 1, "in": [0, 1]}]}, [0, 1, "CALLS", {"in": [1]}]]);
        renumber(&mut r, &map);
        assert_eq!(r, json!([{"s": 1, "x": [{"s": 0, "in": [0, 1]}]}, [0, 1, "CALLS", {"in": [0]}]]));
        assert_eq!(snaps.to_json()[1], json!({"s": 1, "repo": "a", "ref": "main", "commit": "1", "overlay": false}));
    }
}
