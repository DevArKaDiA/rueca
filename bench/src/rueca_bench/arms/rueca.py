"""Rueca arm, driven through the alpha CLI contract (docs/tools.md):

  rueca index <path> --store <dir> --json
  rueca query <tool> '<json>' --root <path> --store <dir>

The binary comes from $RUECA_BIN (or `rueca` on PATH when RUECA_BIN=rueca).
"""

from __future__ import annotations

import json
import os
import shutil
import time
from pathlib import Path
from typing import Any

from ..common import CACHE, run, safe_name, sha256_file
from .base import Arm, CallResult, Handle, is_callable_label, line_match


def _brief(b: dict) -> dict:
    return {
        "path": b.get("path"),
        "name": b.get("name"),
        "label": b.get("label"),
        "start_line": b.get("start_line"),
        "end_line": b.get("end_line"),
        "id": b.get("id"),
        "qualified_name": b.get("qualified_name"),
    }


class RuecaArm(Arm):
    name = "rueca"

    def __init__(self, call_timeout: float = 120.0, index_timeout: float = 1800.0):
        self.call_timeout = call_timeout
        self.index_timeout = index_timeout
        tb = os.environ.get("RUECA_BIN", "")
        self.bin = shutil.which(tb) if tb and os.path.sep not in tb else (tb or None)

    def available(self) -> tuple[bool, str]:
        if not self.bin:
            return False, "RUECA_BIN not set"
        if not Path(self.bin).exists():
            return False, f"RUECA_BIN={self.bin} does not exist"
        return True, ""

    def version(self) -> str:
        if not self.bin:
            return "pending (RUECA_BIN not set)"
        res = run([self.bin, "--version"], timeout=30)
        ver = (res.stdout.strip() or res.stderr.strip() or "unknown").splitlines()[0]
        # RUECA_BIN_LABEL names the source of a branch build (e.g.
        # `feat/lang-go@af4b3b6`), so the report can say which binary
        # produced each language's numbers.
        label = os.environ.get("RUECA_BIN_LABEL")
        return f"{ver} (binary sha256 {sha256_file(Path(self.bin))[:12]}{', ' + label if label else ''})"

    def _store(self, key: str) -> Path:
        return CACHE / "rueca-store" / safe_name(key)

    def _query(self, h: Handle, tool: str, args: dict[str, Any]) -> tuple[Any, float, str | None]:
        res = run(
            [self.bin, "query", tool, json.dumps(args), "--root", str(h.root), "--store", str(h.state["store"])],
            timeout=self.call_timeout,
        )
        if res.timed_out:
            return None, res.seconds, f"timeout after {res.seconds:.0f}s"
        try:
            data = json.loads(res.stdout)
        except json.JSONDecodeError:
            return None, res.seconds, f"non-json output rc={res.rc}: {res.stdout[:300]} {res.stderr[-300:]}"
        if res.rc != 0 or (isinstance(data, dict) and "error" in data):
            return data, res.seconds, f"rc={res.rc}: {data.get('error') if isinstance(data, dict) else ''}"
        return data, res.seconds, None

    def index(self, root: Path, key: str, language: str) -> Handle:
        store = self._store(key)
        shutil.rmtree(store, ignore_errors=True)
        store.mkdir(parents=True, exist_ok=True)
        h = Handle(self.name, key, root, language, state={"store": store})
        res = run([self.bin, "index", str(root), "--store", str(store), "--json"], timeout=self.index_timeout)
        h.index_seconds = res.seconds
        if res.timed_out or res.rc != 0:
            h.ok = False
            h.error = f"index failed rc={res.rc} timed_out={res.timed_out}: {res.stderr[-500:]} {res.stdout[-300:]}"
            return h
        try:
            rep = json.loads(res.stdout)
            h.info = {k: rep.get(k) for k in ("files_total", "files_extracted", "references_total", "references_resolved", "duration_ms")}
        except json.JSONDecodeError:
            h.info = {"raw": res.stdout[-300:]}
        return h

    def _search(self, h: Handle, query: str, limit: int) -> CallResult:
        data, secs, err = self._query(h, "search_symbols", {"query": query, "limit": min(limit, 200)})
        if err:
            return CallResult(False, secs, error=err)
        return CallResult(True, secs, hits=[_brief(b) for b in data.get("results", [])])

    def localize(self, h: Handle, query: str, limit: int = 100) -> CallResult:
        res = self._search(h, query, limit)
        if not res.ok:
            return res
        files: list[str] = []
        funcs: list[dict] = []
        for hit in res.hits:
            if hit["path"] and hit["path"] not in files:
                files.append(hit["path"])
            if is_callable_label(hit["label"]):
                funcs.append(hit)
        res.extra.update({"files": files, "functions": funcs})
        return res

    def resolve(self, h: Handle, func: dict) -> CallResult:
        data, secs, err = self._query(h, "get_symbol", {"name": func["name"], "path": func["path"]})
        if err or not isinstance(data, dict):
            return CallResult(False, secs, error=err or "bad response")
        cands = [data.get("symbol") or {}] + list(data.get("candidates") or [])
        for c in cands:
            if c and c.get("path") == func["path"] and c.get("name") == func["name"] and line_match(c.get("start_line"), func):
                return CallResult(True, secs, hits=[_brief(c)], extra={"ref": c.get("id")})
        return CallResult(False, secs, error="no candidate matched path+name+line")

    def neighbors(self, h: Handle, ref: Any, direction: str) -> CallResult:
        data, secs, err = self._query(h, "trace", {"id": ref, "direction": direction, "depth": 1, "limit": 2000})
        if err:
            return CallResult(False, secs, error=err)
        hits = [
            _brief(n)
            for n in data.get("nodes", [])
            if n.get("depth", 1) == 1 and n.get("direction", direction) == direction
        ]
        return CallResult(True, secs, hits=hits, extra={"truncated": data.get("truncated")})

    def lookup(self, h: Handle, name: str, limit: int = 10) -> CallResult:
        return self._search(h, name, limit)

    # -- data suite ----------------------------------------------------------------

    def data_nodes(self, h: Handle, files: list[str]) -> CallResult:
        """`get_file_outline` of every data file, flattened."""
        t0 = time.perf_counter()
        hits: list[dict] = []
        errors = 0

        def walk(path: str, syms: list[dict]) -> None:
            for s in syms:
                hits.append({"path": s.get("path") or path, "name": s.get("name"), "label": s.get("label"), "start_line": s.get("start_line")})
                walk(path, s.get("children") or [])

        for f in files:
            data, _, err = self._query(h, "get_file_outline", {"path": f})
            if err or not isinstance(data, dict):
                errors += 1
                continue
            walk(f, data.get("symbols") or [])
        return CallResult(True, time.perf_counter() - t0, hits=hits, extra={"outline_errors": errors})

    def env_edges(self, h: Handle, names: list[str]) -> CallResult:
        """`trace` callers over READS / WRITES from `$env:NAME` for every
        candidate name (Rueca has no enumeration tool)."""
        t0 = time.perf_counter()
        reads, writes = [], []
        for name in names:
            data, _, err = self._query(
                h,
                "trace",
                {"qualified_name": f"$env:{name}", "direction": "callers", "edge_types": ["READS", "WRITES"], "depth": 1, "limit": 2000, "include_tests": True},
            )
            if err or not isinstance(data, dict):
                continue
            root = (data.get("root") or {}).get("id")
            nodes = {n.get("id"): n for n in data.get("nodes", [])}
            for e in data.get("edges", []):
                if e.get("dst") != root:
                    continue
                src = nodes.get(e.get("src")) or {}
                if e.get("kind") == "READS":
                    func = src.get("name") if src.get("label") in ("Function", "Method", "Test") else "<module>"
                    reads.append({"path": src.get("path") or (e.get("call_site") or {}).get("path"), "function": func, "env": name, "line": (e.get("call_site") or {}).get("line")})
                elif e.get("kind") == "WRITES":
                    writes.append({"path": src.get("path"), "env": name, "line": src.get("start_line")})
        return CallResult(True, time.perf_counter() - t0, extra={"reads": [r for r in reads if r["path"]], "writes": [w for w in writes if w["path"] and w["line"]]})

    def cleanup(self, h: Handle) -> None:
        shutil.rmtree(h.state["store"], ignore_errors=True)
