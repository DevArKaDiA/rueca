"""RFC 0007 §11.2 latency rows (part of the health suite, `rueca-bench run
--arm rueca-hybrid --suite health`).

Population: the health suite's gated corpus repositories (`health.targets`:
every S1 repository at the base commit of its first instance, every S3 gold
repository at its tag). Each is indexed once with `rueca index
--embed=<preset>` (the `rueca-hybrid` setting the environment names, the
reranker forced off; the shared `RUECA_VECTOR_DIR` makes repositories
already embedded by S1 free) and asked:

- **keyword-length queries**: the `kw` query of every gated S1 instance of
  that repository, and for an S3 repository the bare names of its lookup
  sample (what S3's lookups search for); `search_symbols` with `mode:
  hybrid` at the scoring limits (S1 `result_limit`, lookups 10);
- **raw queries**: the issue text of every gated S1 instance (`raw`).

Measurements per repository:

- **warm**: one `rueca query --batch` process over every keyword query
  then every raw query (`arms/batch.py`); a request's latency is the time
  between consecutive answer lines, the first answer (process start, model
  load) is kept apart as `first_s` and left out of the warm rows;
- **cold**: one `rueca query` process per keyword query (at most
  `COLD_SAMPLE` per repository), start-up and model load included — what a
  shell call costs;
- **reranker**: the first `--rerank-sample` keyword queries with `rerank:
  true` at K = 20 and K = 50 in one batch per backend (its first answer,
  which loads the reranker, left out): the default backend (Metal on a
  Mac), and the CPU when `rueca query` exposes a switch (`--cpu`, or a
  `RUECA_EMBED_CPU` variable its help names; without one the CPU row is
  `unavailable` and says so). Seconds per request, plus the envelope's
  `retrieval.reranker.ms` when the binary reports it.

A binary without `--batch` records cold calls only (the warm rows stay
pending). Raw outputs: `.cache/runs/health/<rueca-hybrid/<tag>>/<name>.json`;
`score()` pools them: p95 with the binomial order-statistic interval
(`stats.quantile_ci`), n, per row.
"""

from __future__ import annotations

import dataclasses
import traceback
from pathlib import Path
from typing import Any

from . import s1 as s1_mod
from .arms.rueca import RuecaArm, RuecaConfig
from .common import GOLD_DIR, RUNS_DIR, load_manifest, log, read_json, safe_name, write_json
from .corpus import prune_clone, worktree
from .health import targets
from .stats import quantile_ci

RERANK_SAMPLE = 3
RERANK_KS = (20, 50)
COLD_SAMPLE = 20
LOOKUP_LIMIT = 10
BACKENDS = ("default", "cpu")


def config() -> RuecaConfig:
    """The hybrid setting the environment names, without the reranker."""
    return dataclasses.replace(RuecaConfig.from_env(mode="hybrid"), rerank=False)


def run_path(name: str, cfg: RuecaConfig | None = None) -> Path:
    return RUNS_DIR / "health" / (cfg or config()).run_label() / f"{safe_name(name)}.json"


# --- queries ------------------------------------------------------------------------------


def s3_lookup_names(name: str) -> list[str]:
    """Bare names of an S3 repository's lookup sample (its gold)."""
    p = GOLD_DIR / "s3" / f"{name.removeprefix('s3-')}.json"
    if not p.exists():
        return []
    gold = read_json(p)
    calls = gold.get("callables") or {}
    return [calls[q]["name"] for q in gold.get("sample") or [] if q in calls and calls[q].get("name")]


def target_queries(t: dict) -> dict[str, list[tuple[str, int]]]:
    """{kw, raw}: (query, limit) of one target (module docstring)."""
    if t["source"] == "s3":
        return {"kw": [(n, LOOKUP_LIMIT) for n in s3_lookup_names(t["name"])], "raw": []}
    limit = load_manifest()["s1"].get("result_limit", 100)
    insts = [i for i in s1_mod.select_instances(split="gated") if i["repo"] == t["repo"]]
    qs = [s1_mod.queries_for(i) for i in insts]
    return {"kw": [(q["kw"], limit) for q in qs if q["kw"]], "raw": [(q["raw"], limit) for q in qs if q["raw"]]}


# --- measuring ----------------------------------------------------------------------------


def _requests(arm: RuecaArm, queries: list[tuple[str, int]], extra: dict[str, Any] | None = None) -> list[tuple[str, dict, str]]:
    return [("search_symbols", {**arm.search_args(q, limit), **(extra or {})}, "scoring") for q, limit in queries]


def warm(arm: RuecaArm, h, qs: dict[str, list[tuple[str, int]]]) -> dict | None:
    """One batch over the keyword then the raw queries."""
    reqs = _requests(arm, qs["kw"]) + _requests(arm, qs["raw"])
    res = arm.prefetch(reqs, h.root, h.state["store"], extra=([], {}))
    if res is None or not res.lines:
        return None
    secs = [x.seconds if x.error is None else None for x in res.lines]
    secs[0] = None  # process start and model load: `first_s`
    k = len(qs["kw"])
    errors = [x.error for x in res.lines if x.error][:3]
    return {"first_s": res.first_s, "kw": secs[:k], "raw": secs[k:], "rc": res.rc, "errors": errors}


def cold(arm: RuecaArm, h, qs: dict[str, list[tuple[str, int]]]) -> dict:
    out: list[float | None] = []
    for q, limit in qs["kw"][:COLD_SAMPLE]:
        _, secs, err, _ = arm.call("search_symbols", arm.search_args(q, limit), h.root, h.state["store"])
        out.append(None if err else secs)
    return {"kw": out}


def _reranker_ms(data: Any) -> float | None:
    if not isinstance(data, dict):
        return None
    return ((data.get("retrieval") or {}).get("reranker") or {}).get("ms")


def rerank(arm: RuecaArm, h, qs: dict[str, list[tuple[str, int]]], sample: int, switch: tuple[list[str], dict[str, str]]) -> dict:
    """K = 20 then K = 50 over the sample, after one discarded warm-up."""
    sample_qs = qs["kw"][:sample]
    if not sample_qs:
        return {}
    reqs = _requests(arm, sample_qs[:1], {"rerank": True, "rerank_k": RERANK_KS[0]})
    for k in RERANK_KS:
        reqs += _requests(arm, sample_qs, {"rerank": True, "rerank_k": k})
    res = arm.prefetch(reqs, h.root, h.state["store"], extra=switch)
    if res is None:
        return {}
    lines = res.lines[1:]
    out: dict = {"first_s": res.first_s, "errors": [x.error for x in res.lines if x.error][:3]}
    for i, k in enumerate(RERANK_KS):
        part = lines[i * len(sample_qs) : (i + 1) * len(sample_qs)]
        out[str(k)] = [x.seconds if x.error is None else None for x in part]
        out[f"ms{k}"] = [_reranker_ms(x.data) for x in part]
    return out


def _switches(arm: RuecaArm) -> dict[str, tuple[list[str], dict[str, str]] | None]:
    return {"default": ([], {}), "cpu": arm.cpu_switch()}


def measure(arm: RuecaArm, t: dict, root: Path, rerank_sample: int) -> dict:
    """Index one target and take every measurement (module docstring)."""
    qs = target_queries(t)
    h = arm.index(root, f"latency-{t['name']}", t["language"])
    rec: dict = {"index_seconds": h.index_seconds, "vectors": (h.info or {}).get("vectors"), "queries": {k: len(v) for k, v in qs.items()}}
    if not h.ok:
        return {**rec, "ok": False, "error": h.error}
    try:
        rec["batch"] = arm.supports_batch
        rec["warm"] = warm(arm, h, qs)
        rec["cold"] = cold(arm, h, qs)
        switches = _switches(arm)
        rec["cpu_switch"] = None if switches["cpu"] is None else (switches["cpu"][0] or list(switches["cpu"][1]))
        rec["rerank"] = {b: rerank(arm, h, qs, rerank_sample, sw) if sw is not None and rec["batch"] else None for b, sw in switches.items()}
    finally:
        arm.cleanup(h)
    return {**rec, "ok": True}


def run(languages: set[str] | None = None, force: bool = False, only: list[str] | None = None, prune: bool = False, rerank_sample: int = RERANK_SAMPLE) -> None:
    cfg = config()
    arm = RuecaArm(config=cfg)
    if not arm.available()[0]:
        log(f"latency: rueca unavailable ({arm.available()[1]})")
        return
    arm.preflight()
    for t in targets(languages):
        if t["split"] != "gated" or (only and t["name"] not in only and t["repo"] not in only):
            continue
        p = run_path(t["name"], cfg)
        if p.exists() and not force and read_json(p).get("ok"):
            continue
        log(f"latency {t['name']} ({cfg.describe()})")
        try:
            with worktree(t["repo"], t["commit"], f"latency-{t['name']}") as root:
                rec = measure(arm, t, root, rerank_sample)
        except Exception as exc:
            rec = {"ok": False, "error": f"{exc}\n{traceback.format_exc()[-1000:]}"}
        rec.update({k: t[k] for k in ("name", "repo", "commit", "language", "source", "split")}, setting=cfg.as_dict(), arm_version=arm.version())
        write_json(p, rec)
        if prune:
            prune_clone(t["repo"])


# --- scoring ------------------------------------------------------------------------------


def p95_row(values: list[float | None], unit: float = 1000.0) -> dict:
    """{value, ci, n} of the 95th percentile, in milliseconds."""
    v = [x for x in values if x is not None]
    q = quantile_ci(v) if v else None
    return {"value": q[0] * unit if q else None, "ci": [q[1] * unit, q[2] * unit] if q else None, "n": len(v)}


def _collect(recs: list[dict], path: tuple[str, ...]) -> list[float | None]:
    out: list[float | None] = []
    for r in recs:
        node: Any = r
        for key in path:
            node = (node or {}).get(key) if isinstance(node, dict) else None
        out += node or []
    return out


def score(cfg: RuecaConfig | None = None) -> dict:
    """Pooled rows over every target with an output (module docstring)."""
    cfg = cfg or config()
    d = run_path("x", cfg).parent
    recs = [read_json(p) for p in sorted(d.glob("*.json"))] if d.is_dir() else []
    ok = [r for r in recs if r.get("ok")]
    if not ok:
        return {"status": "pending", "setting": cfg.run_label(), "targets": len(recs)}
    out: dict = {
        "status": "ok",
        "setting": cfg.run_label(),
        "targets": len(ok),
        "failed": len(recs) - len(ok),
        "batch": all(r.get("batch") for r in ok),
        "warm_kw": p95_row(_collect(ok, ("warm", "kw"))),
        "warm_raw": p95_row(_collect(ok, ("warm", "raw"))),
        "first_s": p95_row([(r.get("warm") or {}).get("first_s") for r in ok]),
        "cold_kw": p95_row(_collect(ok, ("cold", "kw"))),
        "rerank": {},
        "cpu_switch": next((r.get("cpu_switch") for r in ok if r.get("cpu_switch")), None),
    }
    for b in BACKENDS:
        if not any((r.get("rerank") or {}).get(b) for r in ok):
            continue
        out["rerank"][b] = {str(k): p95_row(_collect(ok, ("rerank", b, str(k)))) for k in RERANK_KS}
        out["rerank"][b].update({f"ms{k}": p95_row(_collect(ok, ("rerank", b, f"ms{k}")), unit=1.0) for k in RERANK_KS})
    return out
