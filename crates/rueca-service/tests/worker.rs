//! The `rueca index-worker` child process and [`ChildRunner`] (RFC 0008
//! §7.6–§7.8), against a local bare "host" over `file://`.
//!
//! This binary has no libtest harness (`harness = false`): invoked as
//! `<binary> index-worker` it *is* the worker ([`worker_main`]), so the
//! runner starts it exactly as `rueca serve` starts `rueca index-worker`.
//! Faults come from `RUECA_TEST_WORKER_FAULT`, which the worker honours in
//! debug builds only. Covered: a normal ref round-trips its `IndexReport`
//! (with a filtered-out blob as a `skipped:size` gap); a looping worker is
//! killed at the wall-clock limit and only its ref fails; a worker whose
//! allocation exceeds its address space aborts and fails cleanly (Linux:
//! `RLIMIT_AS`; macOS does not enforce it, so there an allocation no
//! system can satisfy stands in); `RLIMIT_CPU` stops a spinning worker;
//! the parent survives all of them.

mod common;

use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use common::{GitHost, git, open, run_tests};
use rueca_extract::Budget;
use rueca_segment::Store;
use rueca_service::indexer::{ChildRunner, RefRunner, RefTask, run_job};
use rueca_service::jobs::{DesiredRef, JobCache, desired_update, lease_job};
use rueca_service::remote::{GitAccess, HostPolicy};
use rueca_service::worker::{
    FAULT_ENV, WorkerCommand, WorkerLimits, WorkerRequest, address_space_enforced, handle, worker_main,
};
use rueca_service::{RefKind, Retention, ServiceStore};

const NOW: u64 = 1_800_000_000;
const MAX_BYTES: usize = 1000;
const INVOICES: (&str, &str) = ("billing/invoices.py", "def issue_invoice(order):\n    return order\n");

fn main() -> ExitCode {
    if std::env::args().nth(1).as_deref() == Some("index-worker") {
        return ExitCode::from(u8::try_from(worker_main()).unwrap_or(1));
    }
    run_tests(&[
        ("a_normal_ref_round_trips_its_report", a_normal_ref_round_trips_its_report),
        ("a_looping_ref_is_killed_at_the_wall_clock_and_fails_alone", a_looping_ref_is_killed_and_fails_alone),
        ("a_worker_over_its_address_space_fails_cleanly", a_worker_over_its_address_space_fails_cleanly),
        ("the_cpu_limit_stops_a_spinning_worker", the_cpu_limit_stops_a_spinning_worker),
        ("bad_requests_are_refused", bad_requests_are_refused),
    ])
}

/// A host whose `main` has a Python file and a blob over [`MAX_BYTES`],
/// with filters allowed (as hosted servers have them).
fn host() -> GitHost {
    let big = "x".repeat(MAX_BYTES + 200);
    let host = GitHost::new("billing", &[INVOICES, ("assets/big.txt", big.as_str())]);
    git(&host.bare, &["config", "uploadpack.allowFilter", "true"]);
    host
}

fn access() -> GitAccess {
    GitAccess { policy: HostPolicy { allow_file_urls: true, ..HostPolicy::default() }, ..GitAccess::default() }
}

fn runner(mirrors: &Path, limits: WorkerLimits, fault: Option<String>) -> ChildRunner {
    let mut worker = WorkerCommand::current_exe(limits).unwrap();
    worker.env.extend(fault.map(|f| (FAULT_ENV.to_string(), f)));
    let budget = Budget { max_bytes: MAX_BYTES, ..Budget::default() };
    let mut r = ChildRunner::new(mirrors.to_path_buf(), access(), budget, worker);
    r.threads = Some(2);
    r
}

fn limits(wall: Duration) -> WorkerLimits {
    WorkerLimits { wall_time: wall, ..WorkerLimits::default() }
}

fn task<'a>(host_url: &'a str, name: &'a str, commit: &'a str, store: &'a Path) -> RefTask<'a> {
    RefTask { repo: "local/billing", url: host_url, ref_name: name, commit, kind: RefKind::Default, store }
}

fn a_normal_ref_round_trips_its_report() {
    let host = host();
    let (mirrors, staging) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let url = host.url();
    let commit = host.commit("main");
    let r = runner(mirrors.path(), limits(Duration::from_secs(120)), None);
    let run = r.run_ref(&task(&url, "main", &commit, staging.path())).unwrap();
    let stages: Vec<&str> = run.stages.iter().map(|(s, _)| *s).collect();
    assert_eq!(stages, ["fetch", "export", "index"]);
    let seg = Store::open(staging.path()).unwrap().open_segment(&run.segment).unwrap();
    assert_eq!(seg.meta().snapshot.commit, commit);
    let gap = seg.files().iter().find(|f| f.path == "assets/big.txt").expect("the filtered blob is listed");
    assert_eq!((gap.status.as_str(), gap.size), ("skipped:size", MAX_BYTES as u64 + 1));
    assert!(seg.files().iter().any(|f| f.path == INVOICES.0 && f.status == "ok"));
    let export = rueca_index::refs::export_dir(staging.path(), "local/billing", "", "main");
    assert!(!export.exists(), "the export is scratch");

    // The report itself round-trips through the protocol.
    let dest = tempfile::tempdir().unwrap();
    let mirror = mirrors.path().join(format!("{}.git", rueca_service::repo_dir("local/billing", None)));
    let exported = rueca_index::refs::export_with(&mirror, &commit, &dest.path().join("x"), Some(1001)).unwrap();
    let options = rueca_index::IndexOptions {
        root: dest.path().join("x"),
        store: staging.path().to_path_buf(),
        repo: Some("local/billing".into()),
        ref_name: Some("main".into()),
        commit: Some(commit.clone()),
        threads: Some(1),
        budget: Budget { max_bytes: MAX_BYTES, ..Budget::default() },
    };
    let cmd = WorkerCommand::current_exe(WorkerLimits::default()).unwrap();
    let report = rueca_service::worker::run_ref(&cmd, &WorkerRequest::new(options, exported.absent)).unwrap();
    assert_eq!((report.files_total, report.files_skipped, report.files_extracted), (2, 1, 1));
    assert_eq!(report.segment.snapshot.commit, commit);
    assert!(Store::open(staging.path()).unwrap().open_segment(&report.segment.id).is_ok());
}

/// A job with `main` (default) and `feature`, whose worker loops.
fn two_ref_job(host: &GitHost, staging: &ServiceStore) {
    host.push_ref("refs/heads/feature");
    let commit = host.commit("main");
    let url = host.url();
    desired_update(staging.storage.as_ref(), "local/billing", Some(&url), NOW, |job| {
        job.set_desired("main", DesiredRef { commit: commit.clone(), kind: RefKind::Default, seen_at: NOW });
        job.set_desired("feature", DesiredRef { commit: commit.clone(), kind: RefKind::Branch, seen_at: NOW })
    })
    .unwrap();
}

fn a_looping_ref_is_killed_and_fails_alone() {
    let host = host();
    let (mirrors, sdir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let staging = open("memory://worker-loop", sdir.path());
    two_ref_job(&host, &staging);
    let wall = Duration::from_secs(2);
    let r = runner(mirrors.path(), limits(wall), Some("loop:feature".into()));
    let leased = lease_job(staging.storage.as_ref(), &mut JobCache::default(), "indexer/1", NOW + 1).unwrap().unwrap();
    let started = Instant::now();
    let run = run_job(&staging, leased, &r, &Retention::default(), &|| NOW + 1).unwrap();
    let took = started.elapsed();
    let results: Vec<(&str, &str)> = run.refs.iter().map(|o| (o.ref_name.as_str(), o.result)).collect();
    assert_eq!(results, [("main", "indexed"), ("feature", "failed")]);
    let reason = run.refs[1].error.clone().unwrap();
    assert!(reason.contains("wall-clock limit of 2s") && reason.contains("killed"), "{reason}");
    assert_eq!(run.result, "done", "a ref that fails alone does not fail the job");
    assert!(took >= wall && took < wall + Duration::from_secs(60), "{took:?}");
    let job = run.job.unwrap();
    assert!(job.ref_backing_off("feature", &host.commit("main"), NOW + 2), "the looping ref backs off alone");
    eprintln!("looping worker: killed after {took:?} (wall-clock limit {wall:?}); `main` published");
}

fn a_worker_over_its_address_space_fails_cleanly() {
    let host = host();
    let (mirrors, staging) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let url = host.url();
    let commit = host.commit("main");
    // Linux: 2 GiB of address space, an 8 GiB allocation. macOS: the
    // limit is not set; an allocation of 64 TiB fails anyway.
    let (space, bytes): (u64, u64) = if address_space_enforced() { (2 << 30, 8 << 30) } else { (2 << 30, 1 << 46) };
    let lim = WorkerLimits { address_space: Some(space), ..limits(Duration::from_secs(120)) };
    let r = runner(mirrors.path(), lim, Some(format!("alloc:main:{bytes}")));
    let err = r.run_ref(&task(&url, "main", &commit, staging.path())).unwrap_err();
    assert_eq!(err.stage, "index");
    // The worker's stderr tail names the allocation unless a backtrace
    // (`RUST_BACKTRACE=1`, as in CI) pushed that line out of the tail.
    let named =
        err.reason.contains(&format!("allocation of {bytes} bytes")) || err.reason.contains("allocation failed");
    assert!(err.reason.contains("SIGABRT") && named, "{err}");
    eprintln!("address space: {err}");
    // The parent survives and the next ref runs.
    let ok = runner(mirrors.path(), limits(Duration::from_secs(120)), None);
    assert!(ok.run_ref(&task(&url, "main", &commit, staging.path())).is_ok());
}

fn the_cpu_limit_stops_a_spinning_worker() {
    let host = host();
    let (mirrors, staging) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let url = host.url();
    let commit = host.commit("main");
    let lim = WorkerLimits { cpu_time: Some(Duration::from_secs(1)), ..limits(Duration::from_secs(60)) };
    let r = runner(mirrors.path(), lim, Some("loop:main".into()));
    let started = Instant::now();
    let err = r.run_ref(&task(&url, "main", &commit, staging.path())).unwrap_err();
    assert!(err.reason.contains("CPU time limit of 1s"), "{err}");
    assert!(started.elapsed() < Duration::from_secs(30), "stopped by the CPU limit, not the wall clock");
}

fn bad_requests_are_refused() {
    let (_, code) = handle("{}");
    assert_eq!(code, 2);
    let req = WorkerRequest::new(rueca_index::IndexOptions::default(), Vec::new());
    let mut text = serde_json::to_value(&req).unwrap();
    text["version"] = serde_json::json!(9);
    let (resp, code) = handle(&text.to_string());
    assert_eq!(code, 2);
    assert!(serde_json::to_string(&resp).unwrap().contains(r#""stage":"request""#));
    // A missing program is a clean failure too.
    let cmd = WorkerCommand {
        program: "/nonexistent/rueca".into(),
        args: vec!["index-worker".into()],
        env: Vec::new(),
        limits: WorkerLimits::default(),
    };
    let err = rueca_service::worker::run_ref(&cmd, &req).unwrap_err();
    assert!(err.reason.contains("cannot start"), "{err}");
}
