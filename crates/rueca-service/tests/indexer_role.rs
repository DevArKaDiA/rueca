//! The indexer role in process (RFC 0008 §7): a local bare repository as
//! the code host (`file://`, allowed by the test-only policy flag), the
//! reconciler polling it, an indexer leasing and running its job with the
//! in-process stub runner, the updater publishing generations, two updaters
//! electing one leader, and idle expiry under an injected clock.

mod common;

use std::collections::BTreeMap;
use std::sync::{Arc, Barrier};
use std::time::Duration;

use common::{GitHost, InProcessRunner, commit, open, write};
use rueca_service::indexer::{RefError, RefRun, RefRunner, RefTask, run_job};
use rueca_service::jobs::{DesiredRef, JobCache, JobState, desired_update, get_job, lease_job};
use rueca_service::reads::write_reads;
use rueca_service::reconcile::{PollPolicy, Reconciler, RepoSpec, read_catalog};
use rueca_service::remote::{GitAccess, HostPolicy};
use rueca_service::updater::{UPDATER_LEASE_SECS, UPDATER_SKEW_SECS, Updater, UpdaterOptions};
use rueca_service::{PullWhich, RefKind, Retention, ServiceStore, publish_snapshot, pull_generation, pull_repo};

const NOW: u64 = 1_800_000_000;
const DAY: u64 = 24 * 3600;

const INVOICES: (&str, &str) = ("billing/invoices.py", "def issue_invoice(order):\n    return order\n");
const REFUNDS: (&str, &str) = ("billing/refunds.py", "def refund_payment(p):\n    return p\n");

fn access() -> GitAccess {
    GitAccess { policy: HostPolicy { allow_file_urls: true, ..HostPolicy::default() }, ..GitAccess::default() }
}

fn spec(host: &GitHost) -> RepoSpec {
    RepoSpec::new(&host.url(), None, &["release/*".to_string()], &["v*".to_string()], false).unwrap()
}

fn updater(url: &str, dir: &std::path::Path, owner: &str, specs: Vec<RepoSpec>) -> Updater {
    let reconciler = Reconciler::new(specs, PollPolicy::default(), Arc::new(access()));
    let options = UpdaterOptions {
        owner: owner.to_string(),
        retention: Retention::default(),
        housekeeping_every: Duration::from_secs(3600),
    };
    Updater::new(open(url, dir), reconciler, options)
}

fn runner(scratch: &std::path::Path) -> InProcessRunner {
    InProcessRunner { scratch: scratch.to_path_buf(), git: access(), budget: Default::default(), threads: Some(2) }
}

/// Leases the next job and runs it with `runner`.
fn index_next(staging: &ServiceStore, runner: &dyn RefRunner, now: u64) -> rueca_service::indexer::JobRun {
    let leased = lease_job(staging.storage.as_ref(), &mut JobCache::default(), "indexer-1/aaaa", now)
        .unwrap()
        .expect("a leasable job");
    run_job(staging, leased, runner, &Retention::default(), &|| now).unwrap()
}

fn refs_of(desired: &BTreeMap<String, DesiredRef>) -> Vec<String> {
    desired.iter().map(|(n, d)| format!("{n}:{}", d.kind.as_str())).collect()
}

/// A host with `main`, `release/1`, `feature` and the annotated tag `v1`.
fn billing_host() -> GitHost {
    let host = GitHost::new("billing", &[INVOICES]);
    host.push_ref("refs/heads/release/1");
    host.push_ref("refs/heads/feature");
    common::git(&host.src, &["tag", "-a", "v1", "-m", "v1"]);
    let bare = host.bare.to_string_lossy().into_owned();
    common::git(&host.src, &["push", "--quiet", &bare, "v1"]);
    host
}

/// The order and result of a run's refs.
fn order(run: &rueca_service::indexer::JobRun) -> Vec<(&str, &str)> {
    run.refs.iter().map(|o| (o.ref_name.as_str(), o.result)).collect()
}

/// First round: reconcile, index, publish generation 1, serve it.
fn first_round(host: &GitHost, up: &mut Updater, staging: &ServiceStore, runner: &dyn RefRunner, url: &str) {
    // Reconcile: the job wants main, release/1 and v1 (feature is outside the globs).
    let r = up.tick(NOW).unwrap();
    assert!(r.leader && r.reconcile.as_ref().unwrap().polled == 1, "{r:?}");
    let repo = spec(host).repo;
    let job = get_job(staging.storage.as_ref(), &repo).unwrap().unwrap();
    assert_eq!(refs_of(&job.desired), ["main:default", "release/1:branch", "v1:tag"]);
    assert_eq!((job.state, job.priority), (JobState::Pending, 0), "never indexed");
    assert_eq!(job.desired["v1"].commit, host.commit("main"), "the annotated tag is peeled");
    assert_eq!(job.poll.unwrap().next_at, NOW + 45);
    assert_eq!(read_catalog(staging.storage.as_ref(), &repo).unwrap().unwrap().url, Some(host.url()));
    // The indexer runs the job, default first.
    let run = index_next(staging, runner, NOW + 1);
    assert_eq!(order(&run), [("main", "indexed"), ("release/1", "indexed"), ("v1", "indexed")]);
    assert_eq!(run.result, "done");
    // The updater pulls it and publishes generation 1; a replica serves it.
    let r = up.tick(NOW + 5).unwrap();
    assert_eq!(r.generation.as_ref().map(|g| g.generation), Some(1), "{r:?}");
    let rdir = tempfile::tempdir().unwrap();
    let replica = open(url, rdir.path());
    pull_generation(&replica, None).unwrap().unwrap();
    let engine = rueca_query::Engine::for_org(replica.local.dir.clone(), "acme").unwrap();
    let hits = engine.respond("search_symbols", serde_json::json!({"query": "issue invoice"})).unwrap();
    assert!(hits.text.contains("issue_invoice"), "{}", hits.text);
    let listed = engine.respond("list_repositories", serde_json::json!({})).unwrap();
    assert!(listed.text.contains("release/1") && listed.text.contains("v1"), "{}", listed.text);
}

/// Second round: a push and a deleted branch, seen by the next poll.
fn second_round(host: &GitHost, up: &mut Updater, staging: &ServiceStore, runner: &dyn RefRunner) {
    let repo = spec(host).repo;
    let moved = host.push(&[REFUNDS]);
    host.delete_ref("refs/heads/release/1");
    assert_eq!(up.tick(NOW + 10).unwrap().reconcile.unwrap().polled, 0, "not due yet");
    let r = up.tick(NOW + 46).unwrap();
    assert_eq!(r.reconcile.as_ref().unwrap().removed, [format!("{repo}@release/1")]);
    assert_eq!(r.generation.map(|g| g.generation), Some(2), "the removed branch leaves the organization");
    let job = get_job(staging.storage.as_ref(), &repo).unwrap().unwrap();
    assert_eq!(refs_of(&job.desired), ["main:default", "v1:tag"]);
    assert_eq!((job.state, job.priority, job.desired["main"].commit.as_str()), (JobState::Pending, 1, moved.as_str()));
    let catalog = read_catalog(staging.storage.as_ref(), &repo).unwrap().unwrap();
    assert!(catalog.removed.iter().any(|r| r.ref_name == "release/1"));
    let run = index_next(staging, runner, NOW + 47);
    assert_eq!(order(&run), [("main", "indexed"), ("v1", "present")]);
    assert_eq!(up.tick(NOW + 50).unwrap().generation.map(|g| g.generation), Some(3));
    let job = get_job(staging.storage.as_ref(), &repo).unwrap().unwrap();
    assert_eq!(job.poll.unwrap().changed_at, NOW + 46);
}

#[test]
fn polled_refs_are_indexed_published_and_served() {
    let host = billing_host();
    let url = "memory://indexer-role-e2e";
    let (udir, sdir, scratch) =
        (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut up = updater(url, udir.path(), "indexer-1/aaaa", vec![spec(&host)]);
    let staging = open(url, sdir.path());
    let runner = runner(scratch.path());
    first_round(&host, &mut up, &staging, &runner, url);
    second_round(&host, &mut up, &staging, &runner);
}

/// Fails every ref at the `index` stage.
struct Broken;

impl RefRunner for Broken {
    fn run_ref(&self, _: &RefTask<'_>) -> Result<RefRun, RefError> {
        Err(RefError { stage: "index".into(), reason: "the parser looped".into() })
    }
}

#[test]
fn a_failing_job_backs_off_and_is_retried_after_not_before() {
    let host = GitHost::new("payments", &[INVOICES]);
    let url = "memory://indexer-role-backoff";
    let (udir, sdir, scratch) =
        (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut up = updater(url, udir.path(), "indexer-1/aaaa", vec![spec(&host)]);
    let staging = open(url, sdir.path());
    up.tick(NOW).unwrap();
    let run = index_next(&staging, &Broken, NOW + 1);
    let job = run.job.unwrap();
    assert_eq!((run.result, job.attempts, job.not_before), ("failed", 1, Some(NOW + 1 + 600)));
    assert_eq!(job.last_error.unwrap().reason, "the parser looped");
    let none = lease_job(staging.storage.as_ref(), &mut JobCache::default(), "i/1", NOW + 600).unwrap();
    assert!(none.is_none(), "backing off");
    let run = index_next(&staging, &runner(scratch.path()), NOW + 601);
    assert_eq!((run.result, run.epoch), ("done", 2));
    assert_eq!(get_job(staging.storage.as_ref(), &spec(&host).repo).unwrap().unwrap().attempts, 0);
}

#[test]
fn two_updaters_elect_one_leader_and_hand_over_after_expiry() {
    let url = "memory://indexer-role-two-updaters";
    let dirs: Vec<_> = (0..2).map(|_| tempfile::tempdir().unwrap()).collect();
    let mut ups: Vec<Updater> =
        dirs.iter().enumerate().map(|(i, d)| updater(url, d.path(), &format!("indexer-{i}/abcd"), vec![])).collect();
    let start = Barrier::new(2);
    let leaders: Vec<bool> = std::thread::scope(|s| {
        let handles: Vec<_> = ups
            .iter_mut()
            .map(|u| {
                let start = &start;
                s.spawn(move || {
                    start.wait();
                    u.tick(NOW).unwrap().leader
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert_eq!(leaders.iter().filter(|l| **l).count(), 1, "{leaders:?}");
    let (lead, follow) = if leaders[0] { (0, 1) } else { (1, 0) };
    // The leader renews; the follower stays out.
    assert!(ups[lead].tick(NOW + 25).unwrap().leader);
    let until = ups[lead].lease().unwrap().until;
    assert_eq!(until, NOW + 25 + UPDATER_LEASE_SECS);
    assert!(!ups[follow].tick(NOW + 50).unwrap().leader);
    assert!(!ups[follow].tick(until + UPDATER_SKEW_SECS).unwrap().leader, "within the skew");
    // The leader stops; past the skew the follower takes over.
    let r = ups[follow].tick(until + UPDATER_SKEW_SECS + 1).unwrap();
    assert!(r.leader);
    assert_eq!(r.epoch, Some(2));
    assert!(!ups[lead].tick(until + UPDATER_SKEW_SECS + 2).unwrap().leader, "the old leader lost it");
    eprintln!("updaters: indexer-{lead} led epoch 1, indexer-{follow} took epoch 2 after expiry");
}

#[test]
fn idle_pull_requests_and_commits_expire_by_reads() {
    let url = "memory://indexer-role-idle";
    let (idir, udir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let indexer = open(url, idir.path());
    let repo = "github.com/acme/billing";
    let refs =
        [("main", RefKind::Default), ("pr/1", RefKind::Pr), ("pr/2", RefKind::Pr), ("0123456789ab", RefKind::Commit)];
    for (i, (name, kind)) in refs.iter().enumerate() {
        let meta = write(idir.path(), repo, &commit(i + 1), &[INVOICES]);
        publish_snapshot(&indexer, repo, name, *kind, &meta.id, &Retention::default()).unwrap();
        desired_update(indexer.storage.as_ref(), repo, None, NOW, |job| {
            job.set_desired(name, DesiredRef { commit: commit(i + 1), kind: *kind, seen_at: NOW })
        })
        .unwrap();
    }
    // Snapshots were indexed at t0 (the helper's fixed time); pr/2 is read on day 10.
    let t0 = 1_700_000_000;
    let hour = (t0 + 10 * DAY) / 3600 * 3600;
    let pairs = [(repo.to_string(), "pr/2".to_string())].into_iter().collect();
    write_reads(indexer.storage.as_ref(), "replica-1", hour, &pairs).unwrap();
    let mut up = updater(url, udir.path(), "indexer-1/aaaa", vec![]);
    let at = t0 + 14 * DAY + 2 * 3600;
    let expired = up.expire_idle(at).unwrap();
    assert_eq!(expired, [format!("{repo}@0123456789ab"), format!("{repo}@pr/1")]);
    let catalog = read_catalog(indexer.storage.as_ref(), repo).unwrap().unwrap();
    assert_eq!(catalog.refs.keys().collect::<Vec<_>>(), ["main", "pr/2"]);
    let job = get_job(indexer.storage.as_ref(), repo).unwrap().unwrap();
    assert!(!job.desired.contains_key("pr/1") && job.expired["pr/1"] == commit(2));
    // Read evidence keeps pr/2 until 14 days after its last read.
    assert!(up.expire_idle(hour + 3600 + 14 * DAY - 1).unwrap().is_empty());
    assert_eq!(up.expire_idle(hour + 3600 + 14 * DAY + 1).unwrap(), [format!("{repo}@pr/2")]);
    // pr_idle = 0 disables idle expiry.
    up.options.retention.pr_idle = Duration::ZERO;
    assert!(up.expire_idle(at + 100 * DAY).unwrap().is_empty());
    // The updater's mirror drops expired refs on its next pull.
    let pulled = pull_repo(&up.svc, repo, PullWhich::All).unwrap();
    assert_eq!(pulled.refs.iter().map(|r| r.ref_name.as_str()).collect::<Vec<_>>(), ["main"]);
    eprintln!("idle expiry: unread pr/1 and the on-demand commit expired at t0+14d; pr/2 (read on day 10) at day 24");
}
