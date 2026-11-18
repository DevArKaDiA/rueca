"""Statistics of the organization scenario (RFC 0004 §6.1, §7.6; RFC 0006 §8).

Input: one split of `org.score_multirepo()` (rows per instance per arm and
mode). Output, per split (`org_stats`):

- **per arm and mode** (`arms`, issue instances; `impact`, impact
  instances): every 0/1 rate with its Wilson 95% interval and n (File
  Acc@5, Function Acc@10 over the instances with function gold, the same
  for the single answer, consumer hit@10 / hit@20 for impact), the
  organization / single ratio of File Acc@5 with a percentile-bootstrap
  interval (instances resampled; ratio of the two means), tokens per correct
  answer and its organization / single ratio with a bootstrap interval;
- **paired comparisons** (`paired`): for each of `COMPARISONS` and each
  metric (issue: File Acc@5, Function Acc@10 over the instances with
  function gold; impact: consumer hit@10, own File Acc@5), the paired
  bootstrap of mean(A − B) over the instances both arms answered (10,000
  resamples, `[scorecard]` seed, `seeded(base, "org", split, metric, arm_a,
  mode_a, arm_b, mode_b)`), its status as RFC 0004 §6.1 reads a relative
  floor (✅ whole interval ≥ 0, ⚠ it crosses 0, ❌ it is below 0), and the
  power of the comparison:

  - *discordance* d = share of instances where the two arms disagree (one
    hit, the other miss);
  - *observed half-width* of the paired 95% interval;
  - *minimum detectable effect* at 80% power, two-sided α = 0.05, for a
    paired binary outcome (McNemar, normal approximation):
    **MDE ≈ (1.96 + 0.84) · √(d / n)** — the smallest true difference the
    comparison would detect four times in five;
  - the MDE the 21-instance scenario would have had with the same d
    (`OLD_N`), so the gain of the larger population is explicit.

`crossrepo_pools` pools the cross-repository edge counts over every pair,
the pairs without Rust and the Rust pairs alone, with Wilson intervals and
RFC 0006 §8 floors; `rfc0006_rows` reads the organization suite's rows
(Rueca native) with floors, targets and statuses; `org_block` is the
scorecard's `org` entry (gated split only; never a language gate).
"""

from __future__ import annotations

import hashlib
import json
import math
from collections.abc import Sequence

import numpy as np

from .common import load_manifest, scorecard_spec
from .crossrepo import summarize
from .stats import bootstrap_ratio, paired_bootstrap, seeded, wilson

Z_ALPHA = 1.96  # two-sided α = 0.05
Z_POWER = 0.84  # 80% power
OLD_N = 21  # instances of the scenario this one replaces
ARMS = ("rueca", "grep", "cbm")
MODES = ("native", "per_repo")
# (A, B): A − B
COMPARISONS = [
    (("rueca", "native"), ("grep", "native")),
    (("rueca", "native"), ("cbm", "per_repo")),
    (("rueca", "native"), ("rueca", "per_repo")),
    (("grep", "native"), ("grep", "per_repo")),
]
ISSUE_METRICS = ("file_acc@5", "func_acc@10")
# what a Rueca setting with vectors or a second stage (`rueca-hybrid:<tag>`,
# RFC 0007) is paired against: BM25 (the §11.2 rows) and lexical Rueca
SETTING_BASES = (("grep", "native"), ("rueca", "native"))
IMPACT_METRICS = ("consumer_hit@10", "file_acc@5")
# RFC 0006 §8
RATIO_FLOOR, RATIO_TARGET = 0.85, 0.95
TOKENS_TARGET = 1.2
UPDATE_FLOOR, UPDATE_TARGET = 2.0, 1.0
EDGES_FLOOR = {"precision": 0.90, "recall": 0.75}
EDGES_TARGET = {"precision": 0.97, "recall": 0.90}


def arm_order(arms: dict | None) -> list[str]:
    """The original arms, then every Rueca setting present (sorted)."""
    return [*ARMS, *sorted(k for k in (arms or {}) if k not in ARMS)]


def comparisons(arms: dict | None) -> list[tuple[tuple[str, str], tuple[str, str]]]:
    """`COMPARISONS` plus, for every Rueca setting present, its organization
    answer against BM25's and lexical Rueca's."""
    extra = [((k, "native"), base) for k in arm_order(arms) if k not in ARMS for base in SETTING_BASES]
    return [*COMPARISONS, *extra]


def _settings() -> tuple[int, int]:
    sc = scorecard_spec()
    return int(sc.get("bootstrap_resamples", 10000)), int(sc.get("bootstrap_seed", 20261025))


def mde(d: float, n: int) -> float | None:
    """Minimum detectable effect of a paired binary comparison at 80% power,
    two-sided α = 0.05: (1.96 + 0.84) · √(d / n), d the discordance rate."""
    if n <= 0:
        return None
    return (Z_ALPHA + Z_POWER) * math.sqrt(d / n)


def rate(values: Sequence[float]) -> dict:
    """{value, ci (Wilson 95%), n} of 0/1 outcomes."""
    n = len(values)
    k = float(sum(values))
    ci = wilson(k, n)
    return {"value": k / n if n else None, "ci": list(ci) if ci else None, "n": n}


def relative_status(ci: Sequence[float] | None) -> str:
    """RFC 0004 §6.1 relative floor: ✅ interval ≥ 0, ⚠ crosses 0, ❌ below 0."""
    if not ci:
        return "pending"
    lo, hi = ci
    return "❌" if hi < 0 else ("⚠" if lo < 0 else "✅")


def absolute_status(value: float | None, ci: Sequence[float] | None, floor: float, lower: bool = False) -> str:
    """RFC 0004 §6.1 absolute floor: ❌ point below the floor, ⚠ met but the
    interval crosses it, ✅ met with confidence."""
    if value is None:
        return "pending"
    met = value <= floor if lower else value >= floor
    if not met:
        return "❌"
    if ci and None not in ci and ((ci[1] > floor) if lower else (ci[0] < floor)):
        return "⚠"
    return "✅"


def paired_join(rows_a: list[dict], rows_b: list[dict], key: str) -> tuple[list[str], list[float], list[float]]:
    """Instances both arms answered with a value of `key` (joined by id, in
    A's order): ids, A's values, B's values."""
    b = {r["id"]: r.get(key) for r in rows_b}
    ids, va, vb = [], [], []
    for r in rows_a:
        x, y = r.get(key), b.get(r["id"])
        if x is not None and y is not None:
            ids.append(r["id"])
            va.append(float(x))
            vb.append(float(y))
    return ids, va, vb


def paired_comparison(rows_a: list[dict], rows_b: list[dict], key: str, resamples: int, seed: int) -> dict:
    """Paired difference A − B of a 0/1 metric with its bootstrap interval,
    status, discordance and minimum detectable effect (module docstring)."""
    _, a, b = paired_join(rows_a, rows_b, key)
    n = len(a)
    if n == 0:
        return {"n": 0, "status": "pending"}
    pb = paired_bootstrap(a, b, resamples, seed)
    ci = list(pb["ci"]) if pb and pb.get("ci") else None
    disc = sum(1 for x, y in zip(a, b) if x != y)
    d = disc / n
    return {
        "n": n,
        "a": sum(a) / n,
        "b": sum(b) / n,
        "diff": pb["diff"] if pb else None,
        "ci": ci,
        "status": relative_status(ci),
        "discordant": disc,
        "discordance": d,
        "half_width": (ci[1] - ci[0]) / 2 if ci else None,
        "mde": mde(d, n),
        "mde_old_n": mde(d, OLD_N),
    }


def _tokens_ratio_ci(rows: list[dict], resamples: int, seed: int) -> list | None:
    """Bootstrap interval of the tokens-per-correct ratio (organization /
    single) over the rows both answers exist for, instances resampled."""
    pr = [r for r in rows if r.get("fh") is not None and r.get("tokens") is not None and r.get("single_tokens") is not None and r.get("single_fh") is not None]
    if not pr:
        return None
    t = np.array([r["tokens"] for r in pr], dtype=float)
    f = np.array([1.0 if r["fh"] else 0.0 for r in pr])
    st = np.array([r["single_tokens"] for r in pr], dtype=float)
    sf = np.array([1.0 if r["single_fh"] else 0.0 for r in pr])
    rng = np.random.default_rng(seed)
    idx = rng.integers(0, len(pr), size=(resamples, len(pr)))
    num_f, den_f = f[idx].sum(axis=1), sf[idx].sum(axis=1)
    ok = (num_f > 0) & (den_f > 0)
    if not ok.any():
        return None
    ratio = (t[idx].sum(axis=1)[ok] / num_f[ok]) / (st[idx].sum(axis=1)[ok] / den_f[ok])
    lo, hi = np.percentile(ratio, [2.5, 97.5])
    return [float(lo), float(hi)]


def mode_stats(agg: dict, split: str, arm: str, mode: str, resamples: int, seed: int) -> dict:
    """Intervals of one issue aggregate (`org.multirepo_aggregate`)."""
    if agg.get("status") != "ok":
        return {"status": agg.get("status") or "pending", "reason": agg.get("reason")}
    rows = agg["rows"]
    single = [r for r in rows if r.get("single_file_acc@5") is not None]
    ratio_ci = bootstrap_ratio([r["file_acc@5"] for r in single], [r["single_file_acc@5"] for r in single], resamples, seeded(seed, "org", split, "ratio", arm, mode))
    return {
        "status": "ok",
        "n": agg["n"],
        "file_acc@5": rate([r["file_acc@5"] for r in rows]),
        "single_file_acc@5": rate([r["single_file_acc@5"] for r in single]),
        "func_acc@10": rate([r["func_acc@10"] for r in rows if r["func_acc@10"] is not None]),
        "single_func_acc@10": rate([r["single_func_acc@10"] for r in rows if r.get("single_func_acc@10") is not None]),
        "file_acc@5_ratio": {"value": agg.get("file_acc@5_ratio"), "ci": list(ratio_ci) if ratio_ci else None},
        "other_repo_top10": agg.get("other_repo_top10"),
        "tokens_median": agg.get("tokens_median"),
        "single_tokens_median": agg.get("single_tokens_median"),
        "tokens_per_correct": agg.get("tokens_per_correct"),
        "single_tokens_per_correct": agg.get("single_tokens_per_correct"),
        "tokens_per_correct_ratio": {"value": agg.get("tokens_per_correct_ratio"), "ci": _tokens_ratio_ci(rows, resamples, seeded(seed, "org", split, "tokens_ratio", arm, mode))},
        "found_top10": agg.get("found_top10"),
        "single_found_top10": agg.get("single_found_top10"),
    }


def impact_mode_stats(agg: dict) -> dict:
    """An impact aggregate (`org.impact_aggregate`) without its rows."""
    if agg.get("status") != "ok":
        return {"status": agg.get("status") or "pending", "reason": agg.get("reason")}
    return {k: v for k, v in agg.items() if k != "rows"}


def _rows(split_score: dict, arm: str, mode: str, kind: str) -> list[dict] | None:
    a = (split_score.get("arms") or {}).get(arm) or {}
    if a.get("status") != "ok":
        return None
    m = (a.get("impact" if kind == "impact" else "modes") or {}).get(mode) or {}
    return m.get("rows") if m.get("status") == "ok" else None


def org_stats(split_score: dict, split: str, resamples: int | None = None, seed: int | None = None) -> dict:
    """Per-arm intervals and paired comparisons of one split (module
    docstring). Arms or modes without output are `pending`."""
    if resamples is None or seed is None:
        resamples, seed = _settings()
    out: dict = {
        "split": split,
        "instances": split_score.get("instances", 0),
        "impact_instances": split_score.get("impact_instances", 0),
        "counts": split_score.get("counts") or {},
        "arms": {},
        "impact": {},
        "paired": [],
    }
    arms = split_score.get("arms") or {}
    for arm in arm_order(arms):
        a = arms.get(arm)
        if not a or a.get("status") != "ok":
            out["arms"][arm] = {"status": (a or {}).get("status") or "pending", "reason": (a or {}).get("error")}
            continue
        out["arms"][arm] = {mode: mode_stats(m, split, arm, mode, resamples, seed) for mode, m in (a.get("modes") or {}).items()}
        if a.get("impact"):
            out["impact"][arm] = {mode: impact_mode_stats(m) for mode, m in a["impact"].items()}
    for kind, metrics in (("issue", ISSUE_METRICS), ("impact", IMPACT_METRICS)):
        for (aa, ma), (ab, mb) in comparisons(arms):
            ra, rb = _rows(split_score, aa, ma, kind), _rows(split_score, ab, mb, kind)
            for key in metrics:
                entry = {"kind": kind, "metric": key, "a": f"{aa} {ma}", "b": f"{ab} {mb}"}
                if ra is None or rb is None:
                    entry.update(n=0, status="pending")
                else:
                    res = paired_comparison(ra, rb, key, resamples, seeded(seed, "org", split, kind, key, aa, ma, ab, mb))
                    entry.update({k: v for k, v in res.items() if k not in ("a", "b")}, mean_a=res.get("a"), mean_b=res.get("b"))
                out["paired"].append(entry)
    return out


def crossrepo_pools(xr: dict) -> dict:
    """Pooled cross-repository edge counts (`crossrepo.summarize`) over all
    pairs, the pairs without Rust and the Rust pairs, with Wilson intervals
    of precision (correct / predicted) and recall (found / gold) and RFC
    0006 §8 statuses."""
    if not xr or xr.get("status") != "ok":
        return {}
    groups = [g for g in xr.get("groups", []) if g.get("status") == "ok"]
    pools = {"all": groups, "without_rust": [g for g in groups if g.get("language") != "rust"], "rust": [g for g in groups if g.get("language") == "rust"]}
    out: dict = {}
    for name, gs in pools.items():
        if not gs:
            continue
        s = summarize(gs)["all"]
        p = rate([1.0] * s["correct"] + [0.0] * (s["predicted"] - s["correct"]))
        r = rate([1.0] * s["found"] + [0.0] * (s["gold"] - s["found"]))
        out[name] = {
            "pairs": sum(len(g["libraries"]) for g in gs),
            "counts": {k: s[k] for k in ("predicted", "correct", "gold", "found")},
            "precision": {**p, "floor": EDGES_FLOOR["precision"], "target": EDGES_TARGET["precision"], "status": absolute_status(p["value"], p["ci"], EDGES_FLOOR["precision"])},
            "recall": {**r, "floor": EDGES_FLOOR["recall"], "target": EDGES_TARGET["recall"], "status": absolute_status(r["value"], r["ci"], EDGES_FLOOR["recall"])},
        }
    return out


def rfc0006_rows(split_score: dict, stats: dict) -> list[dict]:
    """RFC 0006 §8 rows of the organization suite (Rueca native) with floor,
    target and status; `pending` without a Rueca output."""
    a = (split_score.get("arms") or {}).get("rueca") or {}
    m = ((stats.get("arms") or {}).get("rueca") or {}).get("native") or {}
    rows: list[dict] = []
    if a.get("status") != "ok" or m.get("status") != "ok":
        return [{"key": k, "status": "pending"} for k in ("file_acc@5_ratio", "tokens_per_correct_ratio", "incremental_update", "org_layer_per_step")]
    ratio = m["file_acc@5_ratio"]
    rows.append(
        {
            "key": "file_acc@5_ratio",
            "label": "File Acc@5, organization scope vs the right repository alone",
            "value": m["file_acc@5"]["value"],
            "single": m["single_file_acc@5"]["value"],
            "ratio": ratio["value"],
            "ci": ratio["ci"],
            "n": m["n"],
            "floor": RATIO_FLOOR,
            "target": RATIO_TARGET,
            "status": absolute_status(ratio["value"], ratio["ci"], RATIO_FLOOR),
            "target_met": ratio["value"] is not None and ratio["value"] >= RATIO_TARGET,
        }
    )
    tk = m["tokens_per_correct_ratio"]
    rows.append(
        {
            "key": "tokens_per_correct_ratio",
            "label": "Tokens per correct answer, organization scope vs single repository (top 10)",
            "value": m.get("tokens_per_correct"),
            "single": m.get("single_tokens_per_correct"),
            "ratio": tk["value"],
            "ci": tk["ci"],
            "found": [m.get("found_top10"), m.get("single_found_top10")],
            "floor": None,
            "target": TOKENS_TARGET,
            "status": "—",
            "target_met": tk["value"] is not None and tk["value"] <= TOKENS_TARGET,
        }
    )
    layer = a.get("org_layer") or {}
    rep = layer.get("replace") or {}
    if rep:
        rows.append(
            {
                "key": "incremental_update",
                "label": "Incremental update: re-index one repository of the organization (organization layer vs its own index time)",
                "repo": rep.get("repo"),
                "docs": rep.get("docs"),
                "org_repos": rep.get("org_repos"),
                "value": rep.get("org_ms"),
                "single": rep.get("repo_ms"),
                "first_repo_ms": rep.get("first_repo_ms"),
                "ratio": rep.get("ratio"),
                "floor": UPDATE_FLOOR,
                "target": UPDATE_TARGET,
                "status": absolute_status(rep.get("ratio"), None, UPDATE_FLOOR, lower=True),
                "target_met": rep.get("ratio") is not None and rep["ratio"] <= UPDATE_TARGET,
            }
        )
    if layer.get("steps"):
        rows.append(
            {
                "key": "org_layer_per_step",
                "label": "Organization layer per added repository (median / max over the index --org steps)",
                "steps": layer["steps"],
                "value": layer.get("org_ms_median"),
                "value_max": layer.get("org_ms_max"),
                "single": layer.get("repo_ms_median"),
                "ratio": layer.get("ratio_median"),
                "ratio_max": layer.get("ratio_max"),
                "floor": UPDATE_FLOOR,
                "target": UPDATE_TARGET,
                "status": absolute_status(layer.get("ratio_max"), None, UPDATE_FLOOR, lower=True),
                "target_met": layer.get("ratio_max") is not None and layer["ratio_max"] <= UPDATE_TARGET,
            }
        )
    return rows


def org_settings() -> dict:
    """Caps, members and the sha256 of the `[[org.repos]]` list (what the
    organization is: repositories, languages, pins and splits)."""
    cfg = load_manifest().get("org", {})
    repos = cfg.get("repos", [])
    resamples, seed = _settings()
    return {
        "max_instances_per_repo": cfg.get("max_instances_per_repo"),
        "calibration_max_instances_per_repo": cfg.get("calibration_max_instances_per_repo"),
        "members": sum(1 for r in repos if r.get("split", "gated") == "gated"),
        "calibration_members": sum(1 for r in repos if r.get("split") == "calibration"),
        "pins_sha256": hashlib.sha256(json.dumps([[r["repo"], r["language"], r["commit"], r.get("split", "gated")] for r in repos]).encode()).hexdigest(),
        "resamples": resamples,
        "seed": seed,
        "mde": "(1.96 + 0.84) * sqrt(d / n): paired binary outcome, 80% power, two-sided alpha 0.05, d = discordance rate",
        "mde_old_n": OLD_N,
    }


def org_block(split_score: dict, stats: dict, xr: dict) -> dict:
    """The scorecard's `org` entry: the gated split's per-arm metrics with
    intervals, the paired comparisons with status and MDE, the impact kind,
    the RFC 0006 §8 rows, the cross-repository edge pools, the instance
    counts and the settings. Not a language gate (`rueca-bench check` reads
    `languages` and `config` only)."""
    if not split_score.get("arms"):
        return {"status": "pending", "reason": "no organization output (rueca-bench run --suite org)", "crossrepo": crossrepo_pools(xr), "settings": org_settings()}
    return {
        "status": "ok",
        "split": "gated",
        "population": split_score.get("population"),
        "instances": {"issue": stats["instances"], "impact": stats["impact_instances"], "by_language": stats["counts"]},
        "arms": stats["arms"],
        "impact": stats["impact"],
        "paired": stats["paired"],
        "rfc0006": rfc0006_rows(split_score, stats),
        "crossrepo": crossrepo_pools(xr),
        "settings": org_settings(),
    }
