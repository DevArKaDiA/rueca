//! `POST /mcp`: stateless Streamable HTTP (RFC 0008 §6.1–§6.2) over
//! rueca-mcp's synchronous core (`docs/mcp-protocol.md` §8).
//!
//! One JSON-RPC message per request, one `application/json` response, no
//! SSE, no session (`Mcp-Session-Id` is ignored and never minted):
//!
//! 1. `parse_line(body)`; unparsable → `400`, -32700.
//! 2. `MCP-Protocol-Version` (absent: `2025-03-26`) must be a supported
//!    version → else `400`, -32022.
//! 3. `Mcp-Method`, `Mcp-Name` (for `tools/call`) and
//!    `MCP-Protocol-Version`, when present, must match the body (`method`,
//!    `params.name`, `params._meta` protocol version) → else `400`, -32020
//!    `HeaderMismatch`.
//! 4. `initialize`, or a body whose `_meta` carries a protocol version:
//!    `Session::new()`; anything else: `Session::legacy(<header version>)`.
//! 5. `Step::Reply` → `200`; `Step::Call` → the tool runs on a blocking
//!    thread with the caller's visibility, bounded by
//!    `[http].query_deadline` (on expiry a JSON-RPC error naming the
//!    deadline; the thread finishes on its own) → `200`; `Step::Silent` and
//!    `Step::Cancel` → `202`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Extension;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use rueca_mcp::{ErrorObject, Incoming, JsonObject, Outgoing, RequestId, SUPPORTED_VERSIONS, Session, Step, ToolCall};
use serde_json::{Value, json};
use tracing::field::Empty;

use crate::auth::Principal;
use crate::state::AppState;

/// The version assumed when `MCP-Protocol-Version` is absent (2025-11-25
/// transport spec).
pub const DEFAULT_VERSION: &str = "2025-03-26";

/// JSON-RPC codes used by the transport.
pub mod code {
    pub const PARSE_ERROR: i32 = -32700;
    pub const INTERNAL_ERROR: i32 = -32603;
    pub const UNSUPPORTED_PROTOCOL_VERSION: i32 = -32022;
    pub const HEADER_MISMATCH: i32 = -32020;
    /// The tool call exceeded `[http].query_deadline`.
    pub const DEADLINE: i32 = -32001;
    /// No generation is served yet.
    pub const NOT_READY: i32 = -32002;
}

const META_PROTOCOL_VERSION: &str = "io.modelcontextprotocol/protocolVersion";

/// What the request log records.
struct Facts {
    method: String,
    tool: Option<String>,
    traceparent: Option<String>,
}

pub async fn handle(
    State(st): State<Arc<AppState>>,
    Extension(principal): Extension<Principal>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let incoming = rueca_mcp::parse_line(&body);
    let facts = facts(&incoming);
    let span = tracing::info_span!(
        "mcp",
        method = %facts.method,
        tool = facts.tool.as_deref().unwrap_or("-"),
        principal = %principal.id,
        traceparent = facts.traceparent.as_deref().unwrap_or(""),
        outcome = Empty,
        ms = Empty,
    );
    #[cfg(feature = "otel")]
    if let Some(tp) = facts.traceparent.as_deref() {
        crate::otel::set_parent(&span, tp);
    }
    let started = Instant::now();
    let (outcome, response) = respond(&st, &principal, &headers, incoming, &span).await;
    let ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    span.record("outcome", outcome);
    span.record("ms", ms);
    span.in_scope(|| tracing::info!("mcp request"));
    if facts.tool.is_none() {
        st.metrics.mcp_request("-", outcome, "-", None);
    }
    response
}

async fn respond(
    st: &Arc<AppState>,
    principal: &Principal,
    headers: &HeaderMap,
    incoming: Incoming,
    span: &tracing::Span,
) -> (&'static str, Response) {
    if matches!(incoming, Incoming::Unparsable) {
        let err = ErrorObject::new(code::PARSE_ERROR, "Parse error: the body is not one JSON-RPC message");
        return ("error", rpc_error(StatusCode::BAD_REQUEST, None, err));
    }
    let id = request_id(&incoming);
    let version = match protocol_version(headers) {
        Ok(v) => v,
        Err(err) => return ("error", rpc_error(StatusCode::BAD_REQUEST, id, err)),
    };
    if let Err(err) = check_headers(headers, &incoming) {
        return ("error", rpc_error(StatusCode::BAD_REQUEST, id, err));
    }
    let Some(serving) = st.serving() else {
        let err = ErrorObject::new(code::NOT_READY, "Rueca is not ready: the service store has not been read yet");
        return ("error", rpc_error(StatusCode::SERVICE_UNAVAILABLE, id, err));
    };
    let mut session = if starts_session(&incoming) { Session::new() } else { Session::legacy(version) };
    match serving.server.handle(incoming, &mut session) {
        Step::Reply(out) => ("ok", json_response(StatusCode::OK, out.to_json())),
        Step::Call(call) => call_tool(st, principal, &serving.server, call, span).await,
        Step::Silent | Step::Cancel(_) => ("ok", StatusCode::ACCEPTED.into_response()),
    }
}

/// Runs a tool call with the caller's visibility, under the deadline.
async fn call_tool(
    st: &Arc<AppState>,
    principal: &Principal,
    server: &rueca_mcp::RuecaServer,
    call: ToolCall,
    span: &tracing::Span,
) -> (&'static str, Response) {
    let tool = call.name.clone();
    let scope = scope_of(&call.args);
    let id = call.id.clone();
    let Some(current) = st.current() else {
        st.metrics.mcp_request(&tool, "error", scope, None);
        let err = ErrorObject::new(code::NOT_READY, "Rueca is not ready: no organization generation is served yet");
        return ("error", rpc_error(StatusCode::OK, Some(id), err));
    };
    let visibility = st.visibility.resolve(&principal.repos, current.generation.generation, &current.repos);
    let server = server.with_visibility(visibility);
    let deadline = st.config.http.query_deadline.0;
    let started = Instant::now();
    let task_span = span.clone();
    let task = tokio::task::spawn_blocking(move || task_span.in_scope(|| call.run(&server)));
    let (outcome, response) = match tokio::time::timeout(deadline, task).await {
        Ok(Ok(out)) => (if is_tool_error(&out) { "error" } else { "ok" }, json_response(StatusCode::OK, out.to_json())),
        Ok(Err(join)) => {
            tracing::error!(error = %join, "tool call task failed");
            let err = ErrorObject::new(code::INTERNAL_ERROR, format!("internal error in `{tool}`"));
            ("error", rpc_error(StatusCode::OK, Some(id), err))
        }
        Err(_) => ("deadline", rpc_error(StatusCode::OK, Some(id), deadline_error(deadline))),
    };
    st.metrics.mcp_request(&tool, outcome, scope, Some(started.elapsed()));
    (outcome, response)
}

/// The error of a call that ran past `deadline`.
pub fn deadline_error(deadline: Duration) -> ErrorObject {
    ErrorObject::new(code::DEADLINE, format!("deadline of {:?} exceeded; narrow `repo` or lower `limit`", deadline))
}

/// `ok` / `error`: a tool result with `isError: true` (its last member).
fn is_tool_error(out: &Outgoing) -> bool {
    match out {
        Outgoing::Result { result, .. } => result.get().ends_with("\"isError\":true}"),
        Outgoing::Error { .. } => true,
    }
}

/// `repo` (one repository), `repos` (a list), `org` (absent or `"*"`).
fn scope_of(args: &Value) -> &'static str {
    match args.get("repo") {
        Some(Value::String(s)) if s.trim() != "*" => "repo",
        Some(Value::Array(_)) => "repos",
        _ => "org",
    }
}

fn request_id(incoming: &Incoming) -> Option<RequestId> {
    match incoming {
        Incoming::Request(r) => Some(r.id.clone()),
        _ => None,
    }
}

fn params(incoming: &Incoming) -> Option<&JsonObject> {
    match incoming {
        Incoming::Request(r) => r.params.as_ref(),
        Incoming::Notification(n) => n.params.as_ref(),
        _ => None,
    }
}

fn method(incoming: &Incoming) -> Option<&str> {
    match incoming {
        Incoming::Request(r) => Some(&r.method),
        Incoming::Notification(n) => Some(&n.method),
        _ => None,
    }
}

fn meta_version(incoming: &Incoming) -> Option<&str> {
    params(incoming)?.get("_meta")?.get(META_PROTOCOL_VERSION)?.as_str()
}

fn facts(incoming: &Incoming) -> Facts {
    let m = method(incoming).unwrap_or("-").to_string();
    let p = params(incoming);
    let tool = (m == "tools/call").then(|| p.and_then(|p| p.get("name")).and_then(Value::as_str)).flatten();
    let traceparent = p.and_then(|p| p.get("_meta")).and_then(|m| m.get("traceparent")).and_then(Value::as_str);
    Facts { method: m, tool: tool.map(str::to_string), traceparent: traceparent.map(str::to_string) }
}

/// `initialize`, or a request carrying `_meta` with a protocol version.
fn starts_session(incoming: &Incoming) -> bool {
    method(incoming) == Some("initialize") || meta_version(incoming).is_some()
}

fn header<'h>(headers: &'h HeaderMap, name: &str) -> Option<&'h str> {
    headers.get(name).and_then(|v| v.to_str().ok()).map(str::trim)
}

/// The `MCP-Protocol-Version` header, validated (absent: 2025-03-26).
fn protocol_version(headers: &HeaderMap) -> Result<&str, ErrorObject> {
    match header(headers, "mcp-protocol-version") {
        None => Ok(DEFAULT_VERSION),
        Some(v) if SUPPORTED_VERSIONS.contains(&v) => Ok(v),
        Some(v) => {
            let mut err = ErrorObject::new(code::UNSUPPORTED_PROTOCOL_VERSION, "Unsupported protocol version");
            let data = json!({"requested": v, "supported": SUPPORTED_VERSIONS});
            err.data = serde_json::value::to_raw_value(&data).ok();
            Err(err)
        }
    }
}

/// `Mcp-Method`, `Mcp-Name` and `MCP-Protocol-Version` against the body.
pub fn check_headers(headers: &HeaderMap, incoming: &Incoming) -> Result<(), ErrorObject> {
    let mismatch = |what: &str, header: &str, body: &str| {
        ErrorObject::new(
            code::HEADER_MISMATCH,
            format!("HeaderMismatch: {what} header `{header}` does not match the body (`{body}`)"),
        )
    };
    let body_method = method(incoming).unwrap_or("");
    if let Some(h) = header(headers, "mcp-method")
        && h != body_method
    {
        return Err(mismatch("Mcp-Method", h, body_method));
    }
    if let Some(h) = header(headers, "mcp-name")
        && body_method == "tools/call"
    {
        let name = params(incoming).and_then(|p| p.get("name")).and_then(Value::as_str).unwrap_or("");
        if h != name {
            return Err(mismatch("Mcp-Name", h, name));
        }
    }
    if let (Some(h), Some(meta)) = (header(headers, "mcp-protocol-version"), meta_version(incoming))
        && h != meta
    {
        return Err(mismatch("MCP-Protocol-Version", h, meta));
    }
    Ok(())
}

pub fn json_response(status: StatusCode, body: String) -> Response {
    (status, [(http::header::CONTENT_TYPE, "application/json")], body).into_response()
}

fn rpc_error(status: StatusCode, id: Option<RequestId>, error: ErrorObject) -> Response {
    let body = match id {
        Some(id) => Outgoing::error(id, error).to_json(),
        None => {
            json!({"jsonrpc": "2.0", "id": null, "error": {"code": error.code, "message": error.message}}).to_string()
        }
    };
    json_response(status, body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(http::HeaderName::from_bytes(k.as_bytes()).unwrap(), v.parse().unwrap());
        }
        h
    }

    fn msg(v: Value) -> Incoming {
        rueca_mcp::parse_line(v.to_string().as_bytes())
    }

    #[test]
    fn header_checks() {
        let call = msg(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "search_symbols", "_meta": {META_PROTOCOL_VERSION: "2026-07-28"}}}));
        let cases: &[(&[(&str, &str)], bool)] = &[
            (&[], true),
            (&[("mcp-method", "tools/call"), ("mcp-name", "search_symbols")], true),
            (&[("mcp-method", "tools/list")], false),
            (&[("mcp-name", "trace")], false),
            (&[("mcp-protocol-version", "2026-07-28")], true),
            (&[("mcp-protocol-version", "2025-11-25")], false),
        ];
        for (h, ok) in cases {
            let got = check_headers(&headers(h), &call);
            assert_eq!(got.is_ok(), *ok, "{h:?}");
            if let Err(e) = got {
                assert_eq!(e.code, code::HEADER_MISMATCH);
            }
        }
        let list = msg(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}));
        assert!(check_headers(&headers(&[("mcp-name", "anything")]), &list).is_ok(), "Mcp-Name only for tools/call");
    }

    #[test]
    fn versions_and_scopes() {
        assert_eq!(protocol_version(&headers(&[])).unwrap(), DEFAULT_VERSION);
        assert_eq!(protocol_version(&headers(&[("mcp-protocol-version", "2025-06-18")])).unwrap(), "2025-06-18");
        assert_eq!(protocol_version(&headers(&[("mcp-protocol-version", "1999-01-01")])).unwrap_err().code, -32022);
        assert_eq!(scope_of(&json!({"repo": "github.com/a/b"})), "repo");
        assert_eq!(scope_of(&json!({"repo": ["a", "b"]})), "repos");
        assert_eq!(scope_of(&json!({"repo": "*"})), "org");
        assert_eq!(scope_of(&json!(null)), "org");
        assert!(deadline_error(Duration::from_secs(10)).message.starts_with("deadline of 10s exceeded"));
    }

    #[test]
    fn sessions() {
        assert!(starts_session(&msg(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}))));
        let modern = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list",
            "params": {"_meta": {META_PROTOCOL_VERSION: "2026-07-28"}}});
        assert!(starts_session(&msg(modern)));
        assert!(!starts_session(&msg(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))));
    }
}
