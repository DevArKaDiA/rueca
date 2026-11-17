# MCP protocol reference (Rueca's own layer)

Status: behaviour reference of Rueca's own MCP layer in `crates/rueca-mcp`,
which replaced `rmcp` 3.10.0 on 2026-11-17. Why: `cargo deny` failed on
RUSTSEC-2026-0543 (`chrono` unmaintained). `chrono` reaches Rueca only through `rmcp`: directly,
and through `rmcp`'s `schemars` dependency with feature `chrono04`.
`rueca-query`'s own `schemars = "1"` (default features `derive`, `std`) does
not enable it. The owner chose to write a small MCP layer that behaves the
same for clients.

This document describes what clients observe from `rueca mcp` at `c820290`
(rmcp 3.10.0, Rueca's `ServerHandler`). Every "rmcp does X" statement below
comes from reading rmcp's source (paths in §11). The golden transcripts
(`crates/rueca-mcp/tests/golden/`) are the final word: if they disagree with
this document, the transcripts win and this document gets fixed. Words
**MUST** and **SHOULD** describe the target implementation.

## 1. Scope

Rueca serves **tools only**. It never sends server-to-client requests or
notifications.

- `capabilities`: exactly `{"tools":{}}`. rmcp's
  `ServerCapabilities::builder().enable_tools()` sets
  `ToolsCapability::default()`, and every `None` field is skipped, so there is
  no `listChanged` key.
- Server identity (`Implementation`, fields in this order, `None` fields
  skipped): `{"name":"rueca","title":"Rueca code intelligence","version":"<CARGO_PKG_VERSION of rueca-mcp>"}`.
- `instructions`: `rueca_mcp::INSTRUCTIONS`, unchanged. It contains non-ASCII
  characters (`→ … –`). serde_json writes them as raw UTF-8 and escapes only
  `"`, `\` and control characters, so do the same.
- Tools: `rueca_query::tool_definitions()`, in `TOOL_NAMES` order:
  `search_symbols, search_code, get_symbol, get_snippet, trace, get_file_outline, list_repositories, list_contracts`.

## 2. Framing (stdio)

- One JSON-RPC message per line, UTF-8, ended by `\n`. A trailing `\r` is
  stripped. A leading UTF-8 BOM is stripped. Empty lines are skipped.
- stdout carries protocol messages only. Logs go to stderr through `tracing`
  (the binary already installs a stderr subscriber).
- Output: compact JSON (`serde_json::to_writer`, no spaces), then `\n`, then a
  flush, one message at a time. Only one writer may run at a time, so a line
  is never interleaved with another.
- Line limit: 16 MiB (`DEFAULT_MAX_LINE_LENGTH`). A longer line is discarded up
  to its `\n`, logged at error level, and gets **no reply**.
- Incomplete last line at EOF (no `\n`): dropped.

Input classification. rmcp does this in `try_parse_with_compatibility` and
`receive`, in this order:

| Input line | Reply |
|---|---|
| Not valid JSON (syntax error, truncated, invalid UTF-8) | **none** (debug log). rmcp deliberately sends no `-32700` (rust-sdk#938). |
| Valid JSON that is not a valid message: a batch array, a number, string or `null`, missing `jsonrpc` or `jsonrpc` other than `"2.0"`, `id` null, fractional or above `i64::MAX`, `params` neither an object nor `null`, `_meta` neither an object nor `null`, `method` not a string, mixed shapes (`method` together with `result` or `error`, or both `result` and `error`) | `{"jsonrpc":"2.0","error":{"code":-32600,"message":"Invalid request"}}`. `id` is **omitted**, even when it was readable. One exception, the row below. |
| The same invalid object, but it is a JSON object with a string `method` and either (a) it has no `id` key and `method` is not a standard MCP method, or (b) `method` starts with `notifications/` and is not a standard notification | **none** (ignored as a non-MCP notification) |
| Request (has `method` and `id`) | dispatched (§4 to §6) |
| Notification (has `method`, no `id`) | dispatched (§6) |
| Response or error sent by the client (`result` or `error`, no `method`) | ignored after the lifecycle is chosen. Before it, see §4. |

The standard method list, for rule (a): `initialize, ping, server/discover,
prompts/get, prompts/list, resources/list, resources/read, resources/subscribe,
resources/unsubscribe, resources/templates/list, subscriptions/listen,
tools/call, tools/list, completion/complete, elicitation/create,
logging/setLevel, roots/list, sampling/createMessage`, plus the standard
notifications: `notifications/{cancelled, initialized, message, progress,
prompts/list_changed, resources/list_changed, resources/updated,
roots/list_changed, subscriptions/acknowledged, tools/list_changed}`.

Other framing rules:

- Ids: an integer (`i64`, or a `u64` that fits) or a string. An integral
  float (`1.0`, `1e2`) becomes the integer and is echoed as `1` or `100`.
  `"1"` and `1` are different ids. Responses echo the parsed id.
- Unknown extra top-level members are accepted.
- Batching: removed from MCP in 2025-06-18. rmcp answers an array with the
  `-32600` line above and processes none of its elements. Rueca does the same
  for every protocol version, including 2025-03-26, whose spec required batch
  support. This is a known gap that rmcp shares.
- EOF on stdin: stop reading. Wait up to **5 s** for in-flight requests and
  write their responses. Responses to cancelled requests are still dropped.
  Then exit 0. For EOF before any lifecycle is chosen, see §4.

## 3. JSON-RPC shapes

Key order is fixed. Use `#[derive(Serialize)]` structs, never `json!`, for the
envelopes. serde_json's `preserve_order` happens to be on (enabled by
`oxc_resolver`, which `rueca-index` pulls in), but envelopes must not depend
on it.

```text
result:  {"jsonrpc":"2.0","id":<id>,"result":<object>}
error:   {"jsonrpc":"2.0","id":<id>,"error":{"code":<int>,"message":<string>,"data":<any>}}
         ("id" omitted when unknown, "data" omitted when absent)
```

| Code | Name | Emitted by Rueca when |
|---|---|---|
| -32700 | Parse error | never (see §2) |
| -32600 | Invalid request | an invalid message (§2), id omitted, message `Invalid request` |
| -32601 | Method not found | an unknown or unimplemented method (§6). `message` is **the method string itself**, e.g. `"logging/setLevel"`, with no `data` |
| -32602 | Invalid params | missing per-request `_meta` (§5), or an unknown tool (§6) |
| -32603 | Internal error | never |
| -32022 | UnsupportedProtocolVersion | §5. `message` `Unsupported protocol version`, `data` `{"requested":"<v>","supported":["2024-11-05","2025-03-26","2025-06-18","2025-11-25","2026-07-28"]}` |
| -32020, -32021 | HeaderMismatch, MissingRequiredClientCapability | never on stdio. -32020 belongs to HTTP (RFC 0008). -32021 only applies to `tasks/*`, which Rueca does not advertise. |

Exact messages:

- Missing `_meta`: `request _meta is missing or has malformed required fields: ` followed by the missing keys joined with `, `, in this order:
  `io.modelcontextprotocol/protocolVersion`, `io.modelcontextprotocol/clientCapabilities`.
- Unknown tool (Rueca's own message, unchanged): ``unknown tool `<name>` (available: search_symbols, search_code, get_symbol, get_snippet, trace, get_file_outline, list_repositories, list_contracts)``.

## 4. Lifecycle selection and legacy `initialize`

Supported versions, oldest first, which is also the advertised order (rmcp's
`ProtocolVersion::KNOWN_VERSIONS`): `2024-11-05, 2025-03-26, 2025-06-18,
2025-11-25, 2026-07-28`. A version "has initialize" when it compares below
`2026-07-28` as a plain string comparison. That holds for unknown strings
too: `""` has initialize, `"foo"` does not.

The connection starts in state **Unselected**. Messages are handled **one at a
time, in order**, until a lifecycle is chosen:

1. A request whose method is `ping` and whose params are absent, `null` or an
   object: reply `{}`. Stay Unselected.
2. A request that is a well-formed `initialize`: `params` contains
   `protocolVersion` (string), `capabilities` (an object, see "valid
   ClientCapabilities" in §5) and `clientInfo` (an object with string `name`
   and `version`; optional `title`, `description`, `websiteUrl` strings and an
   `icons` array). Reply with the InitializeResult below. The state becomes
   **Legacy** with `session_version` = the negotiated version.
3. Any other request, including an unknown method or a malformed `initialize`:
   1. Look up the required `_meta` keys (§5). If any is missing or
      malformed, reply -32602 with the message from §3. Stay Unselected.
   2. If it is a well-formed `server/discover`, run the per-request checks
      from §5 and reply. A version that is a string but not supported gets
      -32022; a supported version gets the DiscoverResult. Stay Unselected:
      discover is a probe.
   3. If the `_meta` protocolVersion is not supported, reply -32022. Stay
      Unselected.
   4. Otherwise the state becomes **Inline**. Dispatch the request as in §5
      and §6.
4. A notification, response or error message: ignored (debug log), the
   state stays Unselected. EOF while Unselected is a normal shutdown: exit 0,
   nothing on stdout. rmcp ended the server here with exit 1; see §9.1.

Once Legacy or Inline, requests run **concurrently**. Responses arrive in
completion order, not request order.

Negotiation, for every well-formed `initialize`: if the requested version has
initialize and is supported, echo it. Otherwise answer `2025-11-25`, the
newest supported version with initialize. That covers `2026-07-28`, unknown
versions, newer dates, `2024-10-07` (the TypeScript SDK still lists it), `""`
and `"foo"`. The handshake never fails.

InitializeResult, in this field order (`_meta` absent):

```json
{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"rueca","title":"Rueca code intelligence","version":"0.1.0-alpha.2"},"instructions":"Rueca is a code-intelligence index …"}
```

After the lifecycle is chosen:

- `notifications/initialized` is ignored: no reply, info log `client initialized`.
- A later well-formed `initialize`, in either state, is answered again with
  the same rule, and `session_version` becomes the negotiated version (rmcp
  stored the raw requested one, see §9.3). Legacy stays Legacy, Inline stays
  Inline.
- A malformed `initialize` gets -32601 `"initialize"`.
- Nothing waits for `notifications/initialized`. Requests sent before it are
  served, which matches the TypeScript SDK and the spec's SHOULD NOT.

## 5. Per-request `_meta` and the 2026-07-28 rules

`params._meta` keys, as defined by the 2026-07-28 spec, `basic` page:

| Key | Valid when | Required |
|---|---|---|
| `io.modelcontextprotocol/protocolVersion` | a JSON string (any value) | yes |
| `io.modelcontextprotocol/clientCapabilities` | valid ClientCapabilities | yes |
| `io.modelcontextprotocol/clientInfo` | an Implementation object | no, and never checked |

Valid ClientCapabilities: a JSON object. When present and not `null`, its
known members must be objects: `experimental` and `extensions` (maps of
objects), `roots` (whose `listChanged`, if present, is a boolean), `sampling`,
`elicitation`. Unknown members are allowed. A malformed value counts as
missing.

Every request after the lifecycle is chosen goes through these steps, in this
order. This is rmcp's `Service::handle_request`, `handler/server.rs`.

- `meta_v` = the `_meta` protocolVersion, if it is a string.
- `pv` = `meta_v`, otherwise `session_version` (Legacy only).
- `modern` = `pv >= "2026-07-28"`, as a string comparison.

1. If the method is not a well-formed `initialize` and `meta_v` is present but
   not in the supported list: reply -32022.
2. `needs_meta` = (not a well-formed `initialize`) and (the method is a
   well-formed `server/discover`, or the state is Inline, or
   `meta_v >= "2026-07-28"`). If `needs_meta` and a required key is missing
   or malformed: reply -32602 with the message from §3.
3. `legacy_request` = `!needs_meta && (pv is absent || pv < "2026-07-28")`.
4. Dispatch (§6).
5. Result shape:
   - If `modern`: keep `"resultType":"complete"` on the results that carry it
     (`tools/list`, `tools/call`, `completion/complete`, `prompts/list`,
     `resources/list`, `resources/templates/list`). Missing `ttlMs` and
     `cacheScope` are filled with `0` and `"private"` on the cacheable lists.
   - Otherwise drop `resultType`.
   - Errors are not touched.

The order differs before a lifecycle is chosen (§4 step 3): keys are checked
first, then the version. After it, the version is checked first. Example: a
request with protocolVersion `"1999-01-01"` and no clientCapabilities gets
-32602 while Unselected and -32022 once Inline.

`_meta` is checked again on **every** Inline request. A Legacy connection
also accepts requests that carry `_meta`: they are checked, and their shape
follows `pv`.

Rueca does **not** add `io.modelcontextprotocol/serverInfo` to result `_meta`.
The spec says SHOULD; rmcp only puts it on the discover result. The request's
`_meta` is never echoed back.

DiscoverResult. `params` must be an object (absent params means it is not a
well-formed discover). `params` members other than `_meta` are ignored
(golden check: `DiscoverRequestParams` is `deny_unknown_fields`, but it is
flattened). The result is the same for every version:

```json
{"resultType":"complete","supportedVersions":["2024-11-05","2025-03-26","2025-06-18","2025-11-25","2026-07-28"],"capabilities":{"tools":{}},"instructions":"Rueca is …","ttlMs":0,"cacheScope":"private","_meta":{"io.modelcontextprotocol/serverInfo":{"name":"rueca","title":"Rueca code intelligence","version":"0.1.0-alpha.2"}}}
```

Discover lists every supported version, including the legacy ones. The
TypeScript and Python SDKs list only modern versions. Keep rmcp's list.

## 6. Methods

After the lifecycle is chosen. "Well-formed" means the params parse with
rmcp's typed shape. A request that does not parse falls through to rmcp's
`CustomRequest` and gets **-32601 with the method as message**. That is the
default outcome for every row below.

| Method | Reply |
|---|---|
| `ping` (params absent, `null` or an object) | `legacy_request` ? `{}` : -32601 `"ping"`. Inline 2026 connections get -32601 once Inline. Before that, §4 step 1 answers `{}`. |
| `tools/list` (params absent, `null` or an object; `cursor` ignored, even when it has the wrong type) | legacy: `{"ttlMs":3600000,"cacheScope":"public","tools":[…]}`. modern: `{"resultType":"complete","ttlMs":3600000,"cacheScope":"public","tools":[…]}`. No `nextCursor`. |
| `tools/call` (needs `params.name` as a string; `arguments` absent, `null` or an object; `requestState`, if present, a string) | malformed params get -32602 `invalid params for \`tools/call\`: <why>` (§9.2), an unknown name gets -32602 (§3). Otherwise a CallToolResult, below. |
| `completion/complete` (`params.ref` is `{"type":"ref/prompt","name":<str>}` or `{"type":"ref/resource","uri":<str>}`, and `params.argument` is `{"name":<str>,"value":<str>}`) | `{"completion":{"values":[]}}`. When modern, prefixed with `"resultType":"complete",`. No cache hints. Rueca does not advertise `completions`; rmcp answers anyway. |
| `prompts/list` | `{"prompts":[]}`, or when modern `{"resultType":"complete","ttlMs":0,"cacheScope":"private","prompts":[]}` |
| `resources/list` | same, with key `resources` |
| `resources/templates/list` | same, with key `resourceTemplates` |
| `initialize`, `server/discover` | §4 and §5 |
| `logging/setLevel`, `prompts/get`, `resources/read`, `resources/subscribe`, `resources/unsubscribe`, `subscriptions/listen`, `tasks/get`, `tasks/update`, `tasks/cancel`, `skills/list`, `skills/get`, `resources/directory/read`, `roots/list`, `sampling/createMessage`, `elicitation/create`, anything else | -32601, `message` = the method |

Tool entry, in this order. `title`, `outputSchema`, `icons` and `_meta` are
absent:

```json
{"name":"search_symbols","description":"…","inputSchema":{…schema from rueca-query, verbatim…},"annotations":{"readOnlyHint":true,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false}}
```

`inputSchema` is `ToolDef::input_schema` when it is an object, otherwise
`{}`. Pass it through as a `serde_json::Value`.

CallToolResult. Arguments: `params.arguments` as an object, or `Value::Null`
when absent or `null`. Dispatch runs off the reader thread:
`RuecaServer::call_blocking`, then `Engine::respond`, which waits on the index
gate for up to `INDEX_WAIT` (120 s). Field order is `resultType` (modern
only), `content`, `structuredContent`, `isError`; `_meta` is absent.

```text
ok:     {"content":[{"type":"text","text":<response.text>}],"structuredContent":<response.json>,"isError":false}
error:  {"content":[{"type":"text","text":"<err:#>"}],"isError":true}
panic:  {"content":[{"type":"text","text":"internal error in `<name>`: <panic text>"}],"isError":true}   (§9.4)
```

The error path covers engine errors, bad arguments, "still indexing" and
"automatic indexing failed". `structuredContent` is absent on errors. When
modern, every form starts with `"resultType":"complete",`.

Notifications. None gets a reply:

- `notifications/cancelled` with `{requestId, reason?}`: if that id is in
  flight, forget it. Its response is never written (debug log `dropping
  response for cancelled request`). The blocking engine call is not
  interrupted; rmcp does not interrupt it either. A malformed notification,
  an unknown id, or an id that was already answered: ignored. Ids compare by
  type and value.
- Everything else: ignored. That includes `notifications/initialized`
  (logged), `notifications/progress`, `notifications/roots/list_changed` and
  unknown notifications.
- Duplicate in-flight ids, reproduced from rmcp: the in-flight set is keyed
  by id. The first of the duplicates to finish is written and removes the
  key. The other is dropped.

## 7. Protocol versions

Advertised and accepted: `2024-11-05, 2025-03-26, 2025-06-18, 2025-11-25,
2026-07-28`, in that order. Clients today:

- TypeScript SDK `packages/core/src/constants.ts`: `LATEST_PROTOCOL_VERSION = '2025-11-25'`, supported `[2025-11-25, 2025-06-18, 2025-03-26, 2024-11-05, 2024-10-07]`. Its modern era (`protocolEras.ts`) is `[2026-07-28]`.
- Python SDK `mcp_types/version.py`: handshake versions are `2024-11-05` through `2025-11-25`; the modern list is `2026-07-28`.
- Claude Code 2.1.311: uses the 2026-07-28 lifecycle, not `initialize`. Observed on stdin of the own layer (2026-11-17): a `server/discover` probe (id `server-discover-probe-1`) with `_meta` `protocolVersion` `2026-07-28`, then `tools/list` and every `tools/call` with the same `_meta`. All answers carry `resultType: "complete"`.
- MCP Inspector CLI 2.13.0: sends `initialize` with `2025-11-25` (client `inspector-cli`, capabilities with `roots` and `extensions`), then `notifications/initialized`. Rueca answers `2025-11-25`.

| Version | What differs for Rueca |
|---|---|
| 2024-11-05 | `initialize` handshake; batching undefined; no `title`, `structuredContent` or annotations in the schema (clients ignore extra fields) |
| 2025-03-26 | JSON-RPC batching added (Rueca, like rmcp, rejects it with -32600); tool annotations added |
| 2025-06-18 | batching removed; `structuredContent`, `outputSchema` and `title` added |
| 2025-11-25 | icons, tasks (experimental), input-validation errors returned as tool errors (Rueca already does), `Implementation.description` |
| 2026-07-28 | no handshake; required per-request `_meta`; `server/discover` (MUST); `resultType` on results; `ttlMs` and `cacheScope` on lists; `ping` and `logging/setLevel` removed; error codes -32020, -32021, -32022 |

## 8. Module layout and transport seam

Public API kept for `rueca-cli`: `ServerOptions`, `RuecaServer::{new,
with_index_gate}`, `tools()`, `serve_stdio`, `INSTRUCTIONS`, `INDEX_WAIT`,
`root_to_index`, `index_root`, `ensure_indexed`, `spawn_auto_index`,
`spawn_org_index`, `IndexGate`. `tools()` now returns Rueca's own
`Vec<Tool>` (was `Vec<rmcp::model::Tool>`); `serve` is synchronous over
`std::io` (was async over a tokio stream). Added: `RuecaServer::from_options`
(what `serve_stdio` serves, usable by other transports), `RuecaServer::handle`
and the types below.

```text
crates/rueca-mcp/src/
  lib.rs       RuecaServer (new, with_index_gate, from_options), tools(), IndexGate, auto-index, serve_stdio
  jsonrpc.rs   RequestId, Request, Notification, Incoming {Request, Notification, Response, Invalid, Ignored,
               Unparsable}, parse_line(&[u8]) -> Incoming, ErrorObject, Outgoing {Result, Error} (to_json, to_line)
  protocol.rs  SUPPORTED_VERSIONS, negotiate, Session {Unselected | Legacy | Inline}, Step, RuecaServer::handle,
               the `_meta` checks and the method table
  tools.rs     Tool, ToolAnnotations, the result structs (Serialize, fixed key order), ToolCall::run
  stdio.rs     serve: the reader loop, in-flight set + cancellation, one locked writer, the EOF drain
```

Results are serialized once from `#[derive(Serialize)]` structs into a
`serde_json::value::RawValue` (feature `raw_value`) and embedded verbatim in
the envelope, so no key order depends on serde_json's `preserve_order`. The
result shape (`resultType`, cache hints) is decided before the result is
built, from the per-request facts of §5.

The seam is synchronous and has no I/O:

```rust
pub struct Session { /* Unselected | Legacy | Inline, plus the legacy version */ }
impl Session {
    pub fn new() -> Session;                 // a fresh connection
    pub fn legacy(version: &str) -> Session; // after `initialize`, for stateless HTTP
    pub fn is_selected(&self) -> bool;
}
pub enum Step {
    Reply(Outgoing),     // write it now
    Call(ToolCall),      // run `ToolCall::run(self, &RuecaServer) -> Outgoing` off-thread (blocking)
    Silent,              // notification, client response, ignored line
    Cancel(RequestId),   // forget an in-flight id
}
impl RuecaServer { pub fn handle(&self, msg: Incoming, session: &mut Session) -> Step }
pub fn parse_line(line: &[u8]) -> Incoming;
pub fn serve(server: RuecaServer, input: impl Read, output: impl Write + Send + 'static) -> anyhow::Result<()>;
```

`ToolCall` holds `{id, name, args, modern}`. Tool-name validation happens
inside `handle`, so an unknown tool becomes `Step::Reply` with -32602. The
tool list comes from `rueca_query::tool_definitions()`: a tool added there
needs no change here.

stdio loop (`serve`):

```text
read line → parse_line → handle →
  Reply      → write
  Call       → wait for a slot (512) → insert id into in-flight → thread → on completion, remove id → write only if it was present
  Cancel     → remove id
  EOF        → wait up to 5 s for the in-flight set to empty → close the output → Ok(())
```

RFC 0008's stateless HTTP calls the same `handle` with one message per
`POST`:

- `parse_line(body)`. `Incoming::Unparsable` is the place for an HTTP 400
  with -32700 if the HTTP layer wants one (stdio stays silent).
- `initialize` or a body whose `_meta` has a protocolVersion: `Session::new()`.
- Otherwise: `Session::legacy(<MCP-Protocol-Version header, or "2025-03-26"
  when absent, per the 2025-11-25 transport spec>)`. A fresh Unselected
  session would reject a legacy client's `tools/list` with -32602 (no
  `_meta`). The HTTP layer validates the header first (400 when
  unsupported); `legacy` stores it as given.
- `Step::Reply` → `200` with `Outgoing::to_json()`; `Step::Call` →
  `spawn_blocking(move || call.run(&server))` → `200`; `Step::Silent` and
  `Step::Cancel` → `202`.
- Header checks (-32020) stay in the HTTP layer.

Keep every function at cognitive complexity 15 or less (SonarQube
`rust:S3776`). Dispatch with a method `match` that calls one function per
method. Write the -32600 / ignore classification and the `_meta` check as
small pure functions with table-driven unit tests.

**tokio or std threads: std only (chosen).** Nothing in the protocol
is asynchronous. The engine is blocking, and rmcp needed tokio only for
itself. A design without tokio:

- The main thread reads stdin through `BufRead`, line by line.
- Each `tools/call` runs on its own `std::thread`. A counter caps calls at
  512 in flight, matching tokio's default `max_blocking_threads`; at the cap,
  the reader waits.
- Writes go through one `Mutex` around the output, flushing after every
  message. A failed write closes the output and stops the reader.
- In-flight requests live in a `Mutex<HashSet<RequestId>>`. A `Condvar`
  wakes the EOF drain.

This drops `tokio`, `futures`, `rmcp`, `chrono` and their transitive tree
from `rueca-mcp`. Cancellation works the same, by dropping the response.

`serve` is `serve(server, input: impl Read, output: impl Write + Send +
'static) -> anyhow::Result<()>`, blocking. That is a breaking change; `serve`
was only used by `tests/duplex.rs`, and the crate is at 0.1-alpha. That test
runs over `std::io::pipe()` (stable since 1.87; MSRV is 1.90), and `tokio`
left the dev-dependencies too.

The cost: RFC 0008 brings tokio back for `axum`. Do that in the HTTP layer,
behind its own feature or crate, calling the synchronous `handle`. The core
stays free of tokio.

Dependencies: `serde` (derive), `serde_json` (`raw_value`), `anyhow`,
`tracing`, and the existing `rueca-*` crates. No `rmcp`, no `chrono`, no
`tokio`: `cargo tree -i chrono` prints nothing, and `chrono`, `tokio`,
`futures`, `sha2` and 21 other packages left `Cargo.lock`.

## 9. Accepted differences from rmcp

Decided by the owner. Each one is marked in the code and asserted in
`tests/golden.rs` (its `OVERRIDES` table and `EXIT_1_IN_RMCP` list); nothing
else differs from the golden transcripts.

1. **Before a lifecycle is chosen: a notification, a client response or
   EOF.** rmcp ended `rueca mcp` with exit status 1 (`expect initialized
   request`, `connection closed: initialize request`). Rueca ignores the
   message (debug log) and keeps serving; EOF is a normal shutdown, exit 0,
   nothing on stdout. A 2026 client that sends `notifications/cancelled`
   after a discover probe times out no longer kills the server. Rationale:
   the spec says such messages SHOULD NOT be sent, not that the server must
   die, and EOF is the normal stdio shutdown.
2. **Malformed `tools/call` params** (`params` `{}` or absent, a `name` that
   is missing or not a string, `arguments` not an object, `requestState` not
   a string): -32602 `invalid params for \`tools/call\`: <why>` (`missing
   \`name\``, `` `name` must be a string ``, `` `arguments` must be an
   object ``, `` `requestState` must be a string ``). rmcp answered -32601
   `"tools/call"`, an artifact of its fallback to `CustomRequest`; the spec
   says -32602. Other malformed known methods keep rmcp's -32601 with the
   method as message.
3. **A second `initialize`** is negotiated exactly like the first: the
   session keeps the negotiated version. rmcp stored the raw requested
   version, so a second `initialize` naming 2026-07-28 turned later results
   modern (and `ping` into -32601) on a legacy session: a bug.
4. **Panic text in `tools/call`**: `internal error in \`<tool>\`: <panic
   message>`. rmcp printed a tokio `JoinError` (`task <id> panicked with
   message "…"`), whose task id changes per run.
5. **More than 512 concurrent tool calls**: the reader blocks until a slot
   frees, where rmcp queued them on tokio's blocking pool. Only latency
   changes.

Not a decision but a limit of the reproduction: rmcp's deep typing of
rarely used params (`icons` items, `inputResponses`, nested capability
members) can make a request fall through to -32601 or -32602 in corners this
document does not list. Rueca validates only the shapes listed in §4 to §6.
Every client listed in §7 sends well-formed requests.

Everything else in §2 to §6, including rmcp's quirks (no -32700, the id
dropped on -32600, ping refused once Inline, duplicate-id dropping), is
reproduced.

## 10. Test plan

1. **Golden replay** (`tests/golden.rs` over `tests/golden/`, recorded from
   `rmcp` at `c820290`): every client line of every session is replayed
   against `serve` over `std::io::pipe()`, in process, and stdout is compared
   **byte for byte** (and as parsed JSON). Only the server version string is
   normalized. The transcripts wait for each response, so the order is
   deterministic. `capture.py --bin <new rueca>` run against the new binary
   gives the same files except for §9.1 and §9.2. The transcripts cover:
   - legacy `initialize` with each version, plus `2026-07-28`, `2099-01-01`,
     `2024-10-07`, `""`, and a malformed `initialize`;
   - Unselected `ping`, `server/discover` (good meta, missing meta, an
     unsupported version), and an inline `tools/list` without `_meta`;
   - Inline `ping`, then a second `initialize`;
   - `tools/list` in both shapes; `tools/call` ok, isError, an unknown tool,
     missing `name`, `arguments` given as an array;
   - every -32601 method; `completion/complete`; `prompts/list` and
     `resources/list` in both shapes;
   - a batch, non-object JSON, invalid JSON, `id` set to `1.5`, `1.0`, `null`
     and a string, `jsonrpc` missing;
   - unknown notifications, and `notifications/cancelled` for an in-flight
     slow call (the response must be absent);
   - EOF with a call in flight (the response is written).
2. **Unit tests per module**: `parse_line` classification, `negotiate`,
   `has_initialize`, the `_meta` checks, the result-shape rule, and the
   serialisation key order of every struct.
3. **Existing tests**: `tests/duplex.rs` on std pipes; `lib.rs` tests
   unchanged (`index_gate_states`, `old_segment_format_is_reindexed`,
   `read_only_tools`).
4. **Inspector e2e** (done 2026-11-17, Inspector 2.13.0, legacy lifecycle):
   `npx @modelcontextprotocol/inspector --cli rueca mcp --root <repo>
   --method tools/list` lists the 8 tools; `--method tools/call --tool-name
   search_symbols --tool-arg query=invoice` returns results;
   `--tool-name get_symbol` without arguments returns `isError: true`.
5. **Claude Code e2e** (done 2026-11-17, Claude Code 2.1.311, 2026-07-28
   lifecycle): `claude mcp add rueca -- rueca mcp --root .`; `claude mcp
   list` shows it connected; a `claude -p` prompt answered through
   `get_file_outline` and `trace`.
6. **Gates**: `cargo deny check` (RUSTSEC-2026-0543 gone), `cargo tree -i
   chrono` and `cargo tree -i tokio -p rueca-mcp` both empty, clippy with
   `-D warnings`, MSRV 1.90, SonarQube.

## 11. References

Specification:

- https://modelcontextprotocol.io/specification/2026-07-28/basic: messages, ids, error codes, `_meta` keys.
- https://modelcontextprotocol.io/specification/2026-07-28/basic/lifecycle, which is now "Versioning and Compatibility": -32022, the dual-era server.
- https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/stdio
- https://modelcontextprotocol.io/specification/2026-07-28/server/discover
- https://modelcontextprotocol.io/specification/2026-07-28/basic/patterns/cancellation
- https://modelcontextprotocol.io/specification/2026-07-28/changelog
- https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle: version negotiation; ping allowed before `initialize`.
- https://modelcontextprotocol.io/specification/2025-11-25/changelog
- https://modelcontextprotocol.io/specification/2025-06-18/changelog: batching removed (PR #416), structured output, `title`.

SDKs:

- TypeScript SDK `packages/core/src/constants.ts` and `packages/core-internal/src/shared/protocolEras.ts`: https://github.com/modelcontextprotocol/typescript-sdk
- Python SDK `src/mcp-types/mcp_types/version.py`: https://github.com/modelcontextprotocol/python-sdk

rmcp 3.10.0 (`~/.cargo/registry/src/index.crates.io-*/rmcp-3.10.0/src`):

- `service/server.rs` 478-529: `negotiate_protocol_version`, `missing_request_metadata_error`.
- `service/server.rs` 551-735: lifecycle selection, pre-init ping, discover bootstrap, the first inline request.
- `handler/server.rs` 82-334: per-request version and `_meta` checks, `legacy_request`, dispatch, `fill_missing_cache_hints` and `strip_result_type_for_legacy_peer`.
- `handler/server.rs` 370-680: default handlers, including `on_custom_request` (the -32601 method message).
- `service.rs` 1371-1860 (`serve_inner`): concurrency; cancellation at 1653-1690; response drop at 1529; the 5 s EOF drain at 1794-1830.
- `transport/async_rw.rs` 58 (line limit), 252-340 (`receive`: ignore vs -32600), 396-491 (standard method list, `should_ignore_notification`); tests at 836-1044.
- `model.rs` 152-298 (`ProtocolVersion`, `KNOWN_VERSIONS`, `has_initialize`), 361-400 (id parsing), 594-702 (`JsonRpcError`, `ErrorCode`, `unsupported_protocol_version`), 776-990 (message-shape check), 1282 (`InitializeResult`), 1476-1560 (`DiscoverResult::from_server_info`), 1852 (`CacheScope`), 1894 (`paginated_result!`), 4139 (`CallToolResult`), 5068 and 5112 (result shaping).
- `model/meta.rs` 380-540: `RequestMetaObject` keys and `missing_required_keys`.
- `model/tool.rs` 17-147: `Tool` and `ToolAnnotations`.
- `model/serde_impl.rs`: the params and `_meta` envelope; `CustomRequest` as the catch-all.
- `Cargo.toml`: `schemars` with features `["chrono04"]`, the source of the `chrono` path through `schemars`.
