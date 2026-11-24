"""`docs/bench/multistep-results.md` from `bench/results/multistep.json`."""

from __future__ import annotations

from .cbm_plans import CBM_SKIP
from .tasks import BY_ID

ARM_LABEL = {"plan": "tool-call plan", "js": "code mode, JavaScript", "starlark": "code mode, Starlark", "cbm": "cbm 0.12.0"}


def _n(x: float | None, nd: int = 0) -> str:
    if x is None:
        return "—"
    return f"{x:,.{nd}f}"


def _pct(x: float | None) -> str:
    return "—" if x is None else f"{100 * x:.1f}%"


def arm_table(summary: dict) -> list[str]:
    out = [
        "| Arm | Correct | Tokens read / correct | … with `describe_api` amortised | Tokens written / correct | Round trips | Median wall (s) | `execute` overhead median / p95 (ms) |",
        "|---|---|---|---|---|---|---|---|",
    ]
    for arm, s in summary.items():
        over = f"{_n(s['overhead_ms_median'], 2)} / {_n(s['overhead_ms_p95'], 2)}" if s["overhead_ms_median"] is not None else "—"
        out.append(
            f"| {ARM_LABEL.get(arm, arm)} | {s['correct']}/{s['tasks']} | {_n(s['read_per_correct'])} | {_n(s['read_per_correct_amortised'])} | "
            f"{_n(s['written_per_correct'])} | {s['calls']} | {_n(s['median_seconds'], 3)} | {over} |"
        )
    return out


def paired_lines(paired: dict | None) -> list[str]:
    if not paired or not paired["tasks"]:
        return ["No task was answered correctly by every arm."]
    head = f"Tasks every arm answered correctly ({', '.join(paired['tasks'])}): tokens read (round trips), summed."
    cells = " | ".join(f"{ARM_LABEL.get(a, a)}: {paired['read'][a]:,} ({paired['calls'][a]})" for a in paired["read"])
    return [head, "", f"- {cells}"]


def gate_lines(gates: dict, api: dict) -> list[str]:
    out = ["| Language | Fewer tokens / correct vs plans (gate ≥ 40%) | … amortised | Tasks regressed (gate: none) | Median overhead (gate < 20 ms) | Gate |", "|---|---|---|---|---|---|"]
    for lang, g in gates.items():
        regressed = ", ".join(g["regressed"]) or "none"
        out.append(
            f"| {lang} (`describe_api` {api.get(lang, 0):,} tokens) | {_pct(g['reduction'])} | {_pct(g['reduction_amortised'])} | {regressed} | {_n(g['overhead_ms_median'], 2)} ms | {'**pass**' if g['pass'] else '**fail**'} |"
        )
    return out


def _cell(r: dict | None) -> str:
    if r is None:
        return "—"
    mark = "✓" if r["correct"] else "✗"
    extra = " E" if r.get("error") else ""
    return f"{mark} {r['read']:,} / {r['calls']}{extra}"


def task_table(records: list[dict]) -> list[str]:
    by: dict[str, dict] = {}
    for r in records:
        by.setdefault(r["task"], {})[r["arm"]] = r
    out = [
        "Each cell: correct, tokens read, round trips (`E`: the arm errored). cbm `—`: not expressible (below).",
        "",
        "| Task | Kind | Corpus | Expect | Plan | JavaScript | Starlark | cbm |",
        "|---|---|---|---|---|---|---|---|",
    ]
    for tid, arms in sorted(by.items()):
        t = BY_ID[tid]
        out.append(f"| {tid} | {t.kind} | {t.corpus} | {t.expect} | {_cell(arms.get('plan'))} | {_cell(arms.get('js'))} | {_cell(arms.get('starlark'))} | {_cell(arms.get('cbm'))} |")
    return out


def expect_table(by_expect: dict) -> list[str]:
    out = ["| Tasks | Arm | Correct | Tokens read / correct | Round trips |", "|---|---|---|---|---|"]
    for e, arms in sorted(by_expect.items()):
        for arm, s in arms.items():
            out.append(f"| {e} | {ARM_LABEL.get(arm, arm)} | {s['correct']}/{s['tasks']} | {_n(s['read_per_correct'])} | {s['calls']} |")
    return out


def kind_table(records: list[dict]) -> list[str]:
    """Per task kind: tokens read summed over the kind's tasks, per arm."""
    rows: dict[str, dict[str, list[int]]] = {}
    for r in records:
        k = BY_ID[r["task"]].kind
        acc = rows.setdefault(k, {}).setdefault(r["arm"], [0, 0, 0])
        acc[0] += r["read"]
        acc[1] += int(r["correct"])
        acc[2] += 1
    out = ["| Kind | Plan read (correct/n) | JavaScript | Starlark | cbm |", "|---|---|---|---|---|"]
    for k, arms in sorted(rows.items()):
        cells = [f"{arms[a][0]:,} ({arms[a][1]}/{arms[a][2]})" if a in arms else "—" for a in ("plan", "js", "starlark", "cbm")]
        out.append(f"| {k} | " + " | ".join(cells) + " |")
    return out


def no_help(records: list[dict]) -> list[str]:
    plan = {r["task"]: r for r in records if r["arm"] == "plan"}
    out = []
    for lang in ("js", "starlark"):
        worse = [r["task"] for r in records if r["arm"] == lang and r["task"] in plan and r["read"] >= plan[r["task"]]["read"]]
        out.append(f"- {lang}: reads as many tokens as the plan or more on {', '.join(sorted(worse)) or 'no task'}.")
    return out


def limits(summary: dict) -> list[str]:
    out = []
    for arm, s in summary.items():
        bits = [f"{k} {', '.join(s[k])}" for k in ("truncated", "partial", "timeouts", "errors") if s.get(k)]
        out.append(f"- {ARM_LABEL.get(arm, arm)}: {'; '.join(bits) or 'none'}.")
    return out


def render(doc: dict) -> str:
    meta = doc["meta"]
    L = [
        "# Multi-step suite — code mode (RFC 0009 phase 2)",
        "",
        "Generated by `rueca-bench multistep report` from `bench/results/multistep.json`; method in",
        "`bench/README.md` (*Multi-step efficiency*). Tokens: cl100k_base. Read = tool answers as the agent",
        "receives them (plans, cbm) or script source plus `execute`'s text result (code mode).",
        "",
        f"- Rueca: {meta['binary']}; cbm: {meta.get('cbm') or 'not run'}; tasks: {meta['tasks']}.",
        "",
        "## Per arm",
        "",
        *arm_table(doc["summary"]),
        "",
        "## Paired",
        "",
        *paired_lines(doc.get("paired")),
        "",
        "## Graduation gate (per language)",
        "",
        *gate_lines(doc["gates"], doc["describe_api_tokens"]),
        "",
        "## By expectation",
        "",
        *expect_table(doc["by_expect"]),
        "",
        "## By task kind",
        "",
        *kind_table(doc["records"]),
        "",
        "## Per task",
        "",
        *task_table(doc["records"]),
        "",
        "## Limits hit",
        "",
        *limits(doc["summary"]),
        "",
        "## Where code mode does not help",
        "",
        *no_help(doc["records"]),
        "",
        "## cbm: tasks not expressible",
        "",
        *[f"- {t}: {why}." for t, why in sorted(doc.get("cbm_skipped", CBM_SKIP).items())],
        "",
    ]
    return "\n".join(L)
