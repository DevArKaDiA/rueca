"""Service suite (RFC 0008 §8.2, §8.3; phase 1): the organization suite's
queries served by `rueca serve` over MCP-over-HTTP, against `rueca query`
on the same mirror.

    rueca-bench service [--small] [--split gated|calibration] [--only REPO ...]
                        [--limit N] [--sample N] [--repeat N] [--keep] [--out FILE]

Steps (one work directory, `.cache/service/`, removed at the end unless
`--keep`):

1. **Index.** `--small`: the two repositories of `bench/service/small.toml`,
   cut from this checkout, indexed with one `rueca index --org`. Otherwise
   the multi-repository scenario's members of the split (`org.py`:
   `org_members`, worktrees at the pins, `rueca index --org` one member at a
   time, `add_org_members`; needs `rueca-bench setup`).
2. **Publish.** `rueca publish --repo <id> --org bench` per member into a
   `file://` service store, then `rueca pull --generation` into a separate
   mirror (the hidden RFC 0008 phase 0 commands; the second step builds the
   organization from the mirror and publishes generation 1).
3. **Serve.** `rueca serve --role query` on that store (its own cache
   directory, a static token reading `**`, rate limits out of the way, ports
   picked free); **time to ready** = process start → first `200` from
   `/readyz` (`ready_when = "index"`).
4. **Query.** A minimal MCP client over `http.client` (one keep-alive
   connection, bearer token, `initialize` then `tools/call`): the suite's
   `search_symbols` queries (`{"query", "limit"}`; full mode: the
   multi-repository questions, `queries_for(inst)["kw"]`, read from the
   cached `multirepo-<split>-rueca.json` when present), then `--sample`
   `get_symbol` calls and `trace` calls (`direction: both`, `depth: 2`, on
   callable symbols) derived from the search answers (every k-th distinct
   result id, deterministic). `--repeat` (default 3) replays the whole
   list for latency; only the first pass is checked.
5. **Differential** (§8.3): every first-pass answer against `rueca query
   <tool> '<args>' --org bench --store <mirror>`: the HTTP
   `structuredContent` equals the CLI's JSON, and the text content equals
   `--text`. The first difference (path, both values) is reported.
6. **Measure.** Client latency per tool (p50 / p95 / max, ms) over every
   pass (`client`) and over the first, cold pass alone
   (`client_first_pass`); server latency over every pass from `/metrics`
   (`rueca_mcp_request_seconds` buckets, p50 / p95 interpolated as
   Prometheus' `histogram_quantile`, per tool over every scope and per tool
   and scope); request outcomes; server RSS; binary, store, mirror and
   cache sizes. One unauthenticated request checks the `401` challenge.

Output: `bench/results/service-<UTC timestamp>.json` (or `--out`).
"""

from __future__ import annotations

import hashlib
import http.client
import json
import os
import re
import secrets
import shutil
import signal
import socket
import subprocess
import time
import tomllib
from pathlib import Path
from typing import Any

from .arms import make_arm
from .common import (
    BENCH_DIR,
    CACHE,
    REPO_ROOT,
    RESULTS_DIR,
    WORK_DIR,
    log,
    read_json,
    run,
    safe_name,
    write_json,
    write_org_file,
)
from .common import org_run_path as run_path
from .org import _filtered, add_org_members, org_members, org_questions
from .s1 import queries_for

ORG_NAME = "bench"
SMALL_SPEC = BENCH_DIR / "service" / "small.toml"
WORK = CACHE / "service"
PROTOCOL = "2025-06-18"
READY_TIMEOUT = 600.0
CALL_TIMEOUT = 120.0
CALLABLE = {"Function", "Method", "Constructor"}
_FIXED_DATE = "2026-01-01T00:00:00Z"
_IDENT = ["-c", "user.name=rueca-bench", "-c", "user.email=bench@example.invalid"]


# --- binary ------------------------------------------------------------------------------


def rueca_bin(explicit: str | None = None) -> Path:
    """`--bin`, else $RUECA_BIN (a path or a name on PATH)."""
    b = explicit or os.environ.get("RUECA_BIN", "")
    if not b:
        raise SystemExit("rueca-bench service: set RUECA_BIN (or --bin) to a rueca binary built with the `service` feature")
    p = Path(shutil.which(b) or b) if os.path.sep not in b else Path(b)
    if not p.exists():
        raise SystemExit(f"rueca-bench service: {p} does not exist")
    return p.resolve()


# --- members and queries -----------------------------------------------------------------


def small_members(work: Path) -> tuple[list[Path], list[str]]:
    """The `--small` repositories, cut from this checkout's HEAD into
    `work/repos/`, and its queries."""
    spec = tomllib.loads(SMALL_SPEC.read_text())
    roots = []
    for r in spec["repo"]:
        dest = work / "repos" / safe_name(r["path"])
        dest.mkdir(parents=True)
        arch = subprocess.run(["git", "-C", str(REPO_ROOT), "archive", f"HEAD:{r['path']}"], capture_output=True, check=True)
        subprocess.run(["tar", "-x", "-C", str(dest)], input=arch.stdout, check=True)
        env = {"GIT_AUTHOR_DATE": _FIXED_DATE, "GIT_COMMITTER_DATE": _FIXED_DATE}
        for cmd in (["init", "-q", "-b", "main"], ["add", "-A"], ["commit", "-q", "-m", "snapshot"], ["remote", "add", "origin", r["url"]]):
            run(["git", *_IDENT, *cmd], cwd=dest, env=env, check=True)
        roots.append(dest)
    return roots, list(spec["queries"])


def org_queries(split: str, repos: set[str] | None) -> list[str]:
    """The multi-repository questions of `split` (one keyword query each):
    from the cached Rueca output of that split when it covers the same
    filter, else selected again (`org_questions`, needs `setup`)."""
    raw = run_path(f"multirepo-{split}", "rueca")
    if raw.exists():
        rec = read_json(raw)
        same = (rec.get("filter") or None) == (sorted(repos) if repos else None)
        if rec.get("status") == "ok" and same and all(i.get("query") for i in rec.get("instances", [])):
            log(f"service: {len(rec['instances'])} queries from {raw.name}")
            return [i["query"] for i in rec["instances"]]
    return [queries_for(i)["kw"] for i in org_questions(split, repos)]


def index_org(bin_: Path, work: Path, small: bool, split: str, only: list[str] | None) -> tuple[list[str], list[str], dict]:
    """Index the organization into `work/index`; repository ids, queries,
    and the index record."""
    store, org_file = work / "index", work / "org.toml"
    t0 = time.perf_counter()
    if small:
        roots, queries = small_members(work)
        write_org_file(org_file, roots, name=ORG_NAME)
        res = run([str(bin_), "index", "--org", str(org_file), "--store", str(store), "--json"], timeout=1800, check=True)
        rep = json.loads(res.stdout)
        ids = [i["segment"]["snapshot"]["repo"] for i in rep["indexed"]]
        per = {i["segment"]["snapshot"]["repo"]: {"ok": True, "repo_ms": i["duration_ms"], "docs": i["segment"]["node_count"]} for i in rep["indexed"]}
        return ids, queries, {"seconds": round(time.perf_counter() - t0, 2), "repos": per}
    os.environ["RUECA_BIN"] = str(bin_)
    repos = set(only) if only else None
    members = _filtered(org_members(split), repos)
    if not members:
        raise SystemExit(f"rueca-bench service: no {split} member matches --only {only}")
    queries = org_queries(split, repos)
    roots: list[Path] = []
    index: dict[str, dict] = {}
    add_org_members(make_arm("rueca"), members, store, org_file, roots, index, prefix="service", name=ORG_NAME)
    ids = [v["repo_id"] for v in index.values() if v.get("ok") and v.get("repo_id")]
    return ids, queries, {"seconds": round(time.perf_counter() - t0, 2), "repos": index}


def publish(bin_: Path, work: Path, ids: list[str]) -> dict:
    """Every repository to the `file://` store, then generation 1 built in
    the mirror (`pull --generation`)."""
    url = f"file://{work / 'store'}"
    t0 = time.perf_counter()
    for rid in ids:
        run([str(bin_), "publish", "--repo", rid, "--store", str(work / "index"), "--store-url", url, "--org", ORG_NAME, "--json"], timeout=1800, check=True)
    t1 = time.perf_counter()
    res = run([str(bin_), "pull", "--generation", "--store-url", url, "--store", str(work / "mirror"), "--json"], timeout=1800, check=True)
    gen = (json.loads(res.stdout).get("published") or {}).get("generation")
    return {"url": url, "publish_seconds": round(t1 - t0, 2), "pull_generation_seconds": round(time.perf_counter() - t1, 2), "generation": gen}


# --- the server --------------------------------------------------------------------------


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def write_config(path: Path, store_url: str, cache_dir: Path, port: int, ops_port: int, token: str) -> None:
    digest = hashlib.sha256(token.encode()).hexdigest()
    path.write_text(
        f"""[store]
url = {json.dumps(store_url)}
cache_dir = {json.dumps(str(cache_dir))}
ready_when = "index"
poll = "1s"

[org]
name = "{ORG_NAME}"

[http]
listen = "127.0.0.1:{port}"
ops_listen = "127.0.0.1:{ops_port}"
canonical_url = "http://127.0.0.1:{port}/mcp"
rate_limit = {{ per_second = 1000000, burst = 1000000 }}
ip_rate_limit = {{ per_second = 1000000, burst = 1000000 }}

[[auth.token]]
name = "bench"
hash = "sha256:{digest}"
repos = ["**"]

[telemetry]
log_format = "json"
"""
    )


def get(port: int, path: str, timeout: float = 5.0) -> tuple[int, bytes]:
    conn = http.client.HTTPConnection("127.0.0.1", port, timeout=timeout)
    try:
        conn.request("GET", path)
        r = conn.getresponse()
        return r.status, r.read()
    finally:
        conn.close()


def wait_ready(proc: subprocess.Popen, ops_port: int, t0: float) -> dict:
    """Polls `/healthz` then `/readyz` every 20 ms; seconds since `t0`."""
    out: dict[str, Any] = {"listening_seconds": None, "ready_seconds": None, "readyz": None}
    while time.perf_counter() - t0 < READY_TIMEOUT:
        if proc.poll() is not None:
            raise RuntimeError(f"rueca serve exited with {proc.returncode} before it was ready")
        try:
            if out["listening_seconds"] is None and get(ops_port, "/healthz")[0] == 200:
                out["listening_seconds"] = round(time.perf_counter() - t0, 3)
            status, body = get(ops_port, "/readyz")
            if status == 200:
                out["ready_seconds"] = round(time.perf_counter() - t0, 3)
                out["readyz"] = json.loads(body)
                return out
        except OSError:
            pass
        time.sleep(0.02)
    raise RuntimeError(f"rueca serve not ready after {READY_TIMEOUT:.0f} s")


def stop(proc: subprocess.Popen) -> dict:
    t0 = time.perf_counter()
    proc.send_signal(signal.SIGTERM)
    try:
        rc = proc.wait(timeout=30)
    except subprocess.TimeoutExpired:
        proc.kill()
        rc = proc.wait()
    return {"exit_code": rc, "shutdown_seconds": round(time.perf_counter() - t0, 3)}


def rss_mb(pid: int) -> float | None:
    res = run(["ps", "-o", "rss=", "-p", str(pid)], timeout=10)
    return round(int(res.stdout.strip()) / 1024, 1) if res.rc == 0 and res.stdout.strip().isdigit() else None


# --- MCP over HTTP -----------------------------------------------------------------------


class McpClient:
    """Stateless Streamable HTTP (RFC 0008 §6.1): one JSON-RPC message per
    `POST /mcp`, one JSON answer, on a keep-alive connection."""

    def __init__(self, port: int, token: str | None):
        self.port, self.token, self.next_id = port, token, 0
        self.conn = http.client.HTTPConnection("127.0.0.1", port, timeout=CALL_TIMEOUT)

    def post(self, method: str, params: dict | None = None, name: str | None = None, notify: bool = False) -> tuple[int, dict | None, dict, float]:
        """Status, JSON body (None when empty), headers, client seconds."""
        msg: dict[str, Any] = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            msg["params"] = params
        if not notify:
            self.next_id += 1
            msg["id"] = self.next_id
        headers = {"Content-Type": "application/json", "Accept": "application/json, text/event-stream", "MCP-Protocol-Version": PROTOCOL, "Mcp-Method": method}
        if name:
            headers["Mcp-Name"] = name
        if self.token:
            headers["Authorization"] = f"Bearer {self.token}"
        body = json.dumps(msg).encode()
        t0 = time.perf_counter()
        try:
            self.conn.request("POST", "/mcp", body=body, headers=headers)
            r = self.conn.getresponse()
        except (http.client.HTTPException, OSError):
            self.conn.close()
            self.conn = http.client.HTTPConnection("127.0.0.1", self.port, timeout=CALL_TIMEOUT)
            t0 = time.perf_counter()
            self.conn.request("POST", "/mcp", body=body, headers=headers)
            r = self.conn.getresponse()
        raw = r.read()
        secs = time.perf_counter() - t0
        return r.status, (json.loads(raw) if raw else None), {k.lower(): v for k, v in r.getheaders()}, secs

    def initialize(self) -> dict:
        params = {"protocolVersion": PROTOCOL, "capabilities": {}, "clientInfo": {"name": "rueca-bench", "version": "0"}}
        status, body, _, secs = self.post("initialize", params)
        if status != 200 or not body or "result" not in body:
            raise RuntimeError(f"initialize failed: {status} {body}")
        nstatus, _, _, _ = self.post("notifications/initialized", notify=True)
        return {"status": status, "protocol": body["result"].get("protocolVersion"), "seconds": round(secs, 4), "initialized_status": nstatus}

    def call(self, tool: str, args: dict) -> tuple[dict, float]:
        """The JSON-RPC answer of one `tools/call` and its client seconds."""
        status, body, _, secs = self.post("tools/call", {"name": tool, "arguments": args}, name=tool)
        if status != 200 or body is None:
            return {"http_status": status, "error": body}, secs
        return body, secs


# --- calls -------------------------------------------------------------------------------


def result_ids(structured: Any) -> list[tuple[str, str]]:
    """(id, label) of every symbol in a `search_symbols` answer (grouped
    RFC 0005 results or a flat list)."""
    out = []
    for r in (structured or {}).get("results") or []:
        for s in r.get("symbols") or [r]:
            if isinstance(s, dict) and s.get("id"):
                out.append((s["id"], s.get("label") or ""))
    return out


def derived_calls(answers: list[dict], sample: int) -> list[tuple[str, dict]]:
    """`sample` get_symbol calls and up to `sample` trace calls (callable
    symbols only), every k-th distinct result id in answer order."""
    seen: dict[str, str] = {}
    for a in answers:
        for sid, label in result_ids(a):
            seen.setdefault(sid, label)
    ids = list(seen.items())
    if not ids or sample <= 0:
        return []
    step = max(1, len(ids) // sample)
    picked = ids[::step][:sample]
    callables = [x for x in ids if x[1] in CALLABLE]
    cstep = max(1, len(callables) // sample) if callables else 1
    calls: list[tuple[str, dict]] = [("get_symbol", {"id": sid}) for sid, _ in picked]
    calls += [("trace", {"id": sid, "direction": "both", "depth": 2}) for sid, _ in callables[::cstep][:sample]]
    return calls


def text_of(result: dict) -> str | None:
    for c in result.get("content") or []:
        if c.get("type") == "text":
            return c.get("text")
    return None


def first_diff(a: Any, b: Any, path: str = "$") -> dict | None:
    """The first place two JSON values differ (path, both values cut to 300
    characters), else None."""
    if type(a) is not type(b):
        return {"path": path, "http": _cut(a), "local": _cut(b)}
    if isinstance(a, dict):
        for k in sorted(set(a) | set(b)):
            if k not in a or k not in b:
                return {"path": f"{path}.{k}", "http": _cut(a.get(k, "<absent>")), "local": _cut(b.get(k, "<absent>"))}
            d = first_diff(a[k], b[k], f"{path}.{k}")
            if d:
                return d
        return None
    if isinstance(a, list):
        for i, (x, y) in enumerate(zip(a, b, strict=False)):
            d = first_diff(x, y, f"{path}[{i}]")
            if d:
                return d
        if len(a) != len(b):
            return {"path": f"{path}.length", "http": len(a), "local": len(b)}
        return None
    return None if a == b else {"path": path, "http": _cut(a), "local": _cut(b)}


def _cut(v: Any) -> Any:
    s = v if isinstance(v, str) else json.dumps(v)
    return s if len(s) <= 300 else s[:300] + "…"


def differential(bin_: Path, mirror: Path, tool: str, args: dict, answer: dict) -> dict:
    """One HTTP answer against `rueca query` on the mirror: JSON and text."""
    base = [str(bin_), "query", tool, json.dumps(args), "--org", ORG_NAME, "--store", str(mirror)]
    res = run(base, timeout=CALL_TIMEOUT)
    result = answer.get("result")
    if result is None:  # a JSON-RPC error over HTTP: the CLI must fail too
        ok = res.rc != 0
        return {"json": ok, "text": ok, "diff": None if ok else {"path": "$", "http": _cut(answer), "local": _cut(res.stdout)}}
    if res.rc != 0 or result.get("isError"):
        ok = res.rc != 0 and bool(result.get("isError"))
        return {"json": ok, "text": ok, "diff": None if ok else {"path": "$.isError", "http": _cut(result), "local": _cut(res.stderr[-300:])}}
    local = json.loads(res.stdout)
    diff = first_diff(result.get("structuredContent"), local)
    tres = run([*base, "--text"], timeout=CALL_TIMEOUT)
    text_ok = tres.rc == 0 and tres.stdout.removesuffix("\n") == (text_of(result) or "").removesuffix("\n")
    if diff is None and not text_ok:
        diff = {"path": "content[0].text", "http": _cut(text_of(result)), "local": _cut(tres.stdout)}
    return {"json": diff is None or diff["path"] == "content[0].text", "text": text_ok, "diff": diff}


# --- latency -----------------------------------------------------------------------------


def quantile(xs: list[float], q: float) -> float | None:
    """Nearest-rank quantile."""
    if not xs:
        return None
    s = sorted(xs)
    return s[min(len(s) - 1, max(0, int(-(-q * len(s) // 1)) - 1))]


def client_stats(samples: dict[str, list[float]]) -> dict:
    out = {}
    for tool, xs in sorted(samples.items()):
        ms = [x * 1000 for x in xs]
        out[tool] = {"n": len(ms), "p50_ms": _r(quantile(ms, 0.5)), "p95_ms": _r(quantile(ms, 0.95)), "max_ms": _r(max(ms))}
    return out


def _r(x: float | None) -> float | None:
    return None if x is None else round(x, 2)


_SAMPLE = re.compile(r"^([a-zA-Z_:][a-zA-Z0-9_:]*)(?:\{(.*)\})?\s+(\S+)$")
_LABEL = re.compile(r'([a-zA-Z_][a-zA-Z0-9_]*)="((?:[^"\\]|\\.)*)"')


def parse_metrics(text: str) -> list[tuple[str, dict[str, str], float]]:
    out = []
    for line in text.splitlines():
        if not line or line.startswith("#"):
            continue
        m = _SAMPLE.match(line.strip())
        if m:
            out.append((m.group(1), dict(_LABEL.findall(m.group(2) or "")), float(m.group(3))))
    return out


def histogram_quantile(q: float, buckets: dict[float, float]) -> float | None:
    """Prometheus' `histogram_quantile` over cumulative buckets {le: count}:
    linear inside the bucket holding the rank, the last finite bound when it
    falls in `+Inf`."""
    bounds = sorted(buckets)
    if not bounds or buckets[bounds[-1]] == 0:
        return None
    rank = q * buckets[bounds[-1]]
    prev_b, prev_c = 0.0, 0.0
    for b in bounds:
        c = buckets[b]
        if c >= rank:
            if b == float("inf"):
                return prev_b
            return prev_b + (b - prev_b) * ((rank - prev_c) / (c - prev_c) if c > prev_c else 1.0)
        prev_b, prev_c = b, c
    return prev_b


def server_stats(text: str) -> dict:
    """p50 / p95 (ms) of `rueca_mcp_request_seconds` per tool (every scope
    summed) and per tool and scope; `rueca_mcp_requests_total` by tool and
    outcome."""
    per_tool: dict[str, dict[float, float]] = {}
    per_scope: dict[str, dict[float, float]] = {}
    sums: dict[str, float] = {}
    counts: dict[str, float] = {}
    outcomes: dict[str, dict[str, int]] = {}
    for name, labels, v in parse_metrics(text):
        tool = labels.get("tool", "")
        if name == "rueca_mcp_request_seconds_bucket":
            le = float("inf") if labels["le"] == "+Inf" else float(labels["le"])
            for key, acc in ((tool, per_tool), (f"{tool}/{labels.get('scope', '')}", per_scope)):
                acc.setdefault(key, {})
                acc[key][le] = acc[key].get(le, 0.0) + v
        elif name == "rueca_mcp_request_seconds_sum":
            sums[tool] = sums.get(tool, 0.0) + v
        elif name == "rueca_mcp_request_seconds_count":
            counts[tool] = counts.get(tool, 0.0) + v
        elif name == "rueca_mcp_requests_total":
            outcomes.setdefault(tool, {})[labels.get("outcome", "")] = int(v)

    def q(b: dict[float, float]) -> dict:
        p50, p95 = histogram_quantile(0.5, b), histogram_quantile(0.95, b)
        return {"n": int(b.get(float("inf"), 0)), "p50_ms": _r(p50 * 1000 if p50 is not None else None), "p95_ms": _r(p95 * 1000 if p95 is not None else None)}

    tools = {t: {**q(b), "mean_ms": _r(1000 * sums[t] / counts[t]) if counts.get(t) else None} for t, b in sorted(per_tool.items())}
    return {"tools": tools, "by_scope": {k: q(b) for k, b in sorted(per_scope.items())}, "requests_total": outcomes, "generation": _gauge(text, "rueca_generation")}


def _gauge(text: str, metric: str) -> float | None:
    for name, _, v in parse_metrics(text):
        if name == metric:
            return v
    return None


# --- sizes -------------------------------------------------------------------------------


def dir_bytes(p: Path) -> int:
    return sum(f.stat().st_size for f in p.rglob("*") if f.is_file() and not f.is_symlink()) if p.exists() else 0


# --- the run -----------------------------------------------------------------------------


def run_service(small: bool = False, split: str = "gated", only: list[str] | None = None, limit: int = 10, sample: int = 20, repeat: int = 3, keep: bool = False, out: Path | None = None, bin_arg: str | None = None) -> dict:
    bin_ = rueca_bin(bin_arg)
    shutil.rmtree(WORK, ignore_errors=True)
    WORK.mkdir(parents=True)
    t_all = time.perf_counter()
    report: dict[str, Any] = {
        "suite": "service",
        "mode": "small" if small else f"org-{split}",
        "filter": sorted(only) if only else None,
        "binary": {"path": str(bin_), "version": run([str(bin_), "--version"], timeout=30).stdout.strip(), "bytes": bin_.stat().st_size},
        "args": {"limit": limit, "sample": sample, "repeat": repeat},
    }
    proc = None
    log_fh = None
    try:
        ids, queries, report["index"] = index_org(bin_, WORK, small, split, only)
        log(f"service: {len(ids)} repositories indexed in {report['index']['seconds']} s; {len(queries)} queries")
        report["store"] = publish(bin_, WORK, ids)
        token = "rueca_" + secrets.token_hex(24)
        port, ops_port = free_port(), free_port()
        cfg = WORK / "rueca-service.toml"
        write_config(cfg, report["store"]["url"], WORK / "cache", port, ops_port, token)
        check = run([str(bin_), "serve", "--config", str(cfg), "--check-config"], timeout=60)
        if check.rc != 0:
            raise RuntimeError(f"--check-config failed: {check.stdout}{check.stderr}")
        log_fh = (WORK / "serve.log").open("w")
        t0 = time.perf_counter()
        proc = subprocess.Popen([str(bin_), "serve", "--config", str(cfg), "--role", "query"], stdout=log_fh, stderr=subprocess.STDOUT)
        report["ready"] = wait_ready(proc, ops_port, t0)
        log(f"service: ready in {report['ready']['ready_seconds']} s ({report['ready']['readyz']})")

        status, _, headers, _ = McpClient(port, None).post("tools/list", {})
        report["checks"] = {"unauthenticated_status": status, "www_authenticate": headers.get("www-authenticate")}
        client = McpClient(port, token)
        report["initialize"] = client.initialize()

        lat: dict[str, list[float]] = {}
        first: dict[str, list[float]] = {}
        diffs = {"json_pass": 0, "text_pass": 0, "total": 0, "first_diff": None, "by_tool": {}}
        errors: list[dict] = []

        def ask(tool: str, args: dict, check_it: bool) -> dict:
            answer, secs = client.call(tool, args)
            lat.setdefault(tool, []).append(secs)
            if check_it:
                first.setdefault(tool, []).append(secs)
            if "result" not in answer:
                errors.append({"tool": tool, "args": args, "answer": _cut(answer)})
            if check_it:
                d = differential(bin_, WORK / "mirror", tool, args, answer)
                bt = diffs["by_tool"].setdefault(tool, {"pass": 0, "total": 0})
                bt["total"] += 1
                bt["pass"] += int(d["json"] and d["text"])
                diffs["total"] += 1
                diffs["json_pass"] += int(d["json"])
                diffs["text_pass"] += int(d["text"])
                if d["diff"] and diffs["first_diff"] is None:
                    diffs["first_diff"] = {"tool": tool, "args": args, **d["diff"]}
            return answer

        t_q = time.perf_counter()
        searches = [("search_symbols", {"query": q, "limit": limit}) for q in queries]
        answers = [ask(t, a, True) for t, a in searches]
        derived = derived_calls([(a.get("result") or {}).get("structuredContent") for a in answers], sample)
        for t, a in derived:
            ask(t, a, True)
        for _ in range(max(0, repeat - 1)):
            for t, a in searches + derived:
                ask(t, a, False)
        report["queries"] = {"search_symbols": len(searches), "derived": {t: sum(1 for x, _ in derived if x == t) for t in ("get_symbol", "trace")}, "seconds": round(time.perf_counter() - t_q, 2), "errors": len(errors), "first_error": errors[0] if errors else None}
        report["client"] = client_stats(lat)
        report["client_first_pass"] = client_stats(first)
        status, body = get(ops_port, "/metrics")
        report["server"] = server_stats(body.decode()) if status == 200 else {"error": f"/metrics answered {status}"}
        report["server"]["rss_mb"] = rss_mb(proc.pid)
        report["differential"] = diffs
        report["shutdown"] = stop(proc)
        proc = None
        report["sizes"] = {"binary_bytes": bin_.stat().st_size, "store_bytes": dir_bytes(WORK / "store"), "mirror_bytes": dir_bytes(WORK / "mirror"), "replica_cache_bytes": dir_bytes(WORK / "cache"), "index_store_bytes": dir_bytes(WORK / "index")}
        report["status"] = "ok"
    except Exception as exc:  # recorded, then re-raised after the report is written
        report["status"] = "error"
        report["error"] = f"{type(exc).__name__}: {exc}"
        tail = (WORK / "serve.log").read_text()[-3000:] if (WORK / "serve.log").exists() else ""
        report["serve_log_tail"] = tail
        raise
    finally:
        if proc is not None:
            report["shutdown"] = stop(proc)
        if log_fh is not None:
            log_fh.close()
        report["seconds"] = round(time.perf_counter() - t_all, 1)
        dest = out or RESULTS_DIR / f"service-{time.strftime('%Y%m%dT%H%M%SZ', time.gmtime())}.json"
        write_json(dest, report)
        log(f"service: report {dest}")
        if not keep:
            if not small:
                _remove_worktrees()
            shutil.rmtree(WORK, ignore_errors=True)
    _summary(report)
    return report


def _remove_worktrees() -> None:
    """Worktrees `add_org_members` left behind on an error (it removes each
    one after indexing)."""
    for p in WORK_DIR.glob("service-*"):
        shutil.rmtree(p, ignore_errors=True)


def _summary(r: dict) -> None:
    d = r["differential"]
    log(f"service: time to ready {r['ready']['ready_seconds']} s; differential {d['json_pass']}/{d['total']} JSON, {d['text_pass']}/{d['total']} text")
    for tool, c in r["client"].items():
        s = r["server"]["tools"].get(tool, {})
        log(f"  {tool:<15} client p50 {c['p50_ms']} ms p95 {c['p95_ms']} ms max {c['max_ms']} ms | server p50 {s.get('p50_ms')} ms p95 {s.get('p95_ms')} ms (n={c['n']})")
