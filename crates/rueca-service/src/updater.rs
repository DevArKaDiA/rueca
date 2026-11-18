//! The organization updater (RFC 0008 §5.4, §5.8, §7.2, §7.9): one leader
//! among the indexers, elected by `leases/org-updater.json`.
//!
//! **Lease** ([`hold_updater_lease`]): `{owner, until, epoch}`, created when
//! absent, renewed by its holder once fewer than 40 s are left (a 60 s lease
//! renewed every 20 s), taken over (`epoch + 1`) once `until` is more than
//! 30 s past. Election by conditional writes is only eventually correct;
//! that suffices because every publication is itself a CAS.
//!
//! **Tick** ([`Updater::tick`], every `[indexing].updater_tick`, 5 s): while
//! holding the lease,
//!
//! 1. reconcile ([`Reconciler::tick`]): inventory, polling, desired state;
//! 2. every [`UpdaterOptions::housekeeping_every`] (1 h): **idle expiry**
//!    (pull requests and on-demand refs whose head was indexed, and last
//!    read according to `reads/` ([`crate::reads`]), more than
//!    `pr_idle` ago leave the catalog and the job; `pr_idle = 0` disables
//!    it), then [`collect_garbage`] with the retention (which also expires
//!    closed pull requests);
//! 3. **generation**: adopt the store's generation when it moved
//!    ([`pull_generation`]), pull every repository whose job object changed
//!    since the last tick (a job that finished, or one whose catalog this
//!    tick changed) with [`pull_repo`], rebuild the organization with
//!    `update_org_with` and the package linker, and [`publish_generation`]
//!    when it changed. A [`Conflict`] (another updater published first)
//!    forgets the adopted version, so the next tick pulls the winner's
//!    generation and rebuilds on it. At most one generation per tick: every
//!    job finished since the last one is in it.
//!
//! The package linker reads file facts from the mirror's own facts cache,
//! then from [`UpdaterOptions::facts_from`] (the indexer's staging store,
//! which holds the facts of every snapshot this indexer indexed); a file in
//! neither is parsed from the content packs once and its facts cached in the
//! mirror, so re-linking the reverse dependencies of a pushed repository
//! never parses a file twice.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rueca_index::link::PackageLinker;
use rueca_segment::org::update_org_with;
use rueca_storage::{CasStep, ObjectVersion, Storage, cas_update};
use serde::{Deserialize, Serialize};

use crate::catalog::{RefKind, Retention};
use crate::gc::{GcReport, collect_garbage};
use crate::generation::{Conflict, GenerationReport, publish_generation, pull_generation};
use crate::jobs::{JobCache, desired_update};
use crate::pull::{PullWhich, pull_repo};
use crate::reads::{last_read, last_reads, prune_reads};
use crate::reconcile::{ReconcileReport, Reconciler, catalog_update, read_catalog};
use crate::{CAS_ATTEMPTS, ServiceStore};

/// The updater lease object.
pub const UPDATER_LEASE: &str = "leases/org-updater.json";
pub const UPDATER_LEASE_SECS: u64 = 60;
pub const UPDATER_RENEW_SECS: u64 = 20;
/// Clock skew tolerated before an ended updater lease is taken over.
pub const UPDATER_SKEW_SECS: u64 = 30;

/// `leases/org-updater.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdaterLease {
    pub owner: String,
    pub until: u64,
    pub epoch: u64,
}

/// Takes, keeps or renews the updater lease for `owner`, who holds epoch
/// `held` (`None`: not the leader so far), at `now`. `Some`: `owner` leads
/// with that lease; `None`: another owner does.
pub fn hold_updater_lease(
    storage: &dyn Storage,
    owner: &str,
    held: Option<u64>,
    now: u64,
) -> anyhow::Result<Option<UpdaterLease>> {
    let mine = |l: &UpdaterLease| l.owner == owner && Some(l.epoch) == held;
    let fresh = |epoch| UpdaterLease { owner: owner.to_string(), until: now + UPDATER_LEASE_SECS, epoch };
    let out = cas_update(storage, UPDATER_LEASE, CAS_ATTEMPTS, |cur: Option<UpdaterLease>| {
        Ok(match cur {
            None => CasStep::Write(fresh(1)),
            Some(l) if mine(&l) && l.until.saturating_sub(now) < UPDATER_LEASE_SECS - UPDATER_RENEW_SECS => {
                CasStep::Write(fresh(l.epoch))
            }
            Some(l) if mine(&l) => CasStep::Unchanged(l),
            Some(l) if l.until.saturating_add(UPDATER_SKEW_SECS) < now => CasStep::Write(fresh(l.epoch + 1)),
            Some(l) => CasStep::Unchanged(l),
        })
    })?;
    let lease = out.value;
    let leads = lease.owner == owner && (out.written || Some(lease.epoch) == held);
    Ok(leads.then_some(lease))
}

/// How the updater runs.
pub struct UpdaterOptions {
    /// `<indexer>/<random>`.
    pub owner: String,
    pub retention: Retention,
    /// Idle expiry and garbage collection period (1 h).
    pub housekeeping_every: Duration,
    /// Local stores whose facts caches the package linker also reads (the
    /// indexer's staging store), after the mirror's own (module docs).
    pub facts_from: Vec<PathBuf>,
}

/// What one tick did.
#[derive(Debug, Clone, Default, Serialize)]
pub struct TickReport {
    pub leader: bool,
    pub epoch: Option<u64>,
    pub reconcile: Option<ReconcileReport>,
    /// `repo@ref` expired as idle.
    pub idle_expired: Vec<String>,
    /// `reads/` objects older than the idle window, deleted.
    pub reads_pruned: usize,
    pub gc: Option<GcReport>,
    /// Repositories pulled into the mirror.
    pub pulled: Vec<String>,
    pub generation: Option<GenerationReport>,
    /// Time to build and publish the generation.
    pub generation_seconds: f64,
    /// Of which: profiles, link sets (`update_org_with`) and the upload
    /// ([`publish_generation`]), in milliseconds.
    pub profile_ms: u64,
    pub link_ms: u64,
    pub publish_ms: u64,
    /// Entries whose link set was recomputed.
    pub relinked: Vec<String>,
    /// Another updater published first (rebuilt next tick).
    pub conflict: bool,
}

/// The updater of one indexer (module docs).
pub struct Updater {
    /// The updater's mirror of the store (its own local store directory).
    pub svc: ServiceStore,
    pub reconciler: Reconciler,
    pub options: UpdaterOptions,
    lease: Option<UpdaterLease>,
    jobs: JobCache,
    /// Job object versions at the last tick, by repository.
    seen: HashMap<String, ObjectVersion>,
    /// `org.json` version adopted from the store.
    pointer: Option<ObjectVersion>,
    last_housekeeping: Option<u64>,
}

impl Updater {
    pub fn new(svc: ServiceStore, reconciler: Reconciler, options: UpdaterOptions) -> Updater {
        Updater {
            svc,
            reconciler,
            options,
            lease: None,
            jobs: JobCache::default(),
            seen: HashMap::new(),
            pointer: None,
            last_housekeeping: None,
        }
    }

    /// The lease this updater holds, if it leads.
    pub fn lease(&self) -> Option<&UpdaterLease> {
        self.lease.as_ref()
    }

    /// One tick at `now` (module docs).
    pub fn tick(&mut self, now: u64) -> anyhow::Result<TickReport> {
        let storage = self.svc.storage.clone();
        let held = self.lease.as_ref().map(|l| l.epoch);
        self.lease = hold_updater_lease(storage.as_ref(), &self.options.owner, held, now)?;
        let mut report = TickReport { leader: self.lease.is_some(), epoch: held, ..TickReport::default() };
        let Some(lease) = &self.lease else {
            if held.is_some() {
                tracing::warn!(owner = %self.options.owner, "updater lease lost");
            }
            return Ok(report);
        };
        report.epoch = Some(lease.epoch);
        report.reconcile = Some(self.reconciler.tick(&self.svc, &mut self.jobs, now)?);
        let mut touched = BTreeSet::new();
        if self.last_housekeeping.is_none_or(|t| now >= t + self.options.housekeeping_every.as_secs()) {
            self.housekeeping(now, &mut report, &mut touched)?;
        }
        self.sync_generation(&mut report, touched)?;
        Ok(report)
    }

    /// Idle expiry, then garbage collection.
    fn housekeeping(
        &mut self,
        now: u64,
        report: &mut TickReport,
        touched: &mut BTreeSet<String>,
    ) -> anyhow::Result<()> {
        self.last_housekeeping = Some(now);
        report.idle_expired = self.expire_idle(now)?;
        let idle = self.options.retention.pr_idle.as_secs();
        if idle > 0 {
            report.reads_pruned = prune_reads(self.svc.storage.as_ref(), now.saturating_sub(idle))?;
        }
        touched.extend(report.idle_expired.iter().filter_map(|r| r.split_once('@').map(|(repo, _)| repo.to_string())));
        let at = UNIX_EPOCH + Duration::from_secs(now);
        let gc = collect_garbage(&self.svc, &self.options.retention, at)?;
        touched.extend(gc.refs_expired.iter().filter_map(|r| r.split_once('@').map(|(repo, _)| repo.to_string())));
        report.gc = Some(gc);
        Ok(())
    }

    /// Expires pull requests and on-demand refs unread for `pr_idle`
    /// (module docs, step 2). Returns `repo@ref` of each.
    pub fn expire_idle(&mut self, now: u64) -> anyhow::Result<Vec<String>> {
        let idle = self.options.retention.pr_idle.as_secs();
        if idle == 0 {
            return Ok(Vec::new());
        }
        let storage = self.svc.storage.clone();
        let since = now.saturating_sub(idle);
        let reads = last_reads(storage.as_ref(), since, now)?;
        let mut expired = Vec::new();
        for listed in self.jobs.scan(storage.as_ref())? {
            let repo = listed.job.repo;
            let Some(catalog) = read_catalog(storage.as_ref(), &repo)? else { continue };
            let idle_refs: Vec<(String, String)> = catalog
                .refs
                .iter()
                .filter(|(_, e)| matches!(e.kind, RefKind::Pr | RefKind::Commit))
                .filter_map(|(name, e)| e.snapshots.first().map(|s| (name, s)))
                .filter(|(name, s)| s.indexed_at.max(last_read(&reads, &repo, name).unwrap_or(0)) < since)
                .map(|(name, s)| (name.clone(), s.commit.clone()))
                .collect();
            if idle_refs.is_empty() {
                continue;
            }
            expire_refs(storage.as_ref(), &repo, &idle_refs, now)?;
            expired.extend(idle_refs.iter().map(|(r, _)| format!("{repo}@{r}")));
        }
        if !expired.is_empty() {
            tracing::info!(refs = ?expired, "expired idle refs");
        }
        Ok(expired)
    }

    /// Pulls changed repositories, rebuilds and publishes (module docs,
    /// step 3).
    fn sync_generation(&mut self, report: &mut TickReport, mut touched: BTreeSet<String>) -> anyhow::Result<()> {
        if let Some(p) = pull_generation(&self.svc, self.pointer.as_ref())? {
            self.pointer = Some(p.version);
        }
        let storage = self.svc.storage.clone();
        for listed in self.jobs.scan(storage.as_ref())? {
            if self.seen.get(&listed.job.repo) != Some(&listed.version) {
                touched.insert(listed.job.repo.clone());
                self.seen.insert(listed.job.repo, listed.version);
            }
        }
        for repo in &touched {
            let has_refs = read_catalog(storage.as_ref(), repo)?.is_some_and(|c| !c.refs.is_empty());
            if has_refs {
                pull_repo(&self.svc, repo, PullWhich::All)?;
                report.pulled.push(repo.clone());
            }
        }
        if report.pulled.is_empty() && self.pointer.is_some() {
            return Ok(());
        }
        let started = Instant::now();
        let members = self.svc.mirror_members()?;
        if members.is_empty() {
            return Ok(());
        }
        let linker =
            self.options.facts_from.iter().fold(PackageLinker::new(&self.svc.local.dir), |l, d| l.with_fallback(d));
        let update = update_org_with(&self.svc.local, &self.svc.org, &members, &linker)?;
        report.profile_ms = update.profile_ms;
        report.link_ms = update.link_ms;
        report.relinked = update.linked.into_iter().map(|(entry, _)| entry).collect();
        if !update.published && self.pointer.is_some() {
            return Ok(());
        }
        self.publish(report, started)
    }

    fn publish(&mut self, report: &mut TickReport, started: Instant) -> anyhow::Result<()> {
        let t = Instant::now();
        match publish_generation(&self.svc) {
            Ok(g) => {
                report.publish_ms = t.elapsed().as_millis() as u64;
                report.generation_seconds = started.elapsed().as_secs_f64();
                report.generation = Some(g);
                Ok(())
            }
            Err(e) if e.downcast_ref::<Conflict>().is_some() => {
                tracing::info!(error = %format!("{e:#}"), "generation conflict: rebuilding on the next tick");
                self.pointer = None;
                self.seen.clear();
                report.conflict = true;
                Ok(())
            }
            Err(e) => Err(e),
        }
    }
}

/// Moves `refs` (name, commit) of `repo` out of the catalog and the job
/// (with an expiry tombstone, so polling does not bring them back until
/// their commit moves).
pub fn expire_refs(storage: &dyn Storage, repo: &str, refs: &[(String, String)], now: u64) -> anyhow::Result<()> {
    catalog_update(storage, repo, |c| refs.iter().fold(false, |acc, (name, _)| c.remove_ref(name, now) | acc))?;
    desired_update(storage, repo, None, now, |job| {
        let mut changed = false;
        for (name, commit) in refs {
            changed |= job.remove_desired(name);
            changed |= job.expired.insert(name.clone(), commit.clone()).as_ref() != Some(commit);
        }
        changed
    })?;
    Ok(())
}

/// Unix seconds now.
pub fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}
