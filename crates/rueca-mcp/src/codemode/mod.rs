//! Code mode (RFC 0009, experimental): the `execute` and `describe_api`
//! tools, off unless the server is started with it
//! (`rueca mcp --experimental-code-mode`, or `RUECA_EXPERIMENTAL_CODE_MODE`
//! set to `1` / `true`; see [`enabled_by_env`]). Service mode never turns it
//! on.
//!
//! - `execute` runs a script in a sandbox of `rueca-codemode` (JavaScript
//!   always; Starlark with the `code-mode-starlark` feature). Every
//!   `rueca.*` call of the script goes to [`rueca_query::Engine::respond_with`]
//!   in process, with the script's remaining wall time as the deadline, and
//!   resolves to the call's `results` (see `execute.rs` for the response).
//! - `describe_api` returns the script API generated from
//!   [`rueca_query::tool_definitions`]: TypeScript declarations for `js`, a
//!   docstring reference for `starlark` (`api.rs`). Nothing in it is written
//!   per tool, so the API follows the tools.
//!
//! With code mode off the server lists, accepts and answers exactly what it
//! did before: the two tools are neither listed nor dispatched.

mod api;
mod budget;
mod execute;
mod types;

use std::sync::OnceLock;

use rueca_codemode::{Language, Sandbox};
use rueca_query::{Engine, RespondOptions, Response};
use serde_json::{Value, json};

use crate::tools::{Tool, ToolAnnotations};

pub use execute::{DEFAULT_BUDGET_TOKENS, DEFAULT_TIMEOUT_MS, MAX_TIMEOUT_MS, SCRIPT_CALL_BUDGET_TOKENS};

/// The environment variable that turns code mode on (`1` or `true`).
pub const ENV_ENABLE: &str = "RUECA_EXPERIMENTAL_CODE_MODE";

/// The environment variable that adds `timing` to `execute` results (`1` or
/// `true`), for the benchmark's overhead gate (RFC 0009 §8).
pub const ENV_TIMING: &str = "RUECA_CODE_MODE_TIMING";

/// The tool that runs a script.
pub const EXECUTE: &str = "execute";

/// The tool that returns the script API.
pub const DESCRIBE_API: &str = "describe_api";

/// Whether `value` (an environment variable's) turns a switch on: `1` or
/// `true` (any case, surrounding spaces ignored).
pub fn truthy(value: Option<&str>) -> bool {
    value.map(str::trim).is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

/// Whether [`ENV_ENABLE`] turns code mode on. Read by the CLI for `rueca
/// mcp` and `rueca query`, never by service mode.
pub fn enabled_by_env() -> bool {
    truthy(std::env::var(ENV_ENABLE).ok().as_deref())
}

/// Whether `tool` is one of the code-mode tools.
pub fn handles(tool: &str) -> bool {
    tool == EXECUTE || tool == DESCRIBE_API
}

/// The two code-mode tools, in listing order (after the eight tools).
pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: EXECUTE.to_string(),
            description: EXECUTE_DESCRIPTION.to_string(),
            input_schema: execute_schema(),
            annotations: ToolAnnotations::READ_ONLY,
        },
        Tool {
            name: DESCRIBE_API.to_string(),
            description: DESCRIBE_API_DESCRIPTION.to_string(),
            input_schema: describe_api_schema(),
            annotations: ToolAnnotations::READ_ONLY,
        },
    ]
}

const EXECUTE_DESCRIPTION: &str = "Experimental (code mode): run a short script against the Rueca API and get \
back only what it returns, instead of chaining tool calls and reading every intermediate answer. Read \
`describe_api` (same `language`) once first: it lists the functions, their arguments and results, the script \
contract and the limits. `language`: `js` (default) or `starlark`. Sandboxed: no network, files, clock or \
randomness. Example: {\"code\": \"const r = await rueca.searchSymbols({query: 'parse'}); return \
r.flatMap(g => g.symbols.map(s => s.qn));\"}";

const DESCRIBE_API_DESCRIPTION: &str = "Experimental (code mode): the API reference for `execute` scripts in \
`language` (`js`, default: TypeScript declarations; `starlark`: a docstring reference), with the script contract, \
limits and examples. Read it once before writing a script. Example: {\"language\": \"js\"}";

fn language_schema() -> Value {
    json!({"type": "string", "enum": ["js", "starlark"], "description": "Script language: `js` (JavaScript, default) or `starlark`."})
}

fn scope_schema(what: &str) -> Value {
    json!({
        "description": format!("Default `{what}` of every call in the script that names neither `{what}` nor an `id` (same forms as the tools' `{what}`)."),
        "anyOf": [{"type": "string"}, {"type": "array", "items": {"type": "string"}}]
    })
}

fn execute_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "code": {"type": "string", "description": "The script, at most 16 KB. `js`: the body of an async function; `return` the answer. `starlark`: assign the answer to `result`."},
            "language": language_schema(),
            "repo": scope_schema("repo"),
            "ref": scope_schema("ref"),
            "budget_tokens": {"type": "integer", "description": "Cap on the returned `result` in tokens (bytes / 4), 100–1,000,000 (default 2000): arrays are cut from the end and `truncated` is set."},
            "timeout_ms": {"type": "integer", "description": "Wall time of the script, 1–10,000 ms (default 2000; larger values are clamped)."}
        },
        "required": ["code"],
        "additionalProperties": false
    })
}

fn describe_api_schema() -> Value {
    json!({
        "type": "object",
        "properties": {"language": language_schema()},
        "additionalProperties": false
    })
}

/// The sandboxes and settings of code mode, built once per server.
pub struct CodeMode {
    js: Box<dyn Sandbox + Send + Sync>,
    starlark: Option<Box<dyn Sandbox + Send + Sync>>,
    timing: bool,
}

impl std::fmt::Debug for CodeMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodeMode").field("starlark", &self.starlark.is_some()).field("timing", &self.timing).finish()
    }
}

impl Default for CodeMode {
    fn default() -> CodeMode {
        CodeMode::new()
    }
}

impl CodeMode {
    /// Code mode over the tools of [`rueca_query::TOOL_NAMES`], with `timing`
    /// from [`ENV_TIMING`].
    pub fn new() -> CodeMode {
        let js = rueca_codemode::sandbox(Language::Js, rueca_query::TOOL_NAMES)
            .expect("rueca-mcp builds rueca-codemode with the `js` feature");
        CodeMode {
            js,
            starlark: rueca_codemode::sandbox(Language::Starlark, rueca_query::TOOL_NAMES),
            timing: truthy(std::env::var(ENV_TIMING).ok().as_deref()),
        }
    }

    /// Adds (or not) `timing` to every `execute` result.
    pub fn with_timing(mut self, timing: bool) -> CodeMode {
        self.timing = timing;
        self
    }

    /// Whether this build runs Starlark scripts.
    pub fn has_starlark(&self) -> bool {
        self.starlark.is_some()
    }

    /// Answers code-mode tool `tool` (see [`handles`]), with `opts`
    /// (visibility, deadline) applied to every engine call.
    pub fn respond(&self, engine: &Engine, tool: &str, args: Value, opts: &RespondOptions) -> anyhow::Result<Response> {
        match tool {
            EXECUTE => self.execute(engine, args, opts),
            DESCRIBE_API => self.describe_api(args),
            _ => anyhow::bail!("`{tool}` is not a code-mode tool"),
        }
    }

    fn execute(&self, engine: &Engine, args: Value, opts: &RespondOptions) -> anyhow::Result<Response> {
        let args = execute::ExecuteArgs::parse(args)?;
        let sandbox = self.sandbox(args.language)?;
        execute::run(sandbox, engine, args, opts, self.timing)
    }

    fn describe_api(&self, args: Value) -> anyhow::Result<Response> {
        let language = describe_language(args)?;
        self.sandbox(language)?;
        let text = reference(language).to_string();
        Ok(Response { json: json!({"language": language.as_str(), "reference": text}), text })
    }

    fn sandbox(&self, language: Language) -> anyhow::Result<&(dyn Sandbox + Send + Sync)> {
        match language {
            Language::Js => Ok(self.js.as_ref()),
            Language::Starlark => self.starlark.as_deref().ok_or_else(|| anyhow::anyhow!(NO_STARLARK)),
        }
    }
}

/// The error of a Starlark request to a build without Starlark.
pub const NO_STARLARK: &str = "this rueca was built without Starlark code mode; rebuild it with `--features \
code-mode-starlark` (e.g. `cargo install rueca --features code-mode-starlark`), or use `language: \"js\"`";

/// The script API reference for `language` (generated once per process).
pub fn reference(language: Language) -> &'static str {
    static JS: OnceLock<String> = OnceLock::new();
    static STARLARK: OnceLock<String> = OnceLock::new();
    match language {
        Language::Js => JS.get_or_init(api::typescript),
        Language::Starlark => STARLARK.get_or_init(api::starlark),
    }
}

/// `language` of `describe_api` (default `js`); unknown arguments are errors.
fn describe_language(args: Value) -> anyhow::Result<Language> {
    let obj = match args {
        Value::Null => return Ok(Language::Js),
        Value::Object(m) => m,
        _ => anyhow::bail!("invalid arguments for `describe_api`: expected an object"),
    };
    if let Some(k) = obj.keys().find(|k| k.as_str() != "language") {
        anyhow::bail!("invalid arguments for `describe_api`: unknown argument `{k}` (expected `language`)");
    }
    execute::parse_language(obj.get("language"), DESCRIBE_API)
}
