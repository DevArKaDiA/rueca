"""Scoring (LocBench-style localization metrics; set P/R/F1 for structural questions)."""

from __future__ import annotations

import statistics
from typing import Iterable

from .arms.base import same_def

FILE_KS = (1, 3, 5, 10)
FUNC_KS = (5, 10)


def score_files(ranked: list[str], gold: list[str]) -> dict:
    gold_set = set(gold)
    ranks = [i + 1 for i, p in enumerate(ranked) if p in gold_set]
    out = {f"acc@{k}": float(any(r <= k for r in ranks)) for k in FILE_KS}
    out.update({f"recall@{k}": (sum(1 for r in ranks if r <= k) / len(gold_set)) if gold_set else 0.0 for k in FILE_KS})
    out["mrr"] = 1.0 / ranks[0] if ranks else 0.0
    out["first_rank"] = ranks[0] if ranks else None
    return out


def score_functions(ranked: list[dict], gold: list[dict]) -> dict:
    """ranked: arm hits {path,name,start_line}; gold: tree-sitter funcs with path."""
    first_rank_per_gold: dict[int, int] = {}
    seen: set[tuple] = set()
    rank = 0
    for hit in ranked:
        key = (hit.get("path"), hit.get("name"), hit.get("start_line"))
        if key in seen:
            continue
        seen.add(key)
        rank += 1
        for gi, g in enumerate(gold):
            if gi not in first_rank_per_gold and same_def(hit, g):
                first_rank_per_gold[gi] = rank
    ranks = sorted(first_rank_per_gold.values())
    n = len(gold)
    out = {f"acc@{k}": float(any(r <= k for r in ranks)) for k in FUNC_KS}
    out.update({f"recall@{k}": (sum(1 for r in ranks if r <= k) / n) if n else 0.0 for k in FUNC_KS})
    out["mrr"] = 1.0 / ranks[0] if ranks else 0.0
    out["first_rank"] = ranks[0] if ranks else None
    return out


def prf(pred: list[dict], gold: list[dict]) -> dict:
    uniq: dict[tuple, dict] = {}
    for h in pred:
        uniq.setdefault((h.get("path"), h.get("name"), h.get("start_line")), h)
    preds = list(uniq.values())
    matched_pred = sum(1 for h in preds if any(same_def(h, g) for g in gold))
    matched_gold = sum(1 for g in gold if any(same_def(h, g) for h in preds))
    p = matched_pred / len(preds) if preds else None
    r = matched_gold / len(gold) if gold else 0.0
    f1 = (2 * p * r / (p + r)) if p and r else 0.0
    return {"precision": p, "recall": r, "f1": f1, "n_pred": len(preds), "n_gold": len(gold)}


def mean(xs: Iterable[float | None]) -> float | None:
    vals = [x for x in xs if x is not None]
    return sum(vals) / len(vals) if vals else None


def median(xs: Iterable[float | None]) -> float | None:
    vals = [x for x in xs if x is not None]
    return statistics.median(vals) if vals else None


def p95(xs: Iterable[float | None]) -> float | None:
    vals = sorted(x for x in xs if x is not None)
    if not vals:
        return None
    idx = max(0, min(len(vals) - 1, int(round(0.95 * (len(vals) - 1)))))
    return vals[idx]
