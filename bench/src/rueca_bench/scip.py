"""Minimal SCIP reader (protobuf wire format, no generated code) and S3 gold.

Reads both the deprecated packed `range` / `enclosing_range` fields and the
typed `single_line_*` / `multi_line_*` oneofs (typed wins when both are set),
per scip.proto at scip-code/scip@d556dd3.

Call-edge derivation (RFC 0002 §4.2):
1. Callables = tree-sitter functions/methods of each in-scope file
   (`@overload` stubs excluded). A SCIP
   definition occurrence whose (line, name) equals a tree-sitter callable's
   name identifier binds that SCIP symbol to the callable. If the definition
   carries an enclosing range, it replaces the tree-sitter extent.
2. For every non-definition reference occurrence to a bound symbol, keep it
   only if tree-sitter says it is a call site (identifier is the callee of a
   call / new expression, directly or as the property of a member access).
   For C++ (`constructions` in the grammar's manifest entry) a reference
   on the name a declaration in a body declares is a construction
   (`Token token(a, b);`, `Parser parser(input);`, `T x{a};`, `T x;`):
   scip-clang puts the constructor reference there.
3. The caller is the innermost callable whose extent contains the occurrence.
   Occurrences outside any callable (module level) produce no edge.
4. Both endpoints must be in-scope (repo `include` prefixes, not test paths).
5. Equivalence classes: when one SCIP symbol binds to several callables
   (scip-clang gives same-signature overloads, function template
   specializations and `#if` alternatives one symbol; two C `static`
   functions of one name share one), edges keep the first bound callable
   and `classes` lists every group of callables the gold cannot tell apart
   (`match.py`); class-aware scoring treats a class as one neighbour.
"""

from __future__ import annotations

import random
import re
from collections import defaultdict
from dataclasses import dataclass, field
from pathlib import Path

from .common import is_test_path
from .match import UnionFind, class_edges
from .treesitter import Func, extract_functions, grammar_for, is_call_site, parse, utf16_col_to_byte

ROLE_DEFINITION = 1


# --- protobuf wire decoding ---------------------------------------------------


def _varint(buf: bytes, i: int) -> tuple[int, int]:
    result = 0
    shift = 0
    while True:
        b = buf[i]
        i += 1
        result |= (b & 0x7F) << shift
        if not b & 0x80:
            return result, i
        shift += 7


def _fields(buf: bytes):
    i, n = 0, len(buf)
    while i < n:
        key, i = _varint(buf, i)
        fno, wt = key >> 3, key & 7
        if wt == 0:
            val, i = _varint(buf, i)
            yield fno, wt, val
        elif wt == 2:
            ln, i = _varint(buf, i)
            yield fno, wt, buf[i : i + ln]
            i += ln
        elif wt == 1:
            yield fno, wt, buf[i : i + 8]
            i += 8
        elif wt == 5:
            yield fno, wt, buf[i : i + 4]
            i += 4
        else:
            raise ValueError(f"unsupported wire type {wt}")


def _int32(v: int) -> int:
    v &= 0xFFFFFFFFFFFFFFFF
    return v - (1 << 64) if v >= (1 << 63) else v


def _packed(val, wt: int) -> list[int]:
    if wt == 0:
        return [_int32(val)]
    out = []
    i = 0
    while i < len(val):
        v, i = _varint(val, i)
        out.append(_int32(v))
    return out


def _norm_range(r: list[int]) -> tuple[int, int, int, int] | None:
    if len(r) == 3:
        return r[0], r[1], r[0], r[2]
    if len(r) == 4:
        return r[0], r[1], r[2], r[3]
    return None


def _typed_single(buf: bytes) -> tuple[int, int, int, int]:
    line = sc = ec = 0
    for f, _, v in _fields(buf):
        if f == 1:
            line = _int32(v)
        elif f == 2:
            sc = _int32(v)
        elif f == 3:
            ec = _int32(v)
    return line, sc, line, ec


def _typed_multi(buf: bytes) -> tuple[int, int, int, int]:
    vals = [0, 0, 0, 0]
    for f, _, v in _fields(buf):
        if 1 <= f <= 4:
            vals[f - 1] = _int32(v)
    return vals[0], vals[1], vals[2], vals[3]


@dataclass
class Occ:
    symbol: str
    roles: int
    range: tuple[int, int, int, int]  # 0-based start line, start char, end line, end char
    enclosing: tuple[int, int, int, int] | None


@dataclass
class Doc:
    path: str
    language: str = ""
    position_encoding: int = 0
    occurrences: list[Occ] = field(default_factory=list)


def _occurrence(buf: bytes) -> Occ | None:
    sym = ""
    roles = 0
    rng_vals: list[int] = []
    enc_vals: list[int] = []
    typed = None
    typed_enc = None
    for f, wt, v in _fields(buf):
        if f == 1:
            rng_vals.extend(_packed(v, wt))
        elif f == 2:
            sym = v.decode("utf-8", errors="replace")
        elif f == 3:
            roles = v
        elif f == 7:
            enc_vals.extend(_packed(v, wt))
        elif f == 8:
            typed = _typed_single(v)
        elif f == 9:
            typed = _typed_multi(v)
        elif f == 10:
            typed_enc = _typed_single(v)
        elif f == 11:
            typed_enc = _typed_multi(v)
    r = typed or _norm_range(rng_vals)
    if r is None or not sym:
        return None
    return Occ(sym, roles, r, typed_enc or _norm_range(enc_vals))


def read_index(path: Path) -> list[Doc]:
    data = path.read_bytes()
    docs: list[Doc] = []
    for f, _, v in _fields(data):
        if f != 2:
            continue
        doc = Doc(path="")
        for df, _, dv in _fields(v):
            if df == 1:
                doc.path = dv.decode("utf-8")
            elif df == 4:
                doc.language = dv.decode("utf-8")
            elif df == 6:
                doc.position_encoding = dv
            elif df == 2:
                occ = _occurrence(dv)
                if occ is not None:
                    doc.occurrences.append(occ)
        docs.append(doc)
    return docs


# --- gold derivation -------------------------------------------------------------


def fid(path: str, f: Func | dict) -> str:
    if isinstance(f, dict):
        return f"{path}#{f['qualname']}@{f['def_line']}"
    return f"{path}#{f.qualname}@{f.def_line}"


def in_scope(path: str, include: list[str], exclude: list[str] | tuple[str, ...] = ()) -> bool:
    """`include` / `exclude` are path prefixes (manifest `[[s3.repos]]`)."""
    return (
        any(path == p or path.startswith(p) for p in include)
        and not any(path == p or path.startswith(p) for p in exclude)
        and not is_test_path(path)
        and grammar_for(path) is not None
    )


def derive_call_graph(root: Path, docs: list[Doc], include: list[str], language: str | None = None, exclude: list[str] | tuple[str, ...] = ()) -> dict:
    callables: dict[str, dict] = {}  # fid -> func dict (+ path)
    by_file: dict[str, list[dict]] = {}
    sym_to_fid: dict[str, str] = {}
    trees = {}
    texts: dict[str, list[str]] = {}
    stats = defaultdict(int)
    overload_alias: dict[tuple[str, int, str], dict] = {}
    same_symbol = UnionFind()  # callables bound to one SCIP symbol
    bound: set[str] = set()

    scoped = [d for d in docs if in_scope(d.path, include, exclude)]
    stats["documents_total"] = len(docs)
    stats["documents_in_scope"] = len(scoped)

    for d in scoped:
        src_path = root / d.path
        if not src_path.is_file():
            continue
        data = src_path.read_bytes()
        trees[d.path] = parse(d.path, data, language)
        texts[d.path] = data.decode("utf-8", errors="replace").split("\n")
        funcs = extract_functions(d.path, data, language)
        lines_ = texts[d.path]
        lst = []
        stubs = []
        for f in funcs:
            # typing.overload stubs are declarations, not callables. scip-python
            # emits the definition occurrence on the *first* stub only, so the
            # stub's (line, name) is aliased to the implementation below.
            deco = lines_[f.outer_start - 1 : f.start - 1]
            if any(re.match(r"\s*@(typing\.)?overload\b", ln) for ln in deco):
                stats["overload_stubs_skipped"] += 1
                stubs.append(f)
                continue
            fd = f.as_dict()
            fd["path"] = d.path
            callables[fid(d.path, f)] = fd
            lst.append(fd)
        by_file[d.path] = lst
        for f in stubs:
            impls = [x for x in lst if x["qualname"] == f.qualname and x["start"] > f.start]
            if impls:
                overload_alias[(d.path, f.def_line, f.name)] = impls[-1]

    # bind SCIP definitions to callables
    for d in scoped:
        index = {(f["def_line"], f["name"]): f for f in by_file.get(d.path, [])}
        lines = texts.get(d.path, [])
        for o in d.occurrences:
            if not (o.roles & ROLE_DEFINITION) or o.symbol.startswith("local "):
                continue
            stats["definitions"] += 1
            line0, sc, _, ec = o.range
            if line0 >= len(lines):
                continue
            name = _slice(lines[line0], sc, ec, d.position_encoding)
            f = index.get((line0 + 1, name))
            aliased = False
            if f is None:
                f = overload_alias.get((d.path, line0 + 1, name))
                aliased = f is not None
            if f is None:
                continue
            bound.add(fid(d.path, f))
            if o.symbol not in sym_to_fid:
                sym_to_fid[o.symbol] = fid(d.path, f)
                stats["definitions_bound"] += 1
                stats["definitions_bound_via_overload"] += int(aliased)
            elif sym_to_fid[o.symbol] != fid(d.path, f):
                # one symbol, several callables: the gold cannot tell them apart
                same_symbol.union(sym_to_fid[o.symbol], fid(d.path, f))
            if o.enclosing is not None and not aliased:
                stats["definitions_with_enclosing_range"] += 1
                es, ee = o.enclosing[0] + 1, o.enclosing[2] + 1
                if es <= f["def_line"] <= ee and not f.get("_enc"):
                    f["start"], f["end"], f["_enc"] = min(es, f["start"]), ee, True

    stats["classes_by_symbol"] = len(same_symbol.groups())
    stats["class_members_unbound"] = join_unbound_alternatives(callables, bound, same_symbol, language)

    callers: dict[str, set[str]] = defaultdict(set)
    callees: dict[str, set[str]] = defaultdict(set)
    for d in scoped:
        tree = trees.get(d.path)
        if tree is None:
            continue
        grammar = grammar_for(d.path, language)
        lines = texts[d.path]
        funcs = by_file.get(d.path, [])
        for o in d.occurrences:
            if o.roles & ROLE_DEFINITION or o.symbol.startswith("local "):
                continue
            callee = sym_to_fid.get(o.symbol)
            if callee is None:
                continue
            stats["references_to_callables"] += 1
            line0, sc, l1, ec = o.range
            if line0 >= len(lines) or l1 != line0:
                continue
            b0 = _byte_col(lines[line0], sc, d.position_encoding)
            b1 = _byte_col(lines[line0], ec, d.position_encoding)
            if not is_call_site(tree, grammar, line0, b0, b1):
                continue
            stats["call_sites"] += 1
            line = line0 + 1
            best = None
            for f in funcs:
                if f["start"] <= line <= f["end"] and (best is None or f["end"] - f["start"] < best["end"] - best["start"]):
                    best = f
            if best is None:
                stats["call_sites_module_level"] += 1
                continue
            caller = fid(d.path, best)
            callees[caller].add(callee)
            callers[callee].add(caller)
    stats["edges"] = sum(len(v) for v in callees.values())
    for f in callables.values():
        f.pop("_enc", None)
    graph = {
        "callables": callables,
        "callers": {k: sorted(v) for k, v in callers.items()},
        "callees": {k: sorted(v) for k, v in callees.items()},
        "classes": same_symbol.groups(),
        "stats": dict(stats),
    }
    add_class_stats(graph)
    return graph


# Languages where a definition can exist in alternatives the indexer did not
# compile: C / C++ `#if` branches and Go build tags (whose build
# configuration also selects files of a directory: `_win.cc`, `_unix.go`),
# Python conditional `def`s and `typing.overload` stubs spelled another way.
# Java, TS/JS and PHP have none: an unbound callable there is another
# function (scip-java's anonymous classes are `local` symbols).
_FILE_ALTERNATIVES = ("python",)
_DIR_ALTERNATIVES = ("c", "cpp", "go")


def join_unbound_alternatives(callables: dict[str, dict], bound: set[str], uf: UnionFind, language: str | None) -> int:
    """A callable no SCIP definition binds to (the `#if` / build-tag
    alternative the indexer did not compile, a conditional Python `def`)
    joins the class of the bound callables with its qualified name in the
    same file — or, for C, C++ and Go, the same directory — when they are
    one class; with several distinguishable candidates (overloads with
    their own symbols) it stays alone. Returns how many joined."""
    family = (language or "").lower()
    if family in _DIR_ALTERNATIVES:
        scope_of = lambda p: p.rsplit("/", 1)[0] if "/" in p else ""
    elif family in _FILE_ALTERNATIVES:
        scope_of = lambda p: p
    else:
        return 0
    by_key: dict[tuple, list[str]] = defaultdict(list)
    for k, f in callables.items():
        if k in bound:
            by_key[(scope_of(f["path"]), f["qualname"])].append(k)
    joined = 0
    for k, f in sorted(callables.items()):
        if k in bound:
            continue
        others = by_key.get((scope_of(f["path"]), f["qualname"]), [])
        roots = {uf.find(o) for o in others}
        if len(roots) == 1:
            uf.union(next(iter(roots)), k)
            joined += 1
    return joined


def add_class_stats(graph: dict) -> None:
    """`classes`, `class_members`, `class_edges` in the graph's stats."""
    st = graph["stats"]
    st["classes"] = len(graph.get("classes") or [])
    st["class_members"] = sum(len(c) for c in graph.get("classes") or [])
    st["class_edges"] = class_edges(graph)


def _byte_col(line: str, col: int, encoding: int) -> int:
    if encoding == 1:  # UTF8 code units
        return col
    return utf16_col_to_byte(line, col)


def _slice(line: str, sc: int, ec: int, encoding: int) -> str:
    raw = line.encode("utf-8")
    return raw[_byte_col(line, sc, encoding) : _byte_col(line, ec, encoding)].decode("utf-8", errors="replace")


def _bucket(n: int) -> int:
    return 0 if n == 0 else 1 if n == 1 else 2 if n <= 4 else 3


def sample_functions(graph: dict, n: int, seed: int, min_lines: int) -> list[str]:
    """Stratified by (fan-in bucket, fan-out bucket); skips trivial functions
    (fewer than `min_lines` lines, dunder names, no edges at all)."""
    buckets: dict[tuple[int, int], list[str]] = defaultdict(list)
    for k, f in sorted(graph["callables"].items()):
        fi = len(graph["callers"].get(k, []))
        fo = len(graph["callees"].get(k, []))
        if fi == 0 and fo == 0:
            continue
        if f["end"] - f["start"] + 1 < min_lines:
            continue
        if f["name"].startswith("__") and f["name"].endswith("__"):
            continue
        buckets[(_bucket(fi), _bucket(fo))].append(k)
    rng = random.Random(seed)
    keys = sorted(buckets)
    for k in keys:
        rng.shuffle(buckets[k])
    out: list[str] = []
    while len(out) < n and any(buckets[k] for k in keys):
        for k in keys:
            if buckets[k] and len(out) < n:
                out.append(buckets[k].pop())
    return sorted(out)


def eligible(f: dict, min_lines: int) -> bool:
    """A structural question (RFC 0004 §7.1): not a dunder, body longer than
    one line (`min_lines` lines of extent)."""
    if f["end"] - f["start"] + 1 < min_lines:
        return False
    return not (f["name"].startswith("__") and f["name"].endswith("__"))


def gold_neighbours(graph: dict, fid_: str, direction: str) -> list[str]:
    """Gold depth-1 neighbours of a callable; a recursive callable is its
    own caller and callee."""
    return [x for x in graph[direction].get(fid_, []) if x in graph["callables"]]


def questions(graph: dict, min_lines: int) -> dict[str, list[str]]:
    """Every in-scope callable with at least one gold edge in a direction
    (minus dunders and one-line bodies), per direction, plus their union."""
    out: dict[str, list[str]] = {"callers": [], "callees": []}
    for k, f in sorted(graph["callables"].items()):
        if not eligible(f, min_lines):
            continue
        for d in out:
            if gold_neighbours(graph, k, d):
                out[d].append(k)
    out["all"] = sorted(set(out["callers"]) | set(out["callees"]))
    return out
