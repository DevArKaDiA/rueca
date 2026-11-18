//! Running a leased job (RFC 0008 §7.5, §7.7, §7.9): each desired ref,
//! default first, through a [`RefRunner`] into the indexer's local
//! **staging store**, then [`publish_snapshot`]; the results finish or fail
//! the job.
//!
//! [`run_job`]: refs whose catalog head already is the desired commit are
//! satisfied without work; a ref backing off after failing alone is
//! skipped; the lease is renewed between refs when due, and a lost lease
//! stops the run without writing the job (what was published stays: a
//! duplicated publish is idempotent by `(ref, commit)`). When every
//! attempted ref failed the job fails ([`LeasedJob::fail`]), else it
//! finishes ([`LeasedJob::finish`]) with the refs that failed alone recorded.
//!
//! The runner is a seam: [`ChildRunner`] fetches into a bare, blob-filtered
//! mirror, exports, and indexes in a `rueca index-worker` child process
//! ([`crate::worker`]) under resource limits; tests substitute in-process
//! runners.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use rueca_extract::Budget;
use rueca_index::mirror::Mirror;
use serde::Serialize;

use crate::catalog::{RefKind, Retention};
use crate::jobs::{DesiredRef, Job, JobError, LeaseLost, LeasedJob};
use crate::publish::{PublishReport, publish_snapshot};
use crate::reconcile::read_catalog;
use crate::remote::GitAccess;
use crate::worker::{self, WorkerCommand, WorkerRequest};
use crate::{ServiceStore, repo_dir, unix_now};

/// One ref to index.
#[derive(Debug, Clone, Copy)]
pub struct RefTask<'a> {
    pub repo: &'a str,
    pub url: &'a str,
    pub ref_name: &'a str,
    pub commit: &'a str,
    pub kind: RefKind,
    /// The staging store directory (a local store) to index into.
    pub store: &'a Path,
}

/// A ref indexed into the staging store.
#[derive(Debug, Clone)]
pub struct RefRun {
    /// The local segment id.
    pub segment: String,
    /// Time per stage (`fetch`, `export`, `index`).
    pub stages: Vec<(&'static str, Duration)>,
}

/// Why a ref failed (`stage`: `fetch`, `export`, `index`, `publish`, …).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{stage}: {reason}")]
pub struct RefError {
    pub stage: String,
    pub reason: String,
}

impl RefError {
    pub fn new(stage: &str, e: &anyhow::Error) -> RefError {
        RefError { stage: stage.to_string(), reason: format!("{e:#}") }
    }
}

/// Indexes one ref into the staging store.
pub trait RefRunner: Send + Sync {
    fn run_ref(&self, task: &RefTask<'_>) -> Result<RefRun, RefError>;
}

/// What happened to one ref of a run.
#[derive(Debug, Clone, Serialize)]
pub struct RefOutcome {
    #[serde(rename = "ref")]
    pub ref_name: String,
    pub commit: String,
    /// `indexed`, `present` (already in the catalog), `skipped` (backing
    /// off), `failed`.
    pub result: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip)]
    pub stages: Vec<(&'static str, Duration)>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publish: Option<PublishReport>,
}

/// What [`run_job`] did.
#[derive(Debug, Clone, Serialize)]
pub struct JobRun {
    pub repo: String,
    pub epoch: u64,
    pub refs: Vec<RefOutcome>,
    /// The job as written at the end (`None`: the lease was lost).
    #[serde(skip)]
    pub job: Option<Job>,
    /// `done`, `pending`, `failed` or `lost`.
    pub result: &'static str,
    pub seconds: f64,
}

/// Runs leased job `leased` (module docs) with `runner`, publishing from
/// `staging` with `retention`; `clock` gives unix seconds.
pub fn run_job(
    staging: &ServiceStore,
    mut leased: LeasedJob,
    runner: &dyn RefRunner,
    retention: &Retention,
    clock: &dyn Fn() -> u64,
) -> anyhow::Result<JobRun> {
    let started = Instant::now();
    let storage = staging.storage.as_ref();
    let mut run = JobRun {
        repo: leased.repo().to_string(),
        epoch: leased.epoch,
        refs: Vec::new(),
        job: None,
        result: "lost",
        seconds: 0.0,
    };
    let Some(Results { indexed, failed }) = run_refs(staging, &mut leased, runner, retention, clock, &mut run.refs)?
    else {
        run.seconds = started.elapsed().as_secs_f64();
        return Ok(run);
    };
    let attempted = run.refs.iter().filter(|r| r.result == "indexed" || r.result == "failed").count();
    let outcome = if attempted > 0 && failed.len() == attempted {
        let first = failed.values().next().cloned().unwrap_or_else(|| job_error_default(clock()));
        leased.fail(storage, &first.stage, &first.reason, clock())
    } else {
        leased.finish(storage, &indexed, &failed, clock())
    };
    run.seconds = started.elapsed().as_secs_f64();
    match outcome {
        Ok(job) => {
            run.result = job.state.as_str();
            run.job = Some(job);
            Ok(run)
        }
        Err(e) if e.downcast_ref::<LeaseLost>().is_some() => {
            tracing::warn!(repo = %run.repo, error = %format!("{e:#}"), "lease lost at the end of the job");
            Ok(run)
        }
        Err(e) => Err(e),
    }
}

/// Refs at the desired commit (indexed or present) and refs that failed.
struct Results {
    indexed: BTreeMap<String, String>,
    failed: BTreeMap<String, JobError>,
}

/// Runs every desired ref (module docs); `None` when the lease was lost.
fn run_refs(
    staging: &ServiceStore,
    leased: &mut LeasedJob,
    runner: &dyn RefRunner,
    retention: &Retention,
    clock: &dyn Fn() -> u64,
    outcomes: &mut Vec<RefOutcome>,
) -> anyhow::Result<Option<Results>> {
    let storage = staging.storage.as_ref();
    let catalog = read_catalog(storage, leased.repo())?;
    let mut res = Results { indexed: BTreeMap::new(), failed: BTreeMap::new() };
    for (name, want) in ordered(&leased.job) {
        if catalog.as_ref().and_then(|c| c.head_commit(&name)) == Some(want.commit.as_str()) {
            res.indexed.insert(name.clone(), want.commit.clone());
            outcomes.push(outcome(&name, &want, "present"));
            continue;
        }
        if leased.job.ref_backing_off(&name, &want.commit, clock()) {
            outcomes.push(outcome(&name, &want, "skipped"));
            continue;
        }
        if leased.renew_due(clock()) && !renew(storage, leased, clock())? {
            return Ok(None);
        }
        let out = index_one(staging, &leased.job, &name, &want, runner, retention);
        if out.error.is_none() {
            res.indexed.insert(name.clone(), want.commit.clone());
        } else {
            res.failed.insert(name.clone(), job_error(&out, clock()));
        }
        outcomes.push(out);
    }
    Ok(Some(res))
}

/// Renews the lease; `false` when it was lost (logged).
fn renew(storage: &dyn rueca_storage::Storage, leased: &mut LeasedJob, now: u64) -> anyhow::Result<bool> {
    match leased.renew(storage, now) {
        Ok(()) => Ok(true),
        Err(e) if e.downcast_ref::<LeaseLost>().is_some() => {
            tracing::warn!(repo = %leased.repo(), error = %format!("{e:#}"), "lease lost: stopping the job");
            Ok(false)
        }
        Err(e) => Err(e),
    }
}

/// The desired refs, default first, then by name.
fn ordered(job: &Job) -> Vec<(String, DesiredRef)> {
    let mut refs: Vec<(String, DesiredRef)> = job.desired.iter().map(|(n, d)| (n.clone(), d.clone())).collect();
    refs.sort_by_key(|(n, d)| (d.kind != RefKind::Default, n.clone()));
    refs
}

fn outcome(name: &str, want: &DesiredRef, result: &'static str) -> RefOutcome {
    RefOutcome {
        ref_name: name.to_string(),
        commit: want.commit.clone(),
        result,
        error: None,
        stages: Vec::new(),
        publish: None,
    }
}

/// Runs one ref, then publishes it.
fn index_one(
    staging: &ServiceStore,
    job: &Job,
    name: &str,
    want: &DesiredRef,
    runner: &dyn RefRunner,
    retention: &Retention,
) -> RefOutcome {
    let mut out = outcome(name, want, "failed");
    let url = job.url.as_deref().unwrap_or_default();
    let task = RefTask {
        repo: &job.repo,
        url,
        ref_name: name,
        commit: &want.commit,
        kind: want.kind,
        store: &staging.local.dir,
    };
    let run = match runner.run_ref(&task) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(repo = %job.repo, r#ref = name, error = %e, "ref failed");
            out.error = Some(e.to_string());
            return out;
        }
    };
    out.stages = run.stages;
    let t = Instant::now();
    match publish_snapshot(staging, &job.repo, name, want.kind, &run.segment, retention) {
        Ok(report) => {
            out.stages.push(("upload", t.elapsed()));
            out.result = "indexed";
            out.publish = Some(report);
        }
        Err(e) => out.error = Some(RefError::new("publish", &e).to_string()),
    }
    out
}

fn job_error(out: &RefOutcome, now: u64) -> JobError {
    let text = out.error.clone().unwrap_or_default();
    let (stage, reason) = text.split_once(": ").unwrap_or(("index", text.as_str()));
    JobError { stage: stage.to_string(), reason: reason.to_string(), at: now }
}

fn job_error_default(now: u64) -> JobError {
    JobError { stage: "index".into(), reason: "every ref failed".into(), at: now }
}

/// The runner of `rueca serve --role indexer|all` (RFC 0008 §7.6–§7.8):
/// per ref,
///
/// 1. **fetch** the commit into the repository's bare mirror
///    (`<mirrors>/<repo-dir>.git`, [`Mirror`]: depth 1, blobs over the
///    budget's `max_bytes` filtered out, the ref name as the fallback when
///    the host refuses a commit want), through [`GitAccess`] (allowlist,
///    credentials in the environment); mirrors older than `mirror_max_age`
///    are re-created, and unused ones deleted (checked at most hourly);
/// 2. **export** the tree from the bare mirror into the ref's export
///    directory in the staging store ([`rueca_index::refs::export_dir`]),
///    filtered-out blobs listed as gaps ([`rueca_index::refs::export_with`]);
/// 3. **index** in a `rueca index-worker` child ([`worker::run_ref`]) with
///    the staging store as its store (its `facts/` is the facts cache), the
///    gaps as `skipped:size` files, under [`worker::WorkerLimits`];
///
/// then removes the export (the snapshot's content is in its packs).
/// [`run_job`] publishes the segment.
pub struct ChildRunner {
    pub mirrors: PathBuf,
    pub git: GitAccess,
    pub budget: Budget,
    pub threads: Option<usize>,
    pub worker: WorkerCommand,
    pub mirror_max_age: Duration,
    last_sweep: Mutex<Option<Instant>>,
}

/// How often [`ChildRunner`] sweeps unused mirrors.
const MIRROR_SWEEP: Duration = Duration::from_secs(3600);

impl ChildRunner {
    pub fn new(mirrors: PathBuf, git: GitAccess, budget: Budget, worker: WorkerCommand) -> ChildRunner {
        ChildRunner {
            mirrors,
            git,
            budget,
            threads: None,
            worker,
            mirror_max_age: Duration::from_secs(7 * 24 * 3600),
            last_sweep: Mutex::new(None),
        }
    }

    /// The filter's limit: blobs of more than `max_bytes` stay on the host.
    fn max_bytes(&self) -> u64 {
        self.budget.max_bytes as u64
    }

    /// Deletes mirrors unused for `mirror_max_age`, at most hourly.
    fn sweep_mirrors(&self) {
        let mut last = self.last_sweep.lock().unwrap_or_else(|p| p.into_inner());
        if last.is_some_and(|t| t.elapsed() < MIRROR_SWEEP) {
            return;
        }
        *last = Some(Instant::now());
        for dir in rueca_index::mirror::expire_mirrors(&self.mirrors, self.mirror_max_age, unix_now()) {
            tracing::info!(mirror = %dir.display(), "removed an unused git mirror");
        }
    }

    fn fetch(&self, task: &RefTask<'_>) -> anyhow::Result<Mirror> {
        self.git.policy.check_resolved(task.url)?;
        let inv = self.git.invocation(task.url)?;
        let dir = self.mirrors.join(format!("{}.git", repo_dir(task.repo, None)));
        let mirror = Mirror::open_with(&dir, task.url, Some(self.mirror_max_age), unix_now())?
            .with_git(inv.config_args, inv.env);
        let fetched = mirror.fetch_commit(task.commit, self.max_bytes(), &fallback_refs(task))?;
        tracing::debug!(repo = task.repo, r#ref = task.ref_name, commit = task.commit, via = %fetched.via, "fetched");
        Ok(mirror)
    }
}

/// The full ref names whose tip may stand in for a refused commit want.
fn fallback_refs(task: &RefTask<'_>) -> Vec<String> {
    let name = task.ref_name;
    if let Some(n) = name.strip_prefix("pr/") {
        return vec![format!("refs/pull/{n}/head"), format!("refs/merge-requests/{n}/head")];
    }
    let hex = name.len() >= 7 && name.bytes().all(|b| b.is_ascii_hexdigit());
    match task.kind {
        RefKind::Commit if hex => Vec::new(),
        RefKind::Tag => vec![format!("refs/tags/{name}"), format!("refs/heads/{name}")],
        _ => vec![format!("refs/heads/{name}"), format!("refs/tags/{name}")],
    }
}

impl RefRunner for ChildRunner {
    fn run_ref(&self, task: &RefTask<'_>) -> Result<RefRun, RefError> {
        self.sweep_mirrors();
        let mut stages = Vec::new();
        let t = Instant::now();
        let mirror = self.fetch(task).map_err(|e| RefError::new("fetch", &e))?;
        stages.push(("fetch", t.elapsed()));
        let t = Instant::now();
        let dest = rueca_index::refs::export_dir(task.store, task.repo, "", task.ref_name);
        let gap = Some(self.max_bytes().saturating_add(1));
        let export = rueca_index::refs::export_with(mirror.dir(), task.commit, &dest, gap)
            .map_err(|e| RefError::new("export", &e))?;
        if !export.absent.is_empty() {
            tracing::info!(
                repo = task.repo,
                r#ref = task.ref_name,
                gaps = export.absent.len(),
                "blobs over the size budget left out"
            );
        }
        stages.push(("export", t.elapsed()));
        let t = Instant::now();
        let options = rueca_index::IndexOptions {
            root: dest.clone(),
            store: task.store.to_path_buf(),
            repo: Some(task.repo.to_string()),
            budget: self.budget.clone(),
            threads: self.threads,
            ref_name: Some(task.ref_name.to_string()),
            commit: Some(task.commit.to_string()),
        };
        let result = worker::run_ref(&self.worker, &WorkerRequest::new(options, export.absent));
        if let Err(e) = std::fs::remove_dir_all(&dest) {
            tracing::debug!(dir = %dest.display(), error = %e, "cannot remove the export");
        }
        let report = result?;
        stages.push(("index", t.elapsed()));
        Ok(RefRun { segment: report.segment.id, stages })
    }
}
