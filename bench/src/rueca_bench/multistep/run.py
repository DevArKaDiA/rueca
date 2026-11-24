"""Measuring the multi-step suite: each task answered by its reference
tool-call plan (`plan`), its reference JavaScript and Starlark scripts
through `execute` (`js`, `starlark`) and, where cbm can express it, its cbm
plan (`cbm`). Per task and arm: tokens the agent reads and writes (the bench
tokenizer, cl100k_base), round trips, wall time, `execute` overhead,
limits hit, and the answer scored against the task's gold.

What is counted (RFC 0009 §8):

- **read** — plan / cbm: every tool answer's text as the agent receives it
  (Rueca: `rueca query ... --text`; cbm: its default text rendering); code
  mode: the script source plus `execute`'s text result (or its error line).
  `describe_api`'s text, read once per session, is reported apart and in
  the amortised totals.
- **written** — the tool-call arguments as JSON (code mode: `execute`'s
  arguments, the script included).
- **calls** — round trips (code mode: 1 `execute`; its engine calls are
  `engine_calls`).
- **wall** — the processes' wall time (`rueca query` opens the store per
  call, so a plan pays that once per call); `execute` also reports
  `total_us - engine_us`, the code-mode overhead.
"""

from __future__ import annotations

import json
import os
import time
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Any

from ..arms.cbm import CbmArm
from ..common import BENCH_DIR, count_tokens, run
from . import corpora, plans
from .cbm_plans import CBM_PLANS, CbmAgent
from .tasks import BY_ID, TASKS, Task

SCRIPTS_DIR = BENCH_DIR / "multistep" / "scripts"
LANGS = {"js": "js", "starlark": "star"}
ARMS = ("plan", "js", "starlark", "cbm")
CALL_TIMEOUT = 120.0
CODE_ENV = {"RUECA_EXPERIMENTAL_CODE_MODE": "1"}


def script_path(task_id: str, language: str) -> Path:
    return SCRIPTS_DIR / f"{task_id}.{LANGS[language]}"


def tokens(text: str) -> int:
    return count_tokens(text) or 0


def args_json(args: dict) -> str:
    return json.dumps(args, separators=(",", ":"), ensure_ascii=False)


@dataclass
class Record:
    task: str
    arm: str
    correct: bool = False
    answer: Any = None
    read: int = 0
    written: int = 0
    calls: int = 0
    seconds: float = 0.0
    truncated: bool = False
    error: str | None = None
    engine_calls: int | None = None
    partial: bool = False
    timeout: bool = False
    total_us: int | None = None
    engine_us: int | None = None
    script_tokens: int | None = None
    result_tokens: int | None = None
    trace: list = field(default_factory=list)


# --- scoring ---------------------------------------------------------------------------------


def _as_set(x: Any) -> set | None:
    return set(map(str, x)) if isinstance(x, list) and all(isinstance(v, str) for v in x) else None


def correct(task: Task, answer: Any, gold: Any) -> bool:
    kind = task.answer
    if kind == "set":
        return _as_set(answer) == set(gold)
    if kind == "dict_set":
        return isinstance(answer, dict) and set(answer) == set(gold) and all(_as_set(answer[k]) == set(gold[k]) for k in gold)
    if kind == "int":
        return isinstance(answer, int) and not isinstance(answer, bool) and answer == gold
    return answer == gold


# --- Rueca tool calls --------------------------------------------------------------------------


class RuecaAgent:
    """`plans.Agent` over `rueca query <tool> ... --text`, recording every call."""

    def __init__(self, binary: str, corpus: corpora.Corpus, record: Record):
        self.binary = binary
        self.corpus = corpus
        self.rec = record

    def call(self, tool: str, args: dict[str, Any]) -> str:
        res = run([self.binary, "query", tool, args_json(args), *self.corpus.scope_args(), "--text"], timeout=CALL_TIMEOUT)
        text = res.stdout if res.rc == 0 else (res.stdout or res.stderr)
        self.rec.calls += 1
        self.rec.read += tokens(text)
        self.rec.written += tokens(args_json(args))
        self.rec.seconds += res.seconds
        self.rec.truncated |= "(truncated" in text
        self.rec.timeout |= res.timed_out
        self.rec.trace.append({"tool": tool, "args": args, "tokens": tokens(text)})
        return text


def run_plan(binary: str, task: Task, corpus: corpora.Corpus, gold: Any) -> Record:
    rec = Record(task.id, "plan")
    try:
        rec.answer = plans.PLANS[task.id](RuecaAgent(binary, corpus, rec))
    except Exception as exc:  # a plan that cannot parse its answers is a wrong answer, recorded
        rec.error = f"{type(exc).__name__}: {exc}"
    rec.correct = rec.error is None and correct(task, rec.answer, gold)
    return rec


# --- code mode ---------------------------------------------------------------------------------


def execute_args(language: str) -> dict:
    """`execute`'s arguments besides `code`: the defaults, `language` only for
    Starlark (JavaScript is the default)."""
    return {"language": "starlark"} if language == "starlark" else {}


def execute(binary: str, corpus: corpora.Corpus, language: str, code_file: Path, text: bool) -> tuple[dict | None, str, float, bool]:
    """One `rueca query execute`: (parsed JSON or None, raw stdout, seconds, timed out)."""
    env = {**CODE_ENV, **({} if text else {"RUECA_CODE_MODE_TIMING": "1"})}
    cmd = [binary, "query", "execute", args_json(execute_args(language)), "--code-file", str(code_file), *corpus.scope_args()]
    res = run([*cmd, "--text"] if text else cmd, env=env, timeout=CALL_TIMEOUT)
    try:
        return json.loads(res.stdout), res.stdout, res.seconds, res.timed_out
    except json.JSONDecodeError:
        return None, res.stdout, res.seconds, res.timed_out


def run_script(binary: str, task: Task, corpus: corpora.Corpus, language: str, gold: Any, code_file: Path | None = None) -> Record:
    code_file = code_file or script_path(task.id, language)
    source = code_file.read_text()
    rec = Record(task.id, language, calls=1)
    data, raw, secs, timed_out = execute(binary, corpus, language, code_file, text=False)
    rec.seconds, rec.timeout = secs, timed_out
    rec.script_tokens = tokens(source)
    rec.written = tokens(args_json({**execute_args(language), "code": source}))
    if data is None or "error" in data:
        rec.error = (data or {}).get("error") or raw.strip()[-500:] or "no output"
        rec.timeout |= " timeout error" in rec.error
        rec.result_tokens = tokens(rec.error)
    else:
        _, text, _, _ = execute(binary, corpus, language, code_file, text=True)
        rec.answer = data.get("result")
        rec.engine_calls = data.get("calls")
        rec.truncated = bool(data.get("truncated"))
        rec.partial = bool(data.get("partial"))
        timing = data.get("timing") or {}
        rec.total_us, rec.engine_us = timing.get("total_us"), timing.get("engine_us")
        rec.result_tokens = tokens(text)
    rec.read = rec.script_tokens + rec.result_tokens
    rec.correct = rec.error is None and correct(task, rec.answer, gold)
    return rec


def describe_api(binary: str, corpus: corpora.Corpus, language: str) -> str:
    res = run([binary, "query", "describe_api", args_json({"language": language}), *corpus.scope_args(), "--text"], env=CODE_ENV, timeout=CALL_TIMEOUT)
    if res.rc != 0:
        raise RuntimeError(f"describe_api {language} failed: {res.stdout[-300:]} {res.stderr[-300:]}")
    return res.stdout


# --- cbm ----------------------------------------------------------------------------------------


def run_cbm(arm: CbmArm, project: str, task: Task, gold: Any) -> Record:
    rec = Record(task.id, "cbm")
    agent = CbmAgent(arm, project, rec)
    try:
        rec.answer = CBM_PLANS[task.id](agent)
    except Exception as exc:  # recorded as a wrong answer
        rec.error = f"{type(exc).__name__}: {exc}"
    rec.correct = rec.error is None and correct(task, rec.answer, gold)
    return rec


def cbm_projects(arm: CbmArm, cs: dict[str, corpora.Corpus]) -> dict[str, str]:
    out = {}
    for key in corpora.SINGLE:
        h = arm.index(cs[key].root, f"ms-{key}", "")
        if not h.ok:
            raise RuntimeError(f"cbm index {key}: {h.error}")
        out[key] = h.state["project"]
    return out


# --- the run ------------------------------------------------------------------------------------


def _now() -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


def binary_label(binary: str) -> str:
    res = run([binary, "--version"], timeout=30)
    return (res.stdout.strip() or "unknown") + (f" ({os.environ['RUECA_BIN_LABEL']})" if os.environ.get("RUECA_BIN_LABEL") else "")


def as_dict(rec: Record) -> dict:
    return asdict(rec)


__all__ = ["ARMS", "BY_ID", "TASKS", "Record", "cbm_projects", "correct", "describe_api", "run_cbm", "run_plan", "run_script"]
