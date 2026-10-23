"""S1 gold: files and functions touched by the gold patch, at the base commit.

Method (documented in bench/README.md):
- Files: every file the patch modifies or deletes that exists at the base
  commit, has a source extension of the instance language, and is not a test
  path. Newly created files are excluded (they cannot be localized).
- Functions: parse each gold file at the base commit with tree-sitter. Every
  removed/modified old-side line maps to its innermost enclosing
  function/method. A pure insertion after old line k maps to the innermost
  function containing both k and k+1 (Python: also the function containing k
  when the inserted, non-blank line is indented deeper than the `def`), so
  appending a new top-level function does not count as editing its neighbour.
"""

from __future__ import annotations

import ast
import json
import re
from dataclasses import dataclass, field

from .common import candidate_path
from .corpus import git_show
from .treesitter import Func, extract_functions, grammar_for, innermost, innermost_strict

_HUNK = re.compile(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@")


@dataclass
class FilePatch:
    old_path: str | None
    new_path: str | None
    changed_old_lines: set[int] = field(default_factory=set)
    insertions: list[tuple[int, str]] = field(default_factory=list)  # (after old line k, added text)


def parse_patch(patch: str) -> list[FilePatch]:
    files: list[FilePatch] = []
    cur: FilePatch | None = None
    old_line = 0
    in_hunk = False
    for raw in patch.splitlines():
        if raw.startswith("diff --git "):
            cur = FilePatch(None, None)
            files.append(cur)
            in_hunk = False
            m = re.match(r"diff --git a/(.*) b/(.*)$", raw)
            if m:
                cur.old_path, cur.new_path = m.group(1), m.group(2)
            continue
        if cur is None:
            continue
        if not in_hunk and raw.startswith("--- "):
            p = raw[4:].strip()
            cur.old_path = None if p == "/dev/null" else (p[2:] if p.startswith("a/") else p)
            continue
        if not in_hunk and raw.startswith("+++ "):
            p = raw[4:].strip()
            cur.new_path = None if p == "/dev/null" else (p[2:] if p.startswith("b/") else p)
            continue
        if raw.startswith("new file mode"):
            cur.old_path = None
            continue
        m = _HUNK.match(raw)
        if m:
            in_hunk = True
            old_line = int(m.group(1))
            continue
        if not in_hunk:
            continue
        if raw.startswith("-"):
            cur.changed_old_lines.add(old_line)
            old_line += 1
        elif raw.startswith("+"):
            cur.insertions.append((old_line - 1, raw[1:]))
        elif raw.startswith(" ") or raw == "":
            old_line += 1
        elif raw.startswith("\\"):
            pass
    return files


def _indent(text: str) -> int:
    return len(text) - len(text.lstrip(" \t"))


def functions_for_patch(fp: FilePatch, funcs: list[Func], python: bool) -> list[Func]:
    hit: dict[tuple[int, str], Func] = {}
    for line in fp.changed_old_lines:
        f = innermost(funcs, line)
        if f is not None:
            hit[(f.start, f.qualname)] = f
    for k, text in fp.insertions:
        f = innermost_strict(funcs, k, k + 1)
        if f is None and python and text.strip():
            g = innermost(funcs, k)
            if g is not None and _indent(text) > g.indent:
                f = g
        if f is not None:
            hit[(f.start, f.qualname)] = f
    return sorted(hit.values(), key=lambda f: (f.start, f.qualname))


def compute_s1_gold(repo: str, base_commit: str, patch: str, language: str) -> dict:
    gold_files: list[str] = []
    gold_funcs: list[dict] = []
    skipped: list[dict] = []
    for fp in parse_patch(patch):
        path = fp.old_path
        if path is None:
            skipped.append({"path": fp.new_path, "reason": "new-file"})
            continue
        if not candidate_path(path, language):
            skipped.append({"path": path, "reason": "not-source-or-test"})
            continue
        src = git_show(repo, base_commit, path)
        if src is None:
            skipped.append({"path": path, "reason": "missing-at-base"})
            continue
        gold_files.append(path)
        if grammar_for(path) is None:
            continue
        funcs = extract_functions(path, src.encode("utf-8"))
        for f in functions_for_patch(fp, funcs, python=path.endswith((".py", ".pyi"))):
            d = f.as_dict()
            d["path"] = path
            gold_funcs.append(d)
    return {"files": sorted(set(gold_files)), "functions": gold_funcs, "skipped": skipped}


# --- assembled gold (RFC 0002 §8/§9: prefer existing labels) -------------------
#
# Function-level edit gold, in order of preference:
#   1. LocAgent `edit_functions` (czlll/SWE-bench_Lite) — "path:Class.func".
#   2. SWE-PolyBench `modified_nodes` — "path->program->kind:name->kind:name".
#   3. Telar's tree-sitter labeler above (all other instances).
# Labels are mapped onto tree-sitter definitions at the base commit (to get
# def lines for matching); unmappable labels are counted, not guessed.
# ContextBench `gold_context` spans give a secondary file set (human-annotated
# context needed to solve the task), scored as context-file recall.

_NON_CALLABLE_KINDS = ("class", "interface", "enum", "type_alias", "module", "namespace")


def _funcs_at(repo: str, base: str, path: str, cache: dict) -> list[Func] | None:
    if path not in cache:
        src = git_show(repo, base, path)
        cache[path] = None if src is None else extract_functions(path, src.encode("utf-8"))
    return cache[path]


def _map_label(funcs: list[Func], qual: str) -> Func | None:
    exact = [f for f in funcs if f.qualname == qual]
    if exact:
        return min(exact, key=lambda f: f.start)
    last = qual.rsplit(".", 1)[-1]
    by_name = [f for f in funcs if f.name == last]
    return by_name[0] if len(by_name) == 1 else None


def parse_list(raw) -> list[str]:
    if raw is None:
        return []
    if isinstance(raw, list):
        return [str(x) for x in raw]
    s = str(raw).strip()
    if not s:
        return []
    try:
        val = json.loads(s)
    except json.JSONDecodeError:
        try:
            val = ast.literal_eval(s)
        except (ValueError, SyntaxError):
            return []
    return [str(x) for x in val] if isinstance(val, (list, tuple)) else []


def assembled_functions(source: str, labels: list[str], repo: str, base: str, language: str) -> dict:
    cache: dict = {}
    mapped: dict[tuple[str, int], dict] = {}
    unmapped: list[str] = []
    skipped_kind = 0
    for lab in labels:
        if source == "locagent":
            if ":" not in lab:
                unmapped.append(lab)
                continue
            path, qual = lab.split(":", 1)
        else:  # polybench
            parts = lab.split("->")
            path, items = parts[0], parts[1:]
            names = []
            last_kind = ""
            for it in items:
                kind, _, name = it.partition(":")
                last_kind = kind
                if name:
                    names.append(name)
            if not names or any(k in last_kind for k in _NON_CALLABLE_KINDS):
                skipped_kind += 1
                continue
            qual = ".".join(names)
        if not candidate_path(path, language):
            continue
        funcs = _funcs_at(repo, base, path, cache)
        f = _map_label(funcs or [], qual)
        if f is None:
            unmapped.append(lab)
            continue
        d = f.as_dict()
        d["path"] = path
        mapped[(path, f.start)] = d
    return {
        "functions": sorted(mapped.values(), key=lambda d: (d["path"], d["start"])),
        "unmapped": unmapped,
        "skipped_non_callable": skipped_kind,
    }


def context_files(gold_context, language: str) -> list[str]:
    try:
        spans = json.loads(gold_context) if isinstance(gold_context, str) else (gold_context or [])
    except json.JSONDecodeError:
        return []
    files = []
    for sp in spans:
        p = str(sp.get("file") or "").removeprefix("./")
        if p and candidate_path(p, language) and p not in files:
            files.append(p)
    return files
