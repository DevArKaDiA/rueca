//! The **indexer role** of `rueca serve --role indexer|all` (RFC 0008 §4.2,
//! §6.8, §7): job leasing, the job runner, the updater, the admin
//! endpoints' operations and the role's readiness and metrics.
//!
//! ```text
//!   discover (every [indexing].discover, 5 s) ── lease_job (CAS) ──▶ run_job ──▶ publish_snapshot ──▶ finish / fail
//!   updater  (every [indexing].updater_tick, 5 s, [indexing].org_updater) ── leases/org-updater.json
//!            └─▶ reconcile + polling ─▶ idle expiry + GC (hourly) ─▶ pull changed repos ─▶ update_org_with ─▶ publish_generation
//! ```
//!
//! Local directories: `[indexing].facts_dir` is the **staging store** (a
//! local store the runner indexes into and publishes from; its `facts/`
//! is the facts cache), `[indexing].mirror_dir` holds the runner's bare git
//! mirrors (`<repo-dir>.git`, re-created after `[indexing].mirror_max_age`,
//! 7 d) and, under `store/`, the updater's mirror of the service store.
//!
//! The runner is [`rueca_service::indexer::ChildRunner`]: each ref is
//! fetched into its bare mirror (depth 1, blobs over
//! `[indexing].budget.max_bytes` left on the host), exported, and indexed by
//! this binary's hidden `rueca index-worker` subcommand in a child process
//! limited by `[indexing].worker` (`address_space` 24 GiB — not enforced on
//! macOS —, `cpu_time` 4 h, `wall_time` 30 min); at most `[indexing].jobs`
//! jobs (default 1) run at once.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use anyhow::Context;
use rueca_extract::Budget;
use rueca_service::indexer::{ChildRunner, JobRun, RefRunner, run_job};
use rueca_service::jobs::{DesiredRef, Job, JobCache, JobState, LeasedJob, desired_update, get_job, lease_job};
use rueca_service::reconcile::{PollPolicy, Reconciler, RepoSpec, read_catalog};
use rueca_service::remote::{GitAccess, HostPolicy, RemoteLister};
use rueca_service::updater::{TickReport, Updater, UpdaterOptions, expire_refs, unix_now};
use rueca_service::webhooks::{Applied, Hint, apply_hint};
use rueca_service::worker::{WorkerCommand, WorkerLimits, address_space_enforced};
use rueca_service::{RefKind, Retention, ServiceStore};
use rueca_storage::Storage;
use serde_json::{Value, json};

use crate::config::ServiceConfig;
use crate::metrics::Metrics;
use crate::units::{Dur, Size};

/// `[indexing].hosts` when unset.
pub const DEFAULT_HOSTS: [&str; 3] = ["github.com", "gitlab.com", "bitbucket.org"];
/// The oldest `git` the role accepts (`GIT_NO_LAZY_FETCH`, RFC 0008 §7.6).
pub const MIN_GIT: (u32, u32) = (2, 45);
/// Idle expiry and garbage collection period.
const HOUSEKEEPING: Duration = Duration::from_secs(3600);

/// Checks `[[repo]]` and `[indexing]` (every role: a bad URL is a
/// configuration error wherever it is loaded).
pub fn validate(c: &ServiceConfig, p: &mut Vec<String>) {
    let i = &c.indexing;
    if i.allow_file_urls == Some(true) && !cfg!(debug_assertions) {
        p.push("indexing.allow_file_urls: test-only, refused by release builds".to_string());
    }
    for (key, d) in [("indexing.updater_tick", i.updater_tick), ("indexing.discover", i.discover)] {
        if d.is_some_and(|d| d.0.is_zero()) {
            p.push(format!("{key}: must be positive"));
        }
    }
    if i.worker.as_ref().and_then(|w| w.wall_time).is_some_and(|d| d.0.is_zero()) {
        p.push("indexing.worker.wall_time: must be positive".to_string());
    }
    if i.jobs == Some(0) || i.ls_remote_concurrency == Some(0) {
        p.push("indexing.jobs and indexing.ls_remote_concurrency must be positive".to_string());
    }
    let poll = poll_policy(c);
    if poll.min > poll.max {
        p.push("indexing.poll: min must not exceed max".to_string());
    }
    for host in i.webhooks.keys() {
        if rueca_service::webhooks::WebhookHost::parse(host).is_none() {
            p.push(format!("indexing.webhooks.{host}: unknown host (github, gitlab, bitbucket)"));
        }
    }
    let policy = host_policy(c);
    let mut ids = std::collections::BTreeSet::new();
    for r in &c.repo {
        if let Err(e) = policy.check(&r.url).and_then(|_| repo_spec(r)) {
            p.push(format!("repo `{}`: {e:#}", r.url));
            continue;
        }
        if let Ok(spec) = repo_spec(r)
            && !ids.insert(spec.repo.clone())
        {
            p.push(format!("repo `{}`: repository `{}` is configured twice", r.url, spec.repo));
        }
    }
}

/// The SSRF allowlist (§9.2) from `[indexing]`.
pub fn host_policy(c: &ServiceConfig) -> HostPolicy {
    let i = &c.indexing;
    let hosts =
        if i.hosts.is_empty() { DEFAULT_HOSTS.iter().map(|h| h.to_string()).collect() } else { i.hosts.clone() };
    HostPolicy {
        hosts,
        allow_private_networks: i.allow_private_networks.unwrap_or(false),
        allow_file_urls: cfg!(debug_assertions) && i.allow_file_urls.unwrap_or(false),
    }
}

fn repo_spec(r: &crate::config::RepoConfig) -> anyhow::Result<RepoSpec> {
    let tags = match &r.tags {
        crate::config::Globs::None => Vec::new(),
        crate::config::Globs::One(g) => vec![g.clone()],
        crate::config::Globs::Many(v) => v.clone(),
    };
    RepoSpec::new(&r.url, r.default_branch.as_deref(), &r.branches, &tags, r.prs)
}

/// The configured repositories.
pub fn repo_specs(c: &ServiceConfig) -> anyhow::Result<Vec<RepoSpec>> {
    c.repo.iter().map(repo_spec).collect()
}

/// `[indexing].poll` and `ls_remote_concurrency`.
pub fn poll_policy(c: &ServiceConfig) -> PollPolicy {
    let d = PollPolicy::default();
    let poll = c.indexing.poll.as_ref();
    PollPolicy {
        min: poll.and_then(|p| p.min).map_or(d.min, |x| x.0),
        max: poll.and_then(|p| p.max).map_or(d.max, |x| x.0),
        pr_heads: poll.and_then(|p| p.pr_heads).unwrap_or(false),
        per_host: c.indexing.ls_remote_concurrency.map_or(d.per_host, |n| n as usize),
    }
}

/// `[retention]`.
pub fn retention(c: &ServiceConfig) -> Retention {
    let d = Retention::default();
    let r = &c.retention;
    let n = |v: Option<u32>, dflt: usize| v.map_or(dflt, |x| x as usize);
    let t = |v: Option<Dur>, dflt: Duration| v.map_or(dflt, |x| x.0);
    Retention {
        default_snapshots: n(r.default_snapshots, d.default_snapshots),
        branch_snapshots: n(r.branch_snapshots, d.branch_snapshots),
        tag_snapshots: n(r.tag_snapshots, d.tag_snapshots),
        pr_after_close: t(r.pr_after_close, d.pr_after_close),
        pr_idle: t(r.pr_idle, d.pr_idle),
        grace: t(r.grace, d.grace),
    }
}

fn budget(c: &ServiceConfig) -> Budget {
    let d = Budget::default();
    let Some(b) = c.indexing.budget.as_ref() else { return d };
    Budget {
        max_bytes: b.max_bytes.map_or(d.max_bytes, |Size(s)| usize::try_from(s).unwrap_or(usize::MAX)),
        max_parse_time: b.max_parse_time.map_or(d.max_parse_time, |x| x.0),
        max_depth: b.max_depth.map_or(d.max_depth, |x| x as usize),
    }
}

/// `[indexing].worker`, over [`WorkerLimits::default`].
pub fn worker_limits(c: &ServiceConfig) -> WorkerLimits {
    let d = WorkerLimits::default();
    let Some(w) = c.indexing.worker.as_ref() else { return d };
    WorkerLimits {
        address_space: w.address_space.map(|Size(s)| s).or(d.address_space),
        cpu_time: w.cpu_time.map(|x| x.0).or(d.cpu_time),
        wall_time: w.wall_time.map_or(d.wall_time, |x| x.0),
    }
}

/// The runner: [`ChildRunner`] starting this executable's `index-worker`.
fn child_runner(c: &ServiceConfig, git: GitAccess) -> anyhow::Result<ChildRunner> {
    let limits = worker_limits(c);
    if limits.address_space.is_some() && !address_space_enforced() {
        tracing::warn!(
            "indexing.worker.address_space is not enforced on this platform (macOS); cpu_time and wall_time are"
        );
    }
    let worker = WorkerCommand::current_exe(limits)?;
    let mut runner = ChildRunner::new(mirror_dir(c), git, budget(c), worker);
    runner.threads = c.indexing.threads.filter(|t| *t > 0).map(|t| t as usize);
    if let Some(age) = c.indexing.mirror_max_age {
        runner.mirror_max_age = age.0;
    }
    Ok(runner)
}

/// `[indexing].mirror_dir`, by default `<store dir>/service-mirrors`.
pub fn mirror_dir(c: &ServiceConfig) -> PathBuf {
    c.indexing.mirror_dir.clone().unwrap_or_else(|| rueca_segment::Store::default_dir().join("service-mirrors"))
}

/// `[indexing].facts_dir` (the staging store), by default
/// `<store dir>/service-indexer`.
pub fn facts_dir(c: &ServiceConfig) -> PathBuf {
    c.indexing.facts_dir.clone().unwrap_or_else(|| rueca_segment::Store::default_dir().join("service-indexer"))
}

/// `<host>/<6 hex digits>`: an indexer or replica instance.
pub fn instance_id() -> String {
    let host = std::env::var("HOSTNAME").ok().filter(|h| !h.is_empty()).unwrap_or_else(|| "rueca".to_string());
    let host: String = host.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_')).collect();
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    format!("{host}/{:06x}", (nanos ^ std::process::id().rotate_left(12)) & 0xff_ffff)
}

/// What the role's job discovery last saw.
#[derive(Debug, Clone, Default)]
pub struct JobsSummary {
    /// `(kind, state)` → jobs.
    pub by_state: BTreeMap<(&'static str, &'static str), usize>,
    /// Priority (or `failed`) → leasable jobs.
    pub depth: BTreeMap<String, usize>,
    /// Age of the oldest pending job (seconds).
    pub oldest_pending_s: Option<u64>,
}

/// An admin request that cannot be carried out.
#[derive(Debug, thiserror::Error)]
pub enum AdminError {
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

/// The indexer role's state.
pub struct Indexer {
    pub staging: ServiceStore,
    pub owner: String,
    pub retention: Retention,
    pub discover: Duration,
    pub updater_tick: Duration,
    pub jobs: usize,
    specs: Vec<RepoSpec>,
    git: Arc<GitAccess>,
    runner: Box<dyn RefRunner>,
    cache: Mutex<JobCache>,
    updater: Option<Mutex<Updater>>,
    summary: RwLock<JobsSummary>,
    git_version: Result<(u32, u32), String>,
    metrics: Metrics,
    /// Wakes job discovery before its period (a confirmed webhook).
    pub wake: tokio::sync::Notify,
    /// Wakes the updater before its tick (a job finished here).
    pub wake_updater: tokio::sync::Notify,
}

impl Indexer {
    /// Builds the role over `storage` for organization `org`.
    pub fn new(c: &ServiceConfig, storage: Arc<dyn Storage>, org: &str, metrics: Metrics) -> anyhow::Result<Indexer> {
        let i = &c.indexing;
        let credentials = i
            .credentials
            .iter()
            .filter_map(|(host, s)| s.value().map(|v| (host.to_ascii_lowercase(), v.to_string())))
            .collect();
        let git = Arc::new(GitAccess { policy: host_policy(c), credentials });
        let mirrors = mirror_dir(c);
        let staging = ServiceStore::open_with_storage(storage.clone(), &facts_dir(c), org)?;
        let owner = instance_id();
        let specs = repo_specs(c)?;
        let runner = child_runner(c, (*git).clone())?;
        let updater = if i.org_updater.unwrap_or(true) {
            let svc = ServiceStore::with_storage(storage, &mirrors.join("store"), org)?;
            let lister: Arc<dyn RemoteLister> = git.clone();
            let reconciler = Reconciler::new(specs.clone(), poll_policy(c), lister);
            let options = UpdaterOptions {
                owner: owner.clone(),
                retention: retention(c),
                housekeeping_every: HOUSEKEEPING,
                facts_from: vec![staging.local.dir.clone()],
            };
            Some(Mutex::new(Updater::new(svc, reconciler, options)))
        } else {
            None
        };
        Ok(Indexer {
            staging,
            retention: retention(c),
            discover: i.discover.map_or(Duration::from_secs(5), |d| d.0),
            updater_tick: i.updater_tick.map_or(Duration::from_secs(5), |d| d.0),
            jobs: i.jobs.map_or(1, |n| n as usize),
            specs,
            git,
            runner: Box::new(runner),
            cache: Mutex::new(JobCache::default()),
            updater,
            summary: RwLock::new(JobsSummary::default()),
            git_version: git_version(),
            metrics,
            owner,
            wake: tokio::sync::Notify::new(),
            wake_updater: tokio::sync::Notify::new(),
        })
    }

    pub fn has_updater(&self) -> bool {
        self.updater.is_some()
    }

    /// The configured repositories (`[[repo]]`).
    pub fn specs(&self) -> &[RepoSpec] {
        &self.specs
    }

    /// Confirms a webhook hint with `git ls-remote` and merges it into the
    /// job of `spec` ([`rueca_service::webhooks::apply_hint`]).
    pub fn apply_hint(&self, spec: &RepoSpec, hint: &Hint, now: u64) -> anyhow::Result<Applied> {
        apply_hint(self.storage(), spec, self.git.as_ref(), hint, now)
    }

    pub fn summary(&self) -> JobsSummary {
        self.summary.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    fn storage(&self) -> &dyn Storage {
        self.staging.storage.as_ref()
    }

    /// Readiness (§6.7): `store.json` readable and `git` ≥ 2.45.
    pub fn ready(&self) -> Result<Value, String> {
        match self.storage().get("store.json") {
            Ok(Some(_)) => {}
            Ok(None) => return Err("the store has no store.json".to_string()),
            Err(e) => return Err(format!("store: {e:#}")),
        }
        let (major, minor) = self.git_version.clone()?;
        if (major, minor) < MIN_GIT {
            return Err(format!("git {major}.{minor} found; {}.{} or later is required", MIN_GIT.0, MIN_GIT.1));
        }
        Ok(json!({"store": "ok", "git": format!("{major}.{minor}")}))
    }

    /// Discovery: refreshes the job summary and metrics, then leases the
    /// next job at `now` (errors are logged).
    pub fn lease_next(&self, now: u64) -> Option<LeasedJob> {
        let mut cache = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        let before = cache.conflicts;
        let leased =
            self.refresh(&mut cache, now).and_then(|()| lease_job(self.storage(), &mut cache, &self.owner, now));
        let lost = cache.conflicts - before;
        if lost > 0 {
            self.metrics.counter("rueca_cas_conflicts_total", &[("object", "job")]).increment(lost);
        }
        leased.unwrap_or_else(|e| {
            tracing::warn!(error = %format!("{e:#}"), "job discovery failed");
            None
        })
    }

    /// Recomputes [`JobsSummary`] and its gauges from a scan.
    fn refresh(&self, cache: &mut JobCache, now: u64) -> anyhow::Result<()> {
        let mut s = JobsSummary::default();
        for l in cache.scan(self.storage())? {
            *s.by_state.entry((l.job.kind.as_str(), l.job.state.as_str())).or_default() += 1;
            if l.job.leasable(now) {
                let key =
                    if l.job.state == JobState::Failed { "failed".to_string() } else { l.job.priority.to_string() };
                *s.depth.entry(key).or_default() += 1;
            }
            if l.job.state == JobState::Pending {
                let age = now.saturating_sub(l.job.updated_at);
                s.oldest_pending_s = Some(s.oldest_pending_s.map_or(age, |a| a.max(age)));
            }
        }
        for state in JobState::ALL {
            let n = s.by_state.get(&("index", state.as_str())).copied().unwrap_or(0);
            self.metrics.gauge("rueca_jobs", &[("kind", "index"), ("state", state.as_str())]).set(n as f64);
        }
        for p in ["0", "1", "2", "3", "4", "failed"] {
            let n = s.depth.get(p).copied().unwrap_or(0);
            self.metrics.gauge("rueca_queue_depth", &[("kind", "index"), ("priority", p)]).set(n as f64);
        }
        *self.summary.write().unwrap_or_else(|p| p.into_inner()) = s;
        Ok(())
    }

    /// Runs a leased job and records its metrics.
    pub fn run(&self, leased: LeasedJob) {
        let repo = leased.repo().to_string();
        match run_job(&self.staging, leased, self.runner.as_ref(), &self.retention, &unix_now) {
            Ok(run) => {
                self.record_run(&run);
                if run.refs.iter().any(|r| r.result == "indexed") {
                    self.wake_updater.notify_one();
                }
            }
            Err(e) => {
                tracing::error!(repo, error = %format!("{e:#}"), "job failed to run");
                self.metrics.histogram("rueca_job_seconds", &[("kind", "index"), ("result", "error")]).record(0.0);
            }
        }
    }

    fn record_run(&self, run: &JobRun) {
        tracing::info!(repo = %run.repo, epoch = run.epoch, result = run.result, seconds = run.seconds, refs = ?run.refs, "job finished");
        self.metrics.histogram("rueca_job_seconds", &[("kind", "index"), ("result", run.result)]).record(run.seconds);
        for r in &run.refs {
            for (stage, took) in &r.stages {
                self.metrics.histogram("rueca_publish_seconds", &[("stage", stage)]).record(took.as_secs_f64());
            }
            if let Some(p) = r.publish.as_ref().filter(|p| p.conflicts > 0) {
                self.metrics
                    .counter("rueca_cas_conflicts_total", &[("object", "catalog")])
                    .increment(u64::from(p.conflicts));
            }
        }
    }

    /// One updater tick at `now` (when this indexer runs the updater).
    pub fn tick_updater(&self, now: u64) {
        let Some(updater) = &self.updater else { return };
        let mut u = updater.lock().unwrap_or_else(|p| p.into_inner());
        match u.tick(now) {
            Ok(report) => self.record_tick(&report),
            Err(e) => tracing::warn!(error = %format!("{e:#}"), "updater tick failed"),
        }
    }

    fn record_tick(&self, r: &TickReport) {
        if let Some(g) = &r.generation {
            self.metrics.gauge("rueca_generation", &[]).set(g.generation as f64);
            self.metrics.histogram("rueca_publish_seconds", &[("stage", "generation")]).record(r.generation_seconds);
            tracing::info!(
                generation = g.generation,
                published = g.published,
                pulled = ?r.pulled,
                seconds = r.generation_seconds,
                profile_ms = r.profile_ms,
                link_ms = r.link_ms,
                publish_ms = r.publish_ms,
                relinked = ?r.relinked,
                "updater published"
            );
        }
        if r.conflict {
            self.metrics.counter("rueca_cas_conflicts_total", &[("object", "pointer")]).increment(1);
        }
        for (class, bytes) in r.gc.iter().flat_map(|g| g.deleted_by_class.iter()) {
            self.metrics.counter("rueca_gc_deleted_bytes_total", &[("class", class)]).increment(*bytes);
        }
        if let Some(rec) = r.reconcile.as_ref().filter(|x| !x.changed.is_empty() || !x.errors.is_empty()) {
            tracing::info!(changed = ?rec.changed, removed = ?rec.removed, errors = ?rec.errors, "reconciled");
        }
    }

    /// Every job object (`GET /admin/jobs`).
    pub fn list_jobs(&self) -> anyhow::Result<Vec<Job>> {
        let mut cache = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        Ok(cache.scan(self.storage())?.into_iter().map(|l| l.job).collect())
    }

    /// The URL of repository `repo`: configured, else its job's.
    fn url_of(&self, repo: &str) -> Result<String, AdminError> {
        if let Some(s) = self.specs.iter().find(|s| s.repo == repo) {
            return Ok(s.url.clone());
        }
        get_job(self.storage(), repo)?
            .and_then(|j| j.url)
            .ok_or_else(|| AdminError::NotFound(format!("repository `{repo}` is not configured")))
    }

    /// `POST /admin/index`: resolves `ref_name` on the host and adds it to
    /// the desired state (a tag, a branch outside the globs or a full
    /// commit is an on-demand ref).
    pub fn admin_index(&self, repo: &str, ref_name: &str, now: u64) -> Result<Value, AdminError> {
        let url = self.url_of(repo)?;
        let (name, kind, commit) = self.resolve(repo, &url, ref_name.trim())?;
        let want = DesiredRef { commit: commit.clone(), kind, seen_at: now };
        desired_update(self.storage(), repo, Some(&url), now, |job| job.set_desired(&name, want.clone()))?;
        tracing::info!(repo, r#ref = %name, commit = %commit, "admin: ref requested");
        Ok(json!({"repo": repo, "ref": name, "kind": kind.as_str(), "commit": commit}))
    }

    /// The ref name, kind and commit `ref_name` resolves to on `url`: a
    /// full commit as itself (named by its 12 digits); `pr/<n>` through
    /// `refs/pull/<n>/head` or `refs/merge-requests/<n>/head`; a tag; a
    /// branch (the default branch, or an on-demand ref).
    fn resolve(&self, repo: &str, url: &str, ref_name: &str) -> Result<(String, RefKind, String), AdminError> {
        if let Some(commit) = full_commit(ref_name)? {
            return Ok((commit[..12].to_string(), RefKind::Commit, commit));
        }
        let candidates: Vec<String> = match ref_name.strip_prefix("pr/") {
            Some(n) => vec![format!("refs/pull/{n}/head"), format!("refs/merge-requests/{n}/head")],
            None => vec![format!("refs/tags/{ref_name}"), format!("refs/heads/{ref_name}")],
        };
        let mut patterns: Vec<&str> = candidates.iter().map(String::as_str).collect();
        patterns.push("HEAD");
        let listed = self.git.ls_remote(url, &patterns).map_err(|e| AdminError::Internal(anyhow::Error::from(e)))?;
        let (full, sha) = candidates
            .iter()
            .find_map(|full| listed.refs.get(full).map(|c| (full.as_str(), c.clone())))
            .ok_or_else(|| AdminError::NotFound(format!("`{ref_name}` is not on {url}")))?;
        let configured = self.specs.iter().find(|s| s.repo == repo).and_then(|s| s.default_branch.as_deref());
        let kind = match full {
            f if f.starts_with("refs/tags/") => RefKind::Tag,
            f if f.starts_with("refs/heads/") && configured.or(listed.head.as_deref()) == Some(ref_name) => {
                RefKind::Default
            }
            f if f.starts_with("refs/heads/") => RefKind::Commit,
            _ => RefKind::Pr,
        };
        Ok((ref_name.to_string(), kind, sha))
    }

    /// `DELETE /admin/refs`: removes a ref from the catalog and the desired
    /// state (polling brings a globbed branch or tag back only when it
    /// moves). The default ref cannot be removed.
    pub fn admin_remove(&self, repo: &str, ref_name: &str, now: u64) -> Result<Value, AdminError> {
        let catalog = read_catalog(self.storage(), repo)?;
        let job = get_job(self.storage(), repo)?;
        if catalog.as_ref().and_then(|c| c.default_ref.as_deref()) == Some(ref_name)
            || job.as_ref().and_then(|j| j.desired.get(ref_name)).is_some_and(|d| d.kind == RefKind::Default)
        {
            return Err(AdminError::Invalid(format!("`{ref_name}` is the default ref of `{repo}`")));
        }
        let commit = catalog
            .as_ref()
            .and_then(|c| c.head_commit(ref_name).map(str::to_string))
            .or_else(|| job.as_ref().and_then(|j| j.desired.get(ref_name)).map(|d| d.commit.clone()))
            .ok_or_else(|| AdminError::NotFound(format!("`{repo}` has no ref `{ref_name}`")))?;
        expire_refs(self.storage(), repo, &[(ref_name.to_string(), commit)], now).context("removing the ref")?;
        tracing::info!(repo, r#ref = ref_name, "admin: ref removed");
        Ok(json!({"repo": repo, "ref": ref_name, "removed": true}))
    }
}

/// A full 40-digit commit (`Some`, lowercase); a shorter hex prefix or a
/// malformed name is refused; anything else is a ref name (`None`).
fn full_commit(name: &str) -> Result<Option<String>, AdminError> {
    let hex = !name.is_empty() && name.bytes().all(|b| b.is_ascii_hexdigit());
    if hex && name.len() == 40 {
        return Ok(Some(name.to_ascii_lowercase()));
    }
    if hex && name.len() >= 7 {
        return Err(AdminError::Invalid(
            "give the full 40-digit commit: a prefix cannot be resolved on the host".into(),
        ));
    }
    let pr_ok = name.strip_prefix("pr/").is_none_or(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    if name.is_empty() || name.starts_with('-') || name.contains("..") || name.contains(char::is_whitespace) || !pr_ok {
        return Err(AdminError::Invalid(format!("`{name}` is not a ref name")));
    }
    Ok(None)
}

/// `git --version` → `(major, minor)`.
fn git_version() -> Result<(u32, u32), String> {
    let out = rueca_index::org::git_output(&["--version"], None, &[]).map_err(|e| format!("{e:#}"))?;
    parse_git_version(&out).ok_or_else(|| format!("cannot read the git version from `{}`", out.trim()))
}

fn parse_git_version(text: &str) -> Option<(u32, u32)> {
    let v = text.split_whitespace().nth(2)?;
    let mut parts = v.split('.');
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_versions() {
        assert_eq!(parse_git_version("git version 2.50.1 (Apple Git-155)\n"), Some((2, 50)));
        assert_eq!(parse_git_version("git version 2.45.0"), Some((2, 45)));
        assert!(parse_git_version("nope").is_none());
        assert!((2, 43) < MIN_GIT && (2, 47) >= MIN_GIT);
    }

    #[test]
    fn repository_urls_are_checked_by_every_role() {
        let text = r#"
[store]
url = "memory://x"
[[repo]]
url = "https://gitlab.example.com/a/b.git"
[[repo]]
url = "file:///tmp/x.git"
[[repo]]
url = "https://github.com/acme/billing.git"
[[repo]]
url = "git@github.com:acme/billing.git"
[indexing]
jobs = 0
"#;
        let p = ServiceConfig::parse(text).unwrap().validate();
        for needle in ["not in [indexing].hosts", "only in tests", "configured twice", "must be positive"] {
            assert!(p.iter().any(|x| x.contains(needle)), "{needle} not in {p:#?}");
        }
        let ok = text
            .replace("jobs = 0", "allow_file_urls = true")
            .replace("https://gitlab.example.com/a/b.git", "https://github.com/acme/core.git")
            .replace("git@github.com:acme/billing.git", "https://bitbucket.org/acme/x.git");
        assert_eq!(
            ServiceConfig::parse(&ok).unwrap().validate(),
            Vec::<String>::new(),
            "debug builds accept file URLs"
        );
    }
}
