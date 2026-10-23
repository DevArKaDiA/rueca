"""S1 — localization (deterministic, no LLM).

Per instance: one detached worktree at the base commit; every pending arm
indexes it and answers two queries derived from the issue text (`kw`:
deterministic keyword list, `raw`: the issue text itself). Raw arm outputs
are cached per (arm, instance) in .cache/runs/s1/<arm>/<id>.json, so runs are
resumable; scoring happens in `report`.
"""

from __future__ import annotations

import traceback
from concurrent.futures import ThreadPoolExecutor, as_completed

from .arms import make_arm
from .arms.base import Arm
from .common import GOLD_DIR, RUNS_DIR, candidate_path, load_manifest, log, read_json, safe_name, write_json
from .corpus import ensure_commit, worktree
from .datasets import instance_row, rows_by_id
from .gold import assembled_functions, compute_s1_gold, context_files, parse_list
from .queries import derive_keywords, raw_query

KEEP_RANKED = 50


def instances() -> list[dict]:
    return list(load_manifest()["s1"].get("instances", []))


def _key(d: dict) -> tuple[str, str]:
    return d["path"], d["qualname"]


def s1_gold(inst: dict) -> dict:
    """Gold for one S1 instance; cached in .cache/gold/s1/<id>.json.

    Function gold preference: LocAgent `edit_functions` > SWE-PolyBench
    `modified_nodes` > Telar's tree-sitter labeler (see gold.py)."""
    out_path = GOLD_DIR / "s1" / f"{safe_name(inst['id'])}.json"
    if out_path.exists():
        return read_json(out_path)
    ensure_commit(inst["repo"], inst["base_commit"])
    man = load_manifest()
    row = instance_row(inst)
    ts = compute_s1_gold(inst["repo"], inst["base_commit"], row["patch"], inst["language"])
    gold: dict = {
        "id": inst["id"],
        "files": ts["files"],
        "skipped": ts["skipped"],
        "functions_treesitter": ts["functions"],
        "function_source": "treesitter",
        "functions": ts["functions"],
    }
    assembled = None
    loc = rows_by_id("locagent_lite").get(inst["id"])
    if loc is not None:
        assembled = ("locagent", parse_list(loc.get("edit_functions")))
    else:
        gcol = man["datasets"][inst["dataset"]].get("gold_column")
        if gcol:
            assembled = ("polybench", parse_list(row.get(gcol)))
    if assembled is not None:
        src, labels = assembled
        res = assembled_functions(src, labels, inst["repo"], inst["base_commit"], inst["language"])
        a = {_key(d) for d in res["functions"]}
        b = {_key(d) for d in ts["functions"]}
        gold.update(
            function_source=src,
            functions=res["functions"],
            assembled_labels=len(labels),
            assembled_unmapped=res["unmapped"],
            assembled_skipped_non_callable=res["skipped_non_callable"],
            labeler_agreement={
                "jaccard": (len(a & b) / len(a | b)) if (a | b) else 1.0,
                "assembled_only": sorted(f"{p}:{q}" for p, q in a - b),
                "treesitter_only": sorted(f"{p}:{q}" for p, q in b - a),
            },
        )
    cb = rows_by_id("contextbench").get(inst["id"])
    if cb is not None and cb.get("base_commit") == inst["base_commit"]:
        gold["context_files"] = context_files(cb.get("gold_context"), inst["language"])
    write_json(out_path, gold)
    return gold


def queries_for(inst: dict) -> dict[str, str]:
    text = instance_row(inst)["problem_statement"] or ""
    n = load_manifest()["s1"].get("kw_max_tokens", 12)
    kws = derive_keywords(text, n)
    return {"kw": " ".join(kws), "raw": raw_query(text)}


def run_path(arm: str, iid: str):
    return RUNS_DIR / "s1" / arm / f"{safe_name(iid)}.json"


def _done(arm: str, iid: str) -> bool:
    p = run_path(arm, iid)
    if not p.exists():
        return False
    rec = read_json(p)
    return rec.get("status") in ("ok", "index_failed")


def _filtered_files(files: list[str], language: str) -> list[str]:
    out: list[str] = []
    for f in files:
        if f and candidate_path(f, language) and f not in out:
            out.append(f)
    return out[:KEEP_RANKED]


def _filtered_funcs(funcs: list[dict], language: str) -> list[dict]:
    out = []
    for f in funcs:
        if f.get("path") and candidate_path(f["path"], language):
            out.append({k: f.get(k) for k in ("path", "name", "label", "start_line", "end_line")})
        if len(out) >= KEEP_RANKED:
            break
    return out


def _run_arm(arm: Arm, inst: dict, root, qs: dict[str, str], limit: int) -> dict:
    lang = inst["language"]
    rec: dict = {"arm": arm.name, "id": inst["id"], "repo": inst["repo"], "language": lang}
    h = arm.index(root, f"s1-{inst['id']}", lang)
    rec["index_seconds"] = h.index_seconds
    rec["index_info"] = h.info
    if not h.ok:
        rec["status"] = "index_failed"
        rec["error"] = h.error
        arm.cleanup(h)
        return rec
    rec["queries"] = {}
    try:
        for variant, q in qs.items():
            res = arm.localize(h, q, limit)
            entry: dict = {"ok": res.ok, "seconds": res.seconds, "error": res.error}
            if res.ok:
                entry["files"] = _filtered_files(res.extra.get("files", []), lang)
                entry["functions"] = _filtered_funcs(res.extra.get("functions", []), lang)
                if "functions_name" in res.extra:
                    entry["functions_name"] = _filtered_funcs(res.extra["functions_name"], lang)
            rec["queries"][variant] = entry
    finally:
        arm.cleanup(h)
    rec["status"] = "ok"
    return rec


def run_instance(inst: dict, arm_names: list[str], limit: int, force: bool) -> None:
    pending = [a for a in arm_names if force or not _done(a, inst["id"])]
    if not pending:
        return
    s1_gold(inst)
    qs = queries_for(inst)
    with worktree(inst["repo"], inst["base_commit"], f"s1-{inst['id']}") as root:
        for name in pending:
            arm = make_arm(name)
            t_ok, why = arm.available()
            if not t_ok:
                continue
            log(f"s1 {name:5s} {inst['id']}")
            try:
                rec = _run_arm(arm, inst, root, qs, limit)
            except Exception as exc:  # recorded, never silently dropped
                rec = {"arm": name, "id": inst["id"], "status": "error", "error": f"{exc}\n{traceback.format_exc()[-1500:]}"}
            rec["arm_version"] = arm.version()
            write_json(run_path(name, inst["id"]), rec)


def run(arm_names: list[str], workers: int = 2, only: list[str] | None = None, force: bool = False) -> None:
    limit = load_manifest()["s1"].get("result_limit", 100)
    insts = [i for i in instances() if not only or i["id"] in only or i["repo"] in only]
    log(f"s1: {len(insts)} instances x {arm_names}")
    # instances of one repo run sequentially (shared clone, git lock); repos in parallel
    by_repo: dict[str, list[dict]] = {}
    for i in insts:
        by_repo.setdefault(i["repo"], []).append(i)

    def _repo_job(items: list[dict]) -> None:
        for inst in items:
            try:
                run_instance(inst, arm_names, limit, force)
            except Exception as exc:
                log(f"s1 FAILED {inst['id']}: {exc}")

    with ThreadPoolExecutor(max_workers=max(1, workers)) as ex:
        futs = [ex.submit(_repo_job, items) for items in by_repo.values()]
        for f in as_completed(futs):
            f.result()
