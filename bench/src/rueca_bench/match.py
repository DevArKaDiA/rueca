"""S3 matching: an arm's hit -> exactly one gold callable, and SCIP
equivalence classes.

A hit `{path, name, start_line, qualified_name?}` is assigned to at most one
callable of the gold's universe (the bench labeler's callables):

1. candidates = callables of the hit's file with the hit's bare name whose
   name line, declaration start or decorator start is within ±2 lines of
   the hit's start line (any line when the hit has none);
2. when the arm gives a qualified name, candidates whose qualified name
   agrees with it are preferred (the trailing segments are equal, template
   arguments compared as written, then without them; parameter lists and
   overload comments are ignored);
3. then the nearest line wins (ties: the earlier definition).

So a hit can no longer match two neighbouring overloads, or a definition two
lines away when the exact one exists, and two hits naming one callable are
one prediction.

Equivalence classes (`classes` in an S3 gold file): callables the gold
cannot tell apart — several definitions bound to one SCIP symbol
(scip-clang's same-signature overloads, function template specializations,
`#if` alternatives, C `static` functions of the same name), or a PHPStan
target that maps to several callables. Class-aware scoring maps a callee to
its class, so a prediction of any member matches a gold edge to the class,
once per question.
"""

from __future__ import annotations

import re
from collections import defaultdict

LINE_TOL = 2

_PARAMS_COMMENT = re.compile(r"/\*.*?\*/")


def _strip_params(qn: str) -> str:
    """`a::f(int, T<x>)` -> `a::f`; `a::f(/*const*/)` -> `a::f` (a balanced
    trailing parameter list only)."""
    q = _PARAMS_COMMENT.sub("", qn).rstrip()
    if not q.endswith(")"):
        return q
    depth = 0
    for i in range(len(q) - 1, -1, -1):
        c = q[i]
        if c == ")":
            depth += 1
        elif c == "(":
            depth -= 1
            if depth == 0:
                return q[:i].rstrip() if i > 0 else q
    return q


def qn_segments(qn: str | None) -> list[str]:
    """Top-level segments of a qualified name in any arm's or the labeler's
    spelling: separators `::`, `.`, `:`, `\\`, `/`, `#` outside template
    arguments, parameter list dropped."""
    if not qn:
        return []
    q = _strip_params(qn)
    out: list[str] = []
    cur: list[str] = []
    depth = 0
    i = 0
    while i < len(q):
        c = q[i]
        if c == "<":
            depth += 1
        elif c == ">" and depth > 0:
            depth -= 1
        if depth == 0 and c in ".:\\/#":
            if cur:
                out.append("".join(cur).strip())
            cur = []
            i += 1
            continue
        cur.append(c)
        i += 1
    if cur:
        out.append("".join(cur).strip())
    return [s for s in out if s]


def _no_targs(seg: str) -> str:
    return re.sub(r"<.*>", "", seg).strip()


def qn_agreement(hit_qn: str | None, gold_qn: str | None) -> int:
    """2 = trailing segments equal as written, 1 = equal once template
    arguments are removed, 0 = disagree or unknown."""
    a, b = qn_segments(hit_qn), qn_segments(gold_qn)
    if not a or not b:
        return 0
    k = min(len(a), len(b))
    if a[-k:] == b[-k:]:
        return 2
    if [_no_targs(x) for x in a[-k:]] == [_no_targs(x) for x in b[-k:]]:
        return 1
    return 0


def _distance(line: int | None, f: dict) -> int | None:
    if line is None:
        return 0
    ds = [abs(line - a) for a in (f.get("def_line"), f.get("start"), f.get("outer_start")) if a is not None]
    return min(ds) if ds else None


class CallableIndex:
    """The gold's callables by (path, bare name), with their ids."""

    def __init__(self, callables: dict[str, dict]):
        self.by_name: dict[tuple, list[tuple[str, dict]]] = defaultdict(list)
        for k, f in callables.items():
            self.by_name[(f["path"], f["name"])].append((k, f))
        for v in self.by_name.values():
            v.sort(key=lambda kf: (kf[1].get("def_line") or 0, kf[0]))

    def resolve(self, hit: dict, tol: int = LINE_TOL) -> str | None:
        cands = []
        for k, f in self.by_name.get((hit.get("path"), hit.get("name")), []):
            d = _distance(hit.get("start_line"), f)
            if d is not None and d <= tol:
                cands.append((k, f, d))
        if not cands:
            return None
        qn = hit.get("qualified_name")
        if qn and len(cands) > 1:
            scored = [(qn_agreement(qn, f.get("qualname")), k, f, d) for k, f, d in cands]
            best = max(s for s, *_ in scored)
            if best > 0:
                cands = [(k, f, d) for s, k, f, d in scored if s == best]
        return min(cands, key=lambda c: (c[2], c[1].get("def_line") or 0))[0]


class UnionFind:
    def __init__(self) -> None:
        self.parent: dict[str, str] = {}

    def find(self, x: str) -> str:
        self.parent.setdefault(x, x)
        while self.parent[x] != x:
            self.parent[x] = self.parent[self.parent[x]]
            x = self.parent[x]
        return x

    def union(self, a: str, b: str) -> None:
        ra, rb = self.find(a), self.find(b)
        if ra != rb:
            if rb < ra:
                ra, rb = rb, ra
            self.parent[rb] = ra

    def groups(self) -> list[list[str]]:
        out: dict[str, list[str]] = defaultdict(list)
        for x in self.parent:
            out[self.find(x)].append(x)
        return sorted(sorted(v) for v in out.values() if len(v) > 1)


def class_map(graph: dict) -> dict[str, str]:
    """callable id -> class key (the class's first member) for every member
    of an equivalence class; callables in no class are absent."""
    out: dict[str, str] = {}
    for members in graph.get("classes") or []:
        for m in members:
            out[m] = members[0]
    return out


def class_members(graph: dict) -> dict[str, list[str]]:
    """callable id -> every member of its class (itself included)."""
    out: dict[str, list[str]] = {}
    for members in graph.get("classes") or []:
        for m in members:
            out[m] = list(members)
    return out


def class_edges(graph: dict) -> int:
    """Gold edges whose callee is a member of an equivalence class."""
    cm = class_map(graph)
    return sum(1 for dsts in graph.get("callees", {}).values() for d in dsts if d in cm)
