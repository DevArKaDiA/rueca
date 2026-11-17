//! Rueca service mode, the **query role** (RFC 0008 phase 1): `rueca serve`.
//!
//! A query replica serves MCP over stateless Streamable HTTP from a local
//! mirror of the service store (`rueca-service`), the organization's current
//! generation, kept fresh by a watcher:
//!
//! ```text
//!   MCP client ── POST /mcp ──▶ log · Host/Origin · body limit · IP limit · auth · principal limit · timeout
//!                                 └─▶ rueca-mcp handle (sync) ─▶ Engine::for_org (caller's Visibility)
//!   watcher (every [store].poll) ── pull_generation ──▶ mirror at [store].cache_dir ──▶ readiness, eviction
//!   ops listener: /healthz /readyz /metrics
//! ```
//!
//! - [`config`]: `rueca-service.toml` (RFC 0008 §4.4), precedence, `env:`
//!   secrets, `--check-config`.
//! - [`mcp`]: `POST /mcp` (§6.1–§6.2), the deadline.
//! - [`auth`], [`oidc`]: static tokens and OIDC (§6.3); PRM in [`http`].
//! - [`visibility`]: principal → `Visibility` (§6.4), deny by default.
//! - [`limits`]: rate limits and `Host` / `Origin` checks (§6.5).
//! - [`state`]: the generation watcher, readiness, the disk cache (§5.6,
//!   §6.7).
//! - [`metrics`], [`logging`]: §6.6.
//!
//! The protocol core stays synchronous and free of tokio
//! (`docs/mcp-protocol.md` §8): tokio enters only in this crate.
//! Configuration keys this phase parses but does not act on are listed by
//! [`config::ServiceConfig::unused_sections`] and logged at start-up.

pub mod auth;
pub mod config;
pub mod http;
pub mod limits;
pub mod logging;
pub mod mcp;
pub mod metrics;
pub mod oidc;
#[cfg(feature = "otel")]
pub mod otel;
pub mod state;
pub mod store;
pub mod units;
pub mod visibility;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinHandle;

pub use auth::{Authenticator, Challenge, Principal};
pub use config::{Loaded, Role, ServiceConfig, Sources};
pub use state::AppState;

/// A started service: its bound addresses and the tasks serving them.
pub struct Running {
    pub addr: SocketAddr,
    pub ops_addr: SocketAddr,
    pub state: Arc<AppState>,
    stop: watch::Sender<bool>,
    tasks: Vec<JoinHandle<()>>,
}

impl Running {
    /// Stops accepting, lets in-flight requests finish for up to `drain`,
    /// stops the watcher.
    pub async fn shutdown(self, drain: Duration) {
        let _ = self.stop.send(true);
        let all = futures_util::future::join_all(self.tasks);
        if tokio::time::timeout(drain, all).await.is_err() {
            tracing::warn!(drain_s = drain.as_secs(), "in-flight requests did not finish within the drain; exiting");
        }
    }
}

/// Binds both listeners and starts the HTTP servers and the generation
/// watcher on the current tokio runtime.
pub async fn start(loaded: Loaded) -> anyhow::Result<Running> {
    let Loaded { config, role, .. } = loaded;
    if role != Role::Query {
        tracing::info!(role = role.as_str(), "indexer role: phase 2; only the query loops run");
    }
    for section in config.unused_sections() {
        tracing::info!(section, "configuration parsed, not used in this phase");
    }
    let state = Arc::new(AppState::new(config, role)?);
    let http = &state.config.http;
    let listener = TcpListener::bind(http.listen).await.with_context(|| format!("binding {}", http.listen))?;
    let ops = TcpListener::bind(http.ops_listen).await.with_context(|| format!("binding {}", http.ops_listen))?;
    let (addr, ops_addr) = (listener.local_addr()?, ops.local_addr()?);
    let (stop, stopped) = watch::channel(false);
    let main_app = http::main_router(state.clone()).into_make_service_with_connect_info::<SocketAddr>();
    let ops_app = http::ops_router(state.clone());
    let tasks = vec![
        tokio::spawn(serve_until(listener, main_app, stopped.clone(), "main")),
        tokio::spawn(serve_ops(ops, ops_app, stopped.clone())),
        tokio::spawn(watch_generations(state.clone(), stopped)),
    ];
    tracing::info!(%addr, %ops_addr, store = %state.config.store.url, "rueca serve listening");
    Ok(Running { addr, ops_addr, state, stop, tasks })
}

async fn serve_until(
    listener: TcpListener,
    app: axum::extract::connect_info::IntoMakeServiceWithConnectInfo<axum::Router, SocketAddr>,
    stopped: watch::Receiver<bool>,
    name: &'static str,
) {
    if let Err(e) = axum::serve(listener, app).with_graceful_shutdown(stop_signal(stopped)).await {
        tracing::error!(listener = name, error = %e, "HTTP server failed");
    }
}

async fn serve_ops(listener: TcpListener, app: axum::Router, stopped: watch::Receiver<bool>) {
    if let Err(e) = axum::serve(listener, app).with_graceful_shutdown(stop_signal(stopped)).await {
        tracing::error!(listener = "ops", error = %e, "HTTP server failed");
    }
}

async fn stop_signal(mut stopped: watch::Receiver<bool>) {
    let _ = stopped.wait_for(|s| *s).await;
}

/// The generation watcher: one blocking tick per `[store].poll`.
async fn watch_generations(state: Arc<AppState>, mut stopped: watch::Receiver<bool>) {
    let mut known = None;
    let poll = state.config.store.poll.0;
    loop {
        let st = state.clone();
        let tick = tokio::task::spawn_blocking(move || {
            let mut k = known;
            st.tick(&mut k);
            k
        });
        match tick.await {
            Ok(k) => known = k,
            Err(e) => {
                tracing::error!(error = %e, "generation watcher tick panicked");
                known = None;
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(poll) => {}
            _ = stopped.wait_for(|s| *s) => return,
        }
    }
}

/// `rueca serve`: loads nothing itself; runs `loaded` until SIGTERM or
/// SIGINT, then drains for up to `[http].request_timeout` and returns.
pub fn run(loaded: Loaded) -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().thread_name("rueca-serve").build()?;
    rt.block_on(async move {
        let drain = loaded.config.http.request_timeout.0;
        let running = start(loaded).await?;
        shutdown_signal().await;
        tracing::info!(drain_s = drain.as_secs(), "shutting down: draining in-flight requests");
        running.shutdown(drain).await;
        tracing::info!("stopped");
        Ok(())
    })
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {}
        _ = term => {}
    }
}

/// What `rueca serve --check-config` found.
#[derive(Debug, Default)]
pub struct CheckReport {
    /// Problems that stop the service from starting (exit status 1).
    pub errors: Vec<String>,
    /// Worth knowing, not fatal (e.g. nothing published to the store yet).
    pub warnings: Vec<String>,
}

/// `rueca serve --check-config`: loads the configuration (precedence,
/// `env:` references, validation), then opens the store and reads its
/// `store.json`.
pub fn check(sources: &Sources, env: &dyn Fn(&str) -> Option<String>) -> CheckReport {
    let loaded = match config::load(sources, env) {
        Ok(l) => l,
        Err(errors) => return CheckReport { errors, warnings: Vec::new() },
    };
    let mut report = CheckReport::default();
    let url = &loaded.config.store.url;
    match rueca_storage::open(url, &loaded.config.store.options) {
        Ok(storage) => match storage.get("store.json") {
            Ok(Some(_)) => {}
            Ok(None) => {
                report.warnings.push(format!("store `{url}` has no store.json: nothing has been published yet"))
            }
            Err(e) => report.errors.push(format!("store `{url}`: {e:#}")),
        },
        Err(e) => report.errors.push(format!("store.url: {e:#}")),
    }
    for section in loaded.config.unused_sections() {
        report.warnings.push(format!("{section}: parsed, not used by this phase"));
    }
    report
}
