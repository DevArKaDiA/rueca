"""codebase-memory-mcp arm, driven through `codebase-memory-mcp cli <tool> '<json>'`.

Isolation: CBM_CACHE_DIR and CBM_RUNTIME_DIR point into bench/.cache so the
pinned binary never talks to (or conflicts with) a user-level daemon of a
different version.
"""

from __future__ import annotations

import atexit
import hashlib
import json
import os
import threading
import platform
import re
import time
from pathlib import Path
from typing import Any

from ..common import BIN_DIR, CACHE, count_tokens, load_manifest, pinned_asset, run, safe_name, sha256_file
from ..efficiency import text_prefix_tokens
from .base import Arm, CallResult, Handle, is_callable_label, same_def


def cbm_platform_key() -> str:
    sysname = platform.system().lower()
    machine = platform.machine().lower()
    arch = {"x86_64": "amd64", "amd64": "amd64", "arm64": "arm64", "aarch64": "arm64"}.get(machine, machine)
    return f"{sysname}-{arch}"


def cbm_binary() -> Path:
    """The pinned binary in this cache, or in $RUECA_BENCH_SHARED_CACHE when
    its sha256 matches the pin (common.pinned_asset)."""
    spec = load_manifest()["tools"]["cbm"]["binaries"].get(cbm_platform_key(), {})
    return pinned_asset(BIN_DIR / "codebase-memory-mcp", spec.get("binary_sha256"))


def cbm_runtime_dir() -> Path:
    """CBM_RUNTIME_DIR holds the daemon's unix socket; sun_path is limited to
    104 bytes on macOS, so a deep cache path falls back to a short,
    cache-specific directory under /tmp."""
    d = CACHE / "cbm-run"
    if len(str(d)) <= 52:
        return d
    return Path("/tmp") / f"rueca-bench-cbm-{hashlib.sha256(str(CACHE).encode()).hexdigest()[:10]}"


_DAEMON_LOCK = threading.Lock()
_DAEMON: dict[str, bool] = {"started": False}


class CbmArm(Arm):
    """One warm daemon per bench process (`daemon start`, stopped at exit), as
    in an MCP session; without it every CLI call pays a temporary-daemon
    start-up (~3 s)."""

    name = "cbm"

    def __init__(self, call_timeout: float = 120.0, index_timeout: float = 1800.0):
        self.call_timeout = call_timeout
        self.index_timeout = index_timeout
        self.bin = cbm_binary()
        self.index_mode = load_manifest()["tools"]["cbm"].get("index_mode", "full")
        self.env = {
            "CBM_CACHE_DIR": str(CACHE / "cbm"),
            "CBM_RUNTIME_DIR": str(cbm_runtime_dir()),
        }
        for d in self.env.values():
            Path(d).mkdir(parents=True, exist_ok=True)
            os.chmod(d, 0o700)

    def available(self) -> tuple[bool, str]:
        if not self.bin.exists():
            return False, f"{self.bin} missing (run `rueca-bench setup`)"
        man = load_manifest()["tools"]["cbm"]
        want = man["binaries"].get(cbm_platform_key(), {}).get("binary_sha256")
        if want and sha256_file(self.bin) != want:
            return False, f"sha256 mismatch for {self.bin}"
        return True, ""

    def version(self) -> str:
        res = run([str(self.bin), "--version"], env=self.env, timeout=30)
        return res.stdout.strip() or res.stderr.strip()

    def _ensure_daemon(self) -> None:
        with _DAEMON_LOCK:
            if _DAEMON["started"]:
                return
            # the graph UI is irrelevant here and would contend for port 9749
            run([str(self.bin), "config", "set", "ui_enabled", "false"], env=self.env, timeout=60)
            for _ in range(10):
                run([str(self.bin), "daemon", "start"], env=self.env, timeout=120)
                st = run([str(self.bin), "daemon", "status"], env=self.env, timeout=60)
                if "active (permanent)" in st.stdout:
                    break
                time.sleep(3)  # a previous bench process may still be stopping its daemon
            _DAEMON["started"] = True
            atexit.register(lambda: run([str(self.bin), "daemon", "stop"], env=self.env, timeout=120))

    # -- plumbing ---------------------------------------------------------------

    def _call(self, tool: str, args: dict[str, Any], timeout: float | None = None) -> tuple[Any, float, str | None]:
        data, secs, err, _ = self._call_raw(tool, args, timeout)
        return data, secs, err

    def _call_raw(self, tool: str, args: dict[str, Any], timeout: float | None = None) -> tuple[Any, float, str | None, str]:
        self._ensure_daemon()
        res = run(
            [str(self.bin), "cli", "--quiet", tool, json.dumps(args)],
            env=self.env,
            timeout=timeout or self.call_timeout,
        )
        if res.timed_out:
            return None, res.seconds, f"timeout after {res.seconds:.0f}s", ""
        try:
            data = json.loads(res.stdout) if res.stdout.strip() else None
        except json.JSONDecodeError:
            return None, res.seconds, f"non-json output rc={res.rc}: {res.stdout[:300]} {res.stderr[-300:]}", res.stdout
        if res.rc != 0 or data is None or (isinstance(data, dict) and "error" in data and not data.get("rows")):
            err = data.get("error") if isinstance(data, dict) else res.stderr[-500:]
            return data, res.seconds, f"rc={res.rc}: {err}", res.stdout
        return data, res.seconds, None, res.stdout

    def agent_text(self, tool: str, args: dict[str, Any]) -> str | None:
        """The MCP text content an agent reads for this request: cbm's
        default `tree` rendering (the same request without `format`)."""
        self._ensure_daemon()
        args = {k: v for k, v in args.items() if k not in ("format",)}
        res = run([str(self.bin), "cli", "--quiet", tool, json.dumps(args)], env=self.env, timeout=self.call_timeout)
        if res.timed_out or res.rc != 0:
            return None
        return res.stdout

    @staticmethod
    def _lines(s: Any) -> tuple[int | None, int | None]:
        if isinstance(s, str):
            m = re.match(r"(\d+)(?:-(\d+))?", s)
            if m:
                a = int(m.group(1))
                return a, int(m.group(2) or a)
        return None, None

    # -- index ------------------------------------------------------------------

    def index(self, root: Path, key: str, language: str) -> Handle:
        project = safe_name(f"tb-{key}")[:120]
        h = Handle(self.name, key, root, language, state={"project": project, "nodes": None})
        self._call("delete_project", {"project": project})
        data, secs, err = self._call(
            "index_repository",
            {"repo_path": str(root), "name": project, "mode": self.index_mode},
            timeout=self.index_timeout,
        )
        h.index_seconds = secs
        if err or not isinstance(data, dict) or data.get("status") not in ("indexed", "succeeded", "ok"):
            h.ok = False
            h.error = err or f"unexpected index status: {str(data)[:300]}"
            return h
        h.info = {
            "nodes": data.get("nodes"),
            "edges": data.get("edges"),
            "parse_partial_count": data.get("parse_partial_count"),
            "parse_unusable_count": data.get("parse_unusable_count"),
        }
        return h

    def _nodes(self, h: Handle) -> dict[str, dict]:
        """qualified_name -> brief for every Function/Method node (lazy, cached;
        follows query_graph's snapshot cursor until exhausted)."""
        if h.state["nodes"] is None:
            q = (
                'MATCH (n) WHERE n.label = "Function" OR n.label = "Method" '
                "RETURN n.qualified_name, n.label, n.file_path, n.start_line, n.end_line"
            )
            nodes: dict[str, dict] = {}
            every: list[dict] = []
            args: dict[str, Any] = {
                "project": h.state["project"],
                "query": q,
                "format": "json",
                "max_rows": 99998,
                "max_output_tokens": 1000000,
            }
            errors = []
            for _ in range(200):
                data, _, err = self._call("query_graph", args)
                if err or not isinstance(data, dict):
                    errors.append(err)
                    break
                for qn, label, path, s, e in data.get("rows", []):
                    node = {
                        "path": path,
                        "name": qn.rsplit(".", 1)[-1],
                        "label": label,
                        "start_line": int(s) if str(s).isdigit() else None,
                        "end_line": int(e) if str(e).isdigit() else None,
                        "qn": qn,
                    }
                    every.append(node)
                    nodes.setdefault(qn, node)
                cur = data.get("next_cursor")
                if not data.get("has_more") or not cur:
                    break
                args = {k: v for k, v in args.items() if k != "offset"}
                args["cursor"] = cur
            h.state["nodes"] = nodes
            # every node, overloads included (one qualified name, several nodes)
            h.state["every_node"] = every
            h.state["nodes_errors"] = errors
        return h.state["nodes"]

    def _search(self, h: Handle, args: dict, limit: int, text: bool = True) -> CallResult:
        args = {
            "project": h.state["project"],
            "limit": min(limit, 500),
            "format": "json",
            "max_output_tokens": 1000000,
            **args,
        }
        data, secs, err, raw = self._call_raw("search_graph", args)
        if err:
            return CallResult(False, secs, error=err)
        hits = []
        cols = data.get("cols", [])
        for row in data.get("rows", []):
            r = dict(zip(cols, row))
            s, e = self._lines(r.get("lines"))
            qn = r.get("qn", "")
            hits.append(
                {
                    "path": r.get("file"),
                    "name": qn.rsplit(".", 1)[-1],
                    "label": r.get("label"),
                    "start_line": s,
                    "end_line": e,
                    "qn": qn,
                    "rank": r.get("rank"),
                }
            )
        txt = self.agent_text("search_graph", args) if text else None
        extra = {"total": data.get("total"), "tokens": count_tokens(txt) if txt is not None else None, "tokens_json": count_tokens(raw)}
        if txt is not None:
            extra["prefix"], extra["approx"] = text_prefix_tokens(txt, len(hits))
            extra["format"] = "tree"
        return CallResult(True, secs, hits=hits, extra=extra)

    # -- S1 ---------------------------------------------------------------------

    def localize(self, h: Handle, query: str, limit: int = 100) -> CallResult:
        res = self._search(h, {"query": query}, limit, text=False)
        if not res.ok:
            return res
        files: list[str] = []
        funcs: list[dict] = []
        for hit in res.hits:
            p = hit["path"]
            if p and p not in files and not p.startswith("<"):
                files.append(p)
            if is_callable_label(hit["label"]):
                funcs.append(hit)
        res.extra.update({"files": files, "functions": funcs})
        return res

    # -- S3 ---------------------------------------------------------------------

    def resolve(self, h: Handle, func: dict) -> CallResult:
        t0 = time.perf_counter()
        self._nodes(h)
        for n in h.state["every_node"]:
            if same_def(n, func):
                return CallResult(True, time.perf_counter() - t0, hits=[n], extra={"ref": n["qn"]})
        return CallResult(False, time.perf_counter() - t0, error="definition not found in graph")

    def neighbors(self, h: Handle, ref: Any) -> CallResult:
        """One `trace_path` (direction `both`, depth 1); neighbours are mapped
        back to Function / Method nodes by qualified name."""
        nodes = self._nodes(h)
        args = {
            "project": h.state["project"],
            "function_name": ref,
            "direction": "both",
            "depth": 1,
            "limit": 5000,
            "format": "json",
            "max_output_tokens": 1000000,
        }
        data, secs, err, raw = self._call_raw("trace_path", args)
        if err:
            return CallResult(False, secs, error=err)
        if isinstance(data, dict) and data.get("status") == "ambiguous":
            return CallResult(False, secs, error="ambiguous")
        txt = self.agent_text("trace_path", args)
        out: dict[str, Any] = {"tokens": count_tokens(txt) if txt is not None else None, "tokens_json": count_tokens(raw), "unmapped": 0}
        for key in ("callers", "callees"):
            block = data.get(key) or {}
            cols = block.get("cols", ["name", "hop"])
            hits: list[dict] = []
            for g in block.get("groups", []):
                prefix = g.get("qn_prefix", "")
                for row in g.get("rows", []):
                    r = dict(zip(cols, row))
                    if r.get("hop", 1) != 1:
                        continue
                    qn = f"{prefix}.{r['name']}" if prefix else r["name"]
                    n = nodes.get(qn)
                    if n is None:
                        out["unmapped"] += 1
                        continue
                    hits.append(n)
            out[key] = hits
            out[f"{key}_total"] = data.get(f"{key}_total")
        return CallResult(True, secs, extra=out)

    def lookup(self, h: Handle, name: str, limit: int = 10) -> CallResult:
        return self._search(h, {"query": name}, limit)

    def search_eff(self, h: Handle, query: str, limit: int = 10) -> CallResult:
        """`search_graph` as an agent issues it (limit `limit`): hits in
        rendered order, tokens of the tree text, prefix cost per item."""
        return self._search(h, {"query": query}, limit)

    # -- data suite ----------------------------------------------------------------

    def _rows(self, h: Handle, query: str) -> tuple[list[list], str | None]:
        """All rows of a Cypher query (follows the snapshot cursor)."""
        args: dict[str, Any] = {"project": h.state["project"], "query": query, "format": "json", "max_rows": 99998, "max_output_tokens": 1000000}
        rows: list[list] = []
        for _ in range(200):
            data, _, err = self._call("query_graph", args)
            if err or not isinstance(data, dict):
                return rows, err or "bad response"
            rows += data.get("rows", [])
            cur = data.get("next_cursor")
            if not data.get("has_more") or not cur:
                break
            args["cursor"] = cur
        return rows, None

    def data_nodes(self, h: Handle, files: list[str]) -> CallResult:
        t0 = time.perf_counter()
        want = set(files)
        rows, err = self._rows(h, "MATCH (n) RETURN n.label, n.name, n.file_path, n.start_line")
        hits = [
            {"path": p, "name": nm, "label": lb, "start_line": int(s)}
            for lb, nm, p, s in rows
            if p in want and lb not in ("File", "Folder", "Module", "Project") and str(s).isdigit() and int(s) > 0
        ]
        return CallResult(err is None, time.perf_counter() - t0, hits=hits, error=err)

    def env_edges(self, h: Handle, names: list[str]) -> CallResult:
        """cbm `CONFIGURES` edges: `env_access` (code -> canonical
        `__env__NAME`) are reads; `env_set` (resource -> per-container
        `...#env/<container>/NAME` at the entry's line) are writes."""
        t0 = time.perf_counter()
        rows, err = self._rows(
            h,
            "MATCH (a)-[r:CONFIGURES]->(b) RETURN a.label, a.name, a.file_path, a.start_line, b.name, b.qualified_name, b.file_path, b.start_line, r.strategy",
        )
        reads, writes = [], []
        for alb, aname, apath, aline, bname, bqn, bpath, bline, strat in rows:
            if strat == "env_access" and str(bqn).startswith("__env__"):
                func = aname if alb in ("Function", "Method") else "<module>"
                reads.append({"path": apath, "function": func, "env": bqn[len("__env__"):], "line": int(aline) if str(aline).isdigit() else None})
            elif strat == "env_set":
                env = str(bqn).rsplit("/", 1)[-1]
                writes.append({"path": bpath or apath, "env": env, "line": int(bline) if str(bline).isdigit() else None})
        return CallResult(err is None, time.perf_counter() - t0, error=err, extra={"reads": reads, "writes": [w for w in writes if w["line"]]})

    def cleanup(self, h: Handle) -> None:
        self._call("delete_project", {"project": h.state["project"]})
