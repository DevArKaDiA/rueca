//! The `rueca index-worker` child process (RFC 0008 §7.8): one ref's
//! index run isolated from the indexer, so a crash, an abort in C code, an
//! allocation storm or a loop that ignores its budget fails that ref alone.
//!
//! # Protocol (version 1)
//!
//! - **stdin**: one JSON object, [`WorkerRequest`]:
//!   `{"version": 1, "options": {"root", "store", "repo", "ref_name",
//!   "commit", "threads"}, "budget": {"max_bytes", "max_parse_time_ms",
//!   "max_depth"}, "absent": [{"path", "blob_sha", "size"}]}`. `options`
//!   is [`rueca_index::IndexOptions`]; the budget travels apart because
//!   `IndexOptions::budget` is not serialized; `absent` (optional) lists the
//!   blobs the mirror's filter left out ([`rueca_index::AbsentFile`]),
//!   indexed as `skipped:size` gaps.
//! - **stdout**: one JSON object, [`WorkerResponse`]: `{"ok":
//!   <IndexReport>}` or `{"error": {"stage", "message"}}` (`stage`:
//!   `request` for unreadable input, `index` otherwise).
//! - **stderr**: log lines (JSON objects when the binary installs the JSON
//!   formatter, as `rueca index-worker` does); the parent forwards each to
//!   its own log with the ref, and keeps the last lines for the failure
//!   reason (a panic or an allocation failure prints plain text there).
//! - **exit status**: 0 with `ok`, 1 with `error`, 2 for a request that
//!   cannot be read.
//!
//! # The parent
//!
//! [`run_ref`] spawns [`WorkerCommand::program`] (the indexer's own binary,
//! `rueca index-worker`) with, on Unix, resource limits set between `fork`
//! and `exec` (`setrlimit` through `libc`): `RLIMIT_AS` =
//! [`WorkerLimits::address_space`] (`[indexing].worker.address_space`, 24 GiB
//! by default), `RLIMIT_CPU` = [`WorkerLimits::cpu_time`] (`cpu_time`, 4 h,
//! summed over threads; the hard limit 10 s later turns an ignored
//! `SIGXCPU` into `SIGKILL`) and `RLIMIT_CORE` = 0. Each limit is clamped to
//! the parent's own hard limit. macOS refuses an address-space limit below
//! what a forked process already maps and does not enforce it, so there the
//! limit is not set ([`address_space_enforced`]); containers cap memory
//! with the cgroup limit anyway (its kill shows as `SIGKILL`). A wall-clock
//! timer ([`WorkerLimits::wall_time`], `wall_time`, 30 min) sends `SIGKILL`.
//! The request is written and both pipes are read on threads of their own,
//! so a child that stops reading or writing never blocks the parent.
//!
//! Exit 0 with `ok` is the [`rueca_index::IndexReport`]; anything else — an
//! `error` object, another exit status, a signal, the timeout — is a
//! [`RefError`] naming the cause and the last stderr lines. The parent never
//! dies with a worker.
//!
//! # Fault injection (debug builds only)
//!
//! `RUECA_TEST_WORKER_FAULT` ([`FAULT_ENV`]) is read by the worker only
//! when compiled with `debug_assertions` (tests and `cargo build` without
//! `--release`; release binaries ignore it): `loop:<ref>` makes the worker
//! spin forever when the request's `ref_name` is `<ref>` (a parser that
//! ignores its budget), `alloc:<ref>:<bytes>` makes it allocate `<bytes>`
//! (an allocation storm: with `RLIMIT_AS` below it, the allocation fails and
//! the worker aborts).

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use rueca_extract::Budget;
use rueca_index::{AbsentFile, IndexOptions, IndexReport};
use serde::{Deserialize, Serialize};

use crate::indexer::RefError;

/// Version of the stdin/stdout protocol.
pub const WORKER_PROTOCOL: u32 = 1;

/// The fault-injection variable (module docs; debug builds only).
pub const FAULT_ENV: &str = "RUECA_TEST_WORKER_FAULT";

/// stdout read from a worker, at most (an [`IndexReport`] is a few KiB).
const MAX_STDOUT: u64 = 16 << 20;
/// stderr lines kept for the failure reason.
const TAIL_LINES: usize = 8;
/// Characters kept of one stderr line.
const TAIL_LINE_CHARS: usize = 400;
/// How often the parent checks whether the worker ended.
const POLL: Duration = Duration::from_millis(10);

/// [`Budget`] as the protocol carries it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerBudget {
    pub max_bytes: u64,
    pub max_parse_time_ms: u64,
    pub max_depth: u64,
}

impl From<&Budget> for WorkerBudget {
    fn from(b: &Budget) -> WorkerBudget {
        WorkerBudget {
            max_bytes: b.max_bytes as u64,
            max_parse_time_ms: u64::try_from(b.max_parse_time.as_millis()).unwrap_or(u64::MAX),
            max_depth: b.max_depth as u64,
        }
    }
}

impl From<&WorkerBudget> for Budget {
    fn from(b: &WorkerBudget) -> Budget {
        Budget {
            max_bytes: usize::try_from(b.max_bytes).unwrap_or(usize::MAX),
            max_parse_time: Duration::from_millis(b.max_parse_time_ms),
            max_depth: usize::try_from(b.max_depth).unwrap_or(usize::MAX),
        }
    }
}

/// What the parent writes to the worker's stdin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerRequest {
    pub version: u32,
    pub options: IndexOptions,
    pub budget: WorkerBudget,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub absent: Vec<AbsentFile>,
}

impl WorkerRequest {
    /// A version-1 request for `options` (its budget included).
    pub fn new(options: IndexOptions, absent: Vec<AbsentFile>) -> WorkerRequest {
        let budget = WorkerBudget::from(&options.budget);
        WorkerRequest { version: WORKER_PROTOCOL, options, budget, absent }
    }
}

/// Why a worker failed, as it reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerError {
    pub stage: String,
    pub message: String,
}

/// What the worker writes to stdout.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkerResponse {
    Ok(Box<IndexReport>),
    Error(WorkerError),
}

// ---------------------------------------------------------------------------
// The child
// ---------------------------------------------------------------------------

/// The worker's whole run: reads the request from stdin, indexes, writes
/// the response to stdout; returns the exit status (module docs). The
/// binary installs its logger first.
pub fn worker_main() -> i32 {
    let mut input = String::new();
    let (response, code) = match std::io::stdin().read_to_string(&mut input) {
        Ok(_) => handle(&input),
        Err(e) => (failure("request", format!("reading stdin: {e}")), 2),
    };
    let text = serde_json::to_string(&response)
        .unwrap_or_else(|e| format!(r#"{{"error":{{"stage":"index","message":"cannot encode the report: {e}"}}}}"#));
    let mut out = std::io::stdout().lock();
    if writeln!(out, "{text}").and_then(|()| out.flush()).is_err() {
        return 1;
    }
    code
}

/// Runs one request: the response and the exit status.
pub fn handle(input: &str) -> (WorkerResponse, i32) {
    let req: WorkerRequest = match serde_json::from_str(input) {
        Ok(r) => r,
        Err(e) => return (failure("request", format!("not a worker request: {e}")), 2),
    };
    if req.version != WORKER_PROTOCOL {
        let msg = format!("protocol version {} (this worker speaks {WORKER_PROTOCOL})", req.version);
        return (failure("request", msg), 2);
    }
    let mut options = req.options;
    options.budget = Budget::from(&req.budget);
    test_fault(options.ref_name.as_deref());
    match rueca_index::index_worktree_with(&options, &req.absent) {
        Ok(report) => (WorkerResponse::Ok(Box::new(report)), 0),
        Err(e) => (failure("index", format!("{e:#}")), 1),
    }
}

fn failure(stage: &str, message: String) -> WorkerResponse {
    WorkerResponse::Error(WorkerError { stage: stage.to_string(), message })
}

/// The fault [`FAULT_ENV`] asks for when it names `ref_name` (debug
/// builds only; module docs).
#[cfg(debug_assertions)]
fn test_fault(ref_name: Option<&str>) {
    let Ok(spec) = std::env::var(FAULT_ENV) else { return };
    let mut parts = spec.splitn(3, ':');
    let (Some(mode), Some(target)) = (parts.next(), parts.next()) else { return };
    if ref_name != Some(target) {
        return;
    }
    match mode {
        "loop" => spin_forever(),
        "alloc" => allocate(parts.next().and_then(|n| n.parse().ok()).unwrap_or(1 << 40)),
        _ => {}
    }
}

#[cfg(not(debug_assertions))]
fn test_fault(_ref_name: Option<&str>) {}

/// A loop that ignores every budget.
#[cfg(debug_assertions)]
fn spin_forever() -> ! {
    tracing::warn!("{FAULT_ENV}: spinning forever");
    let mut n = 0u64;
    loop {
        n = std::hint::black_box(n.wrapping_add(1));
    }
}

/// Allocates and touches `bytes` (aborts when the allocation fails).
#[cfg(debug_assertions)]
fn allocate(bytes: usize) {
    tracing::warn!(bytes, "{FAULT_ENV}: allocating");
    let mut v: Vec<u8> = Vec::new();
    v.reserve_exact(bytes);
    v.resize(bytes, 1);
    std::hint::black_box(&v);
}

// ---------------------------------------------------------------------------
// The parent
// ---------------------------------------------------------------------------

/// Resource limits of a worker (module docs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerLimits {
    /// `RLIMIT_AS` in bytes (`None`: unlimited).
    pub address_space: Option<u64>,
    /// `RLIMIT_CPU` (`None`: unlimited).
    pub cpu_time: Option<Duration>,
    /// Wall-clock time before `SIGKILL`.
    pub wall_time: Duration,
}

impl Default for WorkerLimits {
    fn default() -> WorkerLimits {
        WorkerLimits {
            address_space: Some(24 << 30),
            cpu_time: Some(Duration::from_secs(4 * 3600)),
            wall_time: Duration::from_secs(30 * 60),
        }
    }
}

/// Whether this platform enforces [`WorkerLimits::address_space`] (not on
/// macOS: see the module docs).
pub fn address_space_enforced() -> bool {
    cfg!(unix) && !cfg!(target_os = "macos")
}

/// How to start a worker.
#[derive(Debug, Clone)]
pub struct WorkerCommand {
    /// The executable (`rueca`).
    pub program: PathBuf,
    /// Its arguments (`["index-worker"]`).
    pub args: Vec<String>,
    /// Added to the inherited environment.
    pub env: Vec<(String, String)>,
    pub limits: WorkerLimits,
}

impl WorkerCommand {
    /// `<this executable> index-worker`.
    pub fn current_exe(limits: WorkerLimits) -> anyhow::Result<WorkerCommand> {
        let program = std::env::current_exe()
            .map_err(|e| anyhow::anyhow!("cannot locate this executable for `index-worker`: {e}"))?;
        Ok(WorkerCommand { program, args: vec!["index-worker".to_string()], env: Vec::new(), limits })
    }
}

/// How a worker ended.
enum Ended {
    Exited(ExitStatus),
    TimedOut,
}

/// Runs `request` in a worker started by `cmd` (module docs): its
/// [`IndexReport`], or why it failed.
pub fn run_ref(cmd: &WorkerCommand, request: &WorkerRequest) -> Result<IndexReport, RefError> {
    let input = serde_json::to_vec(request).map_err(|e| index_error(format!("encoding the request: {e}")))?;
    let started = Instant::now();
    let mut child = spawn(cmd).map_err(|e| index_error(format!("cannot start {}: {e}", cmd.program.display())))?;
    let ref_name = request.options.ref_name.clone().unwrap_or_default();
    let stdin = child.stdin.take();
    let writer = std::thread::spawn(move || {
        if let Some(mut s) = stdin {
            // A worker that died before reading closes the pipe: its exit
            // status says why.
            let _ = s.write_all(&input);
        }
    });
    let stdout = child.stdout.take();
    let reader = std::thread::spawn(move || read_stdout(stdout));
    let stderr = child.stderr.take();
    let logs = std::thread::spawn(move || forward_stderr(stderr, &ref_name));
    let ended = wait(&mut child, cmd.limits.wall_time);
    let _ = writer.join();
    let out = reader.join().unwrap_or_default();
    let tail = logs.join().unwrap_or_default();
    tracing::debug!(seconds = started.elapsed().as_secs_f64(), "index worker ended");
    interpret(ended, &out, &tail, &cmd.limits)
}

fn index_error(reason: String) -> RefError {
    RefError { stage: "index".to_string(), reason }
}

fn spawn(cmd: &WorkerCommand) -> std::io::Result<Child> {
    let mut c = Command::new(&cmd.program);
    c.args(&cmd.args)
        .envs(cmd.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    rlimits::apply(&mut c, &cmd.limits);
    c.spawn()
}

/// Waits for the child, killing it after `wall`.
fn wait(child: &mut Child, wall: Duration) -> Ended {
    let deadline = Instant::now() + wall;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ended::Exited(status),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Ended::TimedOut;
            }
            Ok(None) => std::thread::sleep(POLL),
            Err(e) => {
                tracing::warn!(error = %e, "waiting for the index worker failed; killing it");
                let _ = child.kill();
                return child.wait().map_or(Ended::TimedOut, Ended::Exited);
            }
        }
    }
}

fn read_stdout(stdout: Option<std::process::ChildStdout>) -> Vec<u8> {
    let mut out = Vec::new();
    if let Some(s) = stdout {
        let _ = s.take(MAX_STDOUT).read_to_end(&mut out);
    }
    out
}

/// Forwards every stderr line to this process's log; returns the last
/// [`TAIL_LINES`].
fn forward_stderr(stderr: Option<std::process::ChildStderr>, ref_name: &str) -> Vec<String> {
    let mut tail = VecDeque::with_capacity(TAIL_LINES);
    let Some(s) = stderr else { return Vec::new() };
    for line in BufReader::new(s).lines().map_while(Result::ok) {
        if line.trim().is_empty() {
            continue;
        }
        forward_line(&line, ref_name);
        if tail.len() == TAIL_LINES {
            tail.pop_front();
        }
        tail.push_back(line.chars().take(TAIL_LINE_CHARS).collect());
    }
    tail.into()
}

/// One worker log line at its own level (`level` of a JSON line; plain
/// text, such as a panic message, at `warn`).
fn forward_line(line: &str, ref_name: &str) {
    let parsed: Option<serde_json::Value> = serde_json::from_str(line).ok();
    let level = parsed.as_ref().and_then(|v| v["level"].as_str()).unwrap_or("WARN").to_ascii_uppercase();
    let message = parsed.as_ref().and_then(|v| v["message"].as_str()).unwrap_or(line);
    match level.as_str() {
        "ERROR" => tracing::error!(worker = true, r#ref = ref_name, log = line, "{message}"),
        "WARN" => tracing::warn!(worker = true, r#ref = ref_name, log = line, "{message}"),
        "INFO" => tracing::info!(worker = true, r#ref = ref_name, log = line, "{message}"),
        _ => tracing::debug!(worker = true, r#ref = ref_name, log = line, "{message}"),
    }
}

/// The outcome of a worker run.
fn interpret(ended: Ended, out: &[u8], tail: &[String], limits: &WorkerLimits) -> Result<IndexReport, RefError> {
    let status = match ended {
        Ended::TimedOut => {
            let wall = limits.wall_time;
            return Err(with_tail(
                format!("the index worker ran past the wall-clock limit of {wall:?} and was killed"),
                tail,
            ));
        }
        Ended::Exited(s) => s,
    };
    let response = std::str::from_utf8(out)
        .ok()
        .and_then(|t| t.lines().rev().find(|l| l.trim_start().starts_with('{')))
        .and_then(|l| serde_json::from_str::<WorkerResponse>(l).ok());
    match (status.success(), response) {
        (true, Some(WorkerResponse::Ok(report))) => Ok(*report),
        (_, Some(WorkerResponse::Error(e))) => Err(RefError { stage: e.stage, reason: e.message }),
        (true, _) => Err(with_tail("the index worker exited without a report".to_string(), tail)),
        (false, _) => Err(with_tail(describe(status, limits), tail)),
    }
}

/// `reason` and the last stderr lines.
fn with_tail(reason: String, tail: &[String]) -> RefError {
    let reason = if tail.is_empty() { reason } else { format!("{reason}; stderr: {}", tail.join(" | ")) };
    index_error(reason)
}

/// Why a worker that did not exit 0 ended.
fn describe(status: ExitStatus, limits: &WorkerLimits) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return rlimits::describe_signal(sig, limits);
        }
    }
    let _ = limits;
    match status.code() {
        Some(code) => format!("the index worker exited with status {code}"),
        None => "the index worker ended abnormally".to_string(),
    }
}

/// `setrlimit` between `fork` and `exec` (Unix).
#[cfg(unix)]
mod rlimits {
    use std::os::unix::process::CommandExt;
    use std::process::Command;

    use super::{WorkerLimits, address_space_enforced};

    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    type Resource = libc::__rlimit_resource_t;
    #[cfg(not(all(target_os = "linux", target_env = "gnu")))]
    type Resource = libc::c_int;

    /// Seconds between the soft and the hard CPU limit.
    const CPU_GRACE_SECS: u64 = 10;

    #[derive(Debug, Clone, Copy)]
    struct Limit {
        resource: Resource,
        soft: libc::rlim_t,
        hard: libc::rlim_t,
    }

    /// The limits to set, clamped to this process's hard limits.
    fn plan(limits: &WorkerLimits) -> Vec<Limit> {
        let mut out = vec![clamp(libc::RLIMIT_CORE, 0, 0)];
        if let Some(cpu) = limits.cpu_time {
            let secs = cpu.as_secs().max(1);
            out.push(clamp(libc::RLIMIT_CPU, secs, secs.saturating_add(CPU_GRACE_SECS)));
        }
        if let Some(bytes) = limits.address_space.filter(|_| address_space_enforced()) {
            out.push(clamp(libc::RLIMIT_AS, bytes, bytes));
        }
        out
    }

    fn clamp(resource: Resource, soft: u64, hard: u64) -> Limit {
        let mut cur = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
        // SAFETY: `getrlimit` writes into the struct it is given.
        let max = if unsafe { libc::getrlimit(resource, &mut cur) } == 0 { cur.rlim_max } else { libc::RLIM_INFINITY };
        let cap = |v: u64| if max == libc::RLIM_INFINITY { v } else { v.min(max) };
        Limit { resource, soft: cap(soft), hard: cap(hard) }
    }

    /// Sets the limits in the child before `exec`.
    pub(super) fn apply(cmd: &mut Command, limits: &WorkerLimits) {
        let plan = plan(limits);
        // SAFETY: the closure runs in the forked child before `exec`; it
        // only calls `setrlimit` (async-signal-safe) over a vector built
        // before the fork, and allocates nothing.
        unsafe {
            cmd.pre_exec(move || {
                for l in &plan {
                    let r = libc::rlimit { rlim_cur: l.soft, rlim_max: l.hard };
                    if libc::setrlimit(l.resource, &r) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
    }

    /// A signal that ended the worker, in words.
    pub(super) fn describe_signal(sig: i32, limits: &WorkerLimits) -> String {
        match (sig, limits.cpu_time) {
            (libc::SIGKILL, _) => {
                "the index worker was killed (SIGKILL: the CPU hard limit, the container's memory limit or an operator)"
                    .to_string()
            }
            (libc::SIGXCPU, Some(t)) => format!("the index worker hit the CPU time limit of {t:?} (SIGXCPU)"),
            (libc::SIGABRT, _) => {
                "the index worker aborted (SIGABRT: an allocation failed or a fatal runtime error)".to_string()
            }
            (libc::SIGSEGV, _) => "the index worker crashed (SIGSEGV)".to_string(),
            (libc::SIGBUS, _) => "the index worker crashed (SIGBUS)".to_string(),
            (other, _) => format!("the index worker was ended by signal {other}"),
        }
    }
}
