"""The acceptance table of RFC 0007 §11.2, complete: every row with Today,
Floor, Target and Status, fed from the cached runs the report already
scores.

| Row | Source |
|---|---|
| S1 File Acc@5 hybrid − BM25 `raw`, − vector `raw`; Function Acc@10 hybrid − BM25 `raw`; per language; `kw` pooled | `semgates` (paired per instance, gated split) |
| Organization File Acc@5 over single repository | the hybrid setting's organization answer (`org`, gated): its ratio to the same setting's single-repository answer, and to lexical Rueca's |
| Organization File Acc@5 hybrid − BM25 | paired, issue instances |
| Organization impact hit@10 hybrid + expansion − BM25 | paired consumer hit@10, impact instances |
| CoIR cheap profile | `bench/results/coir/<preset>-native-512.json` |
| Hybrid latency p95, warm, keyword-length | `latency.score()` (batch; the cold-process p95 next to it) |
| Lexical latency p95 | every gated S1 / S3 Rueca query (RFC 0004 health), pooled |
| Reranker p95, raw-query p95, symbols/s, cold embed | `latency.score()`, the hybrid S1 index reports |
| Re-index after a one-file change | a Rust test, not a bench row |
| Tokens per correct answer, hybrid over lexical | S1 efficiency searches (gated, paired), the organization's |
| Top-10 overlap across backends | the vector arm's calibration runs, default backend vs `RUECA_BENCH_VECTOR_CPU=1` |

The hybrid setting is the one the environment names (`RuecaConfig.from_env
(mode="hybrid")`, as `report` reads every swept arm): run `report` with the
knobs of the runs it should read (`RUECA_EXPAND` for the impact row).
Status: ✅ met with the interval on the right side, ⚠ met but the interval
crosses the floor, ❌ not met, `reported` (no floor), `pending` (no run).
`scorecard.json` `rfc0007` holds the rows and, for `rueca-bench check`,
the rows with a floor as metrics (latency rows are skipped there, as RFC
0004's: machine-dependent).
"""

from __future__ import annotations

import dataclasses
from pathlib import Path
from typing import Any

from . import latency as latency_mod
from . import semgates
from .arms.rueca import RuecaConfig
from .arms.vector import VectorConfig
from .common import CACHE, RESULTS_DIR, RUNS_DIR, read_json
from .compare import Comparison
from .embed import DEFAULT_MAX_TOKENS
from .orgstats import absolute_status, relative_status
from .stats import quantile_ci

RATIO_FLOOR, RATIO_TARGET = 0.85, 0.95
HYBRID_P95_FLOOR, HYBRID_P95_TARGET = 300.0, 100.0
LEXICAL_P95_FLOOR, LEXICAL_P95_TARGET = 100.0, 20.0
OVERLAP_FLOOR = 0.9
IMPACT_TARGET = 0.10
FILE_TARGET, FUNC_TARGET = 0.80, 0.65


def _pts(x: float | None) -> str:
    return "—" if x is None else f"{100 * x:+.1f}"


def _delta(p: dict | None) -> str:
    if not p or p.get("diff") is None:
        return "—"
    ci = p.get("ci")
    return f"{_pts(p['diff'])} ({_pts(ci[0])} … {_pts(ci[1])}), n = {p.get('n', 0)}" if ci else f"{_pts(p['diff'])}, n = {p.get('n', 0)}"


def _ms(r: dict | None) -> str:
    if not r or r.get("value") is None:
        return "—"
    ci = r.get("ci")
    return f"{r['value']:,.0f} ms" + (f" ({ci[0]:,.0f}–{ci[1]:,.0f})" if ci else "") + f", n = {r['n']}"


def row(key: str, metric: str, today: str, floor: str, target: str, status: str, **extra: Any) -> dict:
    return {"key": key, "metric": metric, "today": today, "floor": floor, "target": target, "status": status, **extra}


# --- S1 -----------------------------------------------------------------------------------


def _hybrid_value(s1: dict, variant: str, key: str) -> float | None:
    """Pooled (`overall`) value of the hybrid arm on the gated split."""
    t = ((s1.get("table") or {}).get(semgates.HYBRID) or {}).get(variant) or {}
    return (t.get("overall") or {}).get(key)


def _target_value(value: float | None, target: float) -> str:
    return "—" if value is None else ("met" if value >= target else "not met") + f" (hybrid {100 * value:.1f})"


def s1_rows(s1: dict) -> list[dict]:
    g = semgates.gates(s1, "gated") or {"pooled": [{} for _ in semgates.POOLED_ROWS], "languages": []}
    p = [x.get("paired") for x in g["pooled"]]
    st = [x.get("status", "pending") for x in g["pooled"]]
    f5, f10 = _hybrid_value(s1, "raw", "file_acc@5"), _hybrid_value(s1, "raw", "func_acc@10")
    above = [bool(x and x.get("ci") and x["ci"][0] > 0) for x in p]
    return [
        row("s1_file5_hybrid_bm25_raw", "S1 File Acc@5, hybrid − BM25, `raw`", _delta(p[0]), semgates.FLOORS["lift5"], f"hybrid ≥ 80: {_target_value(f5, FILE_TARGET)}", st[0], value=(p[0] or {}).get("diff"), ci=(p[0] or {}).get("ci")),
        row("s1_file5_hybrid_vector_raw", "S1 File Acc@5, hybrid − vector arm, `raw`", _delta(p[1]), "interval not below 0", f"interval above 0: {'met' if above[1] else 'not met'}", st[1], value=(p[1] or {}).get("diff"), ci=(p[1] or {}).get("ci")),
        row("s1_func10_hybrid_bm25_raw", "S1 Function Acc@10, hybrid − BM25, `raw`", _delta(p[2]), semgates.FLOORS["lift3"], f"hybrid ≥ 65: {_target_value(f10, FUNC_TARGET)}", st[2], value=(p[2] or {}).get("diff"), ci=(p[2] or {}).get("ci")),
        _language_row(g["languages"]),
        row("s1_kw_hybrid_bm25", "S1 `kw` pooled, hybrid − BM25 (File Acc@5)", _delta(p[3]), "interval not below 0", f"interval above 0: {'met' if above[3] else 'not met'}", st[3], value=(p[3] or {}).get("diff"), ci=(p[3] or {}).get("ci")),
    ]


def _language_row(langs: list[dict]) -> dict:
    metric = "S1 per language, both variants (File Acc@5, hybrid − the better of BM25 and the vector arm)"
    floor = "not significantly below the better of BM25 and the vector arm"
    rated = [x for x in langs if x["status"] != "pending"]
    if not rated:
        return row("s1_per_language", metric, "—", floor, "—", "pending")
    below = [f"{x['group']} `{x['variant']}`" for x in rated if x["status"] == "❌"]
    crossing = sum(1 for x in rated if x["status"] == "⚠")
    today = f"{len(rated) - len(below)} of {len(rated)} rows not below ({crossing} crossing 0)" + (f"; below: {', '.join(below)}" if below else "")
    status = "❌" if below else ("⚠" if crossing else "✅")
    return row("s1_per_language", metric, today, floor, "—", status)


# --- organization ---------------------------------------------------------------------------


def org_key(cfg: RuecaConfig | None = None) -> str:
    return (cfg or RuecaConfig.from_env(mode="hybrid")).label()


def _paired(stats: dict, kind: str, metric: str, a: str, b: str) -> dict | None:
    return next((p for p in stats.get("paired") or [] if p["kind"] == kind and p["metric"] == metric and p["a"] == a and p["b"] == b and p.get("n")), None)


def _native(stats: dict, arm: str) -> dict:
    return ((stats.get("arms") or {}).get(arm) or {}).get("native") or {}


def org_ratio_row(stats: dict, key: str) -> dict:
    metric = "Organization File Acc@5 over single repository, gated (hybrid)"
    m, lex = _native(stats, key), _native(stats, "rueca")
    if m.get("status") != "ok":
        return row("org_file5_ratio", metric, f"pending (`{key}`: run the org suite with this setting); lexical Rueca {_x(lex.get('file_acc@5_ratio', {}).get('value'))}", "≥ 0.85×", "≥ 0.95×", "pending")
    r = m["file_acc@5_ratio"]
    single_lex = (lex.get("single_file_acc@5") or {}).get("value")
    vs_lex = m["file_acc@5"]["value"] / single_lex if single_lex else None
    today = f"{_x(r['value'])}" + (f" ({r['ci'][0]:.2f}–{r['ci'][1]:.2f})" if r.get("ci") else "") + f", n = {m['n']}; lexical Rueca {_x((lex.get('file_acc@5_ratio') or {}).get('value'))}; hybrid org / lexical single {_x(vs_lex)}"
    met = r["value"] is not None and r["value"] >= RATIO_TARGET
    return row("org_file5_ratio", metric, today, "≥ 0.85× (RFC 0006)", f"≥ 0.95×: {'met' if met else 'not met'}", absolute_status(r["value"], r.get("ci"), RATIO_FLOOR), value=r["value"], ci=r.get("ci"), floor_value=RATIO_FLOOR)


def _x(v: float | None) -> str:
    return "—" if v is None else f"{v:.2f}×"


def org_bm25_row(stats: dict, key: str) -> dict:
    p = _paired(stats, "issue", "file_acc@5", f"{key} native", "grep native")
    return row("org_file5_hybrid_bm25", "Organization File Acc@5, larger scenario (n = 120), hybrid − BM25", _delta(p), "reported", "—", "reported" if p else "pending", value=(p or {}).get("diff"), ci=(p or {}).get("ci"))


def org_impact_row(stats: dict, key: str) -> dict:
    metric = f"Organization impact hit@10, hybrid + expansion − BM25 (`{key}`)"
    p = _paired(stats, "impact", "consumer_hit@10", f"{key} native", "grep native")
    if p is None:
        return row("org_impact_hit10", metric, "pending (run the org suite with RUECA_BENCH_RUECA_MODE=hybrid and RUECA_EXPAND)", "interval not below 0", "point ≥ +10 and interval above 0", "pending")
    ci = p.get("ci")
    met = p["diff"] is not None and p["diff"] >= IMPACT_TARGET and bool(ci) and ci[0] > 0
    today = _delta(p) + ("" if "-x" in key else " (no expansion in this setting)")
    return row("org_impact_hit10", metric, today, "interval not below 0", f"point ≥ +10 and interval above 0: {'met' if met else 'not met'}", relative_status(ci), value=p["diff"], ci=ci)


# --- CoIR, latency, throughput -------------------------------------------------------------


def coir_row(preset: str, results: Path | None = None) -> dict:
    metric = f"CoIR cheap profile, `{preset}`: mean over tasks with a published number"
    p = (results or RESULTS_DIR / "coir") / f"{preset}-native-{DEFAULT_MAX_TOKENS}.json"
    if not p.exists():
        return row("coir_cheap", metric, "pending (`rueca-bench coir --profile cheap`)", "within 1.0 point of the published mean", "—", "pending")
    s = read_json(p).get("summary") or {}
    if s.get("published_mean") is None:
        return row("coir_cheap", metric, f"mean {s.get('mean')}", "within 1.0 point of the published mean", "—", "pending")
    today = f"{s['mean_published_tasks']:.2f} vs {s['published_mean']:.2f} (Δ {s['delta']:+.2f})"
    return row("coir_cheap", metric, today, "within 1.0 point of the published mean", "—", "✅" if s.get("within_tolerance") else "❌", value=s["delta"])


def latency_row(lat: dict) -> dict:
    metric = "Hybrid latency p95, warm, keyword-length query, no reranker"
    w = lat.get("warm_kw") or {}
    if lat.get("status") != "ok" or w.get("value") is None:
        why = "no `rueca query --batch`: warm not measurable" if lat.get("status") == "ok" else "`rueca-bench run --arm rueca-hybrid --suite health`"
        return row("hybrid_latency_p95_ms", metric, f"pending ({why}); cold process {_ms(lat.get('cold_kw'))}", "≤ 300 ms", "≤ 100 ms", "pending")
    met = w["value"] <= HYBRID_P95_TARGET
    today = f"warm {_ms(w)}; cold process {_ms(lat.get('cold_kw'))}; first answer of a process {_ms(lat.get('first_s'))}"
    return row("hybrid_latency_p95_ms", metric, today, "≤ 300 ms", f"≤ 100 ms: {'met' if met else 'not met'}", absolute_status(w["value"], w.get("ci"), HYBRID_P95_FLOOR, lower=True), value=w["value"], ci=w.get("ci"), better="lower")


def lexical_latency_row(seconds: list[float]) -> dict:
    metric = "Lexical latency p95 (RFC 0004 health), every gated S1 / S3 query"
    q = quantile_ci(seconds) if seconds else None
    if q is None:
        return row("lexical_latency_p95_ms", metric, "pending", "≤ 100 ms, unchanged", "≤ 20 ms", "pending")
    v, ci = q[0] * 1000, [q[1] * 1000, q[2] * 1000]
    today = f"{v:,.0f} ms ({ci[0]:,.0f}–{ci[1]:,.0f}), n = {len(seconds)} (one process per call)"
    return row("lexical_latency_p95_ms", metric, today, "≤ 100 ms, unchanged", f"≤ 20 ms: {'met' if v <= LEXICAL_P95_TARGET else 'not met'}", absolute_status(v, ci, LEXICAL_P95_FLOOR, lower=True), value=v, ci=ci, better="lower")


def _rerank_txt(lat: dict) -> str:
    parts = []
    for backend, label in (("default", "default backend (Metal on a Mac)"), ("cpu", "CPU")):
        r = (lat.get("rerank") or {}).get(backend)
        if r:
            parts.append(f"{label}: K 20 {_ms(r.get('20'))}, K 50 {_ms(r.get('50'))}")
    if not (lat.get("rerank") or {}).get("cpu") and lat.get("status") == "ok":
        parts.append("CPU: unavailable (`rueca query` exposes no CPU switch)" if not lat.get("cpu_switch") else "CPU: pending")
    return "; ".join(parts) or "pending"


def throughput(s1_label: str) -> dict:
    """Symbols embedded per second and the cold embed time, over the
    hybrid S1 index reports (`index_info.vectors`)."""
    d = RUNS_DIR / "s1" / s1_label
    embedded = ms = 0
    for p in sorted(d.glob("*.json")) if d.is_dir() else []:
        v = (read_json(p).get("index_info") or {}).get("vectors") or {}
        if v.get("embedded"):
            embedded += int(v["embedded"])
            ms += int(v.get("embed_ms") or 0)
    return {"embedded": embedded, "embed_ms": ms, "per_s": embedded / (ms / 1000) if ms else None}


def reported_row(lat: dict, tp: dict) -> dict:
    rate = "—" if tp["per_s"] is None else f"{tp['per_s']:,.0f} symbols/s ({tp['embedded']:,} symbols, {tp['embed_ms'] / 3.6e6:.2f} h of embedding)"
    today = f"reranker p95: {_rerank_txt(lat)}. Raw-query hybrid p95, warm: {_ms(lat.get('warm_raw'))}. Embedding: {rate}"
    return row("reported_latency_throughput", "Reranker p95 (Metal, CPU, CUDA); raw-query hybrid p95; symbols/s per preset and backend; cold embed of the S1 corpora", today, "reported", "—", "reported")


# --- tokens, overlap -------------------------------------------------------------------------


def s1_tokens(s1: dict, arm: str, ids: set[str]) -> tuple[float, int]:
    """(Σ tokens of the efficiency answers, answers with a gold file in the
    top 10) of one arm over `ids` (gated)."""
    rows = [r for r in s1.get("per_instance") or [] if r["arm"] == arm and r["id"] in ids and r["variant"] == "kw" and r.get("e")]
    return sum(r["e"]["t"] for r in rows), sum(1 for r in rows if r["e"]["fh"])


def tokens_row(s1: dict, stats: dict, key: str) -> dict:
    per = [r for r in s1.get("per_instance") or [] if r["split"] == "gated" and r["variant"] == "kw" and r.get("e")]
    ids = {r["id"] for r in per if r["arm"] == semgates.HYBRID} & {r["id"] for r in per if r["arm"] == "rueca"}
    th, fh = s1_tokens(s1, semgates.HYBRID, ids)
    tl, fl = s1_tokens(s1, "rueca", ids)
    ratio = (th / fh) / (tl / fl) if fh and fl and tl else None
    org_h, org_l = _native(stats, key).get("tokens_per_correct"), _native(stats, "rueca").get("tokens_per_correct")
    org_ratio = org_h / org_l if org_h and org_l else None
    today = f"S1 {_x(ratio)} over {len(ids)} gated queries ({fh} vs {fl} with a gold file in the top 10); organization {_x(org_ratio)}"
    target = "—" if ratio is None else ("met" if ratio <= 1.0 else "not met")
    return row("tokens_per_correct_ratio", "Tokens per correct answer, hybrid over lexical", today if ratio or org_ratio else "pending", "reported", f"≤ 1×: {target}", "reported" if ratio or org_ratio else "pending", value=ratio)


def overlap_row(cache: Path | None = None) -> dict:
    metric = "Top-10 overlap across backends, calibration queries (vector arm, default backend vs `--cpu`)"
    cfg = dataclasses.replace(VectorConfig.from_env(), cpu=False)
    cols = [f"vector:{cfg.tag()}", f"vector:{cfg.backend_twin().tag()}"]
    res = Comparison(cache or CACHE, "calibration", "grep", cols).run()
    pairs = [o for o in res["overlap"] if o["n"]]
    if not pairs:
        return row("backend_overlap", metric, "pending (`RUECA_BENCH_VECTOR_CPU=1 rueca-bench run --arm vector --suite s1 --split calibration`)", "≥ 0.9", "—", "pending")
    low = min(min(o["files"], o["functions"]) for o in pairs)
    today = "; ".join(f"`{o['variant']}` files {o['files']:.3f}, functions {o['functions']:.3f} (n = {o['n']})" for o in pairs)
    return row("backend_overlap", metric, today, "≥ 0.9", "—", "✅" if low >= OVERLAP_FLOOR else "❌", value=low, floor_value=OVERLAP_FLOOR)


# --- the table --------------------------------------------------------------------------------


def table(s1: dict, orgs: dict, lexical_seconds: list[float], cache: Path | None = None, coir_dir: Path | None = None) -> dict:
    """Every §11.2 row (module docstring)."""
    hybrid = RuecaConfig.from_env(mode="hybrid")
    stats = (orgs.get("stats") or {}).get("gated") or {}
    key = org_key(hybrid)
    lat = latency_mod.score()
    rows = [
        *s1_rows(s1),
        org_ratio_row(stats, key),
        org_bm25_row(stats, key),
        org_impact_row(stats, key),
        coir_row(hybrid.preset, coir_dir),
        latency_row(lat),
        lexical_latency_row(lexical_seconds),
        reported_row(lat, throughput(hybrid.run_label())),
        row("reindex_one_file", "Re-index after a one-file change: vectors embedded", "a Rust test (`rueca-index`), not a bench row", "only that file's symbols, ≤ 1% of rows (test)", "—", "reported"),
        tokens_row(s1, stats, key),
        overlap_row(cache),
    ]
    return {"setting": key, "rows": rows, "metrics": check_metrics(rows)}


def check_metrics(rows: list[dict]) -> dict:
    """The rows with a floor and a value, in `rueca-bench check`'s shape."""
    out = {}
    for r in rows:
        if r["status"] in ("reported", "pending") or r.get("value") is None:
            continue
        out[r["key"]] = {"value": r["value"], "ci": r.get("ci"), "floor": r.get("floor_value", r["floor"]), "better": r.get("better", "higher"), "gated": True}
    return out


def render(L: list[str], t: dict) -> None:
    L += [
        "## RFC 0007 §11.2 acceptance table",
        "",
        f"Hybrid setting `{t['setting']}` (the one the environment names). Paired bootstrap as RFC 0004 §6.1 (gated split); Δ in percentage points (95% CI). "
        "Status: ✅ met with the interval on the right side, ⚠ met but the interval crosses the floor, ❌ not met, *reported* (no floor), *pending* (not run).",
        "",
        "| Metric | Today | Floor | Target | Status |",
        "|---|---|---|---|---|",
    ]
    L += [f"| {r['metric']} | {r['today']} | {r['floor']} | {r['target']} | {r['status']} |" for r in t["rows"]]
    L.append("")
