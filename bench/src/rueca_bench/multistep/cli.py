"""`rueca-bench multistep …` (RFC 0009 phase 2):

  multistep run [--arm plan,js,starlark,cbm] [--only ID ...] [--bin PATH]
  multistep report
  multistep tasks
  multistep probe --task ID --language js|starlark --script FILE [--bin PATH]
  multistep describe-api --language js|starlark --out FILE [--bin PATH]
  multistep gold [--extract] [--bin PATH]     (writes bench/multistep/gold.json)
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

from ..arms.rueca import rueca_binary
from ..common import BENCH_DIR, log, write_json
from . import corpora, inputs, report, suite
from .run import ARMS, describe_api, run_script
from .tasks import BY_ID, TASKS, statements

PREVIEW = 300
GOLD_PATH = BENCH_DIR / "multistep" / "gold.json"


def gold_answers(inp: dict) -> dict:
    """Every task's gold answer, derived from the inputs (`bench/multistep/gold.json`)."""
    return {t.id: t.gold(inp) for t in TASKS}


def add_parser(sub: argparse._SubParsersAction) -> None:
    mp = sub.add_parser("multistep", help="RFC 0009 multi-step efficiency suite: tool-call plans vs code mode (JS, Starlark) vs cbm")
    ms = mp.add_subparsers(dest="ms_cmd", required=True)
    r = ms.add_parser("run", help="run the arms over the tasks; write bench/results/multistep.json and the report")
    r.add_argument("--arm", default=",".join(ARMS), help="comma list of plan, js, starlark, cbm")
    r.add_argument("--only", nargs="*", help="task ids")
    r.add_argument("--bin", default=None, help="rueca binary with code mode and the code-mode-starlark feature (default $RUECA_BIN)")
    ms.add_parser("report", help="write docs/bench/multistep-results.md from bench/results/multistep.json")
    ms.add_parser("tasks", help="print the tasks (id, kind, corpus, statement) as JSON")
    p = ms.add_parser("probe", help="run one script unchanged against a task's corpus; print one JSON line")
    p.add_argument("--task", required=True)
    p.add_argument("--language", required=True, choices=["js", "starlark"])
    p.add_argument("--script", required=True)
    p.add_argument("--bin", default=None)
    d = ms.add_parser("describe-api", help="write describe_api's text for a language to a file")
    d.add_argument("--language", required=True, choices=["js", "starlark"])
    d.add_argument("--out", required=True)
    d.add_argument("--bin", default=None)
    g = ms.add_parser("gold", help="print every task's gold answer (with --extract, first re-extract inputs.json from the bench's gold)")
    g.add_argument("--extract", action="store_true")
    g.add_argument("--bin", default=None)


def _binary(arg: str | None) -> str:
    b = arg or rueca_binary()
    if not b or not Path(b).exists():
        raise SystemExit("rueca-bench multistep: set RUECA_BIN (or --bin) to a rueca binary built with --features code-mode-starlark")
    return b


def probe(task_id: str, language: str, script: Path, binary: str) -> dict:
    task = BY_ID[task_id]
    corpus = corpora.prepare(task.corpus, binary)
    rec = run_script(binary, task, corpus, language, task.gold(inputs.load()), code_file=script)
    preview = json.dumps(rec.answer, ensure_ascii=False)
    return {
        "task": task_id,
        "language": language,
        "correct": rec.correct,
        "error": rec.error,
        "result_preview": preview if len(preview) <= PREVIEW else preview[:PREVIEW] + "…",
        "script_tokens": rec.script_tokens,
        "result_tokens": rec.result_tokens,
        "calls": rec.engine_calls,
        "truncated": rec.truncated,
    }


def dispatch(a: argparse.Namespace) -> int:
    cmd = a.ms_cmd
    if cmd == "tasks":
        print(json.dumps(statements(), indent=1, ensure_ascii=False))
    elif cmd == "report":
        suite.DOC_PATH.write_text(report.render(suite.load_results()))
        log(f"wrote {suite.DOC_PATH}")
    elif cmd == "run":
        arms = tuple(x.strip() for x in a.arm.split(",") if x.strip())
        doc = suite.run_suite(_binary(a.bin), arms, a.only)
        suite.write_results(doc)
        suite.DOC_PATH.write_text(report.render(doc))
        log(f"wrote {suite.RESULTS_PATH} and {suite.DOC_PATH}")
    elif cmd == "probe":
        print(json.dumps(probe(a.task, a.language, Path(a.script), _binary(a.bin)), ensure_ascii=False))
    elif cmd == "describe-api":
        binary = _binary(a.bin)
        Path(a.out).write_text(describe_api(binary, corpora.prepare("flask", binary), a.language))
    elif cmd == "gold":
        inp = inputs.extract(_binary(a.bin)) if a.extract else inputs.load()
        answers = gold_answers(inp)
        write_json(GOLD_PATH, answers)
        for tid, ans in answers.items():
            print(tid, json.dumps(ans, ensure_ascii=False))
    return 0


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(prog="rueca-bench")
    add_parser(p.add_subparsers(dest="cmd", required=True))
    return dispatch(p.parse_args(argv))


if __name__ == "__main__":
    sys.exit(main())
