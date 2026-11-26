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
5. C# (grammar flags): `local_callables` binds scip-dotnet's `local N`
   symbols (local functions) per document, so calls of local functions are
   edges; `type_constructions` makes a reference to a *type* on the type
   of `new T(…)` a call of `T`'s constructors (scip-dotnet records the
   type there, not the constructor), the constructors of one type forming
   an equivalence class when there are several (which overload runs is not
   recorded). `generic_overloads`: scip-dotnet records a call of a
   constructed generic method (`Write<T>(…)`) with the symbol of the first
   overload of its name (`Write().`); when that overload cannot take the
   call's argument count, the call goes to the generic overloads of the
   same name and symbol family that can (one class when several).
6. Equivalence classes: when one SCIP symbol binds to several callables
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
from .treesitter import Func, call_arity_at, creation_type_at, csharp_signatures, extract_functions, grammar_for, grammar_spec, is_call_site, parse, utf16_col_to_byte

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
    keep_start = (language or "").lower() in _TREE_SITTER_START
    kotlin = (language or "").lower() == "kotlin"
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
        local_ok = kotlin or bool(grammar_spec(grammar_for(d.path, language)).get("local_callables"))
        for o in d.occurrences:
            sym = _symbol_key(d.path, o.symbol, local_ok)
            if not (o.roles & ROLE_DEFINITION) or sym is None:
                continue
            stats["definitions"] += 1
            line0, sc, _, ec = o.range
            if line0 >= len(lines):
                continue
            name = _slice(lines[line0], sc, ec, d.position_encoding)
            if kotlin:
                name = kotlin_def_name(o.symbol, name)
            f = index.get((line0 + 1, name))
            if f is None and kotlin:
                f = kotlin_constructor_in(by_file.get(d.path, []), o, name)
            aliased = False
            if f is None:
                f = overload_alias.get((d.path, line0 + 1, name))
                aliased = f is not None
            if f is None:
                continue
            bound.add(fid(d.path, f))
            if sym not in sym_to_fid:
                sym_to_fid[sym] = fid(d.path, f)
                stats["definitions_bound"] += 1
                stats["definitions_bound_via_overload"] += int(aliased)
            elif sym_to_fid[sym] != fid(d.path, f):
                # one symbol, several callables: the gold cannot tell them apart
                same_symbol.union(sym_to_fid[sym], fid(d.path, f))
            if o.enclosing is not None and not aliased and not kotlin:
                stats["definitions_with_enclosing_range"] += 1
                es, ee = o.enclosing[0] + 1, o.enclosing[2] + 1
                if es <= f["def_line"] <= ee and not f.get("_enc"):
                    start = f["start"] if keep_start else min(es, f["start"])
                    f["start"], f["end"], f["_enc"] = start, ee, True

    ctors = _constructors_by_type(sym_to_fid, same_symbol)
    repair = _GenericOverloads(sym_to_fid, callables, trees, same_symbol)
    stats["classes_by_symbol"] = len(same_symbol.groups())
    stats["class_members_unbound"] = join_unbound_alternatives(callables, bound, same_symbol, language)

    callers: dict[str, set[str]] = defaultdict(set)
    callees: dict[str, set[str]] = defaultdict(set)
    for d in scoped:
        tree = trees.get(d.path)
        if tree is None:
            continue
        grammar = grammar_for(d.path, language)
        gspec = grammar_spec(grammar)
        lines = texts[d.path]
        funcs = by_file.get(d.path, [])
        for o in d.occurrences:
            sym = _symbol_key(d.path, o.symbol, kotlin or bool(gspec.get("local_callables")))
            if o.roles & ROLE_DEFINITION or sym is None:
                continue
            callee = sym_to_fid.get(sym)
            if callee is not None and gspec.get("generic_overloads") and o.symbol.endswith("()."):
                callee = repair.retarget(callee, o.symbol, tree, o.range, d.position_encoding, texts[d.path])
            if callee is None and gspec.get("type_constructions"):
                callee = ctors.get(o.symbol)
                construction = callee is not None
            else:
                construction = False
            if callee is None:
                continue
            stats["references_to_callables"] += 1
            line0, sc, l1, ec = o.range
            if line0 >= len(lines) or l1 != line0:
                continue
            b0 = _byte_col(lines[line0], sc, d.position_encoding)
            b1 = _byte_col(lines[line0], ec, d.position_encoding)
            if construction and not creation_type_at(tree, line0, b0, b1):
                continue
            if not is_call_site(tree, grammar, line0, b0, b1):
                continue
            stats["constructions"] += int(construction)
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


def _symbol_key(path: str, symbol: str, keep_locals: bool) -> str | None:
    """The gold's key of a SCIP symbol; `None` = ignored. `local N` symbols
    are unique within a document only: kept (qualified by the path) where
    they name callables — Kotlin (scip-java gives local functions and members
    of local classes / object expressions local symbols) and grammars with
    `local_callables` (C#: scip-dotnet's local functions) — and ignored
    elsewhere."""
    if not symbol.startswith("local "):
        return symbol
    return f"{path}::{symbol}" if keep_locals else None


class _GenericOverloads:
    """The repair of `generic_overloads` (module docs, step 5)."""

    def __init__(self, sym_to_fid: dict[str, str], callables: dict[str, dict], trees: dict, uf: UnionFind):
        self.family: dict[str, list[str]] = defaultdict(list)
        for sym, f in sym_to_fid.items():
            base = re.sub(r"\((\+\d+)?\)\.$", "", sym)
            if base != sym:
                self.family[base].append(f)
        self.callables = callables
        self.trees = trees
        self.uf = uf
        self.sigs: dict[str, dict] = {}

    def _sig(self, f: str) -> tuple[int, int | None, bool] | None:
        c = self.callables.get(f)
        if c is None or c["path"] not in self.trees:
            return None
        if c["path"] not in self.sigs:
            self.sigs[c["path"]] = csharp_signatures(self.trees[c["path"]])
        return self.sigs[c["path"]].get((c["def_line"], c["name"]))

    def retarget(self, callee: str, symbol: str, tree, rng, encoding: int, lines: list[str]) -> str:
        line0, sc, _, ec = rng
        if line0 >= len(lines):
            return callee
        n = call_arity_at(tree, line0, _byte_col(lines[line0], sc, encoding), _byte_col(lines[line0], ec, encoding))
        fits = lambda s: s is not None and n >= s[0] and (s[1] is None or n <= s[1])
        if n is None or fits(self._sig(callee)):
            return callee
        cands = sorted(f for f in self.family.get(symbol[: -len("().")], []) if (s := self._sig(f)) and s[2] and fits(s))
        for f in cands[1:]:
            self.uf.union(cands[0], f)
        return cands[0] if cands else callee


def _constructors_by_type(sym_to_fid: dict[str, str], uf: UnionFind) -> dict[str, str]:
    """scip-dotnet constructor symbols (`T#`.ctor`(…).`) by their type's
    symbol (`T#`): the first constructor's callable, the overloads of one
    type joined as an equivalence class."""
    by_type: dict[str, list[str]] = defaultdict(list)
    for sym, f in sorted(sym_to_fid.items()):
        if "`.ctor`" in sym:
            by_type[sym[: sym.index("`.ctor`")]].append(f)
    for fs in by_type.values():
        for f in fs[1:]:
            uf.union(fs[0], f)
    return {t: fs[0] for t, fs in by_type.items()}


def _strip_descriptor_suffix(desc: str) -> str:
    """A descriptor without its suffix: `#` (type), `.` / `(+1).` (term,
    method), or a trailing `(x)` (parameter, which then ends in `.` and
    names nothing)."""
    if desc.endswith("#"):
        return desc[:-1]
    if desc.endswith(")"):
        return desc[: desc.rfind("(")]
    if not desc.endswith("."):
        return desc
    desc = desc[:-1]
    if desc.endswith(")"):
        i = desc.rfind("(")
        if desc[i + 1 : -1].lstrip("+").isdigit() or desc[i + 1 : -1] in ("", "+"):
            desc = desc[:i]
    return desc


def scip_last_name(symbol: str) -> str:
    """Name of a global SCIP symbol's last descriptor (`a/b/Foo#bar(+1).`
    -> `bar`, `a/b/Foo#` -> `Foo`, `` a/b/Foo#`<init>`(). `` -> `<init>`);
    empty for parameters and local symbols."""
    if symbol.startswith("local "):
        return ""
    desc = _strip_descriptor_suffix(symbol.split(" ", 4)[-1])
    if desc.endswith("`"):
        i = desc.rfind("`", 0, len(desc) - 1)
        return desc[i + 1 : -1] if i >= 0 else ""
    return desc[max(desc.rfind(c) for c in "/#.`") + 1 :]


def kotlin_def_name(symbol: str, sliced: str) -> str | None:
    """The callable name a Kotlin definition occurrence binds to. scip-java's
    Kotlin indexer defines a class's symbol, its primary constructor and
    synthetic members (`values()`, `entries`, data-class accessors) on the
    class name, and a secondary constructor on its whole declaration: a
    constructor binds to the callable named like its class, any other
    symbol only when its own name is the text at the occurrence (the class
    symbol does, so supertype constructor calls `: Base(x)`, which
    reference the class, reach its constructor)."""
    last = scip_last_name(symbol)
    sliced = sliced.strip("`")
    if last == "<init>":
        return scip_last_name(symbol.rsplit("#`<init>`", 1)[0] + "#")
    if last == sliced or symbol.startswith("local "):
        return sliced
    return None


def kotlin_constructor_in(funcs: list[dict], o: Occ, name: str | None) -> dict | None:
    """The secondary constructor a Kotlin `<init>` definition stands for.
    scip-java puts it on the declaration's first token (its KDoc or first
    annotation when it has one) with the whole declaration as enclosing
    range: the callable named like the class whose first line lies in that
    range. Only for occurrences that start their enclosing range (a primary
    constructor's sits on the class name, inside the class's range), or a
    primary constructor whose `constructor` keyword is on its own line."""
    enc = o.enclosing
    if not name or enc is None or scip_last_name(o.symbol) != "<init>":
        return None
    if enc[:2] != o.range[:2]:
        # a primary constructor written with `constructor` on a line of
        # its own (`class Foo\nprivate constructor(…)`): the callable named
        # like the class whose extent (the class header) holds that line
        line = o.range[0] + 1
        inside = [f for f in funcs if f["name"] == name and f["start"] <= line <= f["end"]]
        return min(inside, key=lambda f: f["end"] - f["start"], default=None)
    first, last = enc[0] + 1, enc[2] + 1
    return next((f for f in funcs if f["name"] == name and first <= f["def_line"] <= last), None)


# Indexers whose enclosing range starts above the declaration: rust-analyzer's
# covers the item's doc comments and attributes, which would make a
# documented one-line function a structural question and move its start
# away from where every arm puts it. The tree-sitter start is kept (the end
# still comes from the enclosing range).
_TREE_SITTER_START = ("rust",)

# Languages where a definition can exist in alternatives the indexer did not
# compile: C / C++ `#if` branches and Go build tags (whose build
# configuration also selects files of a directory: `_win.cc`, `_unix.go`),
# Python conditional `def`s and `typing.overload` stubs spelled another way,
# Rust `#[cfg(...)]` items (features, platforms) in one file — a file
# compiled for another platform is not in rust-analyzer's module tree at
# all, so it has no SCIP document and no callables. Java, TS/JS and PHP have
# none: an unbound callable there is another function (scip-java's
# anonymous classes are `local` symbols).
_FILE_ALTERNATIVES = ("python", "rust")
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
        # Seeded on purpose: the sample must be reproducible run to run; it
        # picks benchmark functions and has no security role, hence the waiver
        # of python:S2245 (pseudorandom generator in a security context).
        rng.shuffle(buckets[k])  # NOSONAR
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
