"""External diagnostic: SWE-bench Lite localization (RFC 0007 §11.1 item 4).

The 274 SWE-bench Lite instances that LocAgent labelled (`czlll/SWE-bench_Lite`
`edit_functions`, manifest `locagent_lite`), with their issue text, base
commit and patch from `princeton-nlp`'s SWE-bench Lite (manifest
`swe_bench_lite`); both datasets are already pinned for S1. Every arm runs
exactly as in S1 (`s1.run_instances`: one worktree per base commit, the same
`kw` / `raw` queries, outputs cached in runs/lite/<arm>/<id>.json); gold is
S1's (`s1.s1_gold`: patch files, LocAgent functions mapped to tree-sitter
definitions). Scored with **any-gold** (S1's Acc@k) and **all-gold** (every
gold file in the top 5 / every gold function in the top 10, the LocAgent and
SweRank convention, so published Lite numbers read against the all-gold
columns). Python only; never used for tuning; not part of the scorecard.
`--limit N` takes the first N instances in sha256(instance id) order (a
repository-mixed sample), so a partial run is a deterministic subset.
"""

from __future__ import annotations

from .arms import S1_ARM_NAMES
from .common import GOLD_DIR, read_json, safe_name, stable_key
from .datasets import rows_by_id
from .metrics import mean, score_files, score_functions
from .s1 import run_instances, run_path
from .stats import wilson

SUITE = "lite"
SOURCE = "swe_bench_lite"
GOLD = "locagent_lite"
VARIANTS = ("kw", "raw")
# (key, row getter, population)
LITE_AGG = (
    ("file_acc@5", lambda r: r["file"]["acc@5"], "file"),
    ("file_all_acc@5", lambda r: r["file"]["all_acc@5"], "file"),
    ("func_acc@10", lambda r: r["function"]["acc@10"], "func"),
    ("func_all_acc@10", lambda r: r["function"]["all_acc@10"], "func"),
)


def lite_instances(limit: int | None = None, only: list[str] | None = None) -> list[dict]:
    """Lite instances with LocAgent labels, in sha256(id) order."""
    labelled = rows_by_id(GOLD)
    src = rows_by_id(SOURCE)
    out = [
        {"id": iid, "repo": r["repo"], "language": "python", "base_commit": r["base_commit"], "dataset": SOURCE}
        for iid, r in src.items()
        if iid in labelled and (not only or iid in only or r["repo"] in only)
    ]
    out.sort(key=lambda i: stable_key(i["id"]))
    return out[:limit] if limit else out


def run(arm_names: list[str], limit: int | None = None, workers: int = 2, only: list[str] | None = None, force: bool = False, prune: bool = False) -> None:
    run_instances(lite_instances(limit, only), arm_names, SUITE, workers=workers, force=force, prune=prune)


def _row(arm: str, variant: str, rec: dict, gold: dict) -> dict:
    q = (rec.get("queries") or {}).get(variant) or {}
    ok = rec.get("status") == "ok" and bool(q.get("ok"))
    return {
        "arm": arm,
        "variant": variant,
        "id": rec.get("id"),
        "ok": ok,
        "file": score_files(q.get("files", []) if ok else [], gold["files"]),
        "function": score_functions(q.get("functions", []) if ok else [], gold["functions"]) if gold["functions"] else None,
    }


def _aggregate(rows: list[dict]) -> dict:
    pops = {"file": rows, "func": [r for r in rows if r["function"] is not None]}
    out: dict = {"n": len(rows), "n_func": len(pops["func"]), "n_failed": sum(1 for r in rows if not r["ok"]), "ci": {}}
    for key, get, pop in LITE_AGG:
        vals = [get(r) for r in pops[pop]]
        out[key] = mean(vals)
        ci = wilson(sum(vals), len(vals)) if vals else None
        out["ci"][key] = list(ci) if ci else None
    return out


def score(insts: list[dict] | None = None) -> dict:
    """Rows and aggregates of every arm with local Lite runs."""
    insts = insts if insts is not None else lite_instances()
    rows: list[dict] = []
    for inst in insts:
        gp = GOLD_DIR / "s1" / f"{safe_name(inst['id'])}.json"
        if not gp.exists():
            continue
        gold = read_json(gp)
        for arm in S1_ARM_NAMES:
            rp = run_path(arm, inst["id"], SUITE)
            if rp.exists():
                rec = read_json(rp)
                rows.extend(_row(arm, v, rec, gold) for v in VARIANTS)
    table: dict = {}
    for arm in S1_ARM_NAMES:
        for v in VARIANTS:
            sel = [r for r in rows if r["arm"] == arm and r["variant"] == v]
            if sel:
                table.setdefault(arm, {})[v] = _aggregate(sel)
    return {"instances": len(insts), "table": table, "per_instance": rows}


def _cell(t: dict, key: str) -> str:
    v, ci = t.get(key), (t.get("ci") or {}).get(key)
    if v is None:
        return "—"
    return f"{100 * v:.1f} ({100 * ci[0]:.1f}–{100 * ci[1]:.1f})" if ci else f"{100 * v:.1f}"


def render(L: list[str], lite: dict) -> None:
    """The report section (bench/README.md, *SWE-bench Lite diagnostic*)."""
    if not lite or not lite.get("table"):
        return
    L.append("## SWE-bench Lite diagnostic (RFC 0007 §11.1)")
    L.append("")
    L.append(
        f"External, never tuned on: {lite['instances']} SWE-bench Lite instances with LocAgent function labels (Python). **any** = S1's Acc@k (a gold "
        "item in the top k); **all** = every gold file in the top 5 / every gold function in the top 10 (LocAgent / SweRank: compare published "
        "Lite numbers with these). Percentages, Wilson 95% intervals; `raw` is the issue text."
    )
    L.append("")
    L.append("| Arm | Query | n | File Acc@5 any | File Acc@5 all | n (func) | Func Acc@10 any | Func Acc@10 all |")
    L.append("|---|---|---:|---|---|---:|---|---|")
    for arm, by_v in lite["table"].items():
        for v, t in by_v.items():
            L.append(
                f"| {arm} | `{v}` | {t['n']} | {_cell(t, 'file_acc@5')} | {_cell(t, 'file_all_acc@5')} | {t['n_func']} | "
                f"{_cell(t, 'func_acc@10')} | {_cell(t, 'func_all_acc@10')} |"
            )
    L.append("")
