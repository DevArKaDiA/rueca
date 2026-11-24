"""Gold answers of the multi-step suite, derived mechanically from the gold
the bench already has (`inputs.json`, written by `inputs.extract`):

- `s3[<corpus>]` — the S3 call graph of a pinned repository (SCIP / PHPStan
  gold, `s3.py`): `callables` (`path#Qualname@line` -> label) and `edges`
  (caller, callee) between them;
- `crossrepo` — the independent cross-repository labeler's *uses* of the
  Flask group (`crossrepo.label_python`): (consumer file, library
  repository, library symbol);
- `contracts` — the S2 gold pairs of the ollama group
  (`contracts_gold.build_gold`);
- `multiref` — the multi-ref scenario's classes of the changed files'
  callables (`org.collapse_gold`);
- `org_deps` — the libraries each consumer of `crossrepo.GROUPS` is pinned
  with.

Ids are `path#Qualname` (the S3 key without its `@line`); every function
here is pure, so `tests/test_multistep.py` re-derives every committed answer
from the committed inputs.
"""

from __future__ import annotations

from collections.abc import Callable, Iterable
from dataclasses import dataclass


def bare(key: str) -> str:
    """`path#Qualname@line` -> `path#Qualname`."""
    head, sep, tail = key.rpartition("@")
    return head if sep and tail.isdigit() else key


@dataclass(frozen=True)
class Graph:
    """A call graph over bare ids (S3 gold of one repository)."""

    callables: frozenset[str]
    callers: dict[str, frozenset[str]]
    callees: dict[str, frozenset[str]]

    @classmethod
    def from_inputs(cls, s3: dict) -> Graph:
        callers: dict[str, set[str]] = {}
        callees: dict[str, set[str]] = {}
        for a, b in s3["edges"]:
            a, b = bare(a), bare(b)
            callees.setdefault(a, set()).add(b)
            callers.setdefault(b, set()).add(a)
        return cls(
            frozenset(bare(k) for k in s3["callables"]),
            {k: frozenset(v) for k, v in callers.items()},
            {k: frozenset(v) for k, v in callees.items()},
        )

    def into(self, f: str) -> frozenset[str]:
        return self.callers.get(f, frozenset())

    def out(self, f: str) -> frozenset[str]:
        return self.callees.get(f, frozenset())

    def in_file(self, path: str) -> list[str]:
        return sorted(c for c in self.callables if c.split("#", 1)[0] == path)


def ids(xs: Iterable[str]) -> list[str]:
    return sorted(set(xs))


def two_hop_callers(g: Graph, f: str) -> list[str]:
    first = g.into(f)
    second = {c for x in first for c in g.into(x)}
    return ids((first | second) - {f})


def exactly_two_hop_count(g: Graph, f: str) -> int:
    first = g.into(f)
    second = {c for x in first for c in g.into(x)}
    return len(second - first - {f})


def common_callers(g: Graph, a: str, b: str) -> list[str]:
    return ids(g.into(a) & g.into(b))


def common_callees(g: Graph, a: str, b: str) -> list[str]:
    return ids(g.out(a) & g.out(b))


def callers_of(g: Graph, f: str) -> list[str]:
    return ids(g.into(f))


def callers_where(g: Graph, f: str, keep: Callable[[str], bool]) -> list[str]:
    return ids(c for c in g.into(f) if keep(c))


def leaf_callees(g: Graph, f: str) -> list[str]:
    return ids(c for c in g.out(f) if not g.out(c))


def caller_counts(g: Graph, members: Iterable[str]) -> dict[str, int]:
    return {m: len(g.into(m)) for m in sorted(members)}


def top_by_callers(g: Graph, members: Iterable[str], n: int) -> list[list]:
    ranked = sorted(((m, len(g.into(m))) for m in members), key=lambda x: (-x[1], x[0]))
    return [[m, c] for m, c in ranked[:n]]


def uncalled(g: Graph, members: Iterable[str]) -> list[str]:
    return ids(m for m in members if not g.into(m))


def name_of(fid: str) -> str:
    return fid.split("#", 1)[1].rsplit(".", 1)[-1]


# --- cross-repository uses -----------------------------------------------------------


def files_using(uses: list[list[str]], symbol: str, prefix: str) -> list[str]:
    """Consumer files under `prefix` with a gold use of `symbol`."""
    return ids(f for f, _repo, sym in uses if sym == symbol and f.startswith(prefix))


def files_using_each(uses: list[list[str]], symbols: list[str], prefix: str) -> dict[str, list[str]]:
    return {s: files_using(uses, s, prefix) for s in symbols}


# --- contracts ---------------------------------------------------------------------------


def contract_keys(pairs: list[dict], kind: str, src_repo: str) -> set[str]:
    return {p["key"] for p in pairs if p["kind"] == kind and p["src_repo"] == src_repo}


def contract_dsts(pairs: list[dict], kind: str, key: str) -> list[str]:
    return ids(f"{p['dst_path']}#{p['dst_symbol']}" for p in pairs if p["kind"] == kind and p["key"] == key)


def spec_ops_count(pairs: list[dict], src_repo: str) -> int:
    return len({(p["dst_path"], p["dst_symbol"]) for p in pairs if p["kind"] == "http-spec" and p["src_repo"] == src_repo})


# --- multi-ref ---------------------------------------------------------------------------


def differing(symbols: list[list[str]], path: str) -> list[str]:
    """Callables of `path` whose text differs between the refs, or that
    exist in one ref only (`collapse_gold` classes)."""
    return ids(f"{p}#{qn}" for p, qn, cls in symbols if p == path and cls in ("changed", "added", "removed"))
