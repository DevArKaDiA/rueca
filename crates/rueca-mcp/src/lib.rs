//! MCP server over stdio exposing the Rueca tools (`docs/tools.md`), for one
//! working tree (`--root`) or an organization (`--org`, RFC 0006).
//!
//! Built on the official Rust SDK, [`rmcp`]. Tools are listed from
//! [`rueca_query::tool_definitions`] (deterministic order) and every call is
//! dispatched to [`rueca_query::Engine::respond`] on a blocking thread.
//! Results carry the JSON response (the RFC 0005 envelope, or the alpha shape
//! with `format: verbose`) as `structuredContent` and its compact text
//! rendering ([`rueca_query::render_text`]; compact JSON for `verbose`) as
//! text content, so clients that read `structuredContent` see the full data
//! and models that read text see the short form. Engine errors become tool
//! results with `isError: true`.
//!
//! Protocol versions are whatever `rmcp` 3.7 negotiates by default (Rueca does
//! not override `supported_protocol_versions`): the 2026-07-28 stateless flow
//! (`server/discover`, per-request `_meta`, no session) and the legacy
//! `initialize` handshake for 2024-11-05, 2025-03-26, 2025-06-18 and
//! 2025-11-25. An `initialize` naming 2026-07-28 or an unknown version is
//! answered with 2025-11-25, the newest version that has a handshake.
//!
//! When the served root (or organization, given its file) is not indexed yet,
//! [`serve_stdio`] indexes it on a background thread so `initialize` is
//! answered immediately (clients time out slow start-ups); tool calls wait
//! for the index for up to [`INDEX_WAIT`] and then report "still indexing".
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

#[derive(Debug, Clone, Default)]
pub struct ServerOptions {
    pub store_dir: PathBuf,
    /// Working tree served by default. If not indexed yet and `auto_index` is
    /// set, it is indexed before serving.
    pub root: Option<PathBuf>,
    pub auto_index: bool,
    /// Organization served instead of a root (RFC 0006): `repo` omitted
    /// means the whole organization.
    pub org: Option<String>,
    /// The organization's file, used to build it when the store has no
    /// generation yet (with `auto_index`).
    pub org_config: Option<rueca_index::org::OrgConfig>,
}

/// Instructions sent to clients in `initialize` / `server/discover`.
pub const INSTRUCTIONS: &str = "Rueca is a code-intelligence index of this repository: a graph of \
definitions (functions, methods, classes, …) and the relations between them (calls, inheritance, \
imports), plus the full text of every indexed file. Prefer it over reading whole files or grepping. \
Recommended flow: `search_symbols` to find candidates by name → `get_symbol` for details (parent, \
children, bases, caller/callee counts) or `get_snippet` for the source → `trace` to follow callers \
and callees (impact analysis). Use `search_code` for literals, messages and config keys, and \
`get_file_outline` to see a file's structure cheaply. Responses name each snapshot read \
(`repo@commit (ref)`) and group items by file; an item's id is `repo@commit:path#name`, and its \
printed name also works as `qualified_name`. Symbol ids returned by any tool can be passed as `id` \
to the others. Lines are 1-based. Every tool takes `repo` (one, a list, or \"*\" for every indexed \
repository), `ref` and `budget_tokens` (default 2000); a cut response says `truncated` and how to \
continue (`cursor`). When the server serves an organization, omitting `repo` searches every \
repository of it in one ranking; `list_repositories` lists them. Results come from the last index of \
the working tree; edges carry a confidence (0–1) and the strategy used to resolve them.";

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
    fn call_blocking(&self, tool: &str, args: serde_json::Value) -> anyhow::Result<rueca_query::Response> {
        let index_failure = match &self.gate {
            Some(gate) => gate.wait(INDEX_WAIT).map_err(anyhow::Error::msg)?,
            None => None,
        };
        self.engine.respond(tool, args).map_err(|err| match index_failure {
            Some(reason) => err.context(format!("automatic indexing failed: {reason}")),
            None => err,
        })
    }
}

/// The tools as MCP [`Tool`]s, in `rueca_query::TOOL_NAMES` order.
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
            Ok(Ok(response)) => {
                tracing::debug!(tool = %name, ms = started.elapsed().as_millis() as u64, "tool call ok");
                let mut result = CallToolResult::structured(response.json);
                result.content = vec![ContentBlock::text(response.text)];
                result
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
    /// What is being indexed, for messages (`` `/src/x` ``, ``organization `acme` ``).
    what: String,
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
        IndexGate::named(format!("`{}`", root.display()))
    }

    fn named(what: String) -> IndexGate {
        IndexGate { what, state: Mutex::new(IndexState::Indexing(Instant::now())), changed: Condvar::new() }
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
                "{} is still being indexed ({} s so far); retry in a moment",
                self.what,
                since.elapsed().as_secs()
            )),
        }
    }
}

/// The root to auto-index: `opts.root` when `auto_index` is set and the store
/// has no entry covering it, or that entry's segment is in an older format.
pub fn root_to_index(opts: &ServerOptions) -> Option<PathBuf> {
    let root = opts.root.as_ref().filter(|_| opts.auto_index)?;
    let canon = std::fs::canonicalize(root).unwrap_or_else(|_| root.clone());
    let store = match rueca_segment::Store::open(&opts.store_dir) {
        Ok(s) => s,
        Err(err) => {
            tracing::warn!(error = %err, "cannot read the store catalog; indexing anyway");
            return Some(canon);
        }
    };
    match store.entry_for_path(&canon) {
        Ok(Some(entry)) => {
            // A segment written in an older format is re-indexed instead of
            // failing every tool call.
            match store.segment_format_version(&entry.segment_id) {
                Ok(v) if v != rueca_segment::FORMAT_VERSION => {
                    tracing::info!(root = %canon.display(), version = v, "index has an old segment format; re-indexing");
                    Some(canon)
                }
                _ => {
                    tracing::info!(root = %canon.display(), segment = %entry.segment_id, "serving existing index");
                    None
                }
            }
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
    let store = opts.store_dir.clone();
    let r = root.clone();
    spawn_gate(IndexGate::new(root), move || index_root(&r, &store))
}

/// Starts building the organization on a background thread when it has no
/// generation in the store yet and its file is known (`auto_index`).
pub fn spawn_org_index(opts: &ServerOptions) -> Option<Arc<IndexGate>> {
    let name = opts.org.as_ref().filter(|_| opts.auto_index)?;
    let config = opts.org_config.clone()?;
    match rueca_segment::org::current_generation(&opts.store_dir, name) {
        Ok(Some(g)) => {
            tracing::info!(org = %name, generation = g.generation, "serving existing organization");
            return None;
        }
        Ok(None) => tracing::info!(org = %name, "organization is not built yet; indexing it"),
        Err(err) => tracing::warn!(org = %name, error = %err, "cannot read the organization; indexing it"),
    }
    let store = opts.store_dir.clone();
    spawn_gate(IndexGate::named(format!("organization `{name}`")), move || {
        let report = rueca_index::org::index_org(&rueca_index::org::OrgIndexOptions {
            config,
            store,
            budget: rueca_extract::Budget::default(),
            threads: None,
            only: None,
        })?;
        tracing::info!(org = %report.org, generation = report.update.generation, repos = report.update.repos, ms = report.duration_ms, "organization indexed");
        Ok(())
    })
}

/// Runs `job` on a background thread; tool calls wait on the returned gate.
fn spawn_gate(gate: IndexGate, job: impl FnOnce() -> anyhow::Result<()> + Send + 'static) -> Option<Arc<IndexGate>> {
    let gate = Arc::new(gate);
    let g = gate.clone();
    let spawned = std::thread::Builder::new().name("rueca-auto-index".into()).spawn(move || {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
        let outcome = match outcome {
            Ok(Ok(())) => Ok(()),
            Ok(Err(err)) => Err(format!("{err:#}")),
            Err(_) => Err("the indexer panicked".to_string()),
        };
        if let Err(e) = &outcome {
            tracing::error!(what = %g.what, error = %e, "indexing failed");
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
/// If `opts.root` needs indexing (see [`root_to_index`]), or `opts.org` is
/// not built yet and its file is known, it is indexed on a background thread
/// while the server already answers `initialize`, `server/discover` and
/// `tools/list`; tool calls wait for it (up to [`INDEX_WAIT`]). With
/// `opts.org` the server serves the organization (`opts.root` is ignored).
pub fn serve_stdio(opts: ServerOptions) -> anyhow::Result<()> {
    let (engine, gate) = match &opts.org {
        Some(name) => (Engine::for_org(opts.store_dir.clone(), name)?, spawn_org_index(&opts)),
        None => (Engine::new(opts.store_dir.clone(), opts.root.clone())?, spawn_auto_index(&opts)),
    };
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

        let org = IndexGate::named("organization `acme`".into());
        let msg = org.wait(Duration::from_millis(1)).unwrap_err();
        assert!(msg.starts_with("organization `acme` is still being indexed"), "{msg}");
    }

    #[test]
    fn old_segment_format_is_reindexed() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("app.py"), "def main():\n    pass\n").unwrap();
        let store = tempfile::tempdir().unwrap();
        let opts = ServerOptions {
            store_dir: store.path().into(),
            root: Some(root.path().into()),
            auto_index: true,
            ..ServerOptions::default()
        };
        assert!(root_to_index(&opts).is_some(), "not indexed yet");
        index_root(root.path(), store.path()).unwrap();
        assert_eq!(root_to_index(&opts), None, "current format is served");
        // Rewrite the header's format version as an older build would have.
        let st = rueca_segment::Store::open(store.path()).unwrap();
        let entry = st.entries().unwrap().pop().unwrap();
        let path = st.segment_path(&entry.segment_id);
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[8..12].copy_from_slice(&1u32.to_le_bytes());
        std::fs::write(&path, bytes).unwrap();
        assert_eq!(st.segment_format_version(&entry.segment_id).unwrap(), 1);
        assert!(root_to_index(&opts).is_some(), "old format is re-indexed");
    }

    #[test]
    fn read_only_tools() {
        let tools = tools();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
        assert_eq!(names, rueca_query::TOOL_NAMES);
        assert!(tools.iter().all(|t| t.annotations.as_ref().and_then(|a| a.read_only_hint) == Some(true)));
    }
}
