//! JSON-RPC 2.0 framing: classifying one incoming line and serializing one
//! outgoing message (`docs/mcp-protocol.md` §2 and §3).
//!
//! Pure functions, no I/O: the stdio transport feeds lines in, an HTTP
//! transport would feed request bodies in.

use serde::Serialize;
use serde_json::value::RawValue;
use serde_json::{Map, Value};

/// A JSON object, as found in `params` and `_meta`.
pub type JsonObject = Map<String, Value>;

/// A request id: an integer or a string. `1` and `"1"` are different ids.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(untagged)]
pub enum RequestId {
    Number(i64),
    String(String),
}

impl RequestId {
    /// Reads an id the way rmcp 3.10 does: a string, an `i64`, a `u64` that
    /// fits in one, or an integral float below 2^53 (`1.0` and `1e2` become
    /// `1` and `100`). Anything else (`null`, `1.5`, an object) is not an id.
    pub fn from_value(v: &Value) -> Option<RequestId> {
        match v {
            Value::String(s) => Some(RequestId::String(s.clone())),
            Value::Number(n) => n.as_i64().or_else(|| integral_f64(n.as_f64()?)).map(RequestId::Number),
            _ => None,
        }
    }
}

impl std::fmt::Display for RequestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RequestId::Number(n) => write!(f, "{n}"),
            RequestId::String(s) => write!(f, "{s:?}"),
        }
    }
}

/// The integer a float stands for, when it is one an `f64` represents
/// exactly (2^53 itself is excluded: 2^53 + 1 parses to the same `f64`).
fn integral_f64(f: f64) -> Option<i64> {
    const MAX_EXACT: f64 = 9_007_199_254_740_992.0; // 2^53
    // The range check makes the cast exact.
    (f.fract() == 0.0 && f.abs() < MAX_EXACT).then_some(f as i64)
}

/// A request from the client: it gets exactly one response.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub id: RequestId,
    pub method: String,
    /// `None` when `params` is absent or `null`.
    pub params: Option<JsonObject>,
}

/// A notification from the client: it never gets a response.
#[derive(Debug, Clone, PartialEq)]
pub struct Notification {
    pub method: String,
    pub params: Option<JsonObject>,
}

/// One incoming line, classified (`docs/mcp-protocol.md` §2).
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    Request(Request),
    Notification(Notification),
    /// A response or an error sent by the client. Rueca never sends requests,
    /// so these are ignored.
    Response,
    /// Valid JSON that is not a valid message: answered with -32600 and no id.
    Invalid,
    /// A malformed message that names a method outside MCP (an LSP
    /// notification, a non-standard `notifications/…`): ignored.
    Ignored,
    /// Not JSON at all (syntax error, invalid UTF-8, a blank line): ignored,
    /// no -32700 (rmcp does the same, rust-sdk#938).
    Unparsable,
}

const UTF8_BOM: &[u8] = b"\xEF\xBB\xBF";

/// Classifies one line (without its `\n`; a trailing `\r` and a leading
/// UTF-8 BOM are stripped here).
pub fn parse_line(line: &[u8]) -> Incoming {
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    let line = line.strip_prefix(UTF8_BOM).unwrap_or(line);
    match serde_json::from_slice::<Value>(line) {
        Ok(value) => classify(value),
        Err(_) => Incoming::Unparsable,
    }
}

/// Classifies a parsed JSON value.
pub fn classify(value: Value) -> Incoming {
    let Value::Object(obj) = value else { return Incoming::Invalid };
    let parsed = match Shape::of(&obj) {
        Some(Shape::Request) => parse_request(&obj),
        Some(Shape::Notification) => parse_notification(&obj),
        Some(Shape::Response) => valid_response(&obj).then_some(Incoming::Response),
        Some(Shape::Error) => valid_error(&obj).then_some(Incoming::Response),
        None => None,
    };
    parsed.unwrap_or_else(|| invalid_or_ignored(&obj))
}

/// Which message an object claims to be, from the keys it has (rmcp's
/// `JsonRpcMessageShape`): mixed shapes are no message at all.
enum Shape {
    Request,
    Notification,
    Response,
    Error,
}

impl Shape {
    fn of(obj: &JsonObject) -> Option<Shape> {
        let has = |k: &str| obj.contains_key(k);
        match (has("method"), has("id"), has("result"), has("error")) {
            (true, true, false, false) => Some(Shape::Request),
            (true, false, false, false) => Some(Shape::Notification),
            (false, _, true, false) => Some(Shape::Response),
            (false, _, false, true) => Some(Shape::Error),
            _ => None,
        }
    }
}

fn is_jsonrpc_2(obj: &JsonObject) -> bool {
    obj.get("jsonrpc").and_then(Value::as_str) == Some("2.0")
}

/// `params`: absent, `null` or an object whose `_meta` is absent, `null` or
/// an object. `Err` = not a valid message.
fn parse_params(obj: &JsonObject) -> Result<Option<JsonObject>, ()> {
    match obj.get("params") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(p)) if matches!(p.get("_meta"), None | Some(Value::Null | Value::Object(_))) => {
            Ok(Some(p.clone()))
        }
        Some(_) => Err(()),
    }
}

fn method_and_params(obj: &JsonObject) -> Option<(String, Option<JsonObject>)> {
    if !is_jsonrpc_2(obj) {
        return None;
    }
    let method = obj.get("method")?.as_str()?.to_string();
    let params = parse_params(obj).ok()?;
    Some((method, params))
}

fn parse_request(obj: &JsonObject) -> Option<Incoming> {
    let id = RequestId::from_value(obj.get("id")?)?;
    let (method, params) = method_and_params(obj)?;
    Some(Incoming::Request(Request { id, method, params }))
}

fn parse_notification(obj: &JsonObject) -> Option<Incoming> {
    let (method, params) = method_and_params(obj)?;
    Some(Incoming::Notification(Notification { method, params }))
}

fn valid_response(obj: &JsonObject) -> bool {
    is_jsonrpc_2(obj) && obj.get("id").and_then(RequestId::from_value).is_some()
}

fn valid_error(obj: &JsonObject) -> bool {
    let id_ok = match obj.get("id") {
        None | Some(Value::Null) => true,
        Some(id) => RequestId::from_value(id).is_some(),
    };
    let error_ok = obj.get("error").and_then(Value::as_object).is_some_and(|e| {
        e.get("code").and_then(Value::as_i64).is_some_and(|c| i32::try_from(c).is_ok())
            && e.get("message").is_some_and(Value::is_string)
    });
    is_jsonrpc_2(obj) && id_ok && error_ok
}

/// An invalid message is answered with -32600, unless it names a method
/// outside MCP (rmcp's `should_ignore_notification`): (a) it has no `id`
/// and the method is not a standard MCP method, or (b) the method starts
/// with `notifications/` and is not a standard notification.
fn invalid_or_ignored(obj: &JsonObject) -> Incoming {
    let Some(method) = obj.get("method").and_then(Value::as_str) else { return Incoming::Invalid };
    let foreign = !obj.contains_key("id") && !is_standard_method(method);
    let foreign_notification = method.starts_with("notifications/") && !is_standard_notification(method);
    if foreign || foreign_notification { Incoming::Ignored } else { Incoming::Invalid }
}

const STANDARD_REQUESTS: &[&str] = &[
    "initialize",
    "ping",
    "server/discover",
    "prompts/get",
    "prompts/list",
    "resources/list",
    "resources/read",
    "resources/subscribe",
    "resources/unsubscribe",
    "resources/templates/list",
    "subscriptions/listen",
    "tools/call",
    "tools/list",
    "completion/complete",
    "elicitation/create",
    "logging/setLevel",
    "roots/list",
    "sampling/createMessage",
];

const STANDARD_NOTIFICATIONS: &[&str] = &[
    "notifications/cancelled",
    "notifications/initialized",
    "notifications/message",
    "notifications/progress",
    "notifications/prompts/list_changed",
    "notifications/resources/list_changed",
    "notifications/resources/updated",
    "notifications/roots/list_changed",
    "notifications/subscriptions/acknowledged",
    "notifications/tools/list_changed",
];

fn is_standard_method(method: &str) -> bool {
    STANDARD_REQUESTS.contains(&method) || is_standard_notification(method)
}

fn is_standard_notification(method: &str) -> bool {
    STANDARD_NOTIFICATIONS.contains(&method)
}

/// JSON-RPC error codes Rueca emits (`docs/mcp-protocol.md` §3).
pub mod code {
    pub const INVALID_REQUEST: i32 = -32600;
    pub const METHOD_NOT_FOUND: i32 = -32601;
    pub const INVALID_PARAMS: i32 = -32602;
    pub const UNSUPPORTED_PROTOCOL_VERSION: i32 = -32022;
    /// A tool call stopped by its deadline ([`crate::ToolCall::run_with`];
    /// only a transport that sets one, such as `rueca serve`, sees it).
    pub const DEADLINE_EXCEEDED: i32 = -32001;
}

/// The `error` member of an error response, in wire order: `code`,
/// `message`, `data` (omitted when absent).
#[derive(Debug, Clone, Serialize)]
pub struct ErrorObject {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Box<RawValue>>,
}

impl ErrorObject {
    pub fn new(code: i32, message: impl Into<String>) -> ErrorObject {
        ErrorObject { code, message: message.into(), data: None }
    }

    /// -32601 with the method itself as the message, as rmcp answers.
    pub fn method_not_found(method: &str) -> ErrorObject {
        ErrorObject::new(code::METHOD_NOT_FOUND, method)
    }
}

/// One outgoing message.
#[derive(Debug, Clone)]
pub enum Outgoing {
    /// `{"jsonrpc":"2.0","id":…,"result":…}`; the result is already JSON
    /// text in its final key order.
    Result { id: RequestId, result: Box<RawValue> },
    /// `{"jsonrpc":"2.0","id":…,"error":{…}}`; `id` omitted when unknown.
    Error { id: Option<RequestId>, error: ErrorObject },
}

impl Outgoing {
    /// A result response: `result` is serialized now, in its field order.
    pub fn result(id: RequestId, result: &impl Serialize) -> Outgoing {
        Outgoing::Result { id, result: to_raw(result) }
    }

    pub fn error(id: RequestId, error: ErrorObject) -> Outgoing {
        Outgoing::Error { id: Some(id), error }
    }

    /// The -32600 answer to an invalid message: no id, even when one was readable.
    pub fn invalid_request() -> Outgoing {
        Outgoing::Error { id: None, error: ErrorObject::new(code::INVALID_REQUEST, "Invalid request") }
    }

    pub fn id(&self) -> Option<&RequestId> {
        match self {
            Outgoing::Result { id, .. } => Some(id),
            Outgoing::Error { id, .. } => id.as_ref(),
        }
    }

    /// The compact JSON text of the message (no newline).
    pub fn to_json(&self) -> String {
        let envelope = match self {
            Outgoing::Result { id, result } => {
                Envelope { jsonrpc: "2.0", id: Some(id), result: Some(result), error: None }
            }
            Outgoing::Error { id, error } => {
                Envelope { jsonrpc: "2.0", id: id.as_ref(), result: None, error: Some(error) }
            }
        };
        serde_json::to_string(&envelope).expect("JSON-RPC envelopes serialize")
    }

    /// The stdio line: compact JSON plus `\n`.
    pub fn to_line(&self) -> Vec<u8> {
        let mut line = self.to_json().into_bytes();
        line.push(b'\n');
        line
    }
}

#[derive(Serialize)]
struct Envelope<'a> {
    jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<&'a RequestId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<&'a RawValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'a ErrorObject>,
}

/// Serializes a protocol structure (fixed field order) to JSON text.
pub(crate) fn to_raw(value: &impl Serialize) -> Box<RawValue> {
    serde_json::value::to_raw_value(value).expect("MCP results have string keys and serialize")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(s: &str) -> Incoming {
        parse_line(s.as_bytes())
    }

    fn req(id: RequestId, method: &str) -> Incoming {
        Incoming::Request(Request { id, method: method.into(), params: None })
    }

    #[test]
    fn classification_table() {
        use Incoming::*;
        let n = RequestId::Number;
        let cases: &[(&str, Incoming)] = &[
            (r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#, req(n(1), "ping")),
            (r#"{"jsonrpc":"2.0","id":1.0,"method":"ping"}"#, req(n(1), "ping")),
            (r#"{"jsonrpc":"2.0","id":1e2,"method":"ping"}"#, req(n(100), "ping")),
            (r#"{"jsonrpc":"2.0","id":"1","method":"ping"}"#, req(RequestId::String("1".into()), "ping")),
            (r#"{"jsonrpc":"2.0","id":-7,"method":"ping","params":null}"#, req(n(-7), "ping")),
            (r#"{"jsonrpc":"2.0","id":1,"method":"ping","extra":true}"#, req(n(1), "ping")),
            (r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#, req(n(1), "ping")),
            (r#"{"jsonrpc":"2.0","id":1.5,"method":"ping"}"#, Invalid),
            (r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#, Invalid),
            (r#"{"jsonrpc":"2.0","id":9223372036854775808,"method":"ping"}"#, Invalid),
            (r#"{"jsonrpc":"2.0","id":9007199254740992.0,"method":"ping"}"#, Invalid),
            (r#"{"jsonrpc":"2.0","id":{},"method":"ping"}"#, Invalid),
            (r#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#, Invalid),
            (r#"{"id":1,"method":"ping"}"#, Invalid),
            (r#"{"jsonrpc":"2.0","id":1,"method":5}"#, Invalid),
            (r#"{"jsonrpc":"2.0","id":1,"method":"ping","params":[1]}"#, Invalid),
            (r#"{"jsonrpc":"2.0","id":1,"method":"ping","params":{"_meta":3}}"#, Invalid),
            (r#"{"jsonrpc":"2.0","id":1,"method":"ping","result":{}}"#, Invalid),
            (r#"{"jsonrpc":"2.0","id":1,"result":{},"error":{}}"#, Invalid),
            (r#"{"foo":"bar"}"#, Invalid),
            (r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#, Invalid),
            (r#""hi""#, Invalid),
            ("3", Invalid),
            ("null", Invalid),
            (r#"{"jsonrpc":"2.0","id":1,"result":{}}"#, Response),
            (r#"{"jsonrpc":"2.0","id":1,"result":5}"#, Response),
            (r#"{"jsonrpc":"2.0","error":{"code":-1,"message":"x"}}"#, Response),
            (r#"{"jsonrpc":"2.0","id":null,"error":{"code":-1,"message":"x"}}"#, Response),
            (r#"{"jsonrpc":"2.0","error":{"code":"x","message":"x"}}"#, Invalid),
            (r#"{"jsonrpc":"2.0","result":{}}"#, Invalid),
            // Malformed messages that name a method outside MCP are ignored.
            (r#"{"method":"textDocument/didOpen"}"#, Ignored),
            (r#"{"jsonrpc":"2.0","id":1,"method":"notifications/foo","params":[]}"#, Ignored),
            (r#"{"jsonrpc":"1.0","method":"notifications/initialized"}"#, Invalid),
            (r#"{"jsonrpc":"1.0","id":1,"method":"foo/bar"}"#, Invalid),
            ("{not json", Unparsable),
            ("", Unparsable),
            ("   ", Unparsable),
            ("{\"jsonrpc\":\"2.0\",\"id\":1,", Unparsable),
        ];
        for (line, expected) in cases {
            assert_eq!(&parse(line), expected, "{line}");
        }
        assert_eq!(parse_line(b"\xff\xfe"), Unparsable);
    }

    #[test]
    fn notifications_and_params() {
        let got = parse(r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":3}}"#);
        let Incoming::Notification(n) = got else { panic!("{got:?}") };
        assert_eq!(n.method, "notifications/cancelled");
        assert_eq!(n.params.unwrap()["requestId"], 3);
        // A valid notification with any method is a notification (the protocol ignores it).
        assert!(matches!(parse(r#"{"jsonrpc":"2.0","method":"x/y"}"#), Incoming::Notification(_)));
    }

    #[test]
    fn crlf_and_bom_are_stripped() {
        let n = RequestId::Number(16);
        assert_eq!(parse("{\"jsonrpc\":\"2.0\",\"id\":16,\"method\":\"ping\"}\r"), req(n.clone(), "ping"));
        assert_eq!(parse("\u{feff}{\"jsonrpc\":\"2.0\",\"id\":16,\"method\":\"ping\"}"), req(n, "ping"));
        assert_eq!(parse("\r"), Incoming::Unparsable);
    }

    #[test]
    fn envelopes_in_wire_order() {
        let ok = Outgoing::result(RequestId::Number(1), &json!({}));
        assert_eq!(ok.to_json(), r#"{"jsonrpc":"2.0","id":1,"result":{}}"#);
        let err = Outgoing::error(RequestId::String("a".into()), ErrorObject::method_not_found("foo/bar"));
        assert_eq!(err.to_json(), r#"{"jsonrpc":"2.0","id":"a","error":{"code":-32601,"message":"foo/bar"}}"#);
        assert_eq!(
            Outgoing::invalid_request().to_json(),
            r#"{"jsonrpc":"2.0","error":{"code":-32600,"message":"Invalid request"}}"#
        );
        let mut e = ErrorObject::new(-32022, "Unsupported protocol version");
        e.data = Some(to_raw(&json!({"requested": "x"})));
        let line = Outgoing::error(RequestId::Number(2), e).to_line();
        assert_eq!(
            String::from_utf8(line).unwrap(),
            "{\"jsonrpc\":\"2.0\",\"id\":2,\"error\":{\"code\":-32022,\"message\":\"Unsupported protocol version\",\"data\":{\"requested\":\"x\"}}}\n"
        );
        // Non-ASCII stays raw UTF-8, as serde_json writes it.
        let text = Outgoing::result(RequestId::Number(3), &json!("→ 0–1")).to_json();
        assert_eq!(text, "{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":\"→ 0–1\"}");
    }

    #[test]
    fn id_display() {
        assert_eq!(RequestId::Number(4).to_string(), "4");
        assert_eq!(RequestId::String("x".into()).to_string(), "\"x\"");
    }
}
