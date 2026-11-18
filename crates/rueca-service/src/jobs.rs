//! Job objects and their leases (RFC 0008 §7.5): `jobs/<repo-dir>.json`,
//! one per repository, written by compare-and-swap.
//!
//! A job holds the **desired state** of its repository (ref → commit,
//! written by polling, webhooks and admin requests through
//! [`desired_update`]), a **state** (`pending`, `leased`, `done`,
//! `failed`), a **priority** ([`Priority`]) and a **lease**. Indexers take
//! jobs with [`lease_job`]: the lowest priority, then the oldest
//! `updated_at`, among `pending` jobs whose `not_before` has passed,
//! `leased` jobs whose lease ended more than [`SKEW_SECS`] ago (taken as if
//! pending, the epoch incremented) and, after every one of those, `failed`
//! jobs whose back-off has passed (oldest `not_before` first). A lease lasts
//! [`LEASE_SECS`] and is renewed every [`RENEW_SECS`] by
//! [`LeasedJob::renew`]; a renewal, [`LeasedJob::finish`] or
//! [`LeasedJob::fail`] that finds another owner or epoch fails with
//! [`LeaseLost`] and writes nothing.
//!
//! **Finish**: refs whose indexed commit equals the desired one are
//! satisfied; a ref that failed alone is recorded in `failed_refs` with its
//! own back-off and counts as handled until the back-off passes or its
//! desired commit moves; every desired ref handled → `done` (attempts reset),
//! else (the desired state moved during the run) → `pending`. **Fail**
//! (every attempted ref failed): `attempts + 1`, `failed`, `not_before = now
//! + min(10 min × attempts, 120 min)` (Zoekt's schedule), `last_error`.
//!
//! The lease owner survives `done`, so a later lease can prefer the indexer
//! whose mirror and facts are warm (not used for selection yet). `embed`
//! jobs (`jobs/<repo-dir>.embed.json`, §7.11) share the schema and are never
//! leased in this phase.

use std::collections::{BTreeMap, HashMap};

use anyhow::Context;
use bytes::Bytes;
use rueca_storage::{CasOutcome, CasStep, ObjectVersion, PutError, Storage, cas_update};
use serde::{Deserialize, Serialize};

use crate::catalog::RefKind;
use crate::{CAS_ATTEMPTS, repo_dir};

/// Version of a job object.
pub const JOB_FORMAT: u32 = 1;
/// Where job objects live.
pub const JOBS_PREFIX: &str = "jobs/";
/// Length of a job lease.
pub const LEASE_SECS: u64 = 15 * 60;
/// How often a held lease is renewed.
pub const RENEW_SECS: u64 = 5 * 60;
/// Clock skew tolerated before an ended lease is taken over.
pub const SKEW_SECS: u64 = 30;
/// Back-off step and cap of a failed job (and of a ref that failed alone).
pub const BACKOFF_STEP_SECS: u64 = 10 * 60;
pub const BACKOFF_MAX_SECS: u64 = 120 * 60;

/// What a job does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobKind {
    Index,
    /// Phase 3 (§7.11): written with the same schema, never leased yet.
    Embed,
}

impl JobKind {
    pub fn as_str(self) -> &'static str {
        match self {
            JobKind::Index => "index",
            JobKind::Embed => "embed",
        }
    }
}

/// Where a job is in its life (the diagram of §7.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobState {
    Pending,
    Leased,
    Done,
    Failed,
}

impl JobState {
    pub const ALL: [JobState; 4] = [JobState::Pending, JobState::Leased, JobState::Done, JobState::Failed];

    pub fn as_str(self) -> &'static str {
        match self {
            JobState::Pending => "pending",
            JobState::Leased => "leased",
            JobState::Done => "done",
            JobState::Failed => "failed",
        }
    }
}

/// Why a job is pending, lowest first (§7.2); `failed` jobs come after all
/// of these, by `not_before`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Priority {
    /// The repository has no snapshot at all.
    NeverIndexed = 0,
    DefaultBehind = 1,
    PrBehind = 2,
    BranchBehind = 3,
    /// A tag (or an on-demand commit) is missing.
    TagMissing = 4,
}

impl Priority {
    /// The priority of a ref of `kind` that is behind its desired commit.
    pub fn of(kind: RefKind) -> Priority {
        match kind {
            RefKind::Default => Priority::DefaultBehind,
            RefKind::Pr => Priority::PrBehind,
            RefKind::Branch => Priority::BranchBehind,
            RefKind::Tag | RefKind::Commit => Priority::TagMissing,
        }
    }

    pub fn rank(self) -> u8 {
        self as u8
    }
}

/// One desired ref.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesiredRef {
    /// Full commit.
    pub commit: String,
    pub kind: RefKind,
    /// When this commit was first seen (unix seconds).
    pub seen_at: u64,
}

/// A job lease.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    /// `<indexer>/<random>`.
    pub owner: String,
    /// Unix seconds.
    pub until: u64,
    /// Incremented on every take.
    pub epoch: u64,
}

/// What failed, where and when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobError {
    /// `fetch`, `export`, `index`, `publish`, …
    pub stage: String,
    pub reason: String,
    pub at: u64,
}

/// A ref that failed alone (§7.5 rule 5), with its own back-off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefFailure {
    /// The desired commit that failed.
    pub commit: String,
    pub stage: String,
    pub reason: String,
    pub attempts: u32,
    pub not_before: u64,
}

/// The polling schedule of the repository (§7.4), kept in the job so a new
/// updater continues it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PollState {
    pub next_at: u64,
    pub interval_s: u64,
    /// When the polled refs last changed.
    pub changed_at: u64,
}

/// `jobs/<repo-dir>.json` (RFC 0008 §7.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Job {
    pub format: u32,
    pub kind: JobKind,
    pub repo: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default)]
    pub desired: BTreeMap<String, DesiredRef>,
    pub state: JobState,
    /// [`Priority::rank`] of the most urgent ref behind.
    pub priority: u8,
    #[serde(default)]
    pub lease: Option<Lease>,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default)]
    pub not_before: Option<u64>,
    #[serde(default)]
    pub last_error: Option<JobError>,
    #[serde(default)]
    pub poll: Option<PollState>,
    /// Refs that failed alone, by name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub failed_refs: BTreeMap<String, RefFailure>,
    /// Refs expired from the catalog (idle, removed) → the commit they had:
    /// polling does not bring them back until that commit moves.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub expired: BTreeMap<String, String>,
    pub updated_at: u64,
}

impl Job {
    /// A new job with nothing desired yet (`done`).
    pub fn new(kind: JobKind, repo: &str, url: Option<&str>, now: u64) -> Job {
        Job {
            format: JOB_FORMAT,
            kind,
            repo: repo.to_string(),
            url: url.map(str::to_string),
            desired: BTreeMap::new(),
            state: JobState::Done,
            priority: Priority::NeverIndexed.rank(),
            lease: None,
            attempts: 0,
            not_before: None,
            last_error: None,
            poll: None,
            failed_refs: BTreeMap::new(),
            expired: BTreeMap::new(),
            updated_at: now,
        }
    }

    /// The object path of this job.
    pub fn path(&self) -> String {
        job_path(&self.repo, self.kind)
    }

    /// Whether an indexer may take this job at `now`.
    pub fn leasable(&self, now: u64) -> bool {
        if self.kind != JobKind::Index {
            return false;
        }
        let due = self.not_before.is_none_or(|t| t <= now);
        match self.state {
            JobState::Pending | JobState::Failed => due,
            JobState::Leased => self.lease.as_ref().is_none_or(|l| l.until.saturating_add(SKEW_SECS) < now),
            JobState::Done => false,
        }
    }

    /// Lease order: pending (and expired leases) by priority then
    /// `updated_at`; failed jobs last, by `not_before`.
    pub fn lease_order(&self) -> (u8, u64, u64) {
        match self.state {
            JobState::Failed => (1, self.not_before.unwrap_or(0), self.updated_at),
            _ => (0, u64::from(self.priority), self.updated_at),
        }
    }

    /// Sets ref `name` to `want` (a new commit or kind); a job that is not
    /// leased or failed becomes `pending`, at least as urgent as the ref.
    /// Returns whether anything changed. Clears the ref's expiry tombstone.
    pub fn set_desired(&mut self, name: &str, want: DesiredRef) -> bool {
        self.expired.remove(name);
        let same = self.desired.get(name).is_some_and(|d| d.commit == want.commit && d.kind == want.kind);
        if same {
            return false;
        }
        let rank = Priority::of(want.kind).rank();
        self.desired.insert(name.to_string(), want);
        if self.state == JobState::Done {
            self.state = JobState::Pending;
            self.priority = rank;
        } else {
            self.priority = self.priority.min(rank);
        }
        true
    }

    /// Removes ref `name` from the desired state; whether it was there.
    pub fn remove_desired(&mut self, name: &str) -> bool {
        self.failed_refs.remove(name);
        self.desired.remove(name).is_some()
    }

    /// Whether ref `name` at `commit` failed alone and is still backing off.
    pub fn ref_backing_off(&self, name: &str, commit: &str, now: u64) -> bool {
        self.failed_refs.get(name).is_some_and(|f| f.commit == commit && f.not_before > now)
    }

    /// Whether `owner` holds this job at `epoch`.
    fn held_by(&self, owner: &str, epoch: u64) -> bool {
        self.state == JobState::Leased && self.lease.as_ref().is_some_and(|l| l.owner == owner && l.epoch == epoch)
    }
}

/// `min(10 min × attempts, 120 min)`.
pub fn backoff_secs(attempts: u32) -> u64 {
    BACKOFF_STEP_SECS.saturating_mul(u64::from(attempts)).min(BACKOFF_MAX_SECS)
}

/// `jobs/<repo-dir>.json`, or `jobs/<repo-dir>.embed.json`.
pub fn job_path(repo: &str, kind: JobKind) -> String {
    let rd = repo_dir(repo, None);
    match kind {
        JobKind::Index => format!("{JOBS_PREFIX}{rd}.json"),
        JobKind::Embed => format!("{JOBS_PREFIX}{rd}.embed.json"),
    }
}

/// A job read from the store, with the version it was read at.
#[derive(Debug, Clone)]
pub struct ListedJob {
    pub path: String,
    pub version: ObjectVersion,
    pub job: Job,
}

/// The jobs an indexer or updater has read, by path: a scan lists `jobs/`
/// and re-reads only the objects whose version changed (§7.5 rule 1).
#[derive(Debug, Default)]
pub struct JobCache {
    jobs: HashMap<String, (ObjectVersion, Job)>,
    /// Leases lost to another indexer's write ([`lease_job`]), for
    /// `rueca_cas_conflicts_total{object="job"}`.
    pub conflicts: u64,
}

impl JobCache {
    /// Every job of the store, fresh as of this listing. An unreadable job
    /// object is logged and left out.
    pub fn scan(&mut self, storage: &dyn Storage) -> anyhow::Result<Vec<ListedJob>> {
        let listed = storage.list(JOBS_PREFIX)?;
        let mut out = Vec::with_capacity(listed.len());
        let mut seen = HashMap::with_capacity(listed.len());
        for meta in listed {
            let cached = self.jobs.remove(&meta.path).filter(|(v, _)| v.e_tag.is_some() && *v == meta.version);
            let entry = match cached {
                Some(e) => Some(e),
                None => read_job(storage, &meta.path)?,
            };
            if let Some((version, job)) = entry {
                out.push(ListedJob { path: meta.path.clone(), version: version.clone(), job: job.clone() });
                seen.insert(meta.path, (version, job));
            }
        }
        self.jobs = seen;
        Ok(out)
    }

    /// Forgets one job (re-read at the next scan).
    pub fn forget(&mut self, path: &str) {
        self.jobs.remove(path);
    }
}

/// Reads one job object (`None`: absent or not a job, logged).
fn read_job(storage: &dyn Storage, path: &str) -> anyhow::Result<Option<(ObjectVersion, Job)>> {
    let Some(v) = storage.get(path)? else { return Ok(None) };
    match serde_json::from_slice::<Job>(&v.value) {
        Ok(job) => Ok(Some((v.version, job))),
        Err(e) => {
            tracing::warn!(path, error = %e, "ignoring an unreadable job object");
            Ok(None)
        }
    }
}

/// Reads the index job of `repo` (`None`: none yet).
pub fn get_job(storage: &dyn Storage, repo: &str) -> anyhow::Result<Option<Job>> {
    Ok(read_job(storage, &job_path(repo, JobKind::Index))?.map(|(_, j)| j))
}

/// A lease another indexer took (or the job vanished): stop, discard the
/// output, write nothing.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{path}: lease epoch {epoch} of `{owner}` is lost: {why}")]
pub struct LeaseLost {
    pub path: String,
    pub owner: String,
    pub epoch: u64,
    pub why: String,
}

/// A job this process leased.
#[derive(Debug, Clone)]
pub struct LeasedJob {
    pub path: String,
    /// The job as leased (its `desired` as of the lease).
    pub job: Job,
    pub owner: String,
    pub epoch: u64,
    /// When the lease was last written.
    pub renewed_at: u64,
}

/// Takes the most urgent leasable job (see the module docs) for `owner` at
/// `now`; `None` when no job is leasable. A job another indexer takes first
/// ([`PutError::Precondition`]) is skipped for the next.
pub fn lease_job(
    storage: &dyn Storage,
    cache: &mut JobCache,
    owner: &str,
    now: u64,
) -> anyhow::Result<Option<LeasedJob>> {
    let mut candidates: Vec<ListedJob> = cache.scan(storage)?.into_iter().filter(|l| l.job.leasable(now)).collect();
    candidates.sort_by_key(|l| l.job.lease_order());
    for c in candidates {
        let path = c.path.clone();
        match try_take(storage, c, owner, now)? {
            Some(leased) => return Ok(Some(leased)),
            None => {
                cache.conflicts += 1;
                cache.forget(&path);
            }
        }
    }
    Ok(None)
}

fn try_take(storage: &dyn Storage, c: ListedJob, owner: &str, now: u64) -> anyhow::Result<Option<LeasedJob>> {
    let mut job = c.job;
    let epoch = job.lease.as_ref().map_or(0, |l| l.epoch) + 1;
    job.state = JobState::Leased;
    job.lease = Some(Lease { owner: owner.to_string(), until: now + LEASE_SECS, epoch });
    job.updated_at = now;
    match storage.put_if(&c.path, json_bytes(&job)?, Some(&c.version)) {
        Ok(_) => {
            tracing::info!(repo = %job.repo, epoch, owner, "leased job");
            Ok(Some(LeasedJob { path: c.path, job, owner: owner.to_string(), epoch, renewed_at: now }))
        }
        Err(PutError::Precondition | PutError::AlreadyExists) => Ok(None),
        Err(PutError::Other(e)) => Err(e.context(format!("leasing {}", c.path))),
    }
}

fn json_bytes<T: Serialize>(v: &T) -> anyhow::Result<Bytes> {
    Ok(Bytes::from(serde_json::to_vec_pretty(v)?))
}

impl LeasedJob {
    pub fn repo(&self) -> &str {
        &self.job.repo
    }

    /// Whether the lease should be renewed at `now`.
    pub fn renew_due(&self, now: u64) -> bool {
        now >= self.renewed_at.saturating_add(RENEW_SECS)
    }

    /// Extends the lease to `now + LEASE_SECS` (CAS on the same epoch; a
    /// moved desired state is fine). [`LeaseLost`] when it was taken.
    pub fn renew(&mut self, storage: &dyn Storage, now: u64) -> anyhow::Result<()> {
        let out = self.update(storage, |job| {
            if let Some(l) = job.lease.as_mut() {
                l.until = now + LEASE_SECS;
            }
        })?;
        self.job = out.value;
        self.renewed_at = now;
        Ok(())
    }

    /// Finishes the run: `indexed` maps each ref to the commit now at the
    /// head of its catalog ref (indexed or already there), `failed` the
    /// refs that failed alone. See the module docs, *Finish*.
    pub fn finish(
        self,
        storage: &dyn Storage,
        indexed: &BTreeMap<String, String>,
        failed: &BTreeMap<String, JobError>,
        now: u64,
    ) -> anyhow::Result<Job> {
        let out = self.update(storage, |job| {
            record_ref_failures(job, failed, now);
            for (name, commit) in indexed {
                if job.desired.get(name).is_some_and(|d| d.commit == *commit) {
                    job.failed_refs.remove(name);
                }
            }
            let handled = job.desired.iter().all(|(name, d)| {
                indexed.get(name) == Some(&d.commit) || job.failed_refs.get(name).is_some_and(|f| f.commit == d.commit)
            });
            job.state = if handled { JobState::Done } else { JobState::Pending };
            if handled {
                job.attempts = 0;
                job.not_before = None;
                job.last_error = None;
            }
            job.updated_at = now;
        })?;
        Ok(out.value)
    }

    /// Fails the whole job (every attempted ref failed): `attempts + 1`,
    /// back-off, `last_error`.
    pub fn fail(self, storage: &dyn Storage, stage: &str, reason: &str, now: u64) -> anyhow::Result<Job> {
        let out = self.update(storage, |job| {
            job.attempts += 1;
            job.state = JobState::Failed;
            job.not_before = Some(now + backoff_secs(job.attempts));
            job.last_error = Some(JobError { stage: stage.to_string(), reason: reason.to_string(), at: now });
            job.updated_at = now;
        })?;
        Ok(out.value)
    }

    /// A CAS of the job that first checks the lease is still this one.
    fn update(&self, storage: &dyn Storage, mut f: impl FnMut(&mut Job)) -> anyhow::Result<CasOutcome<Job>> {
        let (owner, epoch) = (self.owner.as_str(), self.epoch);
        cas_update(storage, &self.path, CAS_ATTEMPTS, |cur: Option<Job>| {
            let lost = |why: &str| LeaseLost {
                path: self.path.clone(),
                owner: owner.to_string(),
                epoch,
                why: why.to_string(),
            };
            let mut job = cur.ok_or_else(|| lost("the job object is gone"))?;
            if !job.held_by(owner, epoch) {
                let now_held = job
                    .lease
                    .as_ref()
                    .map_or_else(|| "nobody".to_string(), |l| format!("{} epoch {}", l.owner, l.epoch));
                return Err(lost(&format!("the job is {} and held by {now_held}", job.state.as_str())).into());
            }
            f(&mut job);
            Ok(CasStep::Write(job))
        })
    }
}

/// Records refs that failed alone, each with its own back-off (attempts
/// count only while the desired commit stays the same).
fn record_ref_failures(job: &mut Job, failed: &BTreeMap<String, JobError>, now: u64) {
    for (name, err) in failed {
        let Some(want) = job.desired.get(name) else { continue };
        let attempts = job.failed_refs.get(name).filter(|f| f.commit == want.commit).map_or(0, |f| f.attempts) + 1;
        job.failed_refs.insert(
            name.clone(),
            RefFailure {
                commit: want.commit.clone(),
                stage: err.stage.clone(),
                reason: err.reason.clone(),
                attempts,
                not_before: now + backoff_secs(attempts),
            },
        );
    }
}

/// Merges a change into the index job of `repo` by compare-and-swap,
/// creating the job (with `url`) when absent: `f` edits the fresh job
/// (re-applied on every retry; see [`Job::set_desired`],
/// [`Job::remove_desired`]) and returns whether it changed anything;
/// nothing is written otherwise. Used by polling, webhooks and admin
/// requests.
pub fn desired_update<F>(
    storage: &dyn Storage,
    repo: &str,
    url: Option<&str>,
    now: u64,
    mut f: F,
) -> anyhow::Result<CasOutcome<Job>>
where
    F: FnMut(&mut Job) -> bool,
{
    let path = job_path(repo, JobKind::Index);
    cas_update(storage, &path, CAS_ATTEMPTS, |cur: Option<Job>| {
        let created = cur.is_none();
        let mut job = cur.unwrap_or_else(|| Job::new(JobKind::Index, repo, url, now));
        anyhow::ensure!(job.repo == repo, "{path} is the job of `{}`, not of `{repo}`", job.repo);
        let mut changed = f(&mut job) || created;
        if job.url.is_none() && url.is_some() {
            job.url = url.map(str::to_string);
            changed = true;
        }
        if !changed {
            return Ok(CasStep::Unchanged(job));
        }
        job.updated_at = now;
        Ok(CasStep::Write(job))
    })
    .with_context(|| format!("updating the job of `{repo}`"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn desired(commit: &str, kind: RefKind) -> DesiredRef {
        DesiredRef { commit: commit.into(), kind, seen_at: 1 }
    }

    #[test]
    fn backoff_schedule() {
        let mins: Vec<u64> = (1..=14).map(|a| backoff_secs(a) / 60).collect();
        assert_eq!(mins, [10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120, 120, 120]);
    }

    #[test]
    fn set_desired_moves_done_to_pending_with_the_ref_priority() {
        let mut j = Job::new(JobKind::Index, "r", None, 1);
        assert!(j.set_desired("v1", desired("a", RefKind::Tag)));
        assert_eq!((j.state, j.priority), (JobState::Pending, 4));
        assert!(j.set_desired("main", desired("b", RefKind::Default)));
        assert_eq!(j.priority, 1);
        assert!(!j.set_desired("main", desired("b", RefKind::Default)), "same commit: no change");
        j.state = JobState::Leased;
        assert!(j.set_desired("main", desired("c", RefKind::Default)));
        assert_eq!(j.state, JobState::Leased, "a leased job stays leased");
    }

    #[test]
    fn job_json_shape() {
        let mut j = Job::new(JobKind::Index, "github.com/acme/billing", Some("https://x/y.git"), 7);
        j.set_desired("main", desired("9f1c", RefKind::Default));
        let v = serde_json::to_value(&j).unwrap();
        assert_eq!(v["format"], 1);
        assert_eq!(v["kind"], "index");
        assert_eq!(v["state"], "pending");
        assert_eq!(v["desired"]["main"]["kind"], "default");
        assert!(v["lease"].is_null() && v["not_before"].is_null() && v["last_error"].is_null());
        assert!(v.get("failed_refs").is_none());
        assert_eq!(serde_json::from_value::<Job>(v).unwrap(), j);
        assert_eq!(job_path("github.com/acme/billing", JobKind::Embed), j.path().replace(".json", ".embed.json"));
    }
}
