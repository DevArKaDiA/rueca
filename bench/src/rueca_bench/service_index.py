"""Service indexing suite (RFC 0008 §8.2, §8.3; phase 2): the indexer role
indexing the organization from URLs, push → queryable through signed
webhooks, and fault injection.

    rueca-bench service-index [--small] [--split gated|calibration] [--only REPO ...]
                              [--indexers N ...] [--pushes N] [--no-faults]
                              [--keep] [--out FILE] [--bin PATH]

Steps (one work directory, `.cache/service-index/`, removed at the end
unless `--keep`):

1. **Members.** `--small`: the two repositories of `bench/service/small.toml`
   (cut from this checkout, one commit each). Otherwise the multi-repository
   scenario's members of the split, each cut from its worktree at the pin
   into a one-commit repository (`git archive`; needs `rueca-bench setup`).
2. **Local baseline.** `rueca index <member> --store <dir>` per member,
   timed one at a time: the sum is what one machine needs locally.
3. **Git host.** Every member cloned bare under `host/` (with
   `uploadpack.allowFilter`), served by `git daemon --export-all
   --base-path=host` on an ephemeral loopback port (`git://127.0.0.1:<port>/
   <name>.git`): the cheapest smart-protocol host on macOS and Linux (no CGI
   shim), protocol v2, blob filters and commit wants included. Accepted by
   the test-only `[indexing].allow_file_urls` of debug builds, so the binary
   must be a debug build (`cargo build -p rueca`).
4. **Runs.** For each `--indexers` N (default 1 and 4), on a fresh `file://`
   store and a fresh host: N = 1 is one `rueca serve --role all`; N > 1 is a
   `rueca serve --role query` replica plus N `rueca serve --role indexer`
   processes, `org_updater` on the first only. Discovery and the updater
   tick every second, replicas poll every second, polling every 5 min at
   the earliest (pushes reach the service through webhooks).
   - **Cold index**: process start → every member served at its commit (the
     replica's `/status`), against the local sum; then the catalogs
     (`rueca status --store-url`) and the queries' answers, compared with
     the first run's ("equal results").
   - **Push → queryable** (`--pushes`, default 10): a commit adding a new
     function, pushed to the host, then a signed GitHub `push` delivery to
     one indexer (round robin); measured from the delivery to the first
     `search_symbols` answer naming the function (polled every 100 ms);
     p50 / p95, and the `rueca_publish_seconds` stages (fetch, export,
     index, upload, generation) over the pushes only (bucket deltas of every
     indexer's `/metrics`, quantiles as Prometheus' `histogram_quantile`).
5. **Faults** (the last run, unless `--no-faults`), each `ok` or not with
   its evidence:
   - `looping_parser`: an on-demand ref `loop-wall` whose worker spins
     (`RUECA_TEST_WORKER_FAULT=loop:loop-*`, debug builds) is killed at
     `[indexing].worker.wall_time` (15 s, or 4 × the slowest local index)
     and fails alone; every process survives;
   - `worker_killed`: the worker of `loop-kill` killed with SIGKILL from
     outside mid-job: the ref fails alone, the indexer survives;
   - `lease_taken`: while `loop-lease` runs, its job object is rewritten in
     the store with a forged lease (`epoch + 1`, another owner, already
     expired): the running indexer's finish loses the lease and writes
     nothing, another lease (`epoch + 2`) re-runs the job, the default
     ref's catalog head never regresses;
   - `webhooks_duplicate_reordered`: two pushes delivered newest first,
     then the newest again: `accepted`, `accepted`, `duplicate`; the served
     commit ends, and stays, at the newest;
   - `storage_unwritable`: the store directory made read-only (`chmod -R
     a-w`) for 8 s around a push and its delivery: the confirmation fails
     (counted), the delivery id is forgotten, the host's redelivery after the
     window is accepted and the commit becomes queryable; every process
     survives. A bucket answering 409 / 503 needs an S3-compatible store
     and is not injected here (CAS conflicts are exercised by
     `lease_taken`).

Output: `bench/results/service-index-<UTC timestamp>.json` (or `--out`).
"""

from __future__ import annotations

import fcntl
import hashlib
import hmac
import http.client
import json
import os
import secrets
import shutil
import signal
import subprocess
import time
import uuid
from pathlib import Path
from typing import Any

from .common import CACHE, RESULTS_DIR, log, run, safe_name, write_json
from .corpus import add_worktree, remove_worktree
from .org import _filtered, org_members
from .service import (
    McpClient,
    first_diff,
    free_port,
    get,
    histogram_quantile,
    org_queries,
    parse_metrics,
    quantile,
    rueca_bin,
    small_members,
    stop,
)

ORG_NAME = "bench"
WORK = CACHE / "service-index"
SECRET_ENV = "RUECA_BENCH_GITHUB_SECRET"
FAULT_SPEC = "loop:loop-*"
STAGES = ("fetch", "export", "index", "upload", "generation")
COLD_TIMEOUT = 1800.0
PUSH_TIMEOUT = 300.0
_IDENT = ["-c", "user.name=rueca-bench", "-c", "user.email=bench@example.invalid", "-c", "commit.gpgsign=false"]


def git(args: list[str], cwd: Path | None = None) -> str:
    res = run(["git", *_IDENT, *args], cwd=cwd, timeout=600)
    if res.rc != 0:
        raise RuntimeError(f"git {' '.join(args)}: {res.stderr.strip()}")
    return res.stdout.strip()


# --- members -------------------------------------------------------------------------


def org_sources(work: Path, split: str, only: list[str] | None) -> tuple[list[Path], list[str]]:
    """The split's members cut at their pins into one-commit repositories."""
    repos = set(only) if only else None
    members = _filtered(org_members(split), repos)
    if not members:
        raise SystemExit(f"rueca-bench service-index: no {split} member matches --only {only}")
    roots = []
    for m in members:
        wt = add_worktree(m["repo"], m["commit"], f"service-index-{m['repo']}")
        try:
            dest = work / "repos" / safe_name(m["repo"])
            dest.mkdir(parents=True)
            arch = subprocess.run(["git", "-C", str(wt), "archive", "HEAD"], capture_output=True, check=True)
            subprocess.run(["tar", "-x", "-C", str(dest)], input=arch.stdout, check=True)
            for cmd in (["init", "-q", "-b", "main"], ["add", "-A"], ["commit", "-q", "-m", "snapshot"]):
                git(cmd, cwd=dest)
            roots.append(dest)
        finally:
            remove_worktree(m["repo"], wt)
    return roots, org_queries(split, repos)


def local_baseline(bin_: Path, roots: list[Path], work: Path) -> dict:
    """`rueca index` of each member alone, timed."""
    per = {}
    for root in roots:
        t0 = time.perf_counter()
        run([str(bin_), "index", str(root), "--store", str(work / "local" / root.name), "--json"], timeout=3600, check=True)
        per[root.name] = round(time.perf_counter() - t0, 3)
    return {"per_repo_seconds": per, "sum_seconds": round(sum(per.values()), 3), "max_seconds": max(per.values())}


# --- the git host --------------------------------------------------------------------


class GitHost:
    """Bare repositories under `base`, served by one `git daemon`."""

    def __init__(self, base: Path, sources: list[Path]):
        self.base, self.sources = base, sources
        self.port = free_port()
        self.proc: subprocess.Popen | None = None

    def reset(self) -> None:
        """Fresh bare clones of every source (and fresh push clones)."""
        for d in (self.base, self.base.parent / "clones"):
            shutil.rmtree(d, ignore_errors=True)
        self.base.mkdir(parents=True)
        for src in self.sources:
            bare = self.bare(src.name)
            git(["clone", "--quiet", "--bare", str(src), str(bare)])
            git(["config", "uploadpack.allowFilter", "true"], cwd=bare)

    def start(self) -> None:
        cmd = ["git", "daemon", "--export-all", f"--base-path={self.base}", "--reuseaddr", "--listen=127.0.0.1", f"--port={self.port}", "--informative-errors", str(self.base)]
        self.proc = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        deadline = time.perf_counter() + 20
        while time.perf_counter() < deadline:
            if run(["git", "ls-remote", self.url(self.sources[0].name)], timeout=10).rc == 0:
                return
            time.sleep(0.1)
        raise RuntimeError("git daemon did not start")

    def stop(self) -> None:
        if self.proc is not None:
            self.proc.terminate()
            self.proc.wait(timeout=10)
            self.proc = None

    def bare(self, name: str) -> Path:
        return self.base / f"{name}.git"

    def url(self, name: str) -> str:
        return f"git://127.0.0.1:{self.port}/{name}.git"

    def head(self, name: str) -> str:
        return git(["rev-parse", "main"], cwd=self.bare(name))

    def push(self, name: str, files: dict[str, str]) -> str:
        """Commits `files` on `main` in a clone and pushes it; the commit."""
        clone = self.base.parent / "clones" / name
        if not clone.exists():
            git(["clone", "--quiet", str(self.bare(name)), str(clone)])
        for rel, text in files.items():
            p = clone / rel
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_text(text)
        git(["add", "-A"], cwd=clone)
        git(["commit", "--quiet", "-m", f"bench push {len(files)} file(s)"], cwd=clone)
        git(["push", "--quiet", "origin", "main"], cwd=clone)
        return git(["rev-parse", "HEAD"], cwd=clone)

    def set_ref(self, name: str, ref: str) -> None:
        git(["update-ref", ref, "main"], cwd=self.bare(name))


# --- the fleet -----------------------------------------------------------------------


class Proc:
    def __init__(self, name: str, role: str, updater: bool):
        self.name, self.role, self.updater = name, role, updater
        self.port, self.ops = free_port(), free_port()
        self.popen: subprocess.Popen | None = None
        self.log: Path | None = None

    def alive(self) -> bool:
        return self.popen is not None and self.popen.poll() is None


def proc_config(p: Proc, d: Path, store_url: str, host: GitHost, token: str, wall: int) -> str:
    digest = hashlib.sha256(token.encode()).hexdigest()
    repos = "".join(f'[[repo]]\nurl = "{host.url(s.name)}"\n\n' for s in host.sources)
    return f"""[store]
url = {json.dumps(store_url)}
cache_dir = {json.dumps(str(d / "cache"))}
ready_when = "index"
poll = "1s"

[org]
name = "{ORG_NAME}"

{repos}[http]
listen = "127.0.0.1:{p.port}"
ops_listen = "127.0.0.1:{p.ops}"
canonical_url = "http://127.0.0.1:{p.port}/mcp"
request_timeout = "60s"
rate_limit = {{ per_second = 1000000, burst = 1000000 }}
ip_rate_limit = {{ per_second = 1000000, burst = 1000000 }}

[auth]
admins = ["token:bench"]

[[auth.token]]
name = "bench"
hash = "sha256:{digest}"
repos = ["**"]

[indexing]
allow_file_urls = true
allow_private_networks = true
hosts = ["127.0.0.1"]
discover = "1s"
updater_tick = "1s"
org_updater = {"true" if p.updater else "false"}
mirror_dir = {json.dumps(str(d / "mirrors"))}
facts_dir = {json.dumps(str(d / "facts"))}
poll = {{ min = "5m", max = "8h" }}
worker = {{ wall_time = "{wall}s" }}
webhooks = {{ github = "env:{SECRET_ENV}" }}

[telemetry]
log_format = "json"
"""


class Fleet:
    """The processes of one run: `all` (N = 1) or `query` + N `indexer`."""

    def __init__(self, bin_: Path, run_dir: Path, host: GitHost, n: int, token: str, secret: str, wall: int):
        self.bin, self.dir, self.host, self.n = bin_, run_dir, host, n
        self.token, self.secret, self.wall = token, secret, wall
        self.store = run_dir / "store"
        if n == 1:
            self.procs = [Proc("all", "all", True)]
        else:
            self.procs = [Proc("query", "query", False)] + [Proc(f"indexer-{i}", "indexer", i == 0) for i in range(n)]
        self.query = self.procs[0]
        self.indexers = [p for p in self.procs if p.role != "query"]
        self.started = 0.0

    def start(self) -> None:
        env = {**os.environ, SECRET_ENV: self.secret, "RUECA_TEST_WORKER_FAULT": FAULT_SPEC}
        self.started = time.perf_counter()
        for p in self.procs:
            d = self.dir / p.name
            d.mkdir(parents=True)
            cfg = d / "rueca-service.toml"
            cfg.write_text(proc_config(p, d, f"file://{self.store}", self.host, self.token, self.wall))
            p.log = d / "serve.log"
            with p.log.open("w") as fh:
                p.popen = subprocess.Popen([str(self.bin), "serve", "--config", str(cfg), "--role", p.role], stdout=fh, stderr=subprocess.STDOUT, env=env)
        for p in self.procs:
            wait_listening(p)

    def stop(self) -> list[dict]:
        out = []
        for p in self.procs:
            if p.popen is not None and p.popen.poll() is None:
                out.append({"proc": p.name, **stop(p.popen)})
        return out

    def alive(self) -> bool:
        return all(p.alive() for p in self.procs)

    def logs(self) -> str:
        return "\n".join(p.log.read_text(errors="replace") for p in self.procs if p.log and p.log.exists())


def wait_listening(p: Proc, timeout: float = 120.0) -> None:
    deadline = time.perf_counter() + timeout
    while time.perf_counter() < deadline:
        if not p.alive():
            raise RuntimeError(f"rueca serve ({p.name}) exited: {p.log.read_text()[-2000:] if p.log else ''}")
        try:
            if get(p.ops, "/healthz")[0] == 200:
                return
        except OSError:
            pass
        time.sleep(0.05)
    raise RuntimeError(f"rueca serve ({p.name}) not listening after {timeout:.0f} s")


# --- HTTP helpers --------------------------------------------------------------------


def authed(port: int, method: str, path: str, token: str, body: dict | None = None) -> tuple[int, Any]:
    conn = http.client.HTTPConnection("127.0.0.1", port, timeout=60)
    try:
        headers = {"Authorization": f"Bearer {token}", "Content-Type": "application/json"}
        conn.request(method, path, body=json.dumps(body) if body is not None else None, headers=headers)
        r = conn.getresponse()
        raw = r.read()
        return r.status, (json.loads(raw) if raw else None)
    finally:
        conn.close()


def github_push(url: str, after: str, before: str = "0" * 40) -> bytes:
    """A minimal GitHub `push` payload (the documented shape)."""
    body = {"ref": "refs/heads/main", "before": before, "after": after, "created": False, "deleted": False, "repository": {"clone_url": url, "full_name": url.rsplit("/", 1)[-1]}}
    return json.dumps(body).encode()


def signature(secret: str, body: bytes) -> str:
    return "sha256=" + hmac.new(secret.encode(), body, hashlib.sha256).hexdigest()


def deliver(port: int, secret: str, body: bytes, delivery: str, event: str = "push") -> tuple[int, dict | None, float]:
    """A signed GitHub delivery: status, JSON answer, seconds."""
    conn = http.client.HTTPConnection("127.0.0.1", port, timeout=30)
    headers = {"Content-Type": "application/json", "X-GitHub-Event": event, "X-GitHub-Delivery": delivery, "X-Hub-Signature-256": signature(secret, body)}
    t0 = time.perf_counter()
    try:
        conn.request("POST", "/webhooks/github", body=body, headers=headers)
        r = conn.getresponse()
        raw = r.read()
        return r.status, (json.loads(raw) if raw else None), time.perf_counter() - t0
    finally:
        conn.close()


def served_defaults(fleet: Fleet) -> dict[str, str]:
    """Repository → default commit, as the replica serves them."""
    try:
        status, body = authed(fleet.query.port, "GET", "/status", fleet.token)
    except OSError:
        return {}
    if status != 200 or not body:
        return {}
    return {r["repo"]: r["default"]["commit"] for r in body.get("repositories") or []}


def jobs(p: Proc, token: str) -> list[dict]:
    status, body = authed(p.port, "GET", "/admin/jobs", token)
    return (body or {}).get("jobs") or [] if status == 200 else []


def job_of(fleet: Fleet, repo_name: str) -> dict | None:
    for j in jobs(fleet.indexers[0], fleet.token):
        if j["repo"].rsplit("/", 1)[-1] == repo_name:
            return j
    return None


def wait_until(pred, timeout: float, step: float = 0.1) -> float | None:
    """Seconds until `pred()` held, None after `timeout`."""
    t0 = time.perf_counter()
    while time.perf_counter() - t0 < timeout:
        if pred():
            return time.perf_counter() - t0
        time.sleep(step)
    return None


# --- metrics -------------------------------------------------------------------------


def stage_buckets(fleet: Fleet) -> dict[str, dict[float, float]]:
    """`rueca_publish_seconds` buckets by stage, summed over the indexers."""
    out: dict[str, dict[float, float]] = {}
    for p in fleet.indexers:
        status, body = get(p.ops, "/metrics")
        if status != 200:
            continue
        for name, labels, v in parse_metrics(body.decode()):
            if name == "rueca_publish_seconds_bucket":
                le = float("inf") if labels["le"] == "+Inf" else float(labels["le"])
                acc = out.setdefault(labels.get("stage", ""), {})
                acc[le] = acc.get(le, 0.0) + v
    return out


def counter(fleet: Fleet, metric: str, **labels: str) -> float:
    total = 0.0
    for p in fleet.indexers:
        status, body = get(p.ops, "/metrics")
        if status == 200:
            total += sum(v for n, lab, v in parse_metrics(body.decode()) if n == metric and all(lab.get(k) == x for k, x in labels.items()))
    return total


def stage_stats(before: dict, after: dict) -> dict:
    """p50 / p95 (ms) per stage of the bucket deltas `after - before`."""
    out = {}
    for stage in STAGES:
        a, b = after.get(stage, {}), before.get(stage, {})
        delta = {le: a[le] - b.get(le, 0.0) for le in a}
        n = int(delta.get(float("inf"), 0))
        if n == 0:
            continue
        p50, p95 = histogram_quantile(0.5, delta), histogram_quantile(0.95, delta)
        out[stage] = {"n": n, "p50_ms": round(p50 * 1000, 1) if p50 is not None else None, "p95_ms": round(p95 * 1000, 1) if p95 is not None else None}
    return out


# --- a run ---------------------------------------------------------------------------


def cold_index(fleet: Fleet, expected: dict[str, str]) -> dict:
    """Process start → every member served at its commit."""
    want = set(expected.values())

    def done() -> bool:
        return want <= set(served_defaults(fleet).values())

    secs = wait_until(done, COLD_TIMEOUT, step=0.2)
    if secs is None:
        raise RuntimeError(f"cold index: not every member served after {COLD_TIMEOUT:.0f} s")
    return {"seconds": round(time.perf_counter() - fleet.started, 3), "repos": len(want), "stages": stage_stats({}, stage_buckets(fleet))}


def catalogs(bin_: Path, store: Path) -> list[list[str]]:
    """(repo name, ref, kind, commit) of every catalog of the store."""
    res = run([str(bin_), "status", "--store-url", f"file://{store}", "--json"], timeout=120, check=True)
    st = json.loads(res.stdout)
    rows = [[r["repo"].rsplit("/", 1)[-1], x["ref"], x["kind"], x["commit"] or ""] for r in st["repos"] for x in r["refs"]]
    return sorted(rows)


def answers(fleet: Fleet, queries: list[str]) -> list[Any]:
    client = McpClient(fleet.query.port, fleet.token)
    client.initialize()
    out = []
    for q in queries:
        a, _ = client.call("search_symbols", {"query": q, "limit": 10})
        out.append((a.get("result") or {}).get("structuredContent"))
    return out


def marker_found(client: McpClient, marker: str) -> bool:
    try:
        a, _ = client.call("search_symbols", {"query": marker, "limit": 5})
    except (OSError, ValueError):
        return False
    return marker in json.dumps(a.get("result") or {})


def push_phase(fleet: Fleet, pushes: int, run_tag: str) -> dict:
    """`pushes` commits, each delivered as a signed webhook; push → queryable."""
    client = McpClient(fleet.query.port, fleet.token)
    client.initialize()
    before = stage_buckets(fleet)
    samples, acks = [], []
    for i in range(pushes):
        src = fleet.host.sources[i % len(fleet.host.sources)]
        marker = f"rueca_push_marker_{run_tag}_{i}"
        commit = fleet.host.push(src.name, {f"rueca_bench_push/p{i}.py": f"def {marker}():\n    return {i}\n"})
        target = fleet.indexers[i % len(fleet.indexers)]
        t0 = time.perf_counter()
        status, _, ack = deliver(target.port, fleet.secret, github_push(fleet.host.url(src.name), commit), str(uuid.uuid4()))
        acks.append(ack)
        if status != 202:
            raise RuntimeError(f"push {i}: the webhook answered {status}")
        if wait_until(lambda m=marker: marker_found(client, m), PUSH_TIMEOUT) is None:
            raise RuntimeError(f"push {i}: {marker} not queryable after {PUSH_TIMEOUT:.0f} s")
        samples.append(time.perf_counter() - t0)
    ms = [s * 1000 for s in samples]
    return {
        "pushes": pushes,
        "p50_ms": round(quantile(ms, 0.5) or 0, 1),
        "p95_ms": round(quantile(ms, 0.95) or 0, 1),
        "max_ms": round(max(ms), 1),
        "samples_ms": [round(x, 1) for x in ms],
        "webhook_ack_p95_ms": round((quantile(acks, 0.95) or 0) * 1000, 2),
        "stages": stage_stats(before, stage_buckets(fleet)),
    }


# --- faults --------------------------------------------------------------------------


def admin_index(fleet: Fleet, name: str, ref: str) -> int:
    repo = next(j["repo"] for j in jobs(fleet.indexers[0], fleet.token) if j["repo"].rsplit("/", 1)[-1] == name)
    status, _ = authed(fleet.indexers[0].port, "POST", "/admin/index", fleet.token, {"repo": repo, "ref": ref})
    return status


def ref_failure(fleet: Fleet, name: str, ref: str) -> dict | None:
    j = job_of(fleet, name)
    return ((j or {}).get("failed_refs") or {}).get(ref)


def wait_ref_failure(fleet: Fleet, name: str, ref: str) -> tuple[dict | None, float | None]:
    secs = wait_until(lambda: ref_failure(fleet, name, ref) is not None, fleet.wall * 3 + 120, step=0.5)
    return ref_failure(fleet, name, ref), secs


def other_jobs_done(fleet: Fleet, name: str) -> bool:
    return all(j["state"] == "done" for j in jobs(fleet.indexers[0], fleet.token) if j["repo"].rsplit("/", 1)[-1] != name)


def fault_looping_parser(fleet: Fleet, name: str) -> dict:
    fleet.host.set_ref(name, "refs/heads/loop-wall")
    status = admin_index(fleet, name, "loop-wall")
    failure, secs = wait_ref_failure(fleet, name, "loop-wall")
    ok = status == 202 and failure is not None and failure.get("stage") == "index" and fleet.alive() and other_jobs_done(fleet, name)
    return {"ok": ok, "admin_status": status, "failed_after_s": _r(secs), "failure": failure, "wall_time_s": fleet.wall}


def index_workers() -> list[int]:
    res = run(["pgrep", "-f", " index-worker"], timeout=10)
    return [int(x) for x in res.stdout.split()] if res.rc == 0 else []


def fault_worker_killed(fleet: Fleet, name: str) -> dict:
    fleet.host.set_ref(name, "refs/heads/loop-kill")
    status = admin_index(fleet, name, "loop-kill")
    found = wait_until(lambda: bool(index_workers()), 60, step=0.2)
    time.sleep(1.0)
    killed = index_workers()
    for pid in killed:
        os.kill(pid, signal.SIGKILL)
    failure, secs = wait_ref_failure(fleet, name, "loop-kill")
    reason = (failure or {}).get("reason", "")
    ok = status == 202 and found is not None and bool(killed) and failure is not None and fleet.alive()
    return {"ok": ok, "killed_pids": killed, "failed_after_s": _r(secs), "reason": reason[:300]}


def job_file(store: Path, name: str) -> Path | None:
    for f in (store / "jobs").glob("*.json"):
        if json.loads(f.read_text()).get("repo", "").rsplit("/", 1)[-1] == name:
            return f
    return None


def forge_lease(store: Path, path: Path) -> dict:
    """Rewrites the job's lease (another owner, epoch + 1, already expired)
    under the file store's lock, as another writer would."""
    with (store / ".lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        job = json.loads(path.read_text())
        lease = job.get("lease") or {"epoch": 0}
        job["lease"] = {"owner": "forger/ffffff", "until": int(time.time()) - 60, "epoch": lease["epoch"] + 1}
        tmp = store / ".tmp" / f"forged-{uuid.uuid4().hex}"
        tmp.write_text(json.dumps(job, indent=2))
        tmp.replace(path)
        fcntl.flock(lock, fcntl.LOCK_UN)
    return job["lease"]


def leased_by_worker(fleet: Fleet, name: str) -> bool:
    j = job_of(fleet, name) or {}
    return j.get("state") == "leased" and bool(index_workers())


def fault_lease_taken(fleet: Fleet, name: str) -> dict:
    before = served_defaults(fleet)
    fleet.host.set_ref(name, "refs/heads/loop-lease")
    status = admin_index(fleet, name, "loop-lease")
    if wait_until(lambda: leased_by_worker(fleet, name), 60, step=0.2) is None:
        return {"ok": False, "error": "the job was never seen leased with a running worker", "admin_status": status}
    path = job_file(fleet.store, name)
    forged = forge_lease(fleet.store, path) if path else None
    failure, secs = wait_ref_failure(fleet, name, "loop-lease")
    lost = counter(fleet, "rueca_job_seconds_count", result="lost")
    final = (job_of(fleet, name) or {}).get("lease") or {}
    after = served_defaults(fleet)
    no_regress = all(after.get(r) == c for r, c in before.items())
    ok = forged is not None and failure is not None and lost >= 1 and final.get("epoch", 0) > forged["epoch"] and no_regress and fleet.alive()
    return {"ok": ok, "forged": forged, "final_lease": final, "lost_runs": lost, "rerun_failed_after_s": _r(secs), "catalog_never_regressed": no_regress}


def fault_webhooks(fleet: Fleet, name: str) -> dict:
    url, port = fleet.host.url(name), fleet.indexers[0].port
    c1 = fleet.host.push(name, {"rueca_bench_push/order_1.py": "def reorder_one():\n    return 1\n"})
    c2 = fleet.host.push(name, {"rueca_bench_push/order_2.py": "def reorder_two():\n    return 2\n"})
    d2, d1 = str(uuid.uuid4()), str(uuid.uuid4())
    answers_ = [deliver(port, fleet.secret, github_push(url, c2, c1), d2), deliver(port, fleet.secret, github_push(url, c1), d1), deliver(port, fleet.secret, github_push(url, c2, c1), d2)]
    results = [(a[1] or {}).get("result") for a in answers_]
    served = wait_until(lambda: c2 in served_defaults(fleet).values(), PUSH_TIMEOUT)
    time.sleep(3.0)
    stays = c2 in served_defaults(fleet).values()
    ok = results == ["accepted", "accepted", "duplicate"] and served is not None and stays
    return {"ok": ok, "results": results, "served_newest_after_s": _r(served), "newest_stays": stays}


def fault_storage(fleet: Fleet, name: str) -> dict:
    url, port = fleet.host.url(name), fleet.indexers[0].port
    commit = fleet.host.push(name, {"rueca_bench_push/storage.py": "def storage_outage():\n    return 0\n"})
    failed_before = counter(fleet, "rueca_webhook_confirmations_total", result="failed")
    delivery = str(uuid.uuid4())
    subprocess.run(["chmod", "-R", "a-w", str(fleet.store)], check=True)
    try:
        first = deliver(port, fleet.secret, github_push(url, commit), delivery)
        time.sleep(8.0)
    finally:
        subprocess.run(["chmod", "-R", "u+w", str(fleet.store)], check=True)
    failed = counter(fleet, "rueca_webhook_confirmations_total", result="failed") - failed_before
    again = deliver(port, fleet.secret, github_push(url, commit), delivery)
    served = wait_until(lambda: commit in served_defaults(fleet).values(), PUSH_TIMEOUT)
    results = [(first[1] or {}).get("result"), (again[1] or {}).get("result")]
    ok = results == ["accepted", "accepted"] and failed >= 1 and served is not None and fleet.alive()
    return {"ok": ok, "results": results, "confirmations_failed": failed, "window_s": 8.0, "served_after_redelivery_s": _r(served)}


def run_faults(fleet: Fleet) -> dict:
    names = [s.name for s in fleet.host.sources]
    a, b = names[0], names[-1]
    out = {}
    for key, fn, name in (("looping_parser", fault_looping_parser, a), ("worker_killed", fault_worker_killed, b), ("lease_taken", fault_lease_taken, b if len(names) == 1 else a), ("webhooks_duplicate_reordered", fault_webhooks, b), ("storage_unwritable", fault_storage, a)):
        log(f"service-index: fault {key} on {name}")
        try:
            out[key] = fn(fleet, name)
        except (RuntimeError, OSError, ValueError, KeyError, StopIteration, subprocess.CalledProcessError) as exc:  # recorded per fault; the next one still runs
            out[key] = {"ok": False, "error": f"{type(exc).__name__}: {exc}"}
        log(f"service-index: fault {key}: {'ok' if out[key].get('ok') else 'FAILED'}")
    out["not_injected"] = ["a bucket answering 409 / 503 (needs an S3-compatible store; the file:// backend can only refuse writes, done above)"]
    return out


def _r(x: float | None) -> float | None:
    return None if x is None else round(x, 2)


def one_run(bin_: Path, work: Path, host: GitHost, n: int, queries: list[str], faults: bool, pushes: int, wall: int) -> dict:
    host.reset()
    expected = {s.name: host.head(s.name) for s in host.sources}
    fleet = Fleet(bin_, work / f"run-{n}", host, n, "rueca_" + secrets.token_hex(24), secrets.token_hex(16), wall)
    rec: dict[str, Any] = {"indexers": n, "processes": [f"{p.name}:{p.role}{' (org_updater)' if p.updater else ''}" for p in fleet.procs]}
    try:
        fleet.start()
        rec["cold"] = cold_index(fleet, expected)
        log(f"service-index: {n} indexer(s): cold index {rec['cold']['seconds']} s")
        rec["catalog"] = catalogs(bin_, fleet.store)
        rec["answers"] = answers(fleet, queries)
        rec["push"] = push_phase(fleet, pushes, f"n{n}")
        log(f"service-index: push → queryable p50 {rec['push']['p50_ms']} ms p95 {rec['push']['p95_ms']} ms")
        if faults:
            rec["faults"] = run_faults(fleet)
        rec["all_alive"] = fleet.alive()
    except Exception:
        rec["log_tail"] = fleet.logs()[-4000:]
        raise
    finally:
        rec["shutdown"] = fleet.stop()
    return rec


def compare_runs(runs: list[dict]) -> None:
    """Equal results: catalogs and query answers of every run against the first."""
    base = runs[0]
    for r in runs[1:]:
        diff = first_diff(r["answers"], base["answers"])
        r["equal_to_first"] = {"catalog": r["catalog"] == base["catalog"], "answers": diff is None, "first_diff": diff}


def run_service_index(small: bool = False, split: str = "gated", only: list[str] | None = None, indexers: list[int] | None = None, pushes: int = 10, faults: bool = True, keep: bool = False, out: Path | None = None, bin_arg: str | None = None) -> dict:
    bin_ = rueca_bin(bin_arg)
    counts = indexers or [1, 4]
    shutil.rmtree(WORK, ignore_errors=True)
    WORK.mkdir(parents=True)
    t_all = time.perf_counter()
    report: dict[str, Any] = {"suite": "service-index", "mode": "small" if small else f"org-{split}", "binary": str(bin_), "indexers": counts, "pushes": pushes, "git_host": "git daemon (git://, protocol v2)"}
    host = None
    try:
        roots, queries = small_members(WORK) if small else org_sources(WORK, split, only)
        report["members"] = [r.name for r in roots]
        report["local"] = local_baseline(bin_, roots, WORK)
        log(f"service-index: local `rueca index` sum {report['local']['sum_seconds']} s over {len(roots)} repositories")
        wall = max(15, int(4 * report["local"]["max_seconds"]) + 1)
        host = GitHost(WORK / "host", roots)
        host.reset()
        host.start()
        report["runs"] = [one_run(bin_, WORK, host, n, queries, faults and n == counts[-1], pushes, wall) for n in counts]
        compare_runs(report["runs"])
        report["status"] = "ok"
    except Exception as exc:  # recorded, then re-raised after the report is written
        report["status"] = "error"
        report["error"] = f"{type(exc).__name__}: {exc}"
        raise
    finally:
        if host is not None:
            host.stop()
        report["seconds"] = round(time.perf_counter() - t_all, 1)
        dest = out or RESULTS_DIR / f"service-index-{time.strftime('%Y%m%dT%H%M%SZ', time.gmtime())}.json"
        write_json(dest, report)
        log(f"service-index: report {dest}")
        if not keep:
            subprocess.run(["chmod", "-R", "u+w", str(WORK)], check=False)
            shutil.rmtree(WORK, ignore_errors=True)
    summary(report)
    return report


def summary(r: dict) -> None:
    log(f"service-index: local sum {r['local']['sum_seconds']} s ({len(r['members'])} repositories)")
    for run_ in r["runs"]:
        p = run_["push"]
        eq = run_.get("equal_to_first")
        same = "" if eq is None else f"; equal to the first run: catalog {eq['catalog']}, answers {eq['answers']}"
        log(f"  {run_['indexers']} indexer(s): cold {run_['cold']['seconds']} s; push → queryable p50 {p['p50_ms']} ms p95 {p['p95_ms']} ms (n={p['pushes']}){same}")
        for stage, s in p["stages"].items():
            log(f"    {stage:<10} p50 {s['p50_ms']} ms p95 {s['p95_ms']} ms (n={s['n']})")
        for key, f in (run_.get("faults") or {}).items():
            if isinstance(f, dict):
                log(f"    fault {key}: {'ok' if f.get('ok') else 'FAILED'}")
