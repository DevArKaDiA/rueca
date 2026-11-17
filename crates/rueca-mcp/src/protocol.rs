//! The MCP protocol core: lifecycle selection, version negotiation, the
//! per-request `_meta` rules and method dispatch (`docs/mcp-protocol.md` §4
//! to §6). Synchronous and transport-agnostic: [`RuecaServer::handle`] takes
//! one classified message and a [`Session`] and says what to do with it
//! ([`Step`]); the transport does the I/O and runs tool calls off-thread.

use serde::Serialize;
use serde_json::Value;
use serde_json::value::RawValue;

use crate::jsonrpc::{ErrorObject, Incoming, JsonObject, Notification, Outgoing, Request, RequestId, code, to_raw};
use crate::tools::{
    CAPABILITIES, COMPLETE, CompleteResult, DiscoverMeta, DiscoverResult, EMPTY_COMPLETION, Empty, EmptyList,
    InitializeResult, ListToolsResult, SERVER_INFO, ToolCall,
};
use crate::{INSTRUCTIONS, RuecaServer};

/// Every protocol version Rueca serves, oldest first (the order advertised
/// by `server/discover` and in -32022 errors).
pub const SUPPORTED_VERSIONS: &[&str] = &["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25", "2026-07-28"];

/// The first version without the `initialize` handshake: per-request
/// `_meta`, `server/discover`, `resultType` on results.
const MODERN: &str = "2026-07-28";

/// What `initialize` answers when the requested version has no handshake or
/// is unknown: the newest supported version that has one.
const NEWEST_WITH_INITIALIZE: &str = "2025-11-25";

/// `tools/list` cache hints: the list never changes while the server runs.
const TOOLS_TTL_MS: u64 = 3_600_000;

const META_PROTOCOL_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
const META_CLIENT_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";

/// Whether a version uses the `initialize` handshake: plain string
/// comparison, so unknown strings count too (`""` has one, `"foo"` not).
fn has_initialize(version: &str) -> bool {
    version < MODERN
}

fn is_modern(version: &str) -> bool {
    !has_initialize(version)
}

fn is_supported(version: &str) -> bool {
    SUPPORTED_VERSIONS.contains(&version)
}

/// The version an `initialize` is answered with: the requested one when it
/// is supported and has a handshake, otherwise 2025-11-25. Never fails.
pub fn negotiate(requested: &str) -> &'static str {
    SUPPORTED_VERSIONS.iter().copied().find(|v| *v == requested && has_initialize(v)).unwrap_or(NEWEST_WITH_INITIALIZE)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lifecycle {
    /// Nothing chosen yet: requests are handled one at a time, in order.
    Unselected,
    /// Chosen by `initialize` (2024-11-05 to 2025-11-25).
    Legacy,
    /// Chosen by a first request that carried a supported `_meta`
    /// (2026-07-28): every request must carry `_meta`.
    Inline,
}

/// The state of one connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    lifecycle: Lifecycle,
    /// The negotiated version of a legacy session.
    version: Option<String>,
}

impl Default for Session {
    fn default() -> Session {
        Session::new()
    }
}

impl Session {
    /// A fresh connection, before any lifecycle is chosen.
    pub fn new() -> Session {
        Session { lifecycle: Lifecycle::Unselected, version: None }
    }

    /// A connection that already did the `initialize` handshake at
    /// `version`. For stateless transports (HTTP): a legacy client sends
    /// `initialize` once and later requests without `_meta`, each in its own
    /// HTTP request; they must be served as part of a legacy session
    /// (version from the `MCP-Protocol-Version` header) or a fresh
    /// [`Session::new`] would reject them with -32602.
    pub fn legacy(version: &str) -> Session {
        Session { lifecycle: Lifecycle::Legacy, version: Some(version.to_string()) }
    }

    /// Whether a lifecycle (legacy or inline) has been chosen.
    pub fn is_selected(&self) -> bool {
        self.lifecycle != Lifecycle::Unselected
    }
}

/// What the transport must do with one incoming message.
#[derive(Debug)]
pub enum Step {
    /// Write this response now.
    Reply(Outgoing),
    /// Run this tool call off the reader (it blocks) and write its response,
    /// unless the request is cancelled meanwhile.
    Call(ToolCall),
    /// Nothing to write: a notification, a client response, an ignored line.
    Silent,
    /// `notifications/cancelled`: forget this in-flight request; its
    /// response, if it ever comes, is dropped.
    Cancel(RequestId),
}

/// Per-request facts derived from `_meta` and the session (`docs/mcp-protocol.md` §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RequestShape {
    /// The effective version is 2026-07-28 or later: results carry
    /// `resultType` and list cache hints.
    modern: bool,
    /// Served under the legacy lifecycle: `ping` is answered.
    legacy: bool,
}

impl RuecaServer {
    /// Handles one incoming message on `session` (the protocol core; no I/O).
    ///
    /// The stdio transport calls it for every line, in order, from one
    /// thread. A stateless transport calls it once per message with a
    /// session built for that message (see [`Session::legacy`]).
    pub fn handle(&self, msg: Incoming, session: &mut Session) -> Step {
        match msg {
            Incoming::Request(req) if session.is_selected() => self.serve_request(req, session),
            Incoming::Request(req) => self.select_lifecycle(req, session),
            Incoming::Notification(n) => handle_notification(&n, session),
            Incoming::Invalid => Step::Reply(Outgoing::invalid_request()),
            Incoming::Response | Incoming::Ignored | Incoming::Unparsable => Step::Silent,
        }
    }

    /// The first requests of a connection (§4): `ping` and `server/discover`
    /// leave it unselected, `initialize` chooses legacy, any other request
    /// with a supported `_meta` chooses inline and is served.
    fn select_lifecycle(&self, req: Request, session: &mut Session) -> Step {
        if req.method == "ping" {
            return reply(req.id, Ok(to_raw(&Empty {})));
        }
        if let Some(requested) = initialize_version(&req) {
            let version = negotiate(requested);
            session.lifecycle = Lifecycle::Legacy;
            session.version = Some(version.to_string());
            return reply(req.id, Ok(initialize_result(version)));
        }
        let meta = request_meta(&req);
        let missing = missing_meta_keys(meta);
        if !missing.is_empty() {
            return reply(req.id, Err(missing_meta_error(&missing)));
        }
        let version = meta_version(meta).unwrap_or_default();
        let supported = if is_supported(version) { Ok(()) } else { Err(unsupported(version)) };
        if is_discover(&req) {
            // Discover is a probe: it never selects the lifecycle.
            return reply(req.id, supported.map(|()| discover_result()));
        }
        if let Err(err) = supported {
            return reply(req.id, Err(err));
        }
        session.lifecycle = Lifecycle::Inline;
        self.serve_request(req, session)
    }

    /// A request once the lifecycle is chosen: version and `_meta` checks,
    /// then dispatch.
    fn serve_request(&self, req: Request, session: &mut Session) -> Step {
        match check_request(&req, session) {
            Ok(shape) => self.dispatch(req, shape, session),
            Err(err) => reply(req.id, Err(err)),
        }
    }

    fn dispatch(&self, req: Request, shape: RequestShape, session: &mut Session) -> Step {
        let result = match req.method.as_str() {
            "tools/call" => return self.call_tool(req, shape),
            "tools/list" => Ok(self.list_tools(shape)),
            "initialize" => reinitialize(&req, session),
            "server/discover" => discover(&req),
            "ping" => ping(shape),
            "completion/complete" => complete(&req, shape),
            "prompts/list" => Ok(empty_list("prompts", shape)),
            "resources/list" => Ok(empty_list("resources", shape)),
            "resources/templates/list" => Ok(empty_list("resourceTemplates", shape)),
            other => Err(ErrorObject::method_not_found(other)),
        };
        reply(req.id, result)
    }

    fn list_tools(&self, shape: RequestShape) -> Box<RawValue> {
        to_raw(&ListToolsResult {
            result_type: shape.modern.then_some(COMPLETE),
            ttl_ms: TOOLS_TTL_MS,
            cache_scope: "public",
            tools: &self.tools,
        })
    }

    /// Validates a `tools/call` and hands it to the transport. Unknown tools
    /// are a protocol error (-32602); everything the engine reports later is
    /// a tool result with `isError`.
    fn call_tool(&self, req: Request, shape: RequestShape) -> Step {
        let (name, args) = match tool_call_params(req.params.as_ref()) {
            Ok(call) => call,
            // Accepted difference from rmcp (docs/mcp-protocol.md §9): rmcp
            // answered -32601 with the method as message.
            Err(why) => {
                let message = format!("invalid params for `tools/call`: {why}");
                return reply(req.id, Err(ErrorObject::new(code::INVALID_PARAMS, message)));
            }
        };
        if !self.tools.iter().any(|t| t.name == name) {
            let available: Vec<&str> = self.tools.iter().map(|t| t.name.as_str()).collect();
            let message = format!("unknown tool `{name}` (available: {})", available.join(", "));
            return reply(req.id, Err(ErrorObject::new(code::INVALID_PARAMS, message)));
        }
        Step::Call(ToolCall { id: req.id, name, args, modern: shape.modern })
    }
}

fn reply(id: RequestId, result: Result<Box<RawValue>, ErrorObject>) -> Step {
    Step::Reply(match result {
        Ok(result) => Outgoing::Result { id, result },
        Err(error) => Outgoing::error(id, error),
    })
}

/// Notifications never get a reply. Before a lifecycle is chosen they are
/// ignored: accepted difference from rmcp, which exited 1
/// (docs/mcp-protocol.md §9).
fn handle_notification(n: &Notification, session: &Session) -> Step {
    if !session.is_selected() {
        tracing::debug!(method = %n.method, "ignoring a notification sent before the first request");
        return Step::Silent;
    }
    match n.method.as_str() {
        "notifications/cancelled" => cancelled_request(n.params.as_ref()).map_or(Step::Silent, Step::Cancel),
        "notifications/initialized" => {
            tracing::info!("client initialized");
            Step::Silent
        }
        _ => Step::Silent,
    }
}

/// The `requestId` of a well-formed `notifications/cancelled` (`reason`, if
/// present, a string).
fn cancelled_request(params: Option<&JsonObject>) -> Option<RequestId> {
    let params = params?;
    let reason_ok = params.get("reason").is_none_or(|r| r.is_null() || r.is_string());
    let id = RequestId::from_value(params.get("requestId")?)?;
    reason_ok.then_some(id)
}

/// The per-request checks of §5, in rmcp's order: the `_meta` version, then
/// the required `_meta` keys.
fn check_request(req: &Request, session: &Session) -> Result<RequestShape, ErrorObject> {
    let is_initialize = initialize_version(req).is_some();
    let meta = request_meta(req);
    let meta_v = meta_version(meta);
    if let Some(v) = meta_v.filter(|v| !is_initialize && !is_supported(v)) {
        return Err(unsupported(v));
    }
    let needs_meta =
        !is_initialize && (is_discover(req) || session.lifecycle == Lifecycle::Inline || meta_v.is_some_and(is_modern));
    let missing = if needs_meta { missing_meta_keys(meta) } else { Vec::new() };
    if !missing.is_empty() {
        return Err(missing_meta_error(&missing));
    }
    let modern = meta_v.or(session.version.as_deref()).is_some_and(is_modern);
    Ok(RequestShape { modern, legacy: !needs_meta && !modern })
}

fn request_meta(req: &Request) -> Option<&JsonObject> {
    req.params.as_ref()?.get("_meta")?.as_object()
}

fn meta_version(meta: Option<&JsonObject>) -> Option<&str> {
    meta?.get(META_PROTOCOL_VERSION)?.as_str()
}

/// The required 2026-07-28 `_meta` keys that are missing or malformed, in
/// the order rmcp reports them.
fn missing_meta_keys(meta: Option<&JsonObject>) -> Vec<&'static str> {
    let get = |key: &str| meta.and_then(|m| m.get(key));
    let mut missing = Vec::new();
    if !get(META_PROTOCOL_VERSION).is_some_and(Value::is_string) {
        missing.push(META_PROTOCOL_VERSION);
    }
    if !get(META_CLIENT_CAPABILITIES).is_some_and(valid_client_capabilities) {
        missing.push(META_CLIENT_CAPABILITIES);
    }
    missing
}

fn missing_meta_error(missing: &[&str]) -> ErrorObject {
    let message = format!("request _meta is missing or has malformed required fields: {}", missing.join(", "));
    ErrorObject::new(code::INVALID_PARAMS, message)
}

#[derive(Serialize)]
struct UnsupportedVersion<'a> {
    requested: &'a str,
    supported: &'static [&'static str],
}

fn unsupported(requested: &str) -> ErrorObject {
    let mut err = ErrorObject::new(code::UNSUPPORTED_PROTOCOL_VERSION, "Unsupported protocol version");
    err.data = Some(to_raw(&UnsupportedVersion { requested, supported: SUPPORTED_VERSIONS }));
    err
}

/// ClientCapabilities as rmcp types them: an object whose known members,
/// when present and not `null`, have the right shape. Unknown members pass.
fn valid_client_capabilities(caps: &Value) -> bool {
    caps.as_object().is_some_and(|m| m.iter().all(|(key, value)| valid_capability(key, value)))
}

fn valid_capability(key: &str, value: &Value) -> bool {
    if value.is_null() {
        return true;
    }
    match key {
        "experimental" | "extensions" => value.as_object().is_some_and(|m| m.values().all(Value::is_object)),
        "roots" => value.as_object().is_some_and(|r| optional(r, "listChanged", Value::is_boolean)),
        "sampling" | "elicitation" => value.is_object(),
        _ => true,
    }
}

/// `key` is absent, `null`, or satisfies `pred`.
fn optional(obj: &JsonObject, key: &str, pred: fn(&Value) -> bool) -> bool {
    obj.get(key).is_none_or(|v| v.is_null() || pred(v))
}

fn has_string(obj: &JsonObject, key: &str) -> bool {
    obj.get(key).is_some_and(Value::is_string)
}

/// The requested version of a well-formed `initialize`: `protocolVersion`
/// (a string), valid `capabilities` and an Implementation `clientInfo`.
fn initialize_version(req: &Request) -> Option<&str> {
    if req.method != "initialize" {
        return None;
    }
    let params = req.params.as_ref()?;
    let version = params.get("protocolVersion")?.as_str()?;
    let capabilities = params.get("capabilities").is_some_and(valid_client_capabilities);
    let client_info = params.get("clientInfo").and_then(Value::as_object).is_some_and(valid_implementation);
    (capabilities && client_info).then_some(version)
}

fn valid_implementation(info: &JsonObject) -> bool {
    has_string(info, "name")
        && has_string(info, "version")
        && ["title", "description", "websiteUrl"].iter().all(|k| optional(info, k, Value::is_string))
        && optional(info, "icons", Value::is_array)
}

/// A well-formed `server/discover` has `params` (an object).
fn is_discover(req: &Request) -> bool {
    req.method == "server/discover" && req.params.is_some()
}

fn initialize_result(version: &str) -> Box<RawValue> {
    to_raw(&InitializeResult {
        protocol_version: version,
        capabilities: CAPABILITIES,
        server_info: SERVER_INFO,
        instructions: INSTRUCTIONS,
    })
}

/// `initialize` once the lifecycle is chosen: answered again with the same
/// rule. The session keeps the negotiated version and its lifecycle.
/// Accepted difference from rmcp (docs/mcp-protocol.md §9): rmcp stored the
/// raw requested version, which changed the shape of later results.
fn reinitialize(req: &Request, session: &mut Session) -> Result<Box<RawValue>, ErrorObject> {
    let requested = initialize_version(req).ok_or_else(|| ErrorObject::method_not_found("initialize"))?;
    let version = negotiate(requested);
    session.version = Some(version.to_string());
    Ok(initialize_result(version))
}

fn discover_result() -> Box<RawValue> {
    to_raw(&DiscoverResult {
        result_type: COMPLETE,
        supported_versions: SUPPORTED_VERSIONS,
        capabilities: CAPABILITIES,
        instructions: INSTRUCTIONS,
        ttl_ms: 0,
        cache_scope: "private",
        meta: DiscoverMeta { server_info: SERVER_INFO },
    })
}

fn discover(req: &Request) -> Result<Box<RawValue>, ErrorObject> {
    if is_discover(req) { Ok(discover_result()) } else { Err(ErrorObject::method_not_found("server/discover")) }
}

/// `ping` is part of the legacy lifecycle only; 2026-07-28 removed it.
fn ping(shape: RequestShape) -> Result<Box<RawValue>, ErrorObject> {
    if shape.legacy { Ok(to_raw(&Empty {})) } else { Err(ErrorObject::method_not_found("ping")) }
}

fn empty_list(key: &'static str, shape: RequestShape) -> Box<RawValue> {
    to_raw(&EmptyList { key, modern: shape.modern })
}

/// `completion/complete` is answered (empty) though not advertised, as rmcp does.
fn complete(req: &Request, shape: RequestShape) -> Result<Box<RawValue>, ErrorObject> {
    if !req.params.as_ref().is_some_and(valid_completion_params) {
        return Err(ErrorObject::method_not_found("completion/complete"));
    }
    Ok(to_raw(&CompleteResult { result_type: shape.modern.then_some(COMPLETE), completion: EMPTY_COMPLETION }))
}

fn valid_completion_params(params: &JsonObject) -> bool {
    let reference = params.get("ref").and_then(Value::as_object).is_some_and(valid_reference);
    let argument = params
        .get("argument")
        .and_then(Value::as_object)
        .is_some_and(|a| has_string(a, "name") && has_string(a, "value"));
    reference && argument
}

fn valid_reference(r: &JsonObject) -> bool {
    match r.get("type").and_then(Value::as_str) {
        Some("ref/prompt") => has_string(r, "name"),
        Some("ref/resource") => has_string(r, "uri"),
        _ => false,
    }
}

/// `tools/call` params: `name` a string, `arguments` absent, `null` or an
/// object, `requestState` absent, `null` or a string. `Err` says why not.
fn tool_call_params(params: Option<&JsonObject>) -> Result<(String, Value), &'static str> {
    let params = params.ok_or("missing `name`")?;
    let name = match params.get("name") {
        Some(Value::String(name)) => name.clone(),
        Some(_) => return Err("`name` must be a string"),
        None => return Err("missing `name`"),
    };
    let args = match params.get("arguments") {
        None | Some(Value::Null) => Value::Null,
        Some(args @ Value::Object(_)) => args.clone(),
        Some(_) => return Err("`arguments` must be an object"),
    };
    if !optional(params, "requestState", Value::is_string) {
        return Err("`requestState` must be a string");
    }
    Ok((name, args))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jsonrpc::parse_line;
    use serde_json::json;

    fn server() -> RuecaServer {
        let dir = std::env::temp_dir().join("rueca-mcp-protocol-tests-empty-store");
        RuecaServer::new(rueca_query::Engine::new(dir, None).unwrap())
    }

    fn meta(version: &str) -> Value {
        json!({META_PROTOCOL_VERSION: version, META_CLIENT_CAPABILITIES: {}})
    }

    fn msg(v: Value) -> Incoming {
        parse_line(v.to_string().as_bytes())
    }

    fn request(id: i64, method: &str, params: Value) -> Incoming {
        msg(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
    }

    fn init(id: i64, version: &str) -> Incoming {
        request(
            id,
            "initialize",
            json!({"protocolVersion": version, "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}),
        )
    }

    /// The reply as JSON (panics on anything but a reply).
    fn answer(s: &RuecaServer, session: &mut Session, m: Incoming) -> Value {
        match s.handle(m, session) {
            Step::Reply(out) => serde_json::from_str(&out.to_json()).unwrap(),
            other => panic!("expected a reply, got {other:?}"),
        }
    }

    #[test]
    fn negotiation_table() {
        for (asked, answered) in [
            ("2024-11-05", "2024-11-05"),
            ("2025-03-26", "2025-03-26"),
            ("2025-06-18", "2025-06-18"),
            ("2025-11-25", "2025-11-25"),
            ("2026-07-28", "2025-11-25"),
            ("2099-01-01", "2025-11-25"),
            ("2024-10-07", "2025-11-25"),
            ("1999-01-01", "2025-11-25"),
            ("", "2025-11-25"),
            ("foo", "2025-11-25"),
        ] {
            assert_eq!(negotiate(asked), answered, "{asked}");
        }
        assert!(has_initialize("") && has_initialize("2025-11-25") && !has_initialize("foo"));
        assert!(!has_initialize("2026-07-28") && !has_initialize("2030-01-01"));
    }

    #[test]
    fn meta_validation() {
        let m = |v: Value| missing_meta_keys(v.as_object());
        assert!(m(meta("2026-07-28")).is_empty());
        assert!(m(meta("anything")).is_empty(), "any string is a version here");
        assert_eq!(m(json!({})), [META_PROTOCOL_VERSION, META_CLIENT_CAPABILITIES]);
        assert_eq!(missing_meta_keys(None), [META_PROTOCOL_VERSION, META_CLIENT_CAPABILITIES]);
        assert_eq!(m(json!({META_PROTOCOL_VERSION: 7, META_CLIENT_CAPABILITIES: {}})), [META_PROTOCOL_VERSION]);
        for caps in [
            json!({}),
            json!({"roots": {"listChanged": true}}),
            json!({"roots": null, "sampling": {}, "elicitation": {}, "experimental": {"x": {}}, "other": 3}),
            json!({"roots": {"listChanged": null}}),
        ] {
            assert!(valid_client_capabilities(&caps), "{caps}");
        }
        for caps in [
            json!([]),
            json!("x"),
            json!({"roots": {"listChanged": "yes"}}),
            json!({"roots": 1}),
            json!({"sampling": true}),
            json!({"experimental": {"x": 1}}),
            json!({"extensions": []}),
        ] {
            assert!(!valid_client_capabilities(&caps), "{caps}");
        }
    }

    #[test]
    fn unselected_ping_discover_and_errors() {
        let s = server();
        let mut session = Session::new();
        assert_eq!(
            answer(&s, &mut session, request(1, "ping", json!({"x": 1}))),
            json!({"jsonrpc": "2.0", "id": 1, "result": {}})
        );
        let r = answer(&s, &mut session, request(2, "server/discover", json!({"_meta": meta("2026-07-28")})));
        assert_eq!(r["result"]["supportedVersions"], json!(SUPPORTED_VERSIONS));
        // A legacy version is supported too: discover answers.
        let r = answer(&s, &mut session, request(3, "server/discover", json!({"_meta": meta("2024-11-05")})));
        assert_eq!(r["result"]["resultType"], "complete");
        let r = answer(&s, &mut session, request(4, "server/discover", json!({"_meta": meta("1999-01-01")})));
        assert_eq!(r["error"]["code"], -32022);
        assert_eq!(r["error"]["data"]["requested"], "1999-01-01");
        // Keys first, then the version, while unselected.
        let r =
            answer(&s, &mut session, request(5, "tools/list", json!({"_meta": {META_PROTOCOL_VERSION: "1999-01-01"}})));
        assert_eq!(r["error"]["code"], -32602);
        assert!(!session.is_selected(), "probes and errors leave the lifecycle open");
        // Once inline, the version is checked first.
        answer(&s, &mut session, request(6, "tools/list", json!({"_meta": meta("2026-07-28")})));
        assert_eq!(session.lifecycle, Lifecycle::Inline);
        let r =
            answer(&s, &mut session, request(7, "tools/list", json!({"_meta": {META_PROTOCOL_VERSION: "1999-01-01"}})));
        assert_eq!(r["error"]["code"], -32022);
    }

    #[test]
    fn malformed_initialize() {
        let s = server();
        let mut session = Session::new();
        let r = answer(&s, &mut session, request(1, "initialize", json!({"protocolVersion": "2025-06-18"})));
        assert_eq!(r["error"]["code"], -32602, "unselected: no _meta");
        let r = answer(&s, &mut session, init(2, "2025-06-18"));
        assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
        let r = answer(
            &s,
            &mut session,
            request(
                3,
                "initialize",
                json!({"protocolVersion": 5, "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}),
            ),
        );
        assert_eq!(r["error"], json!({"code": -32601, "message": "initialize"}));
        let bad_info = json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "0", "icons": {}}});
        let r = answer(&s, &mut session, request(4, "initialize", bad_info));
        assert_eq!(r["error"]["code"], -32601);
    }

    #[test]
    fn second_initialize_is_negotiated() {
        let s = server();
        let mut session = Session::new();
        answer(&s, &mut session, init(1, "2025-06-18"));
        // rmcp stored "2026-07-28" here and later results turned modern.
        let r = answer(&s, &mut session, init(2, "2026-07-28"));
        assert_eq!(r["result"]["protocolVersion"], "2025-11-25");
        assert_eq!(session, Session::legacy("2025-11-25"));
        let r = answer(&s, &mut session, request(3, "tools/list", json!({})));
        assert!(r["result"].get("resultType").is_none(), "{r}");
        assert_eq!(answer(&s, &mut session, request(4, "ping", json!({})))["result"], json!({}));
    }

    #[test]
    fn result_shapes_follow_the_version() {
        let s = server();
        let mut session = Session::legacy("2025-06-18");
        let legacy = answer(&s, &mut session, request(1, "prompts/list", json!({})));
        assert_eq!(legacy["result"], json!({"prompts": []}));
        let modern =
            answer(&s, &mut session, request(2, "resources/templates/list", json!({"_meta": meta("2026-07-28")})));
        assert_eq!(
            modern["result"],
            json!({"resultType": "complete", "ttlMs": 0, "cacheScope": "private", "resourceTemplates": []})
        );
        // A legacy session accepts _meta naming a legacy version: legacy shape.
        let r = answer(&s, &mut session, request(3, "tools/list", json!({"_meta": meta("2025-06-18")})));
        assert!(r["result"].get("resultType").is_none());
        // ping with a modern _meta on a legacy session: not a legacy request.
        let r = answer(&s, &mut session, request(4, "ping", json!({"_meta": meta("2026-07-28")})));
        assert_eq!(r["error"], json!({"code": -32601, "message": "ping"}));
        let complete = json!({"ref": {"type": "ref/resource", "uri": "x"}, "argument": {"name": "a", "value": "b"}});
        assert_eq!(
            answer(&s, &mut session, request(5, "completion/complete", complete))["result"],
            json!({"completion": {"values": []}})
        );
        let r = answer(&s, &mut session, request(6, "completion/complete", json!({"ref": {"type": "ref/x"}})));
        assert_eq!(r["error"]["message"], "completion/complete");
    }

    #[test]
    fn dispatch_table() {
        let s = server();
        let mut session = Session::legacy("2025-06-18");
        for method in [
            "logging/setLevel",
            "prompts/get",
            "resources/read",
            "resources/subscribe",
            "tasks/get",
            "roots/list",
            "foo/bar",
        ] {
            let r = answer(&s, &mut session, request(1, method, json!({})));
            assert_eq!(r["error"], json!({"code": -32601, "message": method}), "{method}");
        }
        let r = answer(&s, &mut session, request(2, "server/discover", json!(null)));
        assert_eq!(r["error"]["message"], "server/discover", "no params: not a discover");
        let r = answer(&s, &mut session, request(3, "server/discover", json!({})));
        assert_eq!(r["error"]["code"], -32602, "a discover needs _meta");
    }

    #[test]
    fn tool_call_validation() {
        let s = server();
        let mut session = Session::legacy("2025-06-18");
        let cases = [
            (json!({}), "invalid params for `tools/call`: missing `name`"),
            (json!(null), "invalid params for `tools/call`: missing `name`"),
            (json!({"name": 5}), "invalid params for `tools/call`: `name` must be a string"),
            (
                json!({"name": "trace", "arguments": "x"}),
                "invalid params for `tools/call`: `arguments` must be an object",
            ),
            (
                json!({"name": "trace", "arguments": [1]}),
                "invalid params for `tools/call`: `arguments` must be an object",
            ),
            (
                json!({"name": "trace", "requestState": 1}),
                "invalid params for `tools/call`: `requestState` must be a string",
            ),
        ];
        for (params, message) in cases {
            let r = answer(&s, &mut session, request(1, "tools/call", params.clone()));
            assert_eq!(r["error"], json!({"code": -32602, "message": message}), "{params}");
        }
        let r = answer(&s, &mut session, request(2, "tools/call", json!({"name": "nope"})));
        assert_eq!(r["error"]["code"], -32602);
        assert!(r["error"]["message"].as_str().unwrap().starts_with("unknown tool `nope` (available: search_symbols,"));
        let step = s.handle(request(3, "tools/call", json!({"name": "trace", "arguments": null})), &mut session);
        let Step::Call(call) = step else { panic!("{step:?}") };
        assert_eq!(call, ToolCall { id: RequestId::Number(3), name: "trace".into(), args: Value::Null, modern: false });
        let step = s.handle(
            request(
                4,
                "tools/call",
                json!({"_meta": meta("2026-07-28"), "name": "trace", "arguments": {"a": 1}, "requestState": "s"}),
            ),
            &mut session,
        );
        let Step::Call(call) = step else { panic!("{step:?}") };
        assert!(call.modern);
        assert_eq!(call.args, json!({"a": 1}));
    }

    #[test]
    fn notifications() {
        let s = server();
        let cancel = |id: Value| {
            msg(json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": id}}))
        };
        let mut session = Session::new();
        assert!(matches!(s.handle(cancel(json!(1)), &mut session), Step::Silent), "ignored before the lifecycle");
        let mut session = Session::legacy("2025-06-18");
        assert!(matches!(s.handle(cancel(json!(1)), &mut session), Step::Cancel(RequestId::Number(1))));
        assert!(matches!(s.handle(cancel(json!("a")), &mut session), Step::Cancel(RequestId::String(_))));
        assert!(matches!(s.handle(cancel(json!(1.5)), &mut session), Step::Silent));
        let bad_reason = msg(
            json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": 1, "reason": 2}}),
        );
        assert!(matches!(s.handle(bad_reason, &mut session), Step::Silent));
        for m in ["notifications/initialized", "notifications/progress", "x/y"] {
            assert!(matches!(s.handle(msg(json!({"jsonrpc": "2.0", "method": m})), &mut session), Step::Silent), "{m}");
        }
        assert!(matches!(s.handle(Incoming::Response, &mut Session::new()), Step::Silent));
        assert!(matches!(s.handle(Incoming::Invalid, &mut Session::new()), Step::Reply(_)));
    }
}
