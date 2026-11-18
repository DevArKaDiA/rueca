"""RFC 0007 phase 2 decisions over sweep outputs: which second stage to keep
(graph expansion, centrality prior, reranker) and the reranker gate.

**Choice** (§7.6's plateau rule, extended to the second stage): among the
settings whose objective is within one instance of the best, the
**cheapest** — the reranker off (then the smaller K), edges `off` < `calls`
< `calls+contains` < `calls+contains+imports`, β 0 preferred (then the
smaller β); ties by the higher objective, then the order
given. Across knobs the reranker counts first (seconds per query: off,
then the smaller K), then the edges, then β; the sweeps usually vary one
knob at a time, so this order rarely decides. A knob the sweep does not
vary costs nothing. Leaving one language
out redoes the choice without it (S1 only).

**Reranker gate** (§7.5): on the calibration split, each reranked setting
against the same setting without the reranker, over the instances both
answered: pooled Function Acc@10 must rise by ≥ 3 points and pooled File
Acc@5 fall by no more than one instance (1 / n). Printed per variant and
for the mean of `kw` and `raw` (the sweep's `--variant` decides).

**Organization impact** (§7.3, §11.2): consumer hit@10 of the
organization answer per setting (`org.run_impact_sweep`), paired against
BM25's organization answer (`multirepo-<split>-grep.json`): floor = the
95% interval not below 0, target = point ≥ +10 with the interval above 0;
the choice is the cheapest within one instance of the best hit@10.
"""

from __future__ import annotations

import dataclasses
from collections.abc import Callable

from .arms.rueca import DEFAULT_RERANK_K, EXPANDS, RuecaConfig
from .common import scorecard_spec
from .compare import VARIANTS
from .metrics import mean
from .stats import paired_bootstrap, seeded

EPS = 1e-9
GATE_FUNC_LIFT = 0.03
IMPACT_TARGET = 0.10


def cost(cfg: RuecaConfig) -> tuple:
    """Cheaper first (module docstring): the reranker (seconds per query)
    dominates, then the expansion's edges, then β."""
    return (int(cfg.rerank), cfg.rerank_k if cfg.rerank else 0, EXPANDS.index(cfg.expand), cfg.beta)


def varies_stage2(configs: list[RuecaConfig]) -> bool:
    return len({cfg.stage2_tag() for cfg in configs}) > 1


def cheapest_within(values: dict[str, float | None], configs: list[RuecaConfig], n: int) -> dict | None:
    """The cheapest setting within one instance (1 / n) of the best."""
    known = {c.label(): values.get(c.label()) for c in configs if values.get(c.label()) is not None}
    if not known or n <= 0:
        return None
    best = max(known, key=lambda k: known[k])
    tol = 1.0 / n
    within = [c for c in configs if c.label() in known and known[c.label()] >= known[best] - tol - EPS]
    order = {c.label(): i for i, c in enumerate(configs)}
    chosen = min(within, key=lambda c: (cost(c), -known[c.label()], order[c.label()]))
    return {
        "best": {"setting": best, "objective": known[best]},
        "tolerance": tol,
        "within": [c.label() for c in within],
        "setting": chosen.label(),
        "objective": known[chosen.label()],
        "config": chosen.as_dict(),
    }


# --- reranker gate --------------------------------------------------------------------------


def without_reranker(cfg: RuecaConfig) -> RuecaConfig:
    return dataclasses.replace(cfg, rerank=False, rerank_k=DEFAULT_RERANK_K)


def _paired(on: dict, off: dict, ids: list[str], variant: str, key: str, seed: tuple[str, ...]) -> dict:
    a = [on[i][variant][key] for i in ids if on[i][variant][key] is not None and off[i][variant][key] is not None]
    b = [off[i][variant][key] for i in ids if on[i][variant][key] is not None and off[i][variant][key] is not None]
    if not a:
        return {"n": 0, "diff": None, "ci": None}
    sc = scorecard_spec()
    res = paired_bootstrap(a, b, int(sc.get("bootstrap_resamples", 10000)), seeded(int(sc.get("bootstrap_seed", 20261025)), "rerank-gate", *seed)) or {}
    return {"n": len(a), "diff": sum(a) / len(a) - sum(b) / len(b), "ci": list(res["ci"]) if res.get("ci") else None}


def gate_variant(on: dict, off: dict, variant: str, seed: tuple[str, ...]) -> dict:
    ids = sorted(set(on) & set(off))
    func = _paired(on, off, ids, variant, "func_acc@10", (*seed, variant, "func"))
    file = _paired(on, off, ids, variant, "file_acc@5", (*seed, variant, "file"))
    return {"variant": variant, "n": len(ids), "func": func, "file": file}


def verdict(func_diff: float | None, file_diff: float | None, n: int) -> dict:
    """Δ Function Acc@10 ≥ +3 points and File Acc@5 drop ≤ one instance."""
    if func_diff is None or file_diff is None or n == 0:
        return {"status": "pending", "func_ok": None, "file_ok": None, "one_instance": None}
    func_ok = func_diff >= GATE_FUNC_LIFT - EPS
    file_ok = -file_diff <= 1.0 / n + EPS
    return {"status": "✅ holds" if func_ok and file_ok else "❌ fails", "func_ok": func_ok, "file_ok": file_ok, "one_instance": 1.0 / n}


def rerank_gates(rows: dict[str, dict], configs: list[RuecaConfig], variant: str = "both") -> list[dict]:
    """Every reranked setting whose twin without the reranker was swept."""
    labels = {c.label() for c in configs}
    out = []
    for cfg in configs:
        twin = without_reranker(cfg)
        if not cfg.rerank or twin.label() not in labels or cfg.label() not in rows or twin.label() not in rows:
            continue
        per = {v: gate_variant(rows[cfg.label()], rows[twin.label()], v, (cfg.label(),)) for v in VARIANTS}
        chosen = [per[v] for v in (VARIANTS if variant == "both" else (variant,))]
        fd, ld = mean(p["func"]["diff"] for p in chosen), mean(p["file"]["diff"] for p in chosen)
        out.append({"setting": cfg.label(), "without": twin.label(), "k": cfg.rerank_k, "variants": per, "func_diff": fd, "file_diff": ld, **verdict(fd, ld, min(p["n"] for p in chosen))})
    return out


def _pts(x: float | None) -> str:
    return "—" if x is None else f"{100 * x:+.1f}"


def _ci(p: dict) -> str:
    ci = p.get("ci")
    return f"{_pts(p.get('diff'))} ({_pts(ci[0])} … {_pts(ci[1])})" if ci else _pts(p.get("diff"))


def render_gates(gates: list[dict], variant: str) -> str:
    L = ["## Reranker gate (RFC 0007 §7.5)", ""]
    if not gates:
        return "\n".join([*L, "No reranked setting with its twin without the reranker in this sweep.", ""]) + "\n"
    L += [
        f"Gate: pooled Function Acc@10 +3 points or more and pooled File Acc@5 down by at most one instance, reranked − the same setting without it, over the instances both answered; variant `{variant}`.",
        "",
        "| Reranked setting | Variant | n | Δ Fn Acc@10 (95% CI) | Δ File Acc@5 (95% CI) |",
        "|---|---|---:|---:|---:|",
    ]
    for g in gates:
        for v, p in g["variants"].items():
            L.append(f"| `{g['setting']}` | {v} | {p['n']} | {_ci(p['func'])} | {_ci(p['file'])} |")
    L += ["", "| Reranked setting | K | Δ Fn Acc@10 | ≥ +3 | Δ File Acc@5 | drop ≤ one instance | Gate |", "|---|---:|---:|---|---:|---|---|"]
    for g in gates:
        one = "—" if g["one_instance"] is None else f"{100 * g['one_instance']:.2f}"
        L.append(f"| `{g['setting']}` | {g['k']} | {_pts(g['func_diff'])} | {_yes(g['func_ok'])} | {_pts(g['file_diff'])} | {_yes(g['file_ok'])} (one = {one}) | **{g['status']}** |")
    return "\n".join([*L, ""]) + "\n"


def _yes(x: bool | None) -> str:
    return "—" if x is None else ("yes" if x else "no")


def render_choice(choice: dict | None, lolo: list[dict] | None = None, metric: str = "objective") -> str:
    L = ["## Second-stage choice (RFC 0007 §7.6, cheapest within one instance)", ""]
    if choice is None:
        return "\n".join([*L, "No setting has results yet.", ""]) + "\n"
    L += [
        f"- Best: `{choice['best']['setting']}` ({100 * choice['best']['objective']:.1f} {metric}); one instance = {100 * choice['tolerance']:.2f} points.",
        f"- Within one instance: {', '.join(f'`{x}`' for x in choice['within'])}.",
        f"- **Choice: `{choice['setting']}`** ({100 * choice['objective']:.1f}): reranker off preferred, then edges off < calls < calls+contains < +imports, then β 0.",
        "",
    ]
    if lolo:
        L += ["| Held out | n | Choice without it | Same as the full choice |", "|---|---:|---|---|"]
        L += [f"| {r['group']} | {r['n']} | `{r['setting']}` | {'yes' if r['setting'] == choice['setting'] else '**no**'} |" for r in lolo]
        L.append("")
    return "\n".join(L) + "\n"


def leave_one_out(objective_without: Callable[[str], dict[str, float | None]], groups: list[tuple[str, int]], configs: list[RuecaConfig], n: int) -> list[dict]:
    """The choice redone without each language (`objective_without(group)`:
    values of every setting with that language left out)."""
    out = []
    for g, n_g in groups:
        alt = cheapest_within(objective_without(g), configs, n - n_g)
        if alt is not None:
            out.append({"group": g, "n": n_g, "setting": alt["setting"]})
    return out


# --- organization impact --------------------------------------------------------------------


def impact_status(diff: float | None, ci: list[float] | None) -> tuple[str, bool]:
    """(floor status, target met) of consumer hit@10 setting − BM25."""
    if diff is None or not ci:
        return "pending", False
    lo, hi = ci
    floor = "❌" if hi < 0 else ("⚠" if lo < 0 else "✅")
    return floor, diff >= IMPACT_TARGET - EPS and lo > 0
