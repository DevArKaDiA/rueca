//! Job leases (RFC 0008 §7.5): a lease race between eight indexers, taking
//! over an ended lease, renewing a lost one, and the failure back-off.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Barrier, Mutex};
use std::time::Duration;

use common::SlowCatalogs;
use rueca_service::RefKind;
use rueca_service::jobs::{
    DesiredRef, JobCache, JobState, LEASE_SECS, LeaseLost, LeasedJob, SKEW_SECS, desired_update, get_job, lease_job,
};
use rueca_storage::{MemoryStorage, Storage};

const NOW: u64 = 1_800_000_000;

fn repo(i: usize) -> String {
    format!("github.com/acme/r{i:02}")
}

/// `n` pending jobs, each wanting `main` at some commit.
fn seed(storage: &dyn Storage, n: usize) {
    for i in 0..n {
        desired_update(storage, &repo(i), None, NOW, |job| {
            job.set_desired("main", DesiredRef { commit: format!("{i:040x}"), kind: RefKind::Default, seen_at: NOW })
        })
        .unwrap();
    }
}

fn lease(storage: &dyn Storage, owner: &str, now: u64) -> Option<LeasedJob> {
    lease_job(storage, &mut JobCache::default(), owner, now).unwrap()
}

/// One indexer of the race: leases until nothing is left, recording each
/// `(repo, epoch)` it got.
fn indexer(storage: &dyn Storage, owner: &str, start: &Barrier, taken: &Mutex<Vec<(String, u64, String)>>) {
    let mut cache = JobCache::default();
    start.wait();
    while let Some(l) = lease_job(storage, &mut cache, owner, NOW).unwrap() {
        taken.lock().unwrap().push((l.repo().to_string(), l.epoch, owner.to_string()));
    }
}

#[test]
fn eight_indexers_lease_every_job_exactly_once_per_epoch() {
    let memory: Arc<dyn Storage> = Arc::new(MemoryStorage::new());
    seed(memory.as_ref(), 20);
    let storage: Arc<dyn Storage> = Arc::new(SlowCatalogs(memory.clone(), Duration::from_millis(1)));
    let start = Barrier::new(8);
    let taken = Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for t in 0..8 {
            let (storage, start, taken) = (storage.as_ref(), &start, &taken);
            s.spawn(move || indexer(storage, &format!("indexer-{t}/0000"), start, taken));
        }
    });
    let taken = taken.into_inner().unwrap();
    let pairs: BTreeSet<(String, u64)> = taken.iter().map(|(r, e, _)| (r.clone(), *e)).collect();
    assert_eq!(taken.len(), 20, "{taken:?}");
    assert_eq!(pairs.len(), 20, "a job was leased twice: {taken:?}");
    assert!(pairs.iter().all(|(_, e)| *e == 1));
    let mut per_owner: BTreeMap<&str, usize> = BTreeMap::new();
    for (_, _, o) in &taken {
        *per_owner.entry(o.as_str()).or_default() += 1;
    }
    eprintln!("lease race: 20 jobs, 8 indexers, every job leased once at epoch 1; per indexer {per_owner:?}");
    for i in 0..20 {
        let job = get_job(memory.as_ref(), &repo(i)).unwrap().unwrap();
        assert_eq!(job.state, JobState::Leased);
    }
    assert!(lease(memory.as_ref(), "late/0000", NOW).is_none(), "nothing left to lease");
}

#[test]
fn an_ended_lease_is_stolen_and_the_old_holder_writes_nothing() {
    let storage = MemoryStorage::new();
    seed(&storage, 1);
    let mut first = lease(&storage, "a/1", NOW).unwrap();
    assert_eq!(first.epoch, 1);
    // Within the lease and the skew: not leasable.
    assert!(lease(&storage, "b/2", NOW + LEASE_SECS + SKEW_SECS).is_none());
    // A renewal keeps it ours.
    first.renew(&storage, NOW + 60).unwrap();
    assert!(lease(&storage, "b/2", NOW + LEASE_SECS + SKEW_SECS + 1).is_none(), "renewed until NOW + 60 + lease");
    let stolen_at = NOW + 60 + LEASE_SECS + SKEW_SECS + 1;
    let second = lease(&storage, "b/2", stolen_at).unwrap();
    assert_eq!(second.epoch, 2);
    let err = first.renew(&storage, stolen_at + 1).unwrap_err();
    let lost = err.downcast_ref::<LeaseLost>().unwrap_or_else(|| panic!("{err:#}"));
    assert_eq!((lost.epoch, lost.owner.as_str()), (1, "a/1"));
    let indexed = BTreeMap::from([("main".to_string(), format!("{:040x}", 0))]);
    let err = first.clone().finish(&storage, &indexed, &BTreeMap::new(), stolen_at + 2).unwrap_err();
    assert!(err.downcast_ref::<LeaseLost>().is_some(), "{err:#}");
    assert!(first.fail(&storage, "index", "late", stolen_at + 2).unwrap_err().downcast_ref::<LeaseLost>().is_some());
    let job = get_job(&storage, &repo(0)).unwrap().unwrap();
    assert_eq!((job.state, job.lease.as_ref().unwrap().owner.as_str()), (JobState::Leased, "b/2"));
    eprintln!("steal: epoch 1 of a/1 ended, b/2 took epoch 2; a/1's renew, finish and fail were refused");
    let done = second.finish(&storage, &indexed, &BTreeMap::new(), stolen_at + 3).unwrap();
    assert_eq!(done.state, JobState::Done);
    assert_eq!(done.lease.unwrap().owner, "b/2", "the owner survives done");
}

#[test]
fn a_failed_job_backs_off_linearly_then_is_leased_again() {
    let storage = MemoryStorage::new();
    seed(&storage, 1);
    let mut now = NOW;
    let mut schedule = Vec::new();
    for attempt in 1..=13u64 {
        let l = lease(&storage, "a/1", now).unwrap_or_else(|| panic!("attempt {attempt} not leasable at {now}"));
        let job = l.fail(&storage, "index", "boom", now).unwrap();
        let wait = job.not_before.unwrap() - now;
        assert_eq!(wait, (attempt * 600).min(7200), "attempt {attempt}");
        assert!(lease(&storage, "a/1", now + wait - 1).is_none(), "backing off");
        schedule.push(wait / 60);
        now += wait;
    }
    eprintln!("back-off minutes: {schedule:?}");
    // Desired moved while failed: still waits for its back-off.
    desired_update(&storage, &repo(0), None, now, |job| {
        job.set_desired("main", DesiredRef { commit: "f".repeat(40), kind: RefKind::Default, seen_at: now })
    })
    .unwrap();
    let job = get_job(&storage, &repo(0)).unwrap().unwrap();
    assert_eq!((job.state, job.attempts), (JobState::Failed, 13));
    let l = lease(&storage, "a/1", now).unwrap();
    let indexed = BTreeMap::from([("main".to_string(), "f".repeat(40))]);
    let job = l.finish(&storage, &indexed, &BTreeMap::new(), now).unwrap();
    assert_eq!((job.state, job.attempts, job.not_before, job.last_error), (JobState::Done, 0, None, None));
}

#[test]
fn pending_jobs_lease_by_priority_then_age_and_failed_last() {
    let storage = MemoryStorage::new();
    let want = |kind| DesiredRef { commit: "a".repeat(40), kind, seen_at: NOW };
    for (i, kind) in [(0, RefKind::Tag), (1, RefKind::Pr), (2, RefKind::Default)] {
        desired_update(&storage, &repo(i), None, NOW + i as u64, |j| j.set_desired("x", want(kind))).unwrap();
    }
    let order: Vec<String> = (0..3).map(|_| lease(&storage, "a/1", NOW + 10).unwrap().repo().to_string()).collect();
    assert_eq!(order, [repo(2), repo(1), repo(0)]);
}
