"""Driver of the multi-step suite: run the arms over the tasks, aggregate,
check the graduation gate (RFC 0009 §8) and write the results
(`bench/results/multistep.json`, `docs/bench/multistep-results.md`)."""

from __future__ import annotations

import json
import statistics
from pathlib import Path
from typing import Any

from ..arms.cbm import CbmArm
from ..common import DOCS_DIR, RESULTS_DIR, log, read_json, write_json
from . import corpora, inputs
from .cbm_plans import CBM_SKIP
from .run import (
    ARMS,
    Record,
    as_dict,
    binary_label,
    cbm_projects,
    describe_api,
    run_cbm,
    run_plan,
    run_script,
    tokens,
)
from .tasks import TASKS, Task

RESULTS_PATH = RESULTS_DIR / "multistep.json"
DOC_PATH = DOCS_DIR / "multistep-results.md"
GATE_TOKENS = 0.40  # code mode reads at least 40% fewer tokens per correct answer
GATE_OVERHEAD_MS = 20.0  # median execute overhead


def corpora_for(tasks: list[Task], binary: str) -> dict[str, corpora.Corpus]:
    keys = sorted({t.corpus for t in tasks} | set(corpora.SINGLE))
    return {k: corpora.prepare(k, binary) for k in keys}


def _run_task(binary: str, t: Task, cs: dict, gold: Any, arms: tuple[str, ...], cbm: tuple | None) -> list[Record]:
    out = []
    if "plan" in arms:
        out.append(run_plan(binary, t, cs[t.corpus], gold))
    for lang in ("js", "starlark"):
        if lang in arms:
            out.append(run_script(binary, t, cs[t.corpus], lang, gold))
    if cbm is not None and t.id not in CBM_SKIP:
        arm, projects = cbm
        out.append(run_cbm(arm, projects[t.corpus], t, gold))
    return out


def run_suite(binary: str, arms: tuple[str, ...] = ARMS, only: list[str] | None = None) -> dict:
    tasks = [t for t in TASKS if not only or t.id in only]
    inp = inputs.load()
    cs = corpora_for(tasks, binary)
    cbm = None
    if "cbm" in arms:
        arm = CbmArm()
        ok, why = arm.available()
        if ok:
            cbm = (arm, cbm_projects(arm, cs))
        else:
            log(f"cbm unavailable: {why}")
    records = []
    for t in tasks:
        gold = t.gold(inp)
        for rec in _run_task(binary, t, cs, gold, arms, cbm):
            records.append(rec)
            log(f"multistep {t.id} {rec.arm}: {'ok' if rec.correct else 'WRONG'} read {rec.read} calls {rec.calls} {rec.error or ''}")
    api = {lang: tokens(describe_api(binary, cs["flask"], lang)) for lang in ("js", "starlark") if lang in arms}
    doc = summarize(records, api, tasks)
    doc["meta"] = {"binary": binary_label(binary), "cbm": cbm[0].version() if cbm else None, "tasks": len(tasks), "arms": list(arms)}
    doc["records"] = [_compact(as_dict(r)) for r in records]
    return doc


def _compact(r: dict) -> dict:
    r.pop("trace", None)
    return {k: v for k, v in r.items() if v not in (None, False, [], "")} | {"correct": r["correct"]}


# --- aggregation --------------------------------------------------------------------------------


def _ratio(num: float, den: float) -> float | None:
    return num / den if den else None


def _pct(xs: list[float], q: float) -> float | None:
    if not xs:
        return None
    v = sorted(xs)
    return v[min(len(v) - 1, max(0, round(q * (len(v) - 1))))]


def arm_summary(recs: list[dict], api_tokens: int = 0, n_tasks: int = 0) -> dict:
    ok = sum(1 for r in recs if r["correct"])
    read = sum(r["read"] for r in recs)
    over = [(r["total_us"] - r["engine_us"]) / 1000 for r in recs if r.get("total_us") is not None]
    return {
        "tasks": len(recs),
        "correct": ok,
        "read": read,
        "written": sum(r["written"] for r in recs),
        "calls": sum(r["calls"] for r in recs),
        "read_per_correct": _ratio(read, ok),
        "read_per_correct_amortised": _ratio(read + api_tokens, ok) if api_tokens else None,
        "written_per_correct": _ratio(sum(r["written"] for r in recs), ok),
        "median_seconds": statistics.median([r["seconds"] for r in recs]) if recs else None,
        "overhead_ms_median": statistics.median(over) if over else None,
        "overhead_ms_p95": _pct(over, 0.95),
        "truncated": sorted(r["task"] for r in recs if r.get("truncated")),
        "partial": sorted(r["task"] for r in recs if r.get("partial")),
        "timeouts": sorted(r["task"] for r in recs if r.get("timeout")),
        "errors": sorted(r["task"] for r in recs if r.get("error")),
        "describe_api_tokens": api_tokens or None,
        "suite_tasks": n_tasks,
    }


def gate(summary: dict, by_task: dict, lang: str) -> dict:
    plan, code = summary["plan"], summary[lang]
    base = plan["read_per_correct"]
    cut = 1 - code["read_per_correct"] / base if base and code["read_per_correct"] else None
    cut_am = 1 - code["read_per_correct_amortised"] / base if base and code["read_per_correct_amortised"] else None
    regressed = sorted(t for t, arms in by_task.items() if arms.get("plan", {}).get("correct") and not arms.get(lang, {}).get("correct"))
    over = code["overhead_ms_median"]
    checks = {
        "tokens": cut is not None and cut >= GATE_TOKENS,
        "tokens_amortised": cut_am is not None and cut_am >= GATE_TOKENS,
        "no_regression": not regressed,
        "overhead": over is not None and over < GATE_OVERHEAD_MS,
    }
    return {"reduction": cut, "reduction_amortised": cut_am, "regressed": regressed, "overhead_ms_median": over, "checks": checks, "pass": checks["tokens"] and checks["no_regression"] and checks["overhead"]}


def paired_summary(by_task: dict, arms: list[str]) -> dict:
    """Tokens read over the tasks every arm answered correctly (cbm included
    when it ran: only tasks it can express)."""
    both = sorted(t for t, rs in by_task.items() if all(rs.get(a, {}).get("correct") for a in arms))
    return {"tasks": both, "read": {a: sum(by_task[t][a]["read"] for t in both) for a in arms}, "calls": {a: sum(by_task[t][a]["calls"] for t in both) for a in arms}}


def summarize(records: list[Record], api: dict[str, int], tasks: list[Task]) -> dict:
    recs = [_compact(as_dict(r)) for r in records]
    arms = sorted({r["arm"] for r in recs}, key=ARMS.index)
    by_task: dict[str, dict] = {}
    for r in recs:
        by_task.setdefault(r["task"], {})[r["arm"]] = r
    summary = {a: arm_summary([r for r in recs if r["arm"] == a], api.get(a, 0), len(tasks)) for a in arms}
    kinds = {}
    for t in tasks:
        group = kinds.setdefault(t.expect, {})
        for a in arms:
            group.setdefault(a, []).extend(r for r in recs if r["task"] == t.id and r["arm"] == a)
    by_expect = {e: {a: arm_summary(rs) for a, rs in g.items()} for e, g in kinds.items()}
    gates = {lang: gate(summary, by_task, lang) for lang in ("js", "starlark") if lang in summary and "plan" in summary}
    paired = paired_summary(by_task, arms)
    return {"summary": summary, "paired": paired, "by_expect": by_expect, "gates": gates, "describe_api_tokens": api, "cbm_skipped": {t.id: CBM_SKIP[t.id] for t in tasks if t.id in CBM_SKIP}}


def write_results(doc: dict, path: Path = RESULTS_PATH) -> None:
    write_json(path, doc)


def load_results(path: Path = RESULTS_PATH) -> dict:
    return read_json(path)


def dumps(x: Any) -> str:
    return json.dumps(x, ensure_ascii=False)
