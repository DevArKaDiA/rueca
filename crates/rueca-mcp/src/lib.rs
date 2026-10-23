//! MCP server over stdio exposing the six alpha tools (`docs/tools.md`).
//!
//! Built on the official Rust SDK, [`rmcp`]. Tools are listed from
//! [`rueca_query::tool_definitions`] (deterministic order) and every call is
//! dispatched to [`rueca_query::Engine::call`] on a blocking thread. Results
//! are returned as compact JSON text content plus `structuredContent`; engine
//! errors become tool results with `isError: true`.
//!
//! Protocol versions are whatever `rmcp` 3.7 negotiates by default (Rueca does
//! not override `supported_protocol_versions`): the 2026-07-28 stateless flow
//! (`server/discover`, per-request `_meta`, no session) and the legacy
//! `initialize` handshake for 2024-11-05, 2025-03-26, 2025-06-18 and
//! 2025-11-25. An `initialize` naming 2026-07-28 or an unknown version is
//! answered with 2025-11-25, the newest version that has a handshake.
//!
//! When the served root is not indexed yet, [`serve_stdio`] indexes it on a
//! background thread so `initialize` is answered immediately (clients time
//! out slow start-ups); tool calls wait for the index for up to
//! [`INDEX_WAIT`] and then report "still indexing".
//!
//! Nothing is written to stdout except protocol messages; logs go through
//! `tracing` and the binary installs a stderr subscriber.

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use rmcp::model::{
    CacheScope, CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool, ToolAnnotations,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ErrorData as McpError, ServerHandler, ServiceExt};
use rueca_query::Engine;

pub struct ServerOptions {
    pub store_dir: PathBuf,
    /// Working tree served by default. If not indexed yet and `auto_index` is
    /// set, it is indexed before serving.
    pub root: Option<PathBuf>,
    pub auto_index: bool,
}

/// Instructions sent to clients in `initialize` / `server/discover`.
pub const INSTRUCTIONS: &str = "Rueca is a code-intelligence index of this repository: a graph of \
definitions (functions, methods, classes, …) and the relations between them (calls, inheritance, \
imports), plus the full text of every indexed file. Prefer it over reading whole files or grepping. \
Recommended flow: `search_symbols` to find candidates by name → `get_symbol` for details (parent, \
children, bases, caller/callee counts) or `get_snippet` for the source → `trace` to follow callers \
and callees (impact analysis). Use `search_code` for literals, messages and config keys, and \
`get_file_outline` to see a file's structure cheaply. Symbol ids returned by any tool can be passed \
as `id` to the others. Lines are 1-based. Results come from the last index of the working tree; \
edges carry a confidence (0–1) and the strategy used to resolve them.";

/// The MCP handler: a shared engine plus the advertised tool list.
#[derive(Clone)]
pub struct RuecaServer {
    engine: Arc<Engine>,
    tools: Arc<Vec<Tool>>,
    gate: Option<Arc<IndexGate>>,
}

impl RuecaServer {
    pub fn new(engine: Engine) -> RuecaServer {
        RuecaServer::with_index_gate(engine, None)
    }

    /// A server whose tool calls first wait for a background index
    /// (see [`spawn_auto_index`]).
    pub fn with_index_gate(engine: Engine, gate: Option<Arc<IndexGate>>) -> RuecaServer {
        RuecaServer { engine: Arc::new(engine), tools: Arc::new(tools()), gate }
    }

    /// Runs one tool call (blocking): waits for a pending auto-index, then
    /// dispatches to the engine.
    fn call_blocking(&self, tool: &str, args: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let index_failure = match &self.gate {
            Some(gate) => gate.wait(INDEX_WAIT).map_err(anyhow::Error::msg)?,
            None => None,
        };
        self.engine.call(tool, args).map_err(|err| match index_failure {
            Some(reason) => err.context(format!("automatic indexing failed: {reason}")),
            None => err,
        })
    }
}

/// The six tools as MCP [`Tool`]s, in `rueca_query::TOOL_NAMES` order.
pub fn tools() -> Vec<Tool> {
    rueca_query::tool_definitions()
        .into_iter()
        .map(|d| {
            let schema = match d.input_schema {
                serde_json::Value::Object(m) => m,
                _ => serde_json::Map::new(),
            };
            let mut tool = Tool::new(d.name, d.description, Arc::new(schema));
            tool.annotations =
                Some(ToolAnnotations::new().read_only(true).destructive(false).idempotent(true).open_world(false));
            tool
        })
        .collect()
}

impl ServerHandler for RuecaServer {
    fn get_info(&self) -> ServerConfig {
        let mut implementation = Implementation::new("rueca", env!("CARGO_PKG_VERSION"));
        implementation.title = Some("Rueca code intelligence".into());
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(implementation)
            .with_instructions(INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(self.tools.as_ref().clone())
            .with_ttl_ms(3_600_000)
            .with_cache_scope(CacheScope::Public))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.tools.iter().find(|t| t.name == name).cloned()
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let name: Cow<'static, str> = request.name;
        if !rueca_query::TOOL_NAMES.contains(&name.as_ref()) {
            return Err(McpError::invalid_params(
                format!("unknown tool `{name}` (available: {})", rueca_query::TOOL_NAMES.join(", ")),
                None,
            ));
        }
        let args = request.arguments.map(serde_json::Value::Object).unwrap_or(serde_json::Value::Null);
        let server = self.clone();
        let tool = name.to_string();
        let started = Instant::now();
        let outcome = tokio::task::spawn_blocking(move || server.call_blocking(&tool, args)).await;
        let result = match outcome {
            Ok(Ok(value)) => {
                tracing::debug!(tool = %name, ms = started.elapsed().as_millis() as u64, "tool call ok");
                CallToolResult::structured(value)
            }
            Ok(Err(err)) => {
                tracing::debug!(tool = %name, error = %format!("{err:#}"), "tool call failed");
                CallToolResult::error(vec![ContentBlock::text(format!("{err:#}"))])
            }
            Err(join) => {
                tracing::error!(tool = %name, error = %join, "tool call panicked");
                CallToolResult::error(vec![ContentBlock::text(format!("internal error in `{name}`: {join}"))])
            }
        };
        Ok(result.into())
    }
}

/// How long a tool call waits for a background auto-index before answering
/// "still indexing".
pub const INDEX_WAIT: Duration = Duration::from_secs(120);

/// Tracks a background auto-index of the served root. Tool calls wait on it
/// (up to [`INDEX_WAIT`]) so the first query after start-up sees the index.
pub struct IndexGate {
    root: PathBuf,
    state: Mutex<IndexState>,
    changed: Condvar,
}

#[derive(Debug, Clone, PartialEq)]
enum IndexState {
    Indexing(Instant),
    Done,
    Failed(String),
}

impl IndexGate {
    fn new(root: PathBuf) -> IndexGate {
        IndexGate { root, state: Mutex::new(IndexState::Indexing(Instant::now())), changed: Condvar::new() }
    }

    fn finish(&self, outcome: Result<(), String>) {
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        *st = match outcome {
            Ok(()) => IndexState::Done,
            Err(e) => IndexState::Failed(e),
        };
        self.changed.notify_all();
    }

    /// Blocks until indexing ends or `timeout` passes. `Err` = still indexing.
    /// `Ok(Some(reason))` = indexing failed.
    fn wait(&self, timeout: Duration) -> Result<Option<String>, String> {
        let st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let (st, _) = self
            .changed
            .wait_timeout_while(st, timeout, |s| matches!(s, IndexState::Indexing(_)))
            .unwrap_or_else(|p| p.into_inner());
        match &*st {
            IndexState::Done => Ok(None),
            IndexState::Failed(e) => Ok(Some(e.clone())),
            IndexState::Indexing(since) => Err(format!(
                "`{}` is still being indexed ({} s so far); retry in a moment",
                self.root.display(),
                since.elapsed().as_secs()
            )),
        }
    }
}

/// The root to auto-index: `opts.root` when `auto_index` is set and the store
/// has no entry covering it.
pub fn root_to_index(opts: &ServerOptions) -> Option<PathBuf> {
    let root = opts.root.as_ref().filter(|_| opts.auto_index)?;
    let canon = std::fs::canonicalize(root).unwrap_or_else(|_| root.clone());
    match rueca_segment::Store::open(&opts.store_dir).and_then(|s| s.entry_for_path(&canon)) {
        Ok(Some(entry)) => {
            tracing::info!(root = %canon.display(), segment = %entry.segment_id, "serving existing index");
            None
        }
        Ok(None) => Some(canon),
        Err(err) => {
            tracing::warn!(error = %err, "cannot read the store catalog; indexing anyway");
            Some(canon)
        }
    }
}

/// Indexes `root` into `store` (blocking), logging the outcome.
pub fn index_root(root: &Path, store: &Path) -> anyhow::Result<()> {
    tracing::info!(root = %root.display(), "root is not indexed yet; indexing");
    let index_opts = rueca_index::IndexOptions {
        root: root.to_path_buf(),
        store: store.to_path_buf(),
        repo: None,
        budget: rueca_extract::Budget::default(),
        threads: None,
    };
    let report = rueca_index::index_worktree(&index_opts)?;
    tracing::info!(
        root = %report.root,
        files = report.files_total,
        extracted = report.files_extracted,
        skipped = report.files_skipped,
        nodes = report.segment.node_count,
        edges = report.segment.edge_count,
        ms = report.duration_ms,
        "indexed"
    );
    Ok(())
}

/// Indexes `opts.root` synchronously if it needs it (see [`root_to_index`]).
/// Failures are logged; the server can still start.
pub fn ensure_indexed(opts: &ServerOptions) {
    if let Some(root) = root_to_index(opts)
        && let Err(err) = index_root(&root, &opts.store_dir)
    {
        tracing::error!(root = %root.display(), error = %format!("{err:#}"), "indexing failed");
    }
}

/// Starts indexing `opts.root` on a background thread if it needs it, and
/// returns the gate tool calls wait on.
pub fn spawn_auto_index(opts: &ServerOptions) -> Option<Arc<IndexGate>> {
    let root = root_to_index(opts)?;
    let gate = Arc::new(IndexGate::new(root.clone()));
    let store = opts.store_dir.clone();
    let g = gate.clone();
    let spawned = std::thread::Builder::new().name("rueca-auto-index".into()).spawn(move || {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| index_root(&root, &store)));
        let outcome = match outcome {
            Ok(Ok(())) => Ok(()),
            Ok(Err(err)) => Err(format!("{err:#}")),
            Err(_) => Err("the indexer panicked".to_string()),
        };
        if let Err(e) = &outcome {
            tracing::error!(root = %root.display(), error = %e, "indexing failed");
        }
        g.finish(outcome);
    });
    match spawned {
        Ok(_) => Some(gate),
        Err(err) => {
            tracing::error!(error = %err, "cannot start the indexing thread");
            None
        }
    }
}

/// Serves MCP over any async byte stream pair (used by [`serve_stdio`] and by
/// tests with in-memory pipes) until the peer disconnects.
pub async fn serve<T>(server: RuecaServer, transport: T) -> anyhow::Result<()>
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + 'static,
{
    let running = server.serve(transport).await?;
    running.waiting().await?;
    Ok(())
}

/// Serves MCP on stdin/stdout until the client disconnects. Logs go to stderr.
///
/// If `opts.root` needs indexing (see [`root_to_index`]) it is indexed on a
/// background thread while the server already answers `initialize`,
/// `server/discover` and `tools/list`; tool calls wait for it (up to
/// [`INDEX_WAIT`]).
pub fn serve_stdio(opts: ServerOptions) -> anyhow::Result<()> {
    let gate = spawn_auto_index(&opts);
    let engine = Engine::new(opts.store_dir.clone(), opts.root.clone())?;
    let server = RuecaServer::with_index_gate(engine, gate);
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(async move {
        tracing::info!(version = env!("CARGO_PKG_VERSION"), "rueca MCP server on stdio");
        let running = server.serve(rmcp::transport::stdio()).await?;
        running.waiting().await?;
        anyhow::Ok(())
    })?;
    // Do not wait for a blocking stdin reader thread on shutdown.
    runtime.shutdown_background();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_gate_states() {
        let gate = Arc::new(IndexGate::new(PathBuf::from("/repo")));
        let msg = gate.wait(Duration::from_millis(10)).unwrap_err();
        assert!(msg.contains("`/repo` is still being indexed"), "{msg}");

        let g = gate.clone();
        let waiter = std::thread::spawn(move || g.wait(Duration::from_secs(10)));
        std::thread::sleep(Duration::from_millis(20));
        gate.finish(Ok(()));
        assert_eq!(waiter.join().unwrap(), Ok(None));

        let failed = IndexGate::new(PathBuf::from("/repo"));
        failed.finish(Err("disk full".into()));
        assert_eq!(failed.wait(Duration::ZERO), Ok(Some("disk full".to_string())));
    }

    #[test]
    fn six_read_only_tools() {
        let tools = tools();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
        assert_eq!(names, rueca_query::TOOL_NAMES);
        assert!(tools.iter().all(|t| t.annotations.as_ref().and_then(|a| a.read_only_hint) == Some(true)));
    }
}
