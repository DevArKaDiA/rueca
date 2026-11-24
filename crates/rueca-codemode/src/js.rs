//! JavaScript through QuickJS (`rquickjs`, QuickJS-ng).
//!
//! Each run creates a fresh runtime and context with only the intrinsics a
//! script needs (base objects, `eval` support for compiling the script,
//! RegExp, JSON, Map/Set, Promise): no `Date`, `performance`, `WeakRef`,
//! typed arrays, module loader, `std` or `os` modules. A prelude then
//! removes the global `eval`, blocks the `Function` constructors (direct and
//! through `(async) function* () {}.constructor`) and `Math.random`, and
//! installs `rueca` and `console`.
//!
//! `rueca.<tool>(args)` enqueues the call and returns a pending `Promise`;
//! the run loop drains QuickJS's job queue, serves the queued calls through
//! the [`Host`] in order and resolves their promises, until the script's own
//! promise settles. Values cross as JSON text.
//!
//! # Limits
//!
//! - Wall time: QuickJS's interrupt handler (polled about every 10,000
//!   bytecode operations, and inside some built-ins such as `indexOf`)
//!   checks the deadline and raises an uncatchable error; the run loop
//!   checks it between jobs. A single native call that does not poll runs
//!   to its end: `Array.prototype.copyWithin.call({length: 2 ** 40}, 0, 1)`
//!   ran for over ten minutes in phase 0, so the prelude wraps every
//!   `Array.prototype` method to refuse lengths over [`MAX_ARRAY_LENGTH`].
//!   Other non-polling built-ins are bounded by the memory limit or by that
//!   length (RFC 0009 §13).
//! - Memory: QuickJS's allocator limit, enforced on every allocation
//!   (`JS_SetMemoryLimit`); exceeding it raises a catchable "out of memory"
//!   error, which still ends in [`ErrorKind::Memory`] if the script ends
//!   with it, and the deadline stops a script that keeps catching it.
//! - Stack: `JS_SetMaxStackSize` ([`Limits::stack_bytes`]).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Instant;

use rquickjs::context::EvalOptions;
use rquickjs::context::intrinsic::{Eval, Json, MapSet, Promise as PromiseIntrinsic, RegExp, RegExpCompiler};
use rquickjs::promise::PromiseState;
use rquickjs::{Context, Ctx, Exception, Function, Persistent, Promise, Runtime, Value as JsValue};
use serde_json::Value;

use crate::state::{RunState, check_size};
use crate::{ErrorKind, Host, Language, Limits, Outcome, Sandbox, ScriptError, to_camel_case};

/// Longest array or array-like the generic `Array.prototype` methods accept
/// (a dense array this long is already 32 MiB of values).
pub const MAX_ARRAY_LENGTH: u32 = 1 << 22;

/// The file name scripts are compiled under (it appears in stack traces).
const SCRIPT_NAME: &str = "script";

/// A JavaScript sandbox exposing a fixed set of tools.
#[derive(Debug, Clone)]
pub struct JsSandbox {
    prelude: String,
}

impl JsSandbox {
    /// A sandbox whose `rueca` object has one function per tool, named in
    /// camelCase (`search_symbols` → `rueca.searchSymbols`).
    pub fn new(tools: &[&str]) -> JsSandbox {
        JsSandbox { prelude: prelude(tools) }
    }
}

impl Sandbox for JsSandbox {
    fn language(&self) -> Language {
        Language::Js
    }

    fn run(&self, script: &str, host: &mut dyn Host, limits: &Limits) -> Result<Outcome, ScriptError> {
        check_size(script, limits)?;
        let started = Instant::now();
        let state = Rc::new(RefCell::new(RunState::new(limits, started)));
        let timed_out = Rc::new(Cell::new(false));
        let rt = runtime(limits, state.borrow().deadline(), timed_out.clone())?;
        let ctx = Context::builder()
            .with::<(Eval, RegExpCompiler, RegExp, Json, MapSet, PromiseIntrinsic)>()
            .build(&rt)
            .map_err(internal)?;
        let run = Run { state: state.clone(), queue: Rc::default(), timed_out: timed_out.clone() };
        let result = ctx.with(|ctx| run.execute(&ctx, &self.prelude, script, host));
        drop(run);
        drop(ctx);
        drop(rt);
        let state = Rc::try_unwrap(state).map_err(|_| internal("run state still shared"))?.into_inner();
        finish(result, state, timed_out.get())
    }
}

/// A fresh runtime with the memory, stack and deadline limits.
fn runtime(limits: &Limits, deadline: Instant, timed_out: Rc<Cell<bool>>) -> Result<Runtime, ScriptError> {
    let rt = Runtime::new().map_err(internal)?;
    rt.set_memory_limit(limits.memory_bytes);
    rt.set_max_stack_size(limits.stack_bytes);
    rt.set_interrupt_handler(Some(Box::new(move || {
        let late = Instant::now() >= deadline;
        if late {
            timed_out.set(true);
        }
        late
    })));
    Ok(rt)
}

/// Global, non-strict code compiled under `filename`.
fn options(filename: &str) -> EvalOptions {
    let mut opts = EvalOptions::default();
    opts.global = true;
    opts.filename = Some(filename.to_string());
    opts
}

/// A host call waiting for the run loop.
struct Pending {
    tool: String,
    args: String,
    resolve: Persistent<Function<'static>>,
    reject: Persistent<Function<'static>>,
    /// Created at the call site, so a failed call reports the script line.
    error: Persistent<Exception<'static>>,
}

/// How a script ended inside the interpreter, before classification.
enum Ended {
    Value(Value),
    Thrown(Thrown),
    /// The script's promise never settled and nothing was left to run.
    Stalled,
}

/// A thrown value, flattened while the context is alive.
struct Thrown {
    name: String,
    message: String,
    line: Option<u32>,
    /// Thrown while compiling (a syntax error) rather than while running.
    compiling: bool,
}

/// The shared pieces of one run.
struct Run {
    state: Rc<RefCell<RunState>>,
    queue: Rc<RefCell<Vec<Pending>>>,
    timed_out: Rc<Cell<bool>>,
}

impl Run {
    fn execute(&self, ctx: &Ctx<'_>, prelude: &str, script: &str, host: &mut dyn Host) -> Ended {
        let ended = self.start(ctx, prelude, script).and_then(|promise| self.drive(ctx, &promise, host));
        // Persistent handles must not outlive the context.
        self.queue.borrow_mut().clear();
        ended.unwrap_or_else(|e| e)
    }

    /// Installs the natives, runs the prelude and starts the script.
    fn start<'js>(&self, ctx: &Ctx<'js>, prelude: &str, script: &str) -> Result<Promise<'js>, Ended> {
        self.install(ctx).map_err(|e| Ended::Thrown(thrown(ctx, e, false)))?;
        ctx.eval_with_options::<(), _>(prelude, options("prelude"))
            .map_err(|e| Ended::Thrown(thrown(ctx, e, false)))?;
        // Same first line as the script, so line numbers match.
        let wrapped = format!("(async () => {{{script}\n}})()");
        ctx.eval_with_options::<Promise, _>(wrapped, options(SCRIPT_NAME))
            .map_err(|e| Ended::Thrown(thrown(ctx, e, true)))
    }

    /// `__rueca_call(tool, argsJson)` and `__rueca_log(line)`; the prelude
    /// captures and deletes them.
    fn install<'js>(&self, ctx: &Ctx<'js>) -> rquickjs::Result<()> {
        let globals = ctx.globals();
        let (state, queue) = (self.state.clone(), self.queue.clone());
        let call = Function::new(ctx.clone(), move |ctx: Ctx<'js>, tool: String, args: String| {
            enqueue(&ctx, &state, &queue, tool, args)
        })?;
        globals.set("__rueca_call", call)?;
        let state = self.state.clone();
        let log = Function::new(ctx.clone(), move |line: String| state.borrow_mut().log(&line))?;
        globals.set("__rueca_log", log)?;
        Ok(())
    }

    /// Runs jobs and serves host calls until the script's promise settles.
    fn drive<'js>(&self, ctx: &Ctx<'js>, promise: &Promise<'js>, host: &mut dyn Host) -> Result<Ended, Ended> {
        let deadline = self.state.borrow().deadline();
        loop {
            while ctx.execute_pending_job() {
                if Instant::now() >= deadline {
                    self.timed_out.set(true);
                    return Ok(Ended::Stalled);
                }
            }
            if promise.state() != PromiseState::Pending {
                return Ok(settled(ctx, promise));
            }
            let batch = std::mem::take(&mut *self.queue.borrow_mut());
            if batch.is_empty() {
                return Ok(Ended::Stalled);
            }
            for pending in batch {
                serve(ctx, host, pending).map_err(|e| Ended::Thrown(thrown(ctx, e, false)))?;
            }
        }
    }
}

/// The native behind every `rueca.*` function: counts the call and
/// returns a promise the run loop settles.
fn enqueue<'js>(
    ctx: &Ctx<'js>,
    state: &RefCell<RunState>,
    queue: &RefCell<Vec<Pending>>,
    tool: String,
    args: String,
) -> rquickjs::Result<Promise<'js>> {
    if let Err(message) = state.borrow_mut().admit() {
        return Err(Exception::throw_range(ctx, &message));
    }
    let (promise, resolve, reject) = Promise::new(ctx)?;
    let resolve = Persistent::save(ctx, resolve);
    let reject = Persistent::save(ctx, reject);
    let error = Persistent::save(ctx, Exception::from_message(ctx.clone(), "")?);
    queue.borrow_mut().push(Pending { tool, args, resolve, reject, error });
    Ok(promise)
}

/// Runs one queued call through the host and settles its promise.
fn serve(ctx: &Ctx<'_>, host: &mut dyn Host, pending: Pending) -> rquickjs::Result<()> {
    let args = serde_json::from_str(&pending.args).unwrap_or(Value::Null);
    let resolve = pending.resolve.restore(ctx)?;
    let reject = pending.reject.restore(ctx)?;
    match host.call(&pending.tool, args) {
        Ok(value) => resolve.call::<_, ()>((ctx.json_parse(value.to_string())?,)),
        Err(message) => {
            let error = pending.error.restore(ctx)?;
            error.as_object().set("message", message)?;
            reject.call::<_, ()>((error,))
        }
    }
}

/// The settled script promise as JSON, or what it was rejected with.
fn settled<'js>(ctx: &Ctx<'js>, promise: &Promise<'js>) -> Ended {
    match promise.result::<JsValue>() {
        Some(Ok(value)) => to_json(ctx, value),
        Some(Err(e)) => Ended::Thrown(thrown(ctx, e, false)),
        None => Ended::Stalled,
    }
}

/// `JSON.stringify` of the answer, parsed back (`undefined` → `null`).
fn to_json<'js>(ctx: &Ctx<'js>, value: JsValue<'js>) -> Ended {
    match ctx.json_stringify(value) {
        Ok(None) => Ended::Value(Value::Null),
        Ok(Some(text)) => match text.to_string().map(|t| serde_json::from_str(&t)) {
            Ok(Ok(json)) => Ended::Value(json),
            _ => Ended::Thrown(Thrown {
                name: "ResultError".into(),
                message: "the returned value is not JSON".into(),
                line: None,
                compiling: false,
            }),
        },
        Err(e) => {
            let mut t = thrown(ctx, e, false);
            t.name = format!("ResultError: {}", t.name);
            Ended::Thrown(t)
        }
    }
}

/// Flattens the pending exception of `ctx` after `error`.
fn thrown(ctx: &Ctx<'_>, error: rquickjs::Error, compiling: bool) -> Thrown {
    if !error.is_exception() {
        return Thrown { name: "Error".into(), message: error.to_string(), line: None, compiling };
    }
    let value = ctx.catch();
    let Some(object) = value.as_object() else {
        let message = if value.is_null() { "out of memory".to_string() } else { describe(ctx, value) };
        return Thrown { name: "Error".into(), message, line: None, compiling };
    };
    let name = object.get::<_, String>("name").unwrap_or_else(|_| "Error".into());
    let message = object.get::<_, String>("message").unwrap_or_default();
    let line = object.get::<_, String>("stack").ok().as_deref().and_then(script_line);
    Thrown { name, message, line, compiling }
}

/// A thrown non-object (a string, a number), as text.
fn describe<'js>(ctx: &Ctx<'js>, value: JsValue<'js>) -> String {
    if let Some(s) = value.as_string().and_then(|s| s.to_string().ok()) {
        return s;
    }
    match ctx.json_stringify(value) {
        Ok(Some(s)) => s.to_string().unwrap_or_default(),
        _ => "a non-JSON value was thrown".to_string(),
    }
}

/// The first `script:LINE` frame of a QuickJS stack trace.
fn script_line(stack: &str) -> Option<u32> {
    let marker = format!("{SCRIPT_NAME}:");
    stack.match_indices(&marker).find_map(|(at, _)| {
        let standalone = stack[..at].chars().next_back().is_none_or(|c| !c.is_alphanumeric() && c != '_');
        let digits: String = stack[at + marker.len()..].chars().take_while(char::is_ascii_digit).collect();
        if standalone { digits.parse().ok() } else { None }
    })
}

/// Classifies how the run ended: limits first (a timed-out or over-limit
/// script ends in whatever error the interpreter raised on the way out).
fn finish(ended: Ended, state: RunState, timed_out: bool) -> Result<Outcome, ScriptError> {
    if timed_out {
        return Err(ScriptError::new(ErrorKind::Timeout, "script exceeded its wall-time limit"));
    }
    if let Some(e) = state.call_limit_error() {
        return Err(e);
    }
    match ended {
        Ended::Value(value) => Ok(state.finish(value)),
        Ended::Thrown(t) => Err(classify(t)),
        Ended::Stalled => Err(ScriptError::new(
            ErrorKind::Runtime,
            "the script awaited a promise that never settles (only rueca.* calls resolve)",
        )),
    }
}

fn classify(t: Thrown) -> ScriptError {
    let kind = if t.message.starts_with("out of memory") {
        ErrorKind::Memory
    } else if t.compiling && t.name == "SyntaxError" {
        ErrorKind::Syntax
    } else if t.name.starts_with("ResultError") {
        ErrorKind::Result
    } else {
        ErrorKind::Runtime
    };
    let message = if t.message.is_empty() { t.name } else { format!("{}: {}", t.name, t.message) };
    ScriptError::new(kind, message).with_line(t.line)
}

fn internal(e: impl std::fmt::Display) -> ScriptError {
    ScriptError::new(ErrorKind::Internal, format!("JavaScript engine: {e}"))
}

/// The JavaScript run before every script: hides the natives behind
/// `rueca` and `console`, and removes the non-deterministic and
/// code-from-string built-ins.
fn prelude(tools: &[&str]) -> String {
    let functions: Vec<String> =
        tools.iter().map(|t| format!("{}: tool({})", to_camel_case(t), Value::from(*t))).collect();
    format!(
        r#"(() => {{
  "use strict";
  const call = globalThis.__rueca_call;
  const log = globalThis.__rueca_log;
  delete globalThis.__rueca_call;
  delete globalThis.__rueca_log;
  const tool = (name) => (args) => call(name, JSON.stringify(args === undefined ? {{}} : args));
  const show = (v) => {{
    if (typeof v === "string") return v;
    try {{ const s = JSON.stringify(v); return s === undefined ? String(v) : s; }} catch (e) {{ return String(v); }}
  }};
  const blocked = (what) => function () {{ throw new TypeError(what + " is not available in code mode"); }};
  const hide = (obj, key, value) => Object.defineProperty(obj, key, {{ value, writable: false, configurable: false, enumerable: false }});
  hide(globalThis, "rueca", Object.freeze({{ {functions} }}));
  hide(globalThis, "console", Object.freeze({{ log: (...a) => log(a.map(show).join(" ")) }}));
  delete globalThis.eval;
  hide(Math, "random", blocked("Math.random"));
  for (const f of [function () {{}}, async function () {{}}, function* () {{}}, async function* () {{}}]) {{
    const ctor = blocked("the Function constructor");
    ctor.prototype = Object.getPrototypeOf(f);
    hide(Object.getPrototypeOf(f), "constructor", ctor);
  }}
  const F = blocked("the Function constructor");
  F.prototype = Function.prototype;
  hide(globalThis, "Function", F);
  // Some generic Array built-ins (copyWithin, among others) loop in C
  // without polling the interrupt handler; bound the length they may walk.
  const proto = Array.prototype;
  for (const key of Object.getOwnPropertyNames(proto)) {{
    const orig = proto[key];
    if (key === "constructor" || typeof orig !== "function") continue;
    const guarded = {{ [key](...a) {{
      if (this != null && Object(this).length > {max_length}) throw new RangeError("array-like length over {max_length} in code mode");
      return orig.apply(this, a);
    }} }}[key];
    Object.defineProperty(proto, key, {{ value: guarded, writable: true, configurable: true, enumerable: false }});
  }}
}})();"#,
        functions = functions.join(", "),
        max_length = MAX_ARRAY_LENGTH,
    )
}

#[cfg(test)]
mod tests {
    use super::script_line;

    #[test]
    fn line_from_stack() {
        assert_eq!(script_line("    at <anonymous> (script:3:7)\n"), Some(3));
        assert_eq!(script_line("no line"), None);
        assert_eq!(script_line("at f (eval_script:7:1)\n at <anonymous> (script:2:1)"), Some(2));
    }
}
