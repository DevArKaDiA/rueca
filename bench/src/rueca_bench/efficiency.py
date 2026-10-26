"""Efficiency: tokens per correct answer (RFC 0005 §8, amends RFC 0004 §5).

What is counted is what an agent reads:

- Rueca: the MCP text content (`rueca query ... --text`) when the binary has
  the RFC 0005 contract, else the JSON the CLI prints (alpha contract);
- cbm: the MCP text content, which is cbm's default `tree` rendering
  (`codebase-memory-mcp cli <tool>` without `format`).

The JSON of every answer is still counted (`tokens_json`) and reported as
the old raw-token diagnostic.

Per-item cost. "Tokens until the first gold hit" needs the cost of a prefix
of an answer. Both text renderings put one item per indented line (headers —
cbm's `results: N (cols: ...)`, Rueca's snapshot and file lines — are not
indented), so the prefix cost of item i is the tokens of the text up to the
end of the i-th indented line (every header before it included). JSON answers
(Rueca alpha) are split the same way over the serialized items of
`results`, the rest of the document counted before the first item. When the
item lines cannot be matched to the parsed items (counts differ) the prefix
is the proportional share of the whole answer, flagged `approx`.

Aggregates are ratios of sums over the same queries for both arms
(`tokens per correct answer` = Σ tokens read / Σ correct answers), so a
query that finds nothing still costs its tokens and adds no answer.
"""

from __future__ import annotations

import json
from typing import Any, Sequence

from .common import count_tokens


def _tok(text: str) -> int:
    return count_tokens(text) or 0


def text_prefix_tokens(text: str, n_items: int) -> tuple[list[int], bool]:
    """Prefix token cost after each of the first `n_items` items of a text
    answer (one indented line per item). Returns (prefix costs, approx)."""
    total = _tok(text)
    lines = text.splitlines(keepends=True)
    ends: list[int] = []
    for i, ln in enumerate(lines):
        if ln[:1] in (" ", "\t") and ln.strip():
            ends.append(i)
    if n_items <= 0:
        return [], False
    if len(ends) < n_items:
        return proportional(total, n_items), True
    out = []
    acc = 0
    prev = 0
    for k in range(n_items):
        end = ends[k] + 1
        acc += _tok("".join(lines[prev:end]))
        prev = end
        out.append(acc)
    return out, False


def json_prefix_tokens(raw: str, items: Sequence[Any]) -> tuple[list[int], bool]:
    """Prefix token cost after each item of a JSON answer: the serialized
    items in order, everything else of the document counted up front."""
    total = _tok(raw)
    if not items:
        return [], False
    costs = [_tok(json.dumps(it, separators=(",", ":"), ensure_ascii=False)) for it in items]
    head = max(0, total - sum(costs))
    out = []
    acc = head
    for c in costs:
        acc += c
        out.append(acc)
    return out, False


def proportional(total: int, n_items: int) -> list[int]:
    return [round(total * (k + 1) / n_items) for k in range(n_items)]


# --- scoring --------------------------------------------------------------------


def search_row(items: list[dict], prefix: list[int], tokens: int | None, is_gold_file, is_gold_func, k: int = 10) -> dict:
    """Compact per-query efficiency row of a top-k search answer:
    t = tokens read for the whole answer; fh / ff = 1-based rank of the first
    item in a gold file / the first gold function (0 = none in the top k);
    pf / pff = tokens read until (and including) that item (the whole answer
    when there is none); nh / nf = gold-file items / gold-function items in
    the top k."""
    items = items[:k]
    t = tokens or 0
    fh = next((i + 1 for i, it in enumerate(items) if is_gold_file(it)), 0)
    ff = next((i + 1 for i, it in enumerate(items) if is_gold_func(it)), 0)

    def pre(rank: int) -> int:
        if rank and rank - 1 < len(prefix):
            return min(prefix[rank - 1], t)
        return t

    return {
        "t": t,
        "fh": fh,
        "ff": ff,
        "pf": pre(fh),
        "pff": pre(ff),
        "nh": sum(1 for it in items if is_gold_file(it)),
        "nf": sum(1 for it in items if is_gold_func(it)),
        "n": len(items),
    }


def ratio(num: Sequence[float], den: Sequence[float]) -> float | None:
    d = sum(den)
    return sum(num) / d if d else None


def search_aggregate(rows: list[dict], level: str = "file") -> dict:
    """Pooled tokens per correct answer over search rows:
    `to_first_hit` = Σ tokens read until the first gold hit (the whole answer
    when none) / number of queries with a gold hit in the top k;
    `per_hit` = Σ answer tokens / Σ gold hits in the top k."""
    first, pre, hits = ("fh", "pf", "nh") if level == "file" else ("ff", "pff", "nf")
    n = len(rows)
    found = [1.0 if r[first] else 0.0 for r in rows]
    return {
        "n": n,
        "found": int(sum(found)),
        "hit_rate": (sum(found) / n) if n else None,
        "to_first_hit": ratio([r[pre] for r in rows], found),
        "per_hit": ratio([r["t"] for r in rows], [r[hits] for r in rows]),
        "tokens_median": _median([r["t"] for r in rows]),
        "num_first": [r[pre] for r in rows],
        "den_first": found,
        "num_hit": [r["t"] for r in rows],
        "den_hit": [r[hits] for r in rows],
    }


def trace_aggregate(tokens: list[float], true_edges: list[float]) -> dict:
    """Σ tokens of the trace answers / Σ gold edges they returned."""
    return {
        "n": len(tokens),
        "true_edges": int(sum(true_edges)),
        "per_true_edge": ratio(tokens, true_edges),
        "tokens_median": _median(tokens),
    }


def _median(xs: Sequence[float]) -> float | None:
    v = sorted(x for x in xs if x is not None)
    if not v:
        return None
    m = len(v) // 2
    return float(v[m]) if len(v) % 2 else (v[m - 1] + v[m]) / 2
