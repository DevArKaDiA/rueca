"""Baseline arm: no graph. BM25 over file contents, function names, function bodies;
textual name-match heuristic for callers/callees."""

from __future__ import annotations

import re
import time
from pathlib import Path

from ..bm25 import BM25
from ..common import is_code_file
from ..corpus import list_files
from ..queries import tokenize
from ..treesitter import extract_functions
from .base import Arm, CallResult, Handle

MAX_FILE_BYTES = 1_000_000


class GrepArm(Arm):
    name = "grep"

    def version(self) -> str:
        return "rueca-bench bm25 (k1=1.2,b=0.75) + tree-sitter"

    def index(self, root: Path, key: str, language: str) -> Handle:
        t0 = time.perf_counter()
        h = Handle(self.name, key, root, language)
        file_docs = []
        name_docs = []
        body_docs = []
        funcs: dict[str, dict] = {}
        bodies: dict[str, str] = {}
        for rel in list_files(root):
            if not is_code_file(rel, language):
                continue
            p = root / rel
            try:
                if p.is_symlink() or not p.is_file() or p.stat().st_size > MAX_FILE_BYTES:
                    continue
                data = p.read_bytes()
            except OSError:
                continue
            text = data.decode("utf-8", errors="replace")
            file_docs.append((rel, tokenize(text) + tokenize(rel)))
            lines = text.splitlines()
            for f in extract_functions(rel, data):
                fid = f"{rel}#{f.qualname}@{f.start}"
                funcs[fid] = {
                    "path": rel,
                    "name": f.name,
                    "qualname": f.qualname,
                    "label": f.label,
                    "start_line": f.start,
                    "end_line": f.end,
                    "def_line": f.def_line,
                }
                body = "\n".join(lines[f.start - 1 : f.end])
                bodies[fid] = body
                name_docs.append((fid, tokenize(f.qualname)))
                body_docs.append((fid, tokenize(body)))
        h.state = {
            "files": BM25(file_docs),
            "names": BM25(name_docs),
            "bodies": BM25(body_docs),
            "funcs": funcs,
            "bodies_text": bodies,
        }
        h.index_seconds = time.perf_counter() - t0
        h.info = {"files": len(file_docs), "functions": len(funcs)}
        return h

    def localize(self, h: Handle, query: str, limit: int = 100) -> CallResult:
        t0 = time.perf_counter()
        q = tokenize(query)
        st = h.state
        files = [k for k, _ in st["files"].search(q, limit)]
        fn_names = [dict(st["funcs"][k]) for k, _ in st["names"].search(q, limit)]
        fn_bodies = [dict(st["funcs"][k]) for k, _ in st["bodies"].search(q, limit)]
        return CallResult(
            True,
            time.perf_counter() - t0,
            hits=[],
            # primary function ranking = BM25 over function bodies (signature
            # included); name-only BM25 is kept as a secondary ranking.
            extra={"files": files, "functions": fn_bodies, "functions_name": fn_names},
        )

    def resolve(self, h: Handle, func: dict) -> CallResult:
        t0 = time.perf_counter()
        for fid, f in h.state["funcs"].items():
            if f["path"] == func["path"] and f["name"] == func["name"] and abs(f["start_line"] - func["start"]) <= 2:
                return CallResult(True, time.perf_counter() - t0, hits=[f], extra={"ref": fid})
        return CallResult(False, time.perf_counter() - t0, error="not found")

    def neighbors(self, h: Handle, ref, direction: str) -> CallResult:
        """Textual heuristic: callees = defined function names followed by '(' in
        the body; callers = functions whose body contains `name(`."""
        t0 = time.perf_counter()
        st = h.state
        me = st["funcs"][ref]
        out: dict[str, dict] = {}
        if direction == "callees":
            body = st["bodies_text"][ref]
            body = body.split("\n", 1)[1] if "\n" in body else ""
            called = set(re.findall(r"([A-Za-z_$][A-Za-z0-9_$]*)\s*\(", body))
            for fid, f in st["funcs"].items():
                if f["name"] in called and fid != ref:
                    out[fid] = f
        else:
            pat = re.compile(r"(?<![A-Za-z0-9_$])" + re.escape(me["name"]) + r"\s*\(")
            for fid, body in st["bodies_text"].items():
                if fid == ref:
                    continue
                rest = body.split("\n", 1)[1] if "\n" in body else ""
                if pat.search(rest):
                    out[fid] = st["funcs"][fid]
        return CallResult(True, time.perf_counter() - t0, hits=list(out.values()))

    def lookup(self, h: Handle, name: str, limit: int = 10) -> CallResult:
        t0 = time.perf_counter()
        hits = [dict(h.state["funcs"][k]) for k, _ in h.state["names"].search(tokenize(name), limit)]
        return CallResult(True, time.perf_counter() - t0, hits=hits)
