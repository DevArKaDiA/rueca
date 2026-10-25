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

from ..common import CACHE, count_tokens, run, safe_name, sha256_file
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
        data, secs, err, _ = self._query_raw(h, tool, args)
        return data, secs, err

    def _query_raw(self, h: Handle, tool: str, args: dict[str, Any]) -> tuple[Any, float, str | None, str]:
        """(parsed answer, seconds, error, raw stdout): one `rueca query`
        process per call, on the segment the index step wrote (warm OS page
        cache; the segment is opened by each process)."""
        res = run(
            [self.bin, "query", tool, json.dumps(args), "--root", str(h.root), "--store", str(h.state["store"])],
            timeout=self.call_timeout,
        )
        if res.timed_out:
            return None, res.seconds, f"timeout after {res.seconds:.0f}s", ""
        try:
            data = json.loads(res.stdout)
        except json.JSONDecodeError:
            return None, res.seconds, f"non-json output rc={res.rc}: {res.stdout[:300]} {res.stderr[-300:]}", res.stdout
        if res.rc != 0 or (isinstance(data, dict) and "error" in data):
            return data, res.seconds, f"rc={res.rc}: {data.get('error') if isinstance(data, dict) else ''}", res.stdout
        return data, res.seconds, None, res.stdout

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
            h.info = {k: rep.get(k) for k in ("files_total", "files_extracted", "files_partial", "references_total", "references_resolved", "duration_ms")}
        except json.JSONDecodeError:
            h.info = {"raw": res.stdout[-300:]}
        return h

    def _search(self, h: Handle, query: str, limit: int) -> CallResult:
        data, secs, err, raw = self._query_raw(h, "search_symbols", {"query": query, "limit": min(limit, 200)})
        if err:
            return CallResult(False, secs, error=err)
        return CallResult(True, secs, hits=[_brief(b) for b in data.get("results", [])], extra={"tokens": count_tokens(raw)})

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
        """`get_symbol` by name and path; when neither the symbol nor its
        candidates match (many same-named methods in one file, overloads),
        the file's `get_file_outline` (every symbol of the file, cached per
        index) is scanned — as exhaustive as the cbm arm's node scan."""
        data, secs, err = self._query(h, "get_symbol", {"name": func["name"], "path": func["path"]})
        if not err and isinstance(data, dict):
            cands = [data.get("symbol") or {}] + list(data.get("candidates") or [])
            for c in cands:
                if c and c.get("path") == func["path"] and c.get("name") == func["name"] and line_match(c.get("start_line"), func):
                    return CallResult(True, secs, hits=[_brief(c)], extra={"ref": c.get("id")})
        outlines = h.state.setdefault("outlines", {})
        if func["path"] not in outlines:
            odata, osecs, oerr = self._query(h, "get_file_outline", {"path": func["path"]})
            secs += osecs
            flat: list[dict] = []

            def walk(syms: list[dict]) -> None:
                for sym in syms:
                    flat.append(sym)
                    walk(sym.get("children") or [])

            if not oerr and isinstance(odata, dict):
                walk(odata.get("symbols") or [])
            outlines[func["path"]] = flat
        for c in outlines[func["path"]]:
            if c.get("name") == func["name"] and is_callable_label(c.get("label")) and line_match(c.get("start_line"), func):
                return CallResult(True, secs, hits=[_brief(dict(c, path=func["path"]))], extra={"ref": c.get("id")})
        return CallResult(False, secs, error=err or "no candidate or outline symbol matched path+name+line")

    def neighbors(self, h: Handle, ref: Any) -> CallResult:
        """One `trace` (direction `both`, depth 1, default edge types and
        test exclusion): each depth-1 neighbour with the best confidence of
        the edges that link it to the root in that direction."""
        data, secs, err, raw = self._query_raw(h, "trace", {"id": ref, "direction": "both", "depth": 1, "limit": 2000})
        if err:
            return CallResult(False, secs, error=err)
        root = (data.get("root") or {}).get("id") or ref
        conf: dict[tuple[str, str], float] = {}
        for e in data.get("edges", []):
            c = e.get("confidence")
            c = float(c) if c is not None else 1.0
            if e.get("dst") == root:
                k = ("callers", e.get("src"))
                conf[k] = max(conf.get(k, 0.0), c)
            if e.get("src") == root:
                k = ("callees", e.get("dst"))
                conf[k] = max(conf.get(k, 0.0), c)
        out: dict[str, list[dict]] = {"callers": [], "callees": []}
        for n in data.get("nodes", []):
            d = n.get("direction")
            if n.get("depth", 1) != 1 or d not in out:
                continue
            b = _brief(n)
            b["confidence"] = conf.get((d, n.get("id")))
            out[d].append(b)
        return CallResult(True, secs, extra={**out, "truncated": data.get("truncated"), "tokens": count_tokens(raw)})

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

    # -- health (RFC 0004 §5, S5) ---------------------------------------------------

    def health(self, root: Path, key: str, files: list[str]) -> dict:
        """Parse-error rate and index throughput of one language: `files`
        (repository-relative, every file of the language) are hard-linked
        into a scratch tree (no copy, same relative paths, so the walk's
        excludes apply as in the repository) that is indexed alone, so
        `files_partial / files_extracted` of the IndexReport is the rate
        for that language."""
        scratch = CACHE / "health" / safe_name(key)
        shutil.rmtree(scratch, ignore_errors=True)
        tree = scratch / "tree"
        for rel in files:
            src = root / rel
            if src.is_symlink() or not src.is_file():
                continue
            dst = tree / rel
            dst.parent.mkdir(parents=True, exist_ok=True)
            try:
                os.link(src, dst)
            except OSError:
                shutil.copy2(src, dst)
        store = scratch / "store"
        store.mkdir(parents=True, exist_ok=True)
        try:
            res = run([self.bin, "index", str(tree), "--store", str(store), "--json"], timeout=self.index_timeout)
            if res.rc != 0:
                return {"ok": False, "error": f"index failed rc={res.rc}: {res.stderr[-300:]}", "seconds": res.seconds}
            rep = json.loads(res.stdout)
            return {
                "ok": True,
                "seconds": res.seconds,
                "files_listed": len(files),
                **{k: rep.get(k) for k in ("files_total", "files_extracted", "files_partial", "files_skipped", "duration_ms", "references_total", "references_resolved")},
            }
        finally:
            shutil.rmtree(scratch, ignore_errors=True)

    def cleanup(self, h: Handle) -> None:
        shutil.rmtree(h.state["store"], ignore_errors=True)
