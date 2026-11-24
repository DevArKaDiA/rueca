//! Code mode for Rueca (RFC 0009, experimental): run a short script against
//! the Rueca API inside a sandbox and return only what the script returns.
//!
//! # Design
//!
//! - [`Sandbox`] is the one seam between code mode and a script engine:
//!   [`Sandbox::run`] takes the script, a [`Host`] and the [`Limits`], and
//!   gives back an [`Outcome`] or a [`ScriptError`]. Nothing here depends on
//!   the query engine: the [`Host`] is the only way out of the sandbox, and
//!   code mode's server side implements it over `rueca_query::Engine`.
//! - Two engines ship behind cargo features so the experiment can compare
//!   them (RFC 0009 §6): [`js::JsSandbox`] (feature `js`, QuickJS through
//!   `rquickjs`) and [`starlark::StarlarkSandbox`] (feature `starlark`,
//!   `starlark-rust`). Both are default-on; dropping one is removing its
//!   feature.
//! - A sandbox is built once from the API's tool names (snake_case, as in
//!   `rueca_query::TOOL_NAMES`) and runs any number of scripts, each in a
//!   fresh interpreter: no state survives between runs.
//!
//! # Script shapes
//!
//! JavaScript: the script is the body of a top-level `async` function; its
//! `return` value is the answer. `rueca.<camelCaseTool>(args)` takes one
//! object and returns a `Promise` of the tool's JSON result;
//! `console.log(...)` is captured into [`Outcome::logs`].
//!
//! ```js
//! const hits = await rueca.searchSymbols({ query: "parse", limit: 5 });
//! return hits.map(h => h.name);
//! ```
//!
//! Starlark: the answer is the global `result`. `rueca.<snake_case_tool>(**kwargs)`
//! takes keyword arguments only (a dict can be spread with `**args`) and
//! returns the tool's JSON result; `print(...)` is captured.
//!
//! ```python
//! hits = rueca.search_symbols(query = "parse", limit = 5)
//! result = [h["name"] for h in hits]
//! ```
//!
//! # Limits
//!
//! Every run is bounded by [`Limits`]: wall time, interpreter memory, host
//! calls, captured log bytes and script size. Where an engine cannot enforce
//! a limit exactly, its module docs say which case escapes and by how much.

#[cfg(feature = "js")]
pub mod js;
#[cfg(feature = "starlark")]
pub mod starlark;
#[cfg(any(feature = "js", feature = "starlark"))]
mod state;

use std::fmt;
use std::time::Duration;

use serde_json::Value;

/// The way out of the sandbox: every `rueca.*` call of a script lands here,
/// with the tool's snake_case name and its JSON arguments.
pub trait Host {
    /// Runs `tool` with `args` and returns its JSON result, or an error
    /// message the script sees as a thrown error (JavaScript) or a failed
    /// call (Starlark).
    fn call(&mut self, tool: &str, args: Value) -> Result<Value, String>;
}

/// Bounds on one script run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limits {
    /// Wall time from the start of the run, parsing included.
    pub wall_time: Duration,
    /// Interpreter memory (JavaScript: QuickJS's allocator limit; Starlark:
    /// the Starlark heap, checked periodically; see each module).
    pub memory_bytes: usize,
    /// Native stack the JavaScript interpreter may use (Starlark bounds
    /// recursion by frames instead, [`starlark::MAX_CALL_DEPTH`] when built
    /// with the `starlark` feature).
    pub stack_bytes: usize,
    /// Host calls per script; the call over the limit fails the run.
    pub max_calls: u32,
    /// Bytes of captured log lines; later output is dropped and
    /// [`Outcome::logs_truncated`] set.
    pub max_log_bytes: usize,
    /// Script size, checked before parsing.
    pub max_script_bytes: usize,
}

impl Default for Limits {
    /// RFC 0009 §7.2's defaults: 2 s, 64 MiB, 200 calls, 4 KiB of logs,
    /// 16 KiB of script; 512 KiB of JavaScript stack.
    fn default() -> Limits {
        Limits {
            wall_time: Duration::from_secs(2),
            memory_bytes: 64 << 20,
            stack_bytes: 512 << 10,
            max_calls: 200,
            max_log_bytes: 4 << 10,
            max_script_bytes: 16 << 10,
        }
    }
}

/// What a successful run gives back.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    /// The script's answer as JSON (`null` when it returned nothing).
    pub result: Value,
    /// Captured `console.log` / `print` lines, in order, within
    /// [`Limits::max_log_bytes`].
    pub logs: Vec<String>,
    /// Some output was dropped by the log cap.
    pub logs_truncated: bool,
    /// Host calls the script made.
    pub calls: u32,
}

/// Why a run failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    /// The script does not compile: a syntax error, or (Starlark) a name
    /// that is not defined, which is checked before the script runs.
    Syntax,
    /// The script threw (or failed) while running, including a host error it
    /// did not handle, a call with bad arguments and stack exhaustion.
    Runtime,
    /// [`Limits::wall_time`] passed.
    Timeout,
    /// [`Limits::memory_bytes`] was exceeded.
    Memory,
    /// The script made more than [`Limits::max_calls`] host calls.
    CallLimit,
    /// The script exceeds [`Limits::max_script_bytes`].
    ScriptTooLarge,
    /// The answer cannot be represented as JSON.
    Result,
    /// The engine itself failed (not the script's fault).
    Internal,
}

impl ErrorKind {
    /// A stable lowercase name, for envelopes and logs.
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorKind::Syntax => "syntax",
            ErrorKind::Runtime => "runtime",
            ErrorKind::Timeout => "timeout",
            ErrorKind::Memory => "memory",
            ErrorKind::CallLimit => "call_limit",
            ErrorKind::ScriptTooLarge => "script_too_large",
            ErrorKind::Result => "result",
            ErrorKind::Internal => "internal",
        }
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A failed run: what kind of failure, the interpreter's message and the
/// script line (1-based) when the engine reports one.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{kind} error{}: {message}", line.map(|l| format!(" at line {l}")).unwrap_or_default())]
pub struct ScriptError {
    pub kind: ErrorKind,
    pub message: String,
    pub line: Option<u32>,
}

impl ScriptError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> ScriptError {
        ScriptError { kind, message: message.into(), line: None }
    }

    pub fn with_line(mut self, line: Option<u32>) -> ScriptError {
        self.line = line;
        self
    }
}

/// A script engine that runs one script per call in a fresh interpreter.
pub trait Sandbox {
    /// The language this sandbox runs.
    fn language(&self) -> Language;

    /// Runs `script`, sending every `rueca.*` call to `host`, within
    /// `limits`.
    fn run(&self, script: &str, host: &mut dyn Host, limits: &Limits) -> Result<Outcome, ScriptError>;
}

/// A code-mode language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    /// JavaScript (QuickJS).
    Js,
    /// Starlark.
    Starlark,
}

impl Language {
    /// The name used in the `language` argument: `js` or `starlark`.
    pub fn as_str(self) -> &'static str {
        match self {
            Language::Js => "js",
            Language::Starlark => "starlark",
        }
    }

    /// Parses a `language` argument (`js`, `javascript`, `starlark`).
    pub fn parse(name: &str) -> Option<Language> {
        match name {
            "js" | "javascript" => Some(Language::Js),
            "starlark" => Some(Language::Starlark),
            _ => None,
        }
    }

    /// The languages this build has, in a fixed order.
    pub fn available() -> &'static [Language] {
        &[
            #[cfg(feature = "js")]
            Language::Js,
            #[cfg(feature = "starlark")]
            Language::Starlark,
        ]
    }
}

impl fmt::Display for Language {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A sandbox for `language` exposing `tools` (snake_case names), or `None`
/// when this build does not include that language.
#[cfg_attr(not(any(feature = "js", feature = "starlark")), allow(unused_variables))]
pub fn sandbox(language: Language, tools: &[&str]) -> Option<Box<dyn Sandbox + Send + Sync>> {
    match language {
        #[cfg(feature = "js")]
        Language::Js => Some(Box::new(js::JsSandbox::new(tools))),
        #[cfg(feature = "starlark")]
        Language::Starlark => Some(Box::new(starlark::StarlarkSandbox::new(tools))),
        #[allow(unreachable_patterns)]
        _ => None,
    }
}

/// `search_symbols` → `searchSymbols`: the JavaScript name of a tool.
pub fn to_camel_case(snake: &str) -> String {
    let mut out = String::with_capacity(snake.len());
    let mut upper = false;
    for c in snake.chars() {
        if c == '_' {
            upper = true;
        } else if upper {
            out.extend(c.to_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::to_camel_case;

    #[test]
    fn camel_case() {
        assert_eq!(to_camel_case("search_symbols"), "searchSymbols");
        assert_eq!(to_camel_case("trace"), "trace");
        assert_eq!(to_camel_case("get_file_outline"), "getFileOutline");
    }
}
