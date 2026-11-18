"""S1 — localization (deterministic, no LLM).

Per instance: one detached worktree at the base commit; every pending arm
indexes it and answers two queries derived from the issue text (`kw`:
deterministic keyword list, `raw`: the issue text itself). Raw arm outputs
are cached per (arm, instance) in .cache/runs/s1/<arm>/<id>.json (the vector
arm: runs/s1/vector/<sweep tag>/<id>.json), so runs are resumable; scoring
happens in `report`. Arms that return raw candidate scores (`grep`: BM25,
`vector`: cosine) have them persisted per query (`scores`: top 200 files and
functions, post-filtered like the rankings) for offline fusion sweeps (RFC
0007 §7.6). The SWE-bench Lite diagnostic (`lite.py`) reuses this runner
with its own instances under runs/lite/.
"""

from __future__ import annotations

import traceback
from concurrent.futures import ThreadPoolExecutor, as_completed

from .arms import make_arm, run_label
from .arms.base import Arm
from .common import GOLD_DIR, RUNS_DIR, candidate_path, lang_family, load_manifest, log, read_json, safe_name, split_of, write_json
from .corpus import ensure_commit, prune_clone, worktree
from .datasets import instance_row, rows_by_id
from .gold import assembled_functions, compute_s1_gold, context_files, parse_list
from .queries import derive_keywords, raw_query

KEEP_RANKED = 50
# Efficiency (RFC 0005 §8): the keyword query issued as an agent would, top 10.
EFF_VARIANT = "kw"
EFF_LIMIT = 10


def eff_record(res) -> dict:
    """Compact record of an efficiency search: items in rendered order
    ([path, name, start_line, label]), tokens read, prefix cost per item."""
    if not res.ok:
        return {"ok": False, "error": res.error}
    return {
        "ok": True,
        "seconds": res.seconds,
        "tokens": res.extra.get("tokens"),
        "tokens_json": res.extra.get("tokens_json"),
        "prefix": res.extra.get("prefix") or [],
        "approx": bool(res.extra.get("approx")),
        "format": res.extra.get("format"),
        "items": [[h.get("path"), h.get("name"), h.get("start_line"), h.get("label")] for h in res.hits],
    }


def instances() -> list[dict]:
    return list(load_manifest()["s1"].get("instances", []))


def _key(d: dict) -> tuple[str, str]:
    return d["path"], d["qualname"]


def s1_gold(inst: dict) -> dict:
    """Gold for one S1 instance; cached in .cache/gold/s1/<id>.json.

    Function gold preference: LocAgent `edit_functions` > SWE-PolyBench
    `modified_nodes` > Rueca's tree-sitter labeler (see gold.py)."""
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


def run_path(arm: str, iid: str, suite: str = "s1"):
    return RUNS_DIR / suite / run_label(arm) / f"{safe_name(iid)}.json"


def _done(arm: str, iid: str, suite: str = "s1") -> bool:
    p = run_path(arm, iid, suite)
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


def _filtered_scores(scores: dict, language: str) -> dict:
    """Raw candidate scores, post-filtered like the rankings."""
    return {
        "files": [x for x in scores.get("files", []) if candidate_path(x[0], language)],
        "functions": [x for x in scores.get("functions", []) if candidate_path(x[0], language)],
    }


def index_record(arm: Arm, inst: dict, root) -> tuple[dict, object]:
    """Index one instance: the run record so far and the arm's handle
    (`status` = `index_failed` when indexing failed)."""
    rec: dict = {"arm": arm.name, "id": inst["id"], "repo": inst["repo"], "language": inst["language"]}
    h = arm.index(root, f"s1-{inst['id']}", inst["language"])
    rec["index_seconds"] = h.index_seconds
    rec["index_info"] = h.info
    if not h.ok:
        rec["status"] = "index_failed"
        rec["error"] = h.error
    return rec, h


def _query_entry(arm: Arm, h, q: str, limit: int, lang: str) -> dict:
    res = arm.localize(h, q, limit)
    entry: dict = {"ok": res.ok, "seconds": res.seconds, "error": res.error, "tokens": res.extra.get("tokens"), "tokens_json": res.extra.get("tokens_json")}
    if res.ok:
        entry["files"] = _filtered_files(res.extra.get("files", []), lang)
        entry["functions"] = _filtered_funcs(res.extra.get("functions", []), lang)
        if "functions_name" in res.extra:
            entry["functions_name"] = _filtered_funcs(res.extra["functions_name"], lang)
        if "scores" in res.extra:
            entry["scores"] = _filtered_scores(res.extra["scores"], lang)
    return entry


def query_record(arm: Arm, h, rec: dict, qs: dict[str, str], limit: int, eff: bool = True) -> dict:
    """Answer every query variant on an indexed instance (and, with `eff`,
    the efficiency search): `rec` completed with status `ok`."""
    rec["queries"] = {variant: _query_entry(arm, h, q, limit, rec["language"]) for variant, q in qs.items()}
    eff_q = qs.get(EFF_VARIANT)
    if eff and eff_q is not None and hasattr(arm, "search_eff"):
        rec["eff"] = eff_record(arm.search_eff(h, eff_q, EFF_LIMIT))
    rec["status"] = "ok"
    return rec


def _run_arm(arm: Arm, inst: dict, root, qs: dict[str, str], limit: int) -> dict:
    rec, h = index_record(arm, inst, root)
    try:
        if h.ok:
            query_record(arm, h, rec, qs, limit)
    finally:
        arm.cleanup(h)
    return rec


def run_instance(inst: dict, arm_names: list[str], limit: int, force: bool, suite: str = "s1") -> None:
    pending = [a for a in arm_names if force or not _done(a, inst["id"], suite)]
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
            log(f"{suite} {name:5s} {inst['id']}")
            try:
                rec = _run_arm(arm, inst, root, qs, limit)
            except Exception as exc:  # recorded, never silently dropped
                rec = {"arm": name, "id": inst["id"], "status": "error", "error": f"{exc}\n{traceback.format_exc()[-1500:]}"}
            rec["arm_version"] = arm.version()
            write_json(run_path(name, inst["id"], suite), rec)


def select_instances(only: list[str] | None = None, languages: set[str] | None = None, split: str | None = None) -> list[dict]:
    """S1 instances by id / repository, language family and split."""
    insts = [i for i in instances() if not only or i["id"] in only or i["repo"] in only]
    if languages:
        insts = [i for i in insts if lang_family(i["language"]) in languages]
    if split:
        splits = {r["repo"]: split_of(r) for r in load_manifest()["s1"]["repos"]}
        insts = [i for i in insts if splits.get(i["repo"], "gated") == split]
    return insts


def run(arm_names: list[str], workers: int = 2, only: list[str] | None = None, force: bool = False, languages: set[str] | None = None, prune: bool = False, split: str | None = None) -> None:
    run_instances(select_instances(only, languages, split), arm_names, "s1", workers=workers, force=force, prune=prune)


def run_instances(insts: list[dict], arm_names: list[str], suite: str, workers: int = 2, force: bool = False, prune: bool = False) -> None:
    """Run arms on instances, repositories in parallel (S1 and the Lite
    diagnostic)."""
    limit = load_manifest()["s1"].get("result_limit", 100)
    log(f"{suite}: {len(insts)} instances x {arm_names}")
    by_repo(insts, lambda inst: run_instance(inst, arm_names, limit, force, suite), suite, workers, prune)


def by_repo(insts: list[dict], job, label: str, workers: int, prune: bool) -> None:
    """`job(inst)` for every instance: the instances of one repository
    sequentially (shared clone, git lock), repositories in parallel; a
    failure is logged and the next instance runs."""
    groups: dict[str, list[dict]] = {}
    for i in insts:
        groups.setdefault(i["repo"], []).append(i)

    def _repo_job(items: list[dict]) -> None:
        for inst in items:
            try:
                job(inst)
            except Exception as exc:
                log(f"{label} FAILED {inst['id']}: {exc}")
        if prune:
            prune_clone(items[0]["repo"])

    with ThreadPoolExecutor(max_workers=max(1, workers)) as ex:
        futs = [ex.submit(_repo_job, items) for items in groups.values()]
        for f in as_completed(futs):
            f.result()
