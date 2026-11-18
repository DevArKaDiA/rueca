"""The localization rows of RFC 0007 §11.2 (phase 1 acceptance), read from
the S1 rows of `report.score_s1`: Rueca `hybrid` (the `rueca-hybrid` arm)
against BM25 (`grep`) and the vector arm, paired per instance (RFC 0004
§6.1; the manifest's resamples and seed, 95%).

| Row | Floor |
|---|---|
| File Acc@5, hybrid − BM25, `raw`, pooled | whole interval above 0 and point ≥ +5 |
| File Acc@5, hybrid − vector arm, `raw`, pooled | interval not below 0 |
| Function Acc@10, hybrid − BM25, `raw`, pooled | point ≥ +3, interval not below 0 |
| File Acc@5, hybrid − BM25, `kw`, pooled | interval not below 0 |
| File Acc@5 per language, both variants: hybrid − the better of BM25 and the vector arm there | interval not below 0 |

Status: ✅ met with the whole interval on the right side, ⚠ met but the
interval crosses 0 ("not below 0": not entirely below), ❌ not met,
`pending` without paired rows. Computed for the gated split (the gate) and
the calibration split (where α is tuned: a diagnostic, never the gate).
"""

from __future__ import annotations

from collections import defaultdict

from .common import families, scorecard_spec
from .metrics import mean
from .stats import paired_bootstrap, seeded

HYBRID, BM25, VECTOR = "rueca-hybrid", "grep", "vector"
EPS = 1e-9
SPLITS = ("gated", "calibration")
# (label, other arm, variant, metric, rule)
POOLED_ROWS = (
    ("File Acc@5, hybrid − BM25, `raw`", BM25, "raw", "file", "lift5"),
    ("File Acc@5, hybrid − vector arm, `raw`", VECTOR, "raw", "file", "not_below"),
    ("Function Acc@10, hybrid − BM25, `raw`", BM25, "raw", "func", "lift3"),
    ("File Acc@5, hybrid − BM25, `kw`", BM25, "kw", "file", "not_below"),
)
FLOORS = {
    "lift5": "whole interval above 0 and point ≥ +5",
    "lift3": "point ≥ +3, interval not below 0",
    "not_below": "interval not below 0",
}


def metric(row: dict, key: str) -> float | None:
    part = row["file"] if key == "file" else row["function"]
    if part is None:
        return None
    return part["acc@5" if key == "file" else "acc@10"]


def rows_by_id(per_instance: list[dict], variant: str, split: str, group: str | None = None) -> dict[str, dict[str, dict]]:
    by: dict[str, dict[str, dict]] = defaultdict(dict)
    for r in per_instance:
        if r["variant"] == variant and r["split"] == split and (group is None or r["group"] == group):
            by[r["id"]][r["arm"]] = r
    return by


def paired(by: dict[str, dict[str, dict]], a: str, b: str, key: str, *seed: str) -> dict | None:
    xa, xb = [], []
    for iid in sorted(by):
        ra, rb = by[iid].get(a), by[iid].get(b)
        va = metric(ra, key) if ra else None
        vb = metric(rb, key) if rb else None
        if va is not None and vb is not None:
            xa.append(va)
            xb.append(vb)
    if not xa:
        return None
    sc = scorecard_spec()
    res = paired_bootstrap(xa, xb, int(sc.get("bootstrap_resamples", 10000)), seeded(int(sc.get("bootstrap_seed", 20261025)), "semantic", *seed))
    if res and res.get("ci"):
        res["ci"] = list(res["ci"])
    return res


def status(rule: str, p: dict | None) -> str:
    if not p or not p.get("ci"):
        return "pending"
    lo, hi = p["ci"]
    d = p["diff"]
    if rule == "lift5":
        return "✅" if lo > 0 and d >= 0.05 - EPS else "❌"
    if rule == "lift3" and d < 0.03 - EPS:
        return "❌"
    if hi < 0:
        return "❌"
    return "⚠" if lo < 0 else "✅"


def _better(by: dict[str, dict[str, dict]]) -> str:
    """BM25 or the vector arm, whichever has the higher File Acc@5 on the
    instances all three arms have (BM25 on a tie or without vector rows)."""
    ids = [i for i in by if all(a in by[i] for a in (HYBRID, BM25, VECTOR))]
    if not ids:
        return BM25
    bm = mean(metric(by[i][BM25], "file") for i in ids) or 0.0
    vec = mean(metric(by[i][VECTOR], "file") for i in ids) or 0.0
    return VECTOR if vec > bm + EPS else BM25


def _language_rows(per_instance: list[dict], split: str) -> list[dict]:
    present = {r["group"] for r in per_instance if r["arm"] == HYBRID and r["split"] == split}
    out = []
    for g in [f.label for f in families().values() if f.label in present]:
        for v in ("kw", "raw"):
            by = rows_by_id(per_instance, v, split, g)
            other = _better(by)
            p = paired(by, HYBRID, other, "file", split, g, v, other)
            out.append({"group": g, "variant": v, "better": other, "paired": p, "status": status("not_below", p)})
    return out


def gates(s1: dict, split: str) -> dict | None:
    """The §11.2 rows on one split, or None when `rueca-hybrid` has no rows there."""
    per = s1.get("per_instance") or []
    if not any(r["arm"] == HYBRID and r["split"] == split for r in per):
        return None
    pooled = []
    for label, other, v, key, rule in POOLED_ROWS:
        p = paired(rows_by_id(per, v, split), HYBRID, other, key, split, "pooled", v, other, key)
        pooled.append({"label": label, "floor": FLOORS[rule], "paired": p, "status": status(rule, p)})
    return {"split": split, "pooled": pooled, "languages": _language_rows(per, split)}


def all_gates(s1: dict) -> dict:
    return {sp: g for sp in SPLITS if (g := gates(s1, sp)) is not None}


def _delta(p: dict | None) -> str:
    if not p:
        return "—"
    ci = p.get("ci") or [p["diff"], p["diff"]]
    return f"{100 * p['diff']:+.1f} ({100 * ci[0]:+.1f} … {100 * ci[1]:+.1f})"


def render(L: list[str], by_split: dict) -> None:
    if not by_split:
        return
    L += ["## Semantic search gates (RFC 0007 §11.2)", ""]
    L.append("Rueca `hybrid` (`rueca-hybrid`) paired per instance against BM25 (`grep`) and the vector arm, percentage points (95% CI). The gated split is the gate; the calibration split, where α is tuned, is a diagnostic.")
    L.append("")
    for split, g in by_split.items():
        L += [f"### {split} split", "", "| Row (pooled) | Δ (95% CI) | n | Floor | Status |", "|---|---:|---:|---|---|"]
        for r in g["pooled"]:
            L.append(f"| {r['label']} | {_delta(r['paired'])} | {(r['paired'] or {}).get('n', 0)} | {r['floor']} | {r['status']} |")
        L += ["", "| Language | Variant | Better of BM25 / vector | Δ File Acc@5, hybrid − better | n | Status |", "|---|---|---|---:|---:|---|"]
        for r in g["languages"]:
            L.append(f"| {r['group']} | `{r['variant']}` | {r['better']} | {_delta(r['paired'])} | {(r['paired'] or {}).get('n', 0)} | {r['status']} |")
        L.append("")
