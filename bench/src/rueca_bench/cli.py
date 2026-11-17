"""rueca-bench command line.

  rueca-bench setup  [--select] [--skip-s3] [--lang FAMILY ...]
  rueca-bench run    --arm {grep,cbm,rueca,vector,rueca-hybrid,rueca-semantic,all} --suite {s1,s3,data,health,org,s2,lite,all} [--lang FAMILY ...] [--only ID ...] [--workers N] [--force] [--split gated|calibration] [--limit N]
  rueca-bench coir   [--profile cheap|full] [--preset P] [--dims N] [--max-tokens N] [--instruction task|preset|TEXT] [--tasks A,B] [--limit-corpus N]
  rueca-bench compare --arms ARM[:TAG] ... [--base grep] [--split calibration|gated|all] [--variant kw|raw|both] [--cache DIR] [--json OUT]
  rueca-bench sweep  --arm rueca --mode hybrid|semantic [--alpha A,B,...] [--norm minmax,theoretical] [--n N,...] [--split calibration] [--lang FAMILY ...]
  rueca-bench report [--s2-only]
  rueca-bench check  [--baseline bench/results/scorecard.json] [--current FILE]
  rueca-bench clean  [--work] [--runs ARM]

Language families are the manifest `[languages]` keys (python, ecma, php, ...);
`data` selects the data-format suite (manifest `[[data.repos]]`). `vector`
(dense retrieval through `rueca embed`) and `rueca-hybrid` / `rueca-semantic`
(Rueca indexed with `--embed`; also `--arm rueca` with
RUECA_BENCH_RUECA_MODE=hybrid|semantic) run the localization suites only (s1,
lite) and are never part of `--arm all`; `lite` (the SWE-bench Lite
diagnostic) is never part of `--suite all`.
"""

from __future__ import annotations

import argparse
import shutil
import sys
from pathlib import Path

from . import check as check_mod
from . import coir as coir_mod
from . import compare as compare_mod
from . import contracts as contracts_mod
from . import data as data_mod
from . import health as health_mod
from . import lite as lite_mod
from . import org as org_mod
from . import report as report_mod
from . import s1, s3
from . import sweep as sweep_mod
from .arms import ARM_NAMES, S1_ARM_NAMES, canonical_arm, make_arm
from .arms.rueca import rueca_binary
from .common import CACHE, RUNS_DIR, WORK_DIR, families, log, write_json
from .embed import DEFAULT_MAX_TOKENS, DEFAULT_PRESET
from .prepare import setup

SUITES = {"s1", "s3", "data", "health", "org", "s2", "lite"}
ALL_SUITES = {"s1", "s3", "data", "health", "org", "s2"}


def _canonical(names: list[str]) -> list[str]:
    """`rueca` under RUECA_BENCH_RUECA_MODE=hybrid|semantic runs as
    `rueca-<mode>` (localization suites only)."""
    try:
        out = [canonical_arm(n) for n in names]
    except ValueError as exc:
        raise SystemExit(f"rueca-bench: {exc}") from exc
    if out != names:
        log(f"RUECA_BENCH_RUECA_MODE: the rueca arm runs as {', '.join(n for n in out if n not in names)} (localization suites only)")
    return out


def _ready(n: str) -> bool:
    """Builds the arm: False when unavailable (reported as pending); exits
    on a misconfiguration or a failed preflight."""
    try:
        arm = make_arm(n)
    except ValueError as exc:  # a fusion knob out of range
        raise SystemExit(f"arm {n}: {exc}") from exc
    ok, why = arm.available()
    if not ok:
        log(f"arm {n} unavailable: {why} (reported as pending)")
        return False
    if hasattr(arm, "preflight"):
        try:
            arm.preflight()
        except RuntimeError as exc:
            raise SystemExit(f"arm {n}: {exc}") from exc
    return True


def _arms(arg: str) -> list[str]:
    names = list(ARM_NAMES) if arg == "all" else [a.strip() for a in arg.split(",") if a.strip()]
    for n in names:
        if n not in S1_ARM_NAMES:
            raise SystemExit(f"unknown arm {n!r}; choose from {S1_ARM_NAMES} or all")
    return [n for n in _canonical(names) if _ready(n)]


def _suites(arg: str) -> set[str]:
    wanted = {x.strip() for x in arg.split(",") if x.strip()}
    if "all" in wanted:
        wanted = (wanted - {"all"}) | ALL_SUITES
    unknown = wanted - SUITES
    if unknown:
        raise SystemExit(f"unknown suite(s) {sorted(unknown)}")
    return wanted


def _coir(a: argparse.Namespace) -> None:
    coir_mod.require_mteb()
    run = coir_mod.CoirRun(
        preset=a.preset,
        dims=a.dims,
        max_tokens=a.max_tokens,
        instruction=a.instruction,
        profile=a.profile,
        tasks=[t.strip() for t in a.tasks.split(",") if t.strip()] if a.tasks else None,
        limit_corpus=a.limit_corpus,
    )
    emb = coir_mod.make_embedder(rueca_binary(), a.preset, a.max_tokens, a.batch, a.cpu, a.threads)
    policy = coir_mod.InstructionPolicy(a.instruction, a.preset)
    if any(policy.for_task(t) for t in run.task_names()) and not emb.supports_instruction():
        raise SystemExit("rueca-bench coir: `rueca embed` has no --instruction flag; pass --instruction preset to use the preset's own")
    print("\n".join(coir_mod.summary_lines(run.run(emb))))


def _compare(a: argparse.Namespace) -> None:
    cache = Path(a.cache).resolve() if a.cache else CACHE
    res = compare_mod.Comparison(cache, a.split, a.base, a.arms).run()
    if a.json:
        write_json(Path(a.json), res)
    print(compare_mod.render(res, a.variant), end="")


def _sweep(a: argparse.Namespace, langs: set[str] | None) -> None:
    try:
        sw = sweep_mod.Sweep(
            mode=a.mode,
            preset=a.preset,
            alphas=sweep_mod.parse_alphas(a.alpha),
            norms=[x.strip() for x in a.norm.split(",") if x.strip()],
            ns=[int(x) for x in a.n.split(",") if x.strip()],
            split=a.split,
        )
        sw.configs()
    except ValueError as exc:
        raise SystemExit(f"rueca-bench sweep: {exc}") from exc
    if not a.report_only:
        sw.run(only=a.only, languages=langs, workers=a.workers, force=a.force, prune=a.prune)
    text, res = sweep_mod.report(sw, a.base, a.variant)
    if a.json:
        write_json(Path(a.json), res)
    print(text, end="")


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
    rp.add_argument("--suite", default="all", help="s1, s3, data, health, org, s2, lite, all, or a comma list")
    rp.add_argument("--only", nargs="*", help="instance ids / repos (s1), suite names (s3), or for org: gated / calibration and/or repositories (a smoke run of the multi-repository scenario)")
    rp.add_argument("--workers", type=int, default=2, help="S1 repositories / S3 sets processed in parallel")
    rp.add_argument("--force", action="store_true", help="ignore cached arm outputs")
    rp.add_argument("--lang", nargs="*", help="language families to run (default: all)")
    rp.add_argument("--prune", action="store_true", help="delete each repository clone once its runs are done (bounded disk, e.g. CI)")
    rp.add_argument("--limit", type=int, default=None, help="lite: the first N instances (sha256 order)")
    rp.add_argument("--split", choices=["gated", "calibration"], default=None, help="s1: only the instances of one split")

    op = sub.add_parser("coir", help="CoIR nDCG@10 of a `rueca embed` preset (needs `--group coir`)")
    op.add_argument("--profile", choices=sorted(coir_mod.PROFILES), default="cheap")
    op.add_argument("--preset", default=DEFAULT_PRESET)
    op.add_argument("--dims", type=int, default=None, help="Matryoshka dimensions (default: the model's)")
    op.add_argument("--max-tokens", type=int, default=DEFAULT_MAX_TOKENS)
    op.add_argument("--instruction", default="task", help="task (Qwen3's per-task prompts), preset (the binary's default) or a literal instruction")
    op.add_argument("--tasks", default=None, help="comma-separated mteb task names (overrides --profile)")
    op.add_argument("--limit-corpus", type=int, default=None, help="smoke run: at most N documents per task")
    op.add_argument("--batch", type=int, default=None)
    op.add_argument("--cpu", action="store_true")
    op.add_argument("--threads", type=int, default=None)

    mp = sub.add_parser("compare", help="decision tables over cached S1 outputs: metrics per arm / tag, paired bootstrap against a base, RFC 0007 §7.6 objective, top-10 overlap")
    mp.add_argument("--suite", choices=["s1"], default="s1")
    mp.add_argument("--split", choices=["calibration", "gated", "all"], default="calibration")
    mp.add_argument("--base", default="grep", help="the arm (or arm:tag) every column is paired against")
    mp.add_argument("--arms", nargs="+", required=True, help="arm (every tag it has, e.g. vector) or arm:tag (one sweep setting)")
    mp.add_argument("--variant", choices=["kw", "raw", "both"], default="both", help="query variants shown (the JSON always holds both)")
    mp.add_argument("--cache", default=None, help="bench cache to read (default: this checkout's .cache); never written")
    mp.add_argument("--json", default=None, help="also write the result as JSON to this file")

    wp = sub.add_parser("sweep", help="RFC 0007 §7.6 fusion sweep of the Rueca arm on the calibration split, then compare tables and the plateau choice")
    wp.add_argument("--suite", choices=["s1"], default="s1")
    wp.add_argument("--split", choices=["calibration"], default="calibration", help="tuning happens on the calibration split only (RFC 0004 §7.7)")
    wp.add_argument("--arm", choices=["rueca"], default="rueca")
    wp.add_argument("--mode", choices=["hybrid", "semantic"], default="hybrid")
    wp.add_argument("--alpha", default="0,0.1,...,1", help="comma list, or first,second,...,last (semantic mode: ignored)")
    wp.add_argument("--norm", default="minmax,theoretical", help="comma list of minmax, theoretical")
    wp.add_argument("--n", default="100", help="comma list of candidates per list (RUECA_FUSION_N)")
    wp.add_argument("--preset", default=None, help="embedding preset (default: RUECA_BENCH_VECTOR_PRESET, else qwen3-0.6b-q8)")
    wp.add_argument("--base", default="grep", help="the arm the compare tables pair against")
    wp.add_argument("--variant", choices=["kw", "raw", "both"], default="both", help="objective of the plateau choice (both: mean of kw and raw)")
    wp.add_argument("--lang", nargs="*", help="language families to run (default: all)")
    wp.add_argument("--only", nargs="*", help="instance ids / repositories")
    wp.add_argument("--workers", type=int, default=1, help="repositories in parallel")
    wp.add_argument("--force", action="store_true", help="re-run settings already cached")
    wp.add_argument("--prune", action="store_true", help="delete each repository clone once its runs are done")
    wp.add_argument("--report-only", action="store_true", help="run nothing: tables and plateau from the cached outputs")
    wp.add_argument("--json", default=None, help="also write compare tables and plateau as JSON")

    pr = sub.add_parser("report", help="score cached outputs; write bench/results/*.json and docs/bench/alpha-results.md")
    pr.add_argument("--s2-only", action="store_true", help="publish S2 alone: s2.json, the contracts family of scorecard.json, the S2 rows of alpha-results.md")

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
        wanted = _suites(a.suite)
        loc_arms = arms
        arms = [x for x in arms if x in ARM_NAMES]
        if "health" in wanted and run_code and "rueca" in arms:
            health_mod.run(languages=code_langs, force=a.force, only=a.only, prune=a.prune)
        if "s1" in wanted and run_code:
            s1.run(loc_arms, workers=a.workers, only=a.only, force=a.force, languages=code_langs, prune=a.prune, split=a.split)
        if "lite" in wanted:
            lite_mod.run(loc_arms, limit=a.limit, workers=a.workers, only=a.only, force=a.force, prune=a.prune)
        if "s3" in wanted and run_code:
            s3.run(arms, only=a.only, force=a.force, languages=code_langs, workers=a.workers, prune=a.prune)
        if "data" in wanted and (not langs or "data" in langs):
            data_mod.run(arms, only=a.only, force=a.force)
        if "org" in wanted and not langs:
            # self-contained: the single-repository answers come from the same org run
            org_mod.run_all(arms, force=a.force, only=a.only)
        if "s2" in wanted and not langs:
            contracts_mod.run(arms, force=a.force)  # cross-service edges (S2): one organization per group
    elif a.cmd == "coir":
        _coir(a)
    elif a.cmd == "compare":
        _compare(a)
    elif a.cmd == "sweep":
        _sweep(a, langs)
    elif a.cmd == "report":
        if a.s2_only:
            report_mod.publish_s2()
        else:
            report_mod.report()
    elif a.cmd == "check":
        return check_mod.main(a.baseline, a.current)
    elif a.cmd == "clean":
        if a.work:
            shutil.rmtree(WORK_DIR, ignore_errors=True)
        for arm in a.runs or []:
            for suite in ("s1", "s3", "data", "health", "lite"):
                shutil.rmtree(RUNS_DIR / suite / arm, ignore_errors=True)
            for kind in ("multirepo", "multirepo-gated", "multirepo-calibration", "multiref"):
                (RUNS_DIR / "org" / f"{kind}-{arm}.json").unlink(missing_ok=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
