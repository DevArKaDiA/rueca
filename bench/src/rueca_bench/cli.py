"""rueca-bench command line.

  rueca-bench setup  [--select] [--skip-s3] [--lang FAMILY ...]
  rueca-bench run    --arm {grep,cbm,rueca,all} --suite {s1,s3,data,health,all} [--lang FAMILY ...] [--only ID ...] [--workers N] [--force]
  rueca-bench report
  rueca-bench check  [--baseline bench/results/scorecard.json] [--current FILE]
  rueca-bench clean  [--work] [--runs ARM]

Language families are the manifest `[languages]` keys (python, ecma, php, ...);
`data` selects the data-format suite (manifest `[[data.repos]]`).
"""

from __future__ import annotations

import argparse
import shutil
import sys

from . import check as check_mod
from . import data as data_mod
from . import health as health_mod
from . import report as report_mod
from . import s1, s3
from .arms import ARM_NAMES, make_arm
from .common import RUNS_DIR, WORK_DIR, families, log
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
    p = argparse.ArgumentParser(prog="rueca-bench", description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)

    sp = sub.add_parser("setup", help="fetch pinned tools/datasets/repos and build gold labels")
    sp.add_argument("--select", action="store_true", help="regenerate [[s1.instances]] in manifest.toml")
    sp.add_argument("--skip-s3", action="store_true")
    sp.add_argument("--workers", type=int, default=4)
    sp.add_argument("--lang", nargs="*", help="language families to prepare (default: all)")
    sp.add_argument("--no-cbm", action="store_true", help="do not fetch the cbm binary (Rueca-only runs, e.g. the CI regression check)")

    rp = sub.add_parser("run", help="run arms on suites (resumable)")
    rp.add_argument("--arm", default="all")
    rp.add_argument("--suite", default="all", help="s1, s3, data, health, all, or a comma list")
    rp.add_argument("--only", nargs="*", help="instance ids / repos (s1) or suite names (s3)")
    rp.add_argument("--workers", type=int, default=2, help="S1 repositories / S3 sets processed in parallel")
    rp.add_argument("--force", action="store_true", help="ignore cached arm outputs")
    rp.add_argument("--lang", nargs="*", help="language families to run (default: all)")
    rp.add_argument("--prune", action="store_true", help="delete each repository clone once its runs are done (bounded disk, e.g. CI)")

    sub.add_parser("report", help="score cached outputs; write bench/results/*.json and docs/bench/alpha-results.md")

    kp = sub.add_parser("check", help="regression check (RFC 0004 §7.8): compare a scorecard with the committed one")
    kp.add_argument("--baseline", default=None, help="committed scorecard (default: bench/results/scorecard.json at git HEAD)")
    kp.add_argument("--current", default=None, help="scorecard to check (default: bench/results/scorecard.json)")

    cp = sub.add_parser("clean", help="remove work trees and/or cached arm outputs")
    cp.add_argument("--work", action="store_true")
    cp.add_argument("--runs", nargs="*", help="arm names whose cached outputs to delete")

    a = p.parse_args(argv)
    langs = set(getattr(a, "lang", None) or []) or None
    for lang in langs or ():
        if lang != "data" and lang not in families():
            raise SystemExit(f"unknown language family {lang!r}; choose from {list(families())}")
    if a.cmd == "setup":
        setup(select=a.select, workers=a.workers, skip_s3=a.skip_s3, languages=langs, cbm=not a.no_cbm)
    elif a.cmd == "run":
        arms = _arms(a.arm)
        if not arms:
            log("no available arms")
            return 1
        code_langs = {x for x in langs if x != "data"} if langs else None
        run_code = not langs or bool(code_langs)
        wanted = {x.strip() for x in a.suite.split(",") if x.strip()}
        if "all" in wanted:
            wanted = {"s1", "s3", "data", "health"}
        unknown = wanted - {"s1", "s3", "data", "health"}
        if unknown:
            raise SystemExit(f"unknown suite(s) {sorted(unknown)}")
        if "health" in wanted and run_code and "rueca" in arms:
            health_mod.run(languages=code_langs, force=a.force, only=a.only, prune=a.prune)
        if "s1" in wanted and run_code:
            s1.run(arms, workers=a.workers, only=a.only, force=a.force, languages=code_langs, prune=a.prune)
        if "s3" in wanted and run_code:
            s3.run(arms, only=a.only, force=a.force, languages=code_langs, workers=a.workers, prune=a.prune)
        if "data" in wanted and (not langs or "data" in langs):
            data_mod.run(arms, only=a.only, force=a.force)
    elif a.cmd == "report":
        report_mod.report()
    elif a.cmd == "check":
        return check_mod.main(a.baseline, a.current)
    elif a.cmd == "clean":
        if a.work:
            shutil.rmtree(WORK_DIR, ignore_errors=True)
        for arm in a.runs or []:
            for suite in ("s1", "s3", "data", "health"):
                shutil.rmtree(RUNS_DIR / suite / arm, ignore_errors=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
