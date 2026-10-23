"""telar-bench command line.

  telar-bench setup  [--select] [--skip-s3]
  telar-bench run    --arm {grep,cbm,telar,all} --suite {s1,s3,all} [--only ID ...] [--workers N] [--force]
  telar-bench report
  telar-bench clean  [--work] [--runs ARM]
"""

from __future__ import annotations

import argparse
import shutil
import sys

from . import report as report_mod
from . import s1, s3
from .arms import ARM_NAMES, make_arm
from .common import RUNS_DIR, WORK_DIR, log
from .prepare import setup


def _arms(arg: str) -> list[str]:
    names = list(ARM_NAMES) if arg == "all" else [a.strip() for a in arg.split(",") if a.strip()]
    for n in names:
        if n not in ARM_NAMES:
            raise SystemExit(f"unknown arm {n!r}; choose from {ARM_NAMES} or all")
    out = []
    for n in names:
        ok, why = make_arm(n).available()
        if ok:
            out.append(n)
        else:
            log(f"arm {n} unavailable: {why} (reported as pending)")
    return out


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(prog="telar-bench", description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)

    sp = sub.add_parser("setup", help="fetch pinned tools/datasets/repos and build gold labels")
    sp.add_argument("--select", action="store_true", help="regenerate [[s1.instances]] in manifest.toml")
    sp.add_argument("--skip-s3", action="store_true")
    sp.add_argument("--workers", type=int, default=4)

    rp = sub.add_parser("run", help="run arms on suites (resumable)")
    rp.add_argument("--arm", default="all")
    rp.add_argument("--suite", default="all", choices=["s1", "s3", "all"])
    rp.add_argument("--only", nargs="*", help="instance ids / repos (s1) or suite names (s3)")
    rp.add_argument("--workers", type=int, default=2, help="S1 repos processed in parallel")
    rp.add_argument("--force", action="store_true", help="ignore cached arm outputs")

    sub.add_parser("report", help="score cached outputs; write bench/results/*.json and docs/bench/alpha-results.md")

    cp = sub.add_parser("clean", help="remove work trees and/or cached arm outputs")
    cp.add_argument("--work", action="store_true")
    cp.add_argument("--runs", nargs="*", help="arm names whose cached outputs to delete")

    a = p.parse_args(argv)
    if a.cmd == "setup":
        setup(select=a.select, workers=a.workers, skip_s3=a.skip_s3)
    elif a.cmd == "run":
        arms = _arms(a.arm)
        if not arms:
            log("no available arms")
            return 1
        if a.suite in ("s1", "all"):
            s1.run(arms, workers=a.workers, only=a.only, force=a.force)
        if a.suite in ("s3", "all"):
            s3.run(arms, only=a.only, force=a.force)
    elif a.cmd == "report":
        report_mod.report()
    elif a.cmd == "clean":
        if a.work:
            shutil.rmtree(WORK_DIR, ignore_errors=True)
        for arm in a.runs or []:
            for suite in ("s1", "s3"):
                shutil.rmtree(RUNS_DIR / suite / arm, ignore_errors=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
