//! MCP result shapes (`docs/mcp-protocol.md` §4 to §6) as `Serialize`
//! structs, so their key order is fixed by the field order, and the tool-call
//! job that runs off the reader thread.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Instant;

use rueca_query::{DeadlineExceeded, RespondOptions};
use serde::Serialize;
use serde_json::Value;

use crate::RuecaServer;
use crate::jsonrpc::{ErrorObject, Outgoing, RequestId, code};

/// `resultType` of every complete 2026-07-28 result.
pub(crate) const COMPLETE: &str = "complete";

/// A tool as advertised in `tools/list`: `name`, `description`,
/// `inputSchema`, `annotations` (`title`, `outputSchema`, `icons` and
/// `_meta` are never sent).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Tool {
    pub name: String,
    pub description: String,
    /// The JSON Schema of the arguments, from [`rueca_query::tool_definitions`], verbatim.
    pub input_schema: Value,
    pub annotations: ToolAnnotations,
}

/// Behaviour hints of a tool. Every Rueca tool is read-only, idempotent and
/// closed-world.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolAnnotations {
    pub read_only_hint: bool,
    pub destructive_hint: bool,
    pub idempotent_hint: bool,
    pub open_world_hint: bool,
}

impl ToolAnnotations {
    pub const READ_ONLY: ToolAnnotations = ToolAnnotations {
        read_only_hint: true,
        destructive_hint: false,
        idempotent_hint: true,
        open_world_hint: false,
    };
}

/// `{}`: the ping result, and the `tools` capability.
#[derive(Serialize)]
pub(crate) struct Empty {}

#[derive(Serialize)]
pub(crate) struct Capabilities {
    tools: Empty,
}

/// Exactly `{"tools":{}}` (no `listChanged`).
pub(crate) const CAPABILITIES: Capabilities = Capabilities { tools: Empty {} };

/// The server's `Implementation`: `name`, `title`, `version`.
#[derive(Serialize)]
pub(crate) struct Implementation {
    name: &'static str,
    title: &'static str,
    version: &'static str,
}

pub(crate) const SERVER_INFO: Implementation =
    Implementation { name: "rueca", title: "Rueca code intelligence", version: env!("CARGO_PKG_VERSION") };

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InitializeResult<'a> {
    pub protocol_version: &'a str,
    pub capabilities: Capabilities,
    pub server_info: Implementation,
    pub instructions: &'static str,
}

#[derive(Serialize)]
pub(crate) struct DiscoverMeta {
    #[serde(rename = "io.modelcontextprotocol/serverInfo")]
    pub server_info: Implementation,
}

/// The `server/discover` result: the same for every version (rmcp never
/// strips its `resultType`).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DiscoverResult {
    pub result_type: &'static str,
    pub supported_versions: &'static [&'static str],
    pub capabilities: Capabilities,
    pub instructions: &'static str,
    pub ttl_ms: u64,
    pub cache_scope: &'static str,
    #[serde(rename = "_meta")]
    pub meta: DiscoverMeta,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ListToolsResult<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_type: Option<&'static str>,
    pub ttl_ms: u64,
    pub cache_scope: &'static str,
    pub tools: &'a [Tool],
}

/// `prompts/list`, `resources/list`, `resources/templates/list`: an empty
/// list under `key`; when modern, prefixed with `resultType` and the most
/// conservative cache hints (`ttlMs` 0, `cacheScope` `private`).
pub(crate) struct EmptyList {
    pub key: &'static str,
    pub modern: bool,
}

impl Serialize for EmptyList {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(None)?;
        if self.modern {
            map.serialize_entry("resultType", COMPLETE)?;
            map.serialize_entry("ttlMs", &0)?;
            map.serialize_entry("cacheScope", "private")?;
        }
        map.serialize_entry(self.key, &[] as &[Value])?;
        map.end()
    }
}

#[derive(Serialize)]
pub(crate) struct Completion {
    values: [&'static str; 0],
}

/// `completion/complete`: always empty. No cache hints.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CompleteResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_type: Option<&'static str>,
    pub completion: Completion,
}

pub(crate) const EMPTY_COMPLETION: Completion = Completion { values: [] };

#[derive(Serialize)]
struct TextContent {
    #[serde(rename = "type")]
    kind: &'static str,
    text: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CallToolResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    result_type: Option<&'static str>,
    content: [TextContent; 1],
    #[serde(skip_serializing_if = "Option::is_none")]
    structured_content: Option<Value>,
    is_error: bool,
}

impl CallToolResult {
    fn new(modern: bool, text: String, structured: Option<Value>) -> CallToolResult {
        CallToolResult {
            result_type: modern.then_some(COMPLETE),
            is_error: structured.is_none(),
            content: [TextContent { kind: "text", text }],
            structured_content: structured,
        }
    }
}

/// A validated `tools/call`, to run off the reader thread (it blocks: it
/// may wait for the index, then runs the engine).
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub id: RequestId,
    pub name: String,
    /// `params.arguments`, or `null` when absent.
    pub args: Value,
    /// Whether the result carries `"resultType":"complete"` (2026-07-28).
    pub modern: bool,
}

impl ToolCall {
    /// Runs the call and builds its response. Engine errors and panics
    /// become results with `isError: true`, visible to the model.
    pub fn run(self, server: &RuecaServer) -> Outgoing {
        self.run_with(server, RespondOptions::default())
    }

    /// [`ToolCall::run`] with per-call options (the caller's visibility, a
    /// deadline; [`rueca_query::Engine::respond_with`]). A call stopped by
    /// its deadline is answered with the JSON-RPC error
    /// [`DEADLINE_EXCEEDED`](crate::DEADLINE_EXCEEDED), whose message names
    /// the deadline, rather than a tool result.
    pub fn run_with(self, server: &RuecaServer, opts: RespondOptions) -> Outgoing {
        let started = Instant::now();
        let ToolCall { id, name, args, modern } = self;
        let outcome = catch_unwind(AssertUnwindSafe(|| server.call_blocking(&name, args, opts)));
        if let Ok(Err(err)) = &outcome
            && let Some(exceeded) = err.downcast_ref::<DeadlineExceeded>()
        {
            tracing::debug!(tool = %name, error = %exceeded, "tool call stopped by its deadline");
            return Outgoing::error(id, ErrorObject::new(code::DEADLINE_EXCEEDED, exceeded.to_string()));
        }
        let result = match outcome {
            Ok(Ok(response)) => {
                let ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                tracing::debug!(tool = %name, ms, "tool call ok");
                CallToolResult::new(modern, response.text, Some(response.json))
            }
            Ok(Err(err)) => {
                tracing::debug!(tool = %name, error = %format!("{err:#}"), "tool call failed");
                CallToolResult::new(modern, format!("{err:#}"), None)
            }
            Err(payload) => {
                let message = panic_message(payload.as_ref());
                tracing::error!(tool = %name, error = %message, "tool call panicked");
                // Accepted difference from rmcp (docs/mcp-protocol.md §9): rmcp
                // printed a tokio JoinError, with a task id that changes per run.
                CallToolResult::new(modern, format!("internal error in `{name}`: {message}"), None)
            }
        };
        Outgoing::result(id, &result)
    }
}

/// The text of a panic payload (`panic!("…")` gives a `&str` or a `String`).
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        return (*s).to_string();
    }
    match payload.downcast_ref::<String>() {
        Some(s) => s.clone(),
        None => "panic with a non-string payload".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(v: &impl Serialize) -> String {
        serde_json::to_string(v).unwrap()
    }

    #[test]
    fn key_order_of_every_result() {
        let init = InitializeResult {
            protocol_version: "2025-06-18",
            capabilities: CAPABILITIES,
            server_info: SERVER_INFO,
            instructions: "i",
        };
        let v = env!("CARGO_PKG_VERSION");
        assert_eq!(
            json(&init),
            format!(
                r#"{{"protocolVersion":"2025-06-18","capabilities":{{"tools":{{}}}},"serverInfo":{{"name":"rueca","title":"Rueca code intelligence","version":"{v}"}},"instructions":"i"}}"#
            )
        );
        let tool = Tool {
            name: "t".into(),
            description: "d".into(),
            input_schema: serde_json::json!({}),
            annotations: ToolAnnotations::READ_ONLY,
        };
        let list = ListToolsResult { result_type: Some(COMPLETE), ttl_ms: 1, cache_scope: "public", tools: &[tool] };
        assert_eq!(
            json(&list),
            r#"{"resultType":"complete","ttlMs":1,"cacheScope":"public","tools":[{"name":"t","description":"d","inputSchema":{},"annotations":{"readOnlyHint":true,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false}}]}"#
        );
        assert_eq!(json(&EmptyList { key: "prompts", modern: false }), r#"{"prompts":[]}"#);
        assert_eq!(
            json(&EmptyList { key: "resources", modern: true }),
            r#"{"resultType":"complete","ttlMs":0,"cacheScope":"private","resources":[]}"#
        );
        assert_eq!(
            json(&CompleteResult { result_type: None, completion: EMPTY_COMPLETION }),
            r#"{"completion":{"values":[]}}"#
        );
        assert_eq!(
            json(&CallToolResult::new(true, "x".into(), Some(serde_json::json!({"a": 1})))),
            r#"{"resultType":"complete","content":[{"type":"text","text":"x"}],"structuredContent":{"a":1},"isError":false}"#
        );
        assert_eq!(
            json(&CallToolResult::new(false, "bad".into(), None)),
            r#"{"content":[{"type":"text","text":"bad"}],"isError":true}"#
        );
    }

    #[test]
    fn panic_messages() {
        let r = catch_unwind(|| panic!("boom {}", 1)).unwrap_err();
        assert_eq!(panic_message(r.as_ref()), "boom 1");
        let r = catch_unwind(|| panic!("static")).unwrap_err();
        assert_eq!(panic_message(r.as_ref()), "static");
        let r = catch_unwind(|| std::panic::panic_any(7)).unwrap_err();
        assert_eq!(panic_message(r.as_ref()), "panic with a non-string payload");
    }
}
