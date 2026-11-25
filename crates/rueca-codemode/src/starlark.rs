//! Starlark through `starlark-rust` (Meta).
//!
//! Each run parses the script and evaluates it in a fresh module on a fresh
//! heap; the answer is the module's global `result` (`null` when unset).
//! The dialect is standard Starlark plus top-level statements and f-strings,
//! without `load`. The built-ins are the standard ones plus `struct`, `map`,
//! `filter`, `partial`, `set`, `json` and `print` (captured into the logs);
//! there is no clock, randomness, file or network access in the language.
//! `rueca.<tool>(**kwargs)` takes keyword arguments only and returns the
//! tool's JSON result as Starlark values.
//!
//! # Limits
//!
//! - Wall time: the evaluator's cancellation hook checks the deadline every
//!   1,000 "ticks" (function calls and loop iterations). A single built-in
//!   operation runs to its end before the next check: `"x" * (1 << 30)`
//!   took 3.1 s against a 2 s limit in phase 0.
//! - Memory: `Evaluator::set_max_heap_size` compares the Starlark heap's
//!   peak with the limit at the same 1,000-tick checks, and the run checks
//!   it once more at the end. It is **not** enforced on allocation, and it
//!   cannot be in-process:
//!   - one built-in operation allocates its whole result before any check
//!     (`"x" * (1 << 30)` peaked at 3 GiB of resident memory under a
//!     64 MiB limit); repetition counts are `i32`, so `("x" * 131072) *
//!     2147483647` asks for 256 TiB, the allocation fails and the
//!     **process aborts** (SIGABRT), as every failed Rust allocation does;
//!   - data a value keeps on the Rust heap (dictionaries' tables) is not
//!     counted: a dictionary grew to 1.2 GiB resident in 2 s under a
//!     64 MiB limit and was stopped only by the deadline.
//!
//!   Enforcing it needs a separate process with OS limits (RFC 0009 §7.3,
//!   §13).
//! - Recursion: at most [`MAX_CALL_DEPTH`] Starlark frames.

use std::cell::{Cell, RefCell};
use std::fmt;
use std::time::Instant;

use allocative::Allocative;
use serde_json::Value;
use starlark::PrintHandler;
use starlark::any::ProvidesStaticType;
use starlark::environment::{Globals, GlobalsBuilder, LibraryExtension, Module};
use starlark::eval::{Arguments, Evaluator};
use starlark::starlark_simple_value;
use starlark::syntax::{AstModule, Dialect};
use starlark::values::{NoSerialize, StarlarkPagable, StarlarkValue, Value as StarlarkValueRef, starlark_value};

use crate::state::{RunState, check_size};
use crate::{ErrorKind, Host, Language, Limits, Outcome, Sandbox, ScriptError};

/// Most nested Starlark calls a script may make.
pub const MAX_CALL_DEPTH: usize = 200;

/// The file name scripts are parsed under.
const SCRIPT_NAME: &str = "script.star";

/// A Starlark sandbox exposing a fixed set of tools.
pub struct StarlarkSandbox {
    globals: Globals,
}

impl fmt::Debug for StarlarkSandbox {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StarlarkSandbox").finish_non_exhaustive()
    }
}

impl StarlarkSandbox {
    /// A sandbox whose `rueca` namespace has one function per tool, under
    /// its snake_case name.
    pub fn new(tools: &[&str]) -> StarlarkSandbox {
        use LibraryExtension::{Filter, Json, Map, Partial, Print, SetType, StructType};
        let globals = GlobalsBuilder::extended_by(&[StructType, Map, Filter, Partial, Print, Json, SetType])
            .with_namespace("rueca", |b| {
                for tool in tools {
                    b.set(tool, ToolFunction { name: (*tool).to_string() });
                }
            })
            .build();
        StarlarkSandbox { globals }
    }
}

/// Standard Starlark, plus top-level `for` / `if` and f-strings, minus
/// `load`.
fn dialect() -> Dialect {
    Dialect { enable_load: false, enable_top_level_stmt: true, enable_f_strings: true, ..Dialect::Standard }
}

impl Sandbox for StarlarkSandbox {
    fn language(&self) -> Language {
        Language::Starlark
    }

    fn run(&self, script: &str, host: &mut dyn Host, limits: &Limits) -> Result<Outcome, ScriptError> {
        check_size(script, limits)?;
        let state = RefCell::new(RunState::new(limits, Instant::now()));
        let timed_out = Cell::new(false);
        let ast = AstModule::parse(SCRIPT_NAME, script.to_owned(), &dialect())
            .map_err(|e| script_error(ErrorKind::Syntax, &e))?;
        let mut cx = HostContext { host, state: &state };
        let ended = Module::with_temp_heap(|module| evaluate(&module, ast, &self.globals, limits, &mut cx, &timed_out));
        let state = state.into_inner();
        if timed_out.get() {
            return Err(ScriptError::new(ErrorKind::Timeout, "script exceeded its wall-time limit"));
        }
        if let Some(e) = state.call_limit_error() {
            return Err(e);
        }
        Ok(state.finish(ended?))
    }
}

/// Evaluates `ast` in `module` under the limits and reads `result`.
fn evaluate(
    module: &Module<'_>,
    ast: AstModule,
    globals: &Globals,
    limits: &Limits,
    cx: &mut HostContext<'_>,
    timed_out: &Cell<bool>,
) -> Result<Value, ScriptError> {
    let deadline = cx.state.borrow().deadline();
    let logs = Logs(cx.state);
    let mut eval = Evaluator::new(module);
    eval.set_max_callstack_size(MAX_CALL_DEPTH).map_err(internal)?;
    eval.set_max_heap_size(limits.memory_bytes.max(1)).map_err(internal)?;
    eval.set_check_cancelled(Box::new(|| {
        let late = Instant::now() >= deadline;
        if late {
            timed_out.set(true);
        }
        late
    }));
    eval.set_print_handler(&logs);
    eval.extra_mut = Some(cx);
    if let Err(e) = eval.eval_module(ast, globals) {
        return Err(script_error(runtime_kind(&e), &e));
    }
    drop(eval);
    // The periodic check may not have run after the last allocation.
    let peak = module.heap().peak_allocated_bytes();
    if peak > limits.memory_bytes {
        return Err(ScriptError::new(
            ErrorKind::Memory,
            format!("Starlark heap reached {peak} bytes; the limit is {}", limits.memory_bytes),
        ));
    }
    match module.get("result") {
        None => Ok(Value::Null),
        Some(v) => {
            v.to_json_value().map_err(|e| ScriptError::new(ErrorKind::Result, format!("`result` is not JSON: {e}")))
        }
    }
}

/// Undefined names are found before the script runs (like a syntax
/// error); memory-limit errors are reported by the evaluator as plain
/// errors.
fn runtime_kind(e: &starlark::Error) -> ErrorKind {
    match e.kind() {
        starlark::ErrorKind::Parser(_) | starlark::ErrorKind::Scope(_) => ErrorKind::Syntax,
        _ if e.to_string().contains("Heap memory limit") => ErrorKind::Memory,
        _ => ErrorKind::Runtime,
    }
}

fn script_error(kind: ErrorKind, e: &starlark::Error) -> ScriptError {
    let line = e.span().map(|s| s.resolve_span().begin.line + 1).and_then(|l| u32::try_from(l).ok());
    ScriptError::new(kind, e.without_diagnostic().to_string()).with_line(line)
}

fn internal(e: impl fmt::Display) -> ScriptError {
    ScriptError::new(ErrorKind::Internal, format!("Starlark engine: {e}"))
}

/// What `rueca.*` functions reach through the evaluator's `extra_mut`.
#[derive(ProvidesStaticType)]
struct HostContext<'a> {
    host: &'a mut dyn Host,
    state: &'a RefCell<RunState>,
}

/// `print` into the run's logs.
struct Logs<'a>(&'a RefCell<RunState>);

impl PrintHandler for Logs<'_> {
    fn println(&self, text: &str) -> starlark::Result<()> {
        self.0.borrow_mut().log(text);
        Ok(())
    }
}

/// An error a host call returned, as seen by the script.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct HostError(String);

/// One `rueca.<tool>` function.
#[derive(Debug, ProvidesStaticType, NoSerialize, StarlarkPagable, Allocative)]
struct ToolFunction {
    name: String,
}
starlark_simple_value!(ToolFunction);

impl fmt::Display for ToolFunction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "rueca.{}", self.name)
    }
}

#[starlark_value(type = "function")]
impl<'v> StarlarkValue<'v> for ToolFunction {
    fn invoke(
        &self,
        _me: StarlarkValueRef<'v>,
        args: &Arguments<'v, '_>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<StarlarkValueRef<'v>> {
        args.no_positional_args(eval.heap())?;
        let mut map = serde_json::Map::new();
        for (name, value) in args.names_map()? {
            map.insert(name.as_str().to_owned(), value.to_json_value().map_err(starlark::Error::new_value)?);
        }
        let cx = eval
            .extra_mut
            .as_mut()
            .and_then(|e| e.downcast_mut::<HostContext>())
            .ok_or_else(|| starlark::Error::new_other(HostError("rueca is not available here".into())))?;
        let result = cx
            .state
            .borrow_mut()
            .call(&mut *cx.host, &self.name, Value::Object(map))
            .map_err(|m| starlark::Error::new_native(HostError(m)))?;
        Ok(eval.heap().alloc(result))
    }
}
