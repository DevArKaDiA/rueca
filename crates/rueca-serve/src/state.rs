//! The app state and the **generation watcher** (RFC 0008 §5.6, §6.7).
//!
//! The replica's disk cache is a phase-0 mirror ([`ServiceStore`]) at
//! `[store].cache_dir`. Every `[store].poll` (5 s) the watcher calls
//! [`pull_generation`] with the version of `org.json` it last read: one
//! conditional request when nothing changed. A new generation is downloaded
//! whole (`pull_generation` fetches every segment, pack and organization
//! file it names, defaults and refs) and installed under the mirror's lock.
//! Then:
//!
//! - **warm-up**: through the engine that serves calls, with
//!   `Visibility::All`, `list_repositories` and a one-result
//!   `search_symbols` open the generation's organization index (its tantivy
//!   segments and routing profiles) and every default segment into the
//!   engine's caches, so the first call after readiness opens nothing.
//!   Timed into `rueca_generation_warm_seconds` and the `serving generation`
//!   log line (`warm_ms`);
//! - **readiness**: only once the warm-up finished is the generation
//!   published (`/readyz`, `/status`, `rueca_generation`); with
//!   `ready_when = "warm"` every default segment is also opened and verified
//!   again (timed into `rueca_segment_open_seconds`);
//! - **eviction**: when the mirror's segments, packs and organization files
//!   exceed `[store].cache_bytes`, segment files that the current generation
//!   does not name are deleted, least recently accessed first. Limits:
//!   packs and superseded organization index files are never evicted, and
//!   the current generation is never evicted even when it alone exceeds the
//!   budget.
//!
//! **A generation change** keeps `/readyz` on the old generation until the
//! new one is warm, but not the engine: it re-reads the local pointer on
//! every call, and `pull_generation` writes that pointer before the warm-up
//! starts. A call arriving in between reads the new generation and opens
//! what it needs itself (calls meanwhile wait on the engine's
//! organization-index lock, not on a second open); segments the two
//! generations share stay open.
//!
//! The organization is `[org].name`, else the one the store's `store.json`
//! names; until the store has a `store.json` and a generation the app is not
//! ready. Errors are logged and retried on the next tick.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, bail};
use rueca_mcp::RuecaServer;
use rueca_query::{Engine, RespondOptions, Visibility};
use rueca_segment::org::{OrgGeneration, current_generation, org_dir};
use rueca_service::reads::{ReadsRecorder, write_reads};
use rueca_service::{PullGenerationReport, ServiceStore, pull_generation};
use rueca_storage::{ObjectVersion, Storage};

use crate::auth::{AuthChain, Authenticator};
use crate::config::{ReadyWhen, Role, ServiceConfig};
use crate::indexer::Indexer;
use crate::limits::{HostGuard, Limits};
use crate::metrics::Metrics;
use crate::store::CountingStorage;
use crate::visibility::VisibilityCache;
use crate::webhooks::Webhooks;

/// The `search_symbols` query of the warm-up: any short query searches
/// every repository's segment of the organization index.
const WARM_QUERY: &str = "main";

/// What serves MCP once the organization is known.
pub struct Serving {
    pub svc: ServiceStore,
    pub server: RuecaServer,
    /// The engine `server` runs (a clone: same caches), for the warm-up.
    pub engine: Engine,
    pub org: String,
}

/// The generation currently served.
pub struct Current {
    pub generation: OrgGeneration,
    /// Repository ids, sorted.
    pub repos: Vec<String>,
    /// Default segments in the cache / in the generation.
    pub warm: (usize, usize),
}

/// Shared by every handler and the watcher.
pub struct AppState {
    pub config: ServiceConfig,
    pub role: Role,
    pub metrics: Metrics,
    pub auth: Arc<dyn Authenticator>,
    /// OIDC issuers (PRM `authorization_servers`); empty: no PRM.
    pub issuers: Vec<String>,
    /// The protected resource (`canonical_url`, else `http://<listen>/mcp`).
    pub resource: String,
    pub guard: HostGuard,
    pub limits: Limits,
    pub visibility: VisibilityCache,
    /// The indexer role (`indexer`, `all`).
    pub indexer: Option<Arc<Indexer>>,
    /// Webhook receivers of the indexer role (`[indexing].webhooks`).
    pub webhooks: Option<Arc<Webhooks>>,
    /// Read evidence of the query role (`reads/`, RFC 0008 §5.8); `None`
    /// with `[retention].pr_idle = "0"` or in the indexer role.
    pub reads: Option<ReadsRecorder>,
    storage: Arc<dyn Storage>,
    serving: RwLock<Option<Arc<Serving>>>,
    current: RwLock<Option<Arc<Current>>>,
    last_error: Mutex<Option<String>>,
    ticks: AtomicU64,
}

impl AppState {
    pub fn new(config: ServiceConfig, role: Role) -> anyhow::Result<AppState> {
        let metrics = Metrics::new();
        let backend = rueca_storage::open(&config.store.url, &config.store.options)?;
        let storage: Arc<dyn Storage> = Arc::new(CountingStorage::new(backend, metrics.clone()));
        let resource = config.resource_url();
        let chain = AuthChain::new(&config.auth, &resource)?;
        let indexer = match role {
            Role::Query => None,
            Role::Indexer | Role::All => {
                let org = indexer_org(&config, storage.as_ref())?;
                Some(Arc::new(Indexer::new(&config, storage.clone(), &org, metrics.clone())?))
            }
        };
        let webhooks = match role {
            Role::Query => None,
            Role::Indexer | Role::All => Webhooks::new(&config)?.map(Arc::new),
        };
        let idle = crate::indexer::retention(&config).pr_idle;
        let reads = (role != Role::Indexer && !idle.is_zero())
            .then(|| ReadsRecorder::new(&crate::indexer::instance_id().replace('/', "-")));
        Ok(AppState {
            indexer,
            webhooks,
            reads,
            role,
            metrics,
            issuers: chain.issuers(),
            auth: Arc::new(chain),
            resource,
            guard: HostGuard::new(&config.http),
            limits: Limits::new(&config.http),
            visibility: VisibilityCache::default(),
            storage,
            serving: RwLock::new(None),
            current: RwLock::new(None),
            last_error: Mutex::new(None),
            ticks: AtomicU64::new(0),
            config,
        })
    }

    pub fn serving(&self) -> Option<Arc<Serving>> {
        self.serving.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub fn current(&self) -> Option<Arc<Current>> {
        self.current.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Completed watcher ticks.
    pub fn ticks(&self) -> u64 {
        self.ticks.load(Ordering::SeqCst)
    }

    pub fn last_error(&self) -> Option<String> {
        self.last_error.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// The PRM URL (RFC 9728: the well-known path inserted before the
    /// resource's path), when an OIDC issuer is configured.
    pub fn prm_url(&self) -> Option<String> {
        if self.issuers.is_empty() {
            return None;
        }
        let u = url::Url::parse(&self.resource).ok()?;
        let path = u.path().trim_end_matches('/');
        Some(format!("{}/.well-known/oauth-protected-resource{path}", u.origin().ascii_serialization()))
    }

    /// Records the refs a tool call reads (RFC 0008 §5.8); writes the
    /// previous hour's object when the hour turned (blocking).
    pub fn note_read(&self, args: &serde_json::Value) {
        let Some(rec) = &self.reads else { return };
        if let Some((hour, pairs)) = rec.record(args, unix_now()) {
            self.write_reads(rec, hour, &pairs);
        }
    }

    /// Writes the read pairs of an hour that ended (`all`: the current
    /// hour's too, at shutdown).
    pub fn flush_reads(&self, all: bool) {
        let Some(rec) = &self.reads else { return };
        if let Some((hour, pairs)) = rec.take(unix_now(), all) {
            self.write_reads(rec, hour, &pairs);
        }
    }

    fn write_reads(&self, rec: &ReadsRecorder, hour: u64, pairs: &BTreeSet<(String, String)>) {
        if let Err(e) = write_reads(self.storage.as_ref(), rec.replica(), hour, pairs) {
            tracing::warn!(error = %format!("{e:#}"), "writing read evidence failed");
        }
    }

    /// One watcher tick (blocking): see the module docs.
    pub fn tick(&self, known: &mut Option<ObjectVersion>) {
        self.flush_reads(false);
        let outcome = self.try_tick(known);
        match &outcome {
            Ok(()) => *self.last_error.lock().unwrap_or_else(|p| p.into_inner()) = None,
            Err(e) => {
                let msg = format!("{e:#}");
                tracing::warn!(error = %msg, "generation watcher");
                *self.last_error.lock().unwrap_or_else(|p| p.into_inner()) = Some(msg);
            }
        }
        self.limits.retain_recent();
        self.ticks.fetch_add(1, Ordering::SeqCst);
    }

    fn try_tick(&self, known: &mut Option<ObjectVersion>) -> anyhow::Result<()> {
        let serving = self.ensure_serving()?;
        let Some(report) = pull_generation(&serving.svc, known.as_ref())? else {
            return Ok(());
        };
        let dir = &serving.svc.local.dir;
        let g = current_generation(dir, &serving.org)?.context("the pulled generation is not installed")?;
        let warm_up = self.warm_up(&serving, &g)?;
        let current = self.install(&serving, &report, g)?;
        *known = Some(report.version.clone());
        tracing::info!(
            org = %serving.org,
            generation = report.generation,
            repos = report.repos,
            segments_downloaded = report.segments_downloaded,
            bytes_downloaded = report.bytes_downloaded,
            warm = %format!("{}/{}", current.warm.0, current.warm.1),
            warm_ms = u64::try_from(warm_up.as_millis()).unwrap_or(u64::MAX),
            "serving generation"
        );
        *self.current.write().unwrap_or_else(|p| p.into_inner()) = Some(Arc::new(current));
        if let Err(e) = self.evict(&serving) {
            tracing::warn!(error = %format!("{e:#}"), "cache eviction failed");
        }
        Ok(())
    }

    /// The serving half, built on the first tick that finds the store.
    fn ensure_serving(&self) -> anyhow::Result<Arc<Serving>> {
        if let Some(s) = self.serving() {
            return Ok(s);
        }
        let stored = read_store_org(self.storage.as_ref())?;
        let org = match (&self.config.org.name, stored) {
            (_, None) => bail!("store `{}` has no store.json yet (nothing published)", self.config.store.url),
            (Some(want), Some(have)) if *want != have => {
                bail!("store `{}` serves organization `{have}`, not `{want}` ([org].name)", self.config.store.url)
            }
            (_, Some(have)) => have,
        };
        let svc = ServiceStore::with_storage(self.storage.clone(), &self.config.store.cache_dir, &org)?;
        let engine = Engine::for_org(svc.local.dir.clone(), &org)?;
        let serving = Arc::new(Serving { svc, server: RuecaServer::new(engine.clone()), engine, org });
        *self.serving.write().unwrap_or_else(|p| p.into_inner()) = Some(serving.clone());
        Ok(serving)
    }

    /// Opens the installed generation `g` through the serving engine (see
    /// the module docs, *warm-up*) and times it. An organization without
    /// repositories has nothing to open.
    fn warm_up(&self, serving: &Serving, g: &OrgGeneration) -> anyhow::Result<Duration> {
        let started = Instant::now();
        if g.repos.is_empty() {
            return Ok(started.elapsed());
        }
        let opts = || RespondOptions { visibility: Some(Visibility::All), ..RespondOptions::default() };
        let engine = &serving.engine;
        engine
            .respond_with("list_repositories", serde_json::json!({}), opts())
            .context("warm-up: list_repositories")?;
        let search = serde_json::json!({"query": WARM_QUERY, "limit": 1});
        engine.respond_with("search_symbols", search, opts()).context("warm-up: search_symbols")?;
        let took = started.elapsed();
        self.metrics.histogram("rueca_generation_warm_seconds", &[]).record(took.as_secs_f64());
        Ok(took)
    }

    /// The installed generation `g` as served: with `ready_when = "warm"`
    /// its default segments verified, and the cache metrics recorded.
    fn install(&self, serving: &Serving, report: &PullGenerationReport, g: OrgGeneration) -> anyhow::Result<Current> {
        let defaults: Vec<&str> = g.repos.iter().map(|r| r.default.segment_id.as_str()).collect();
        let present = defaults.iter().filter(|id| serving.svc.local.segment_path(id).exists()).count();
        if self.config.store.ready_when == ReadyWhen::Warm {
            for id in &defaults {
                let started = Instant::now();
                serving.svc.local.open_segment(id).with_context(|| format!("opening segment {id}"))?;
                self.metrics.histogram("rueca_segment_open_seconds", &[]).record(started.elapsed().as_secs_f64());
            }
        }
        let named: usize = g.repos.iter().map(|r| r.entries().count()).sum();
        let misses = report.segments_downloaded.min(named);
        self.metrics.cache_requests("segment", "miss", misses as u64);
        self.metrics.cache_requests("segment", "hit", (named - misses) as u64);
        self.metrics.cache_requests("pack", "miss", report.packs_downloaded as u64);
        self.metrics.cache_requests("org_index", "miss", report.files_downloaded as u64);
        self.metrics.gauge("rueca_generation", &[]).set(g.generation as f64);
        let repos = g.repos.iter().map(|r| r.repo.clone()).collect();
        Ok(Current { warm: (present, defaults.len()), repos, generation: g })
    }

    /// Deletes segment files the current generation does not name, least
    /// recently accessed first, while the cache exceeds its budget.
    fn evict(&self, serving: &Serving) -> anyhow::Result<()> {
        let dir = &serving.svc.local.dir;
        let usage = self.cache_usage(serving);
        self.publish_usage(&usage);
        let budget = self.config.store.cache_bytes.0;
        let mut total: u64 = usage.values().sum();
        if total <= budget {
            return Ok(());
        }
        let live: BTreeSet<String> = self
            .current()
            .map(|c| c.generation.repos.iter().flat_map(|r| r.entries()).map(|e| e.segment_id.clone()).collect())
            .unwrap_or_default();
        let mut candidates = Vec::new();
        for entry in fs::read_dir(dir.join("segments"))?.flatten() {
            let path = entry.path();
            let Some(id) = path.file_stem().and_then(|s| s.to_str()) else { continue };
            if live.contains(id) {
                continue;
            }
            let meta = entry.metadata()?;
            let used = meta.accessed().or_else(|_| meta.modified()).unwrap_or(UNIX_EPOCH);
            candidates.push((used, meta.len(), path));
        }
        candidates.sort();
        for (_, len, path) in candidates {
            if total <= budget {
                break;
            }
            fs::remove_file(&path).with_context(|| format!("evicting {}", path.display()))?;
            total = total.saturating_sub(len);
            tracing::info!(segment = %path.display(), bytes = len, "evicted from the cache");
        }
        self.publish_usage(&self.cache_usage(serving));
        Ok(())
    }

    /// Bytes of the cache by class.
    pub fn cache_usage(&self, serving: &Serving) -> BTreeMap<&'static str, u64> {
        let dir = &serving.svc.local.dir;
        BTreeMap::from([
            ("segment", dir_bytes(&dir.join("segments"))),
            ("pack", dir_bytes(&dir.join("packs"))),
            ("org_index", dir_bytes(&org_dir(dir, &serving.org))),
        ])
    }

    fn publish_usage(&self, usage: &BTreeMap<&'static str, u64>) {
        for (cache, bytes) in usage {
            self.metrics.gauge("rueca_cache_bytes", &[("cache", cache)]).set(*bytes as f64);
        }
    }

    /// Seconds since the served generation was published.
    pub fn lag_seconds(&self) -> Option<u64> {
        let created = self.current()?.generation.created_at;
        Some(unix_now().saturating_sub(created))
    }

    /// Refreshes the gauges computed at read time (before `/metrics`).
    pub fn refresh_gauges(&self) {
        if let Some(lag) = self.lag_seconds() {
            self.metrics.gauge("rueca_generation_lag_seconds", &[]).set(lag as f64);
        }
    }
}

/// The organization of the indexer role: `[org].name` (creating
/// `store.json` if needed), else the one the store's `store.json` names.
fn indexer_org(config: &ServiceConfig, storage: &dyn Storage) -> anyhow::Result<String> {
    if let Some(name) = &config.org.name {
        return Ok(name.clone());
    }
    read_store_org(storage)?.context("the indexer role needs [org].name (the store has no store.json yet)")
}

/// The organization `store.json` names (`None`: not a store yet).
fn read_store_org(storage: &dyn Storage) -> anyhow::Result<Option<String>> {
    let Some(v) = storage.get("store.json")? else { return Ok(None) };
    let info: serde_json::Value = serde_json::from_slice(&v.value).context("store.json is not valid")?;
    let org = info.get("org").and_then(|o| o.as_str()).context("store.json names no organization")?;
    Ok(Some(org.to_string()))
}

/// Total size of the files under `dir` (recursive; missing: 0).
fn dir_bytes(dir: &Path) -> u64 {
    let Ok(rd) = fs::read_dir(dir) else { return 0 };
    rd.flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_bytes(&e.path()),
            Ok(_) => e.metadata().map_or(0, |m| m.len()),
            Err(_) => 0,
        })
        .sum()
}

pub fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}
