"""`rueca-bench report`: score cached arm outputs against gold.

Writes bench/results/{meta,s1,s3,data}.json and docs/bench/alpha-results.md.
Failures count as misses (zero scores) and are listed, never dropped.

Results are additive across checkouts: an (arm, instance) or (arm, S3 set)
whose raw output is not in this cache keeps its published entry from
bench/results/*.json (S1 per-instance rows, S3 per-question rows or, for
entries published before rows were stored, the set's aggregates), so adding
a language never requires re-running — or silently drops — the others. The
report says which entries were carried.
"""

from __future__ import annotations

import datetime as dt
import platform
import subprocess
import sys
from collections import Counter, defaultdict

from .arms import ARM_NAMES, make_arm
from .arms.base import is_callable_label, same_def
from .common import (
    DOCS_DIR,
    GOLD_DIR,
    MANIFEST_PATH,
    RESULTS_DIR,
    RUNS_DIR,
    families,
    lang_group,
    load_manifest,
    log,
    read_json,
    safe_name,
    sha256_file,
    write_json,
)
from . import data as data_mod
from .metrics import mean, median, p95, prf, score_files, score_functions
from .s1 import instances as s1_instances
from .s3 import MICRO_SUITES, gold_path, prediction_in_scope, suites

VARIANTS = ("kw", "raw")
OVERALL = "overall"
CTX_KS = (5, 10)


def language_groups() -> list[str]:
    """Report columns, one per language family, in manifest order."""
    return [f.label for f in families().values()]


def overall_scope() -> set[str] | None:
    """Labels of the families the `overall` column may pool (manifest
    `[bench] overall`); None = every family."""
    keys = load_manifest().get("bench", {}).get("overall")
    if keys is None:
        return None
    return {families()[k].label for k in keys if k in families()}


def common_groups(rows_by_arm: dict[str, set[str]]) -> list[str]:
    """Groups in which every arm that produced any result has results: the
    `overall` column pools only these (restricted to `[bench] overall`), so
    an arm that is still pending for a language — or a language added after
    the alpha — does not make the overall numbers incomparable."""
    present = [g for g in rows_by_arm.values() if g]
    if not present:
        return []
    shared = set.intersection(*present)
    scope = overall_scope()
    return [g for g in language_groups() if g in shared and (scope is None or g in scope)]


# --- published results (carry-forward) ---------------------------------------------


def load_published() -> dict:
    """bench/results/*.json as last written (read before this report
    overwrites them)."""
    out = {}
    for name in ("meta", "s1", "s3"):
        p = RESULTS_DIR / f"{name}.json"
        out[name] = read_json(p) if p.exists() else {}
    return out


# --- metadata -------------------------------------------------------------------


def _sysctl(key: str) -> str:
    try:
        return subprocess.run(["sysctl", "-n", key], capture_output=True, text=True, timeout=5).stdout.strip()
    except (OSError, subprocess.SubprocessError):
        return ""


def machine_info() -> dict:
    mem = _sysctl("hw.memsize")
    return {
        "platform": platform.platform(),
        "machine": platform.machine(),
        "cpu": _sysctl("machdep.cpu.brand_string") or platform.processor(),
        "cpus": _sysctl("hw.ncpu"),
        "memory_gb": round(int(mem) / 2**30) if mem.isdigit() else None,
        "python": sys.version.split()[0],
    }


def _recorded_versions(name: str) -> list[str]:
    vers: set[str] = set()
    for suite in ("s1", "s3", "data"):
        for p in sorted((RUNS_DIR / suite / name).glob("*.json")):
            v = read_json(p).get("arm_version")
            if v:
                vers.add(v)
    return sorted(vers)


def versions_by_group(published: dict | None = None, carried_groups: set[str] | None = None) -> dict:
    """{group: {arm: [versions]}} from cached outputs, so a report that mixes
    binaries across languages (e.g. a language pack on a branch) says so.
    Groups whose results were carried keep their published versions."""
    out: dict = defaultdict(lambda: defaultdict(set))
    for g, d in ((published or {}).get("versions_by_group") or {}).items():
        if carried_groups and g in carried_groups:
            for a, vs in d.items():
                out[g][a].update(vs)
    for suite in ("s1", "s3"):
        for name in ARM_NAMES:
            for p in sorted((RUNS_DIR / suite / name).glob("*.json")):
                rec = read_json(p)
                if rec.get("arm_version") and rec.get("language"):
                    try:
                        out[lang_group(rec["language"])][name].add(rec["arm_version"])
                    except KeyError:
                        continue
    return {g: {a: sorted(v) for a, v in d.items()} for g, d in out.items()}


def arm_status() -> dict:
    """Versions are taken from the cached outputs (what was actually run);
    the live binary is only consulted when nothing has been run yet."""
    out = {}
    for name in ARM_NAMES:
        arm = make_arm(name)
        ok, why = arm.available()
        rec = _recorded_versions(name)
        out[name] = {
            "available": ok or bool(rec),
            "reason": why,
            "version": "; ".join(rec) if rec else (arm.version() if ok else None),
            "recorded_versions": rec,
        }
    return out


# --- S1 -------------------------------------------------------------------------------


def _ctx_recall(ranked: list[str], ctx: list[str]) -> dict:
    if not ctx:
        return {}
    s = set(ctx)
    return {f"ctx_recall@{k}": len(s & set(ranked[:k])) / len(s) for k in CTX_KS}


def _gold_summary(gold: dict) -> dict:
    return {
        "function_source": gold.get("function_source"),
        "n_files": len(gold["files"]),
        "n_functions": len(gold["functions"]),
        "has_context": bool(gold.get("context_files")),
        "jaccard": gold["labeler_agreement"]["jaccard"] if "labeler_agreement" in gold else None,
        "assembled_unmapped": len(gold.get("assembled_unmapped", [])),
        "assembled_labels": gold.get("assembled_labels", 0),
    }


def _summary_from_row(row: dict) -> dict:
    """Gold summary of a carried instance, from its published score row."""
    return {
        "function_source": row.get("function_source"),
        "n_files": row.get("n_gold_files", 0),
        "n_functions": row.get("n_gold_functions", 0),
        "has_context": bool(row.get("context")),
        "jaccard": None,
        "assembled_unmapped": 0,
        "assembled_labels": 0,
    }


def score_s1(published: dict | None = None) -> dict:
    insts = s1_instances()
    published = published or {}
    pub_rows = {(r["arm"], r["variant"], r["id"]): r for r in published.get("per_instance", [])}
    pub_gold = published.get("gold_per_instance", {})
    per: list[dict] = []
    summaries: dict[str, dict] = {}
    carried: Counter = Counter()
    for inst in insts:
        gp = GOLD_DIR / "s1" / f"{safe_name(inst['id'])}.json"
        gold = read_json(gp) if gp.exists() else None
        if gold is not None:
            summaries[inst["id"]] = _gold_summary(gold)
        for arm in ARM_NAMES:
            rp = RUNS_DIR / "s1" / arm / f"{safe_name(inst['id'])}.json"
            if not rp.exists() or gold is None:
                old = [pub_rows.get((arm, v, inst["id"])) for v in VARIANTS]
                old = [dict(r, group=lang_group(inst["language"])) for r in old if r]
                if old:
                    per.extend(old)
                    carried[(arm, lang_group(inst["language"]))] += 1
                    if inst["id"] not in summaries:
                        summaries[inst["id"]] = pub_gold.get(inst["id"]) or _summary_from_row(old[0])
                continue
            rec = read_json(rp)
            for variant in VARIANTS:
                q = (rec.get("queries") or {}).get(variant) or {}
                ok = rec.get("status") == "ok" and q.get("ok")
                files = q.get("files", []) if ok else []
                funcs = q.get("functions", []) if ok else []
                row = {
                    "arm": arm,
                    "variant": variant,
                    "id": inst["id"],
                    "repo": inst["repo"],
                    "language": inst["language"],
                    "group": lang_group(inst["language"]),
                    "ok": bool(ok),
                    "error": None if ok else (q.get("error") or rec.get("error") or rec.get("status")),
                    "index_seconds": rec.get("index_seconds"),
                    "query_seconds": q.get("seconds"),
                    "function_source": gold.get("function_source"),
                    "n_gold_files": len(gold["files"]),
                    "n_gold_functions": len(gold["functions"]),
                    "file": score_files(files, gold["files"]),
                    "function": score_functions(funcs, gold["functions"]) if gold["functions"] else None,
                    "context": _ctx_recall(files, gold.get("context_files") or []),
                }
                if q.get("functions_name") is not None and gold["functions"]:
                    row["function_name_only"] = score_functions(q["functions_name"] if ok else [], gold["functions"])
                per.append(row)

    def agg(rows: list[dict]) -> dict:
        if not rows:
            return {"n": 0}
        fr = [r for r in rows if r["function"] is not None]
        cr = [r for r in rows if r["context"]]
        out = {
            "n": len(rows),
            "n_failed": sum(1 for r in rows if not r["ok"]),
            "file_acc@1": mean(r["file"]["acc@1"] for r in rows),
            "file_acc@3": mean(r["file"]["acc@3"] for r in rows),
            "file_acc@5": mean(r["file"]["acc@5"] for r in rows),
            "file_recall@5": mean(r["file"]["recall@5"] for r in rows),
            "file_recall@10": mean(r["file"]["recall@10"] for r in rows),
            "file_mrr": mean(r["file"]["mrr"] for r in rows),
            "n_func": len(fr),
            "func_acc@5": mean(r["function"]["acc@5"] for r in fr),
            "func_acc@10": mean(r["function"]["acc@10"] for r in fr),
            "func_recall@5": mean(r["function"]["recall@5"] for r in fr),
            "func_recall@10": mean(r["function"]["recall@10"] for r in fr),
            "func_mrr": mean(r["function"]["mrr"] for r in fr),
            "n_ctx": len(cr),
            "ctx_recall@5": mean(r["context"]["ctx_recall@5"] for r in cr),
            "ctx_recall@10": mean(r["context"]["ctx_recall@10"] for r in cr),
            "index_s_median": median(r["index_seconds"] for r in rows),
            "index_s_p95": p95(r["index_seconds"] for r in rows),
            "query_s_median": median(r["query_seconds"] for r in rows),
            "query_s_p95": p95(r["query_seconds"] for r in rows),
        }
        nr = [r for r in rows if r.get("function_name_only")]
        if nr:
            out["func_acc@5_name_only"] = mean(r["function_name_only"]["acc@5"] for r in nr)
            out["func_acc@10_name_only"] = mean(r["function_name_only"]["acc@10"] for r in nr)
        return out

    overall_groups = common_groups({arm: {r["group"] for r in per if r["arm"] == arm} for arm in ARM_NAMES})
    table: dict = {}
    for arm in ARM_NAMES:
        for variant in VARIANTS:
            rows = [r for r in per if r["arm"] == arm and r["variant"] == variant]
            if not rows:
                continue
            t = table.setdefault(arm, {}).setdefault(variant, {})
            for g in language_groups():
                gr = [r for r in rows if r["group"] == g]
                if gr:
                    t[g] = agg(gr)
            t[OVERALL] = agg([r for r in rows if r["group"] in overall_groups])
            for lang in sorted({r["language"] for r in rows}):
                t[f"lang:{lang}"] = agg([r for r in rows if r["language"] == lang])
            for repo in sorted({r["repo"] for r in rows}):
                t[f"repo:{repo}"] = agg([r for r in rows if r["repo"] == repo])

    golds = [summaries[i["id"]] for i in insts if i["id"] in summaries]
    agree = [g["jaccard"] for g in golds if g.get("jaccard") is not None]

    def stats(sel: list[dict], ginsts: list[dict]) -> dict:
        ag = [g["jaccard"] for g in sel if g.get("jaccard") is not None]
        return {
            "instances": len(ginsts),
            "by_language": dict(Counter(i["language"] for i in ginsts)),
            "by_group": dict(Counter(lang_group(i["language"]) for i in ginsts)),
            "by_repo": dict(Counter(i["repo"] for i in ginsts)),
            "by_dataset": dict(Counter(i["dataset"] for i in ginsts)),
            "function_source": dict(Counter(g["function_source"] for g in sel)),
            "with_function_gold": sum(1 for g in sel if g["n_functions"]),
            "with_context_gold": sum(1 for g in sel if g["has_context"]),
            "labeler_agreement_jaccard_mean": mean(ag),
            "labeler_agreement_n": len(ag),
            "assembled_unmapped": sum(g.get("assembled_unmapped", 0) for g in sel),
            "assembled_labels": sum(g.get("assembled_labels", 0) for g in sel),
            "mean_gold_files": mean(g["n_files"] for g in sel),
            "mean_gold_functions": mean(g["n_functions"] for g in sel if g["n_functions"]),
        }

    gold_stats = stats(golds, insts)
    gold_stats["per_group"] = {}
    for grp in language_groups():
        gi = [i for i in insts if lang_group(i["language"]) == grp]
        if gi:
            gold_stats["per_group"][grp] = stats([summaries[i["id"]] for i in gi if i["id"] in summaries], gi)
    del agree
    failures = [
        {k: r[k] for k in ("arm", "variant", "id", "error")} for r in per if not r["ok"]
    ]
    return {
        "table": table,
        "per_instance": per,
        "gold": gold_stats,
        "gold_per_instance": summaries,
        "carried": {f"{a}:{g}": n for (a, g), n in sorted(carried.items())},
        "failures": failures,
        "overall_groups": overall_groups,
    }


# --- S3 -------------------------------------------------------------------------------


def _score_answers(answers: list[dict], graph: dict, gold_meta: dict) -> list[dict]:
    rows = []
    for a in answers:
        f = graph["callables"][a["fid"]]
        row = {"fid": a["fid"], "found": bool(a["resolve"]["ok"]), "seconds": []}
        for direction in ("callers", "callees"):
            gold = [graph["callables"][x] for x in graph[direction].get(a["fid"], []) if x in graph["callables"]]
            d = a.get(direction) or {}
            # same post-filter for every arm: in-scope path and a callable label
            # (module / file pseudo-callers are not in the gold)
            preds = (
                [h for h in d.get("hits", []) if prediction_in_scope(h.get("path"), gold_meta) and is_callable_label(h.get("label"))]
                if d.get("ok")
                else []
            )
            row[direction] = prf(preds, gold)
            row[direction]["matched_pred"] = sum(1 for h in {(h.get("path"), h.get("name"), h.get("start_line")): h for h in preds}.values() if any(same_def(h, g) for g in gold))
            row[direction]["matched_gold"] = sum(1 for g in gold if any(same_def(h, g) for h in preds))
            if d.get("seconds") is not None:
                row["seconds"].append(d["seconds"])
        if "lookup" in a:
            hits = a["lookup"].get("hits", []) if a["lookup"].get("ok") else []
            hits = [h for h in hits if prediction_in_scope(h.get("path"), gold_meta) and is_callable_label(h.get("label"))]
            rank = next((i + 1 for i, h in enumerate(hits) if same_def(h, f)), None)
            row["lookup_rank"] = rank
            row["seconds"].append(a["lookup"].get("seconds"))
        rows.append(row)
    return rows


def _agg_s3(rows: list[dict]) -> dict:
    if not rows:
        return {"n": 0}
    out: dict = {"n": len(rows), "definition_found": mean(float(r["found"]) for r in rows)}
    for direction in ("callers", "callees"):
        nz = [r[direction] for r in rows if r[direction]["n_gold"] > 0]
        mp = sum(r[direction]["matched_pred"] for r in rows)
        npred = sum(r[direction]["n_pred"] for r in rows)
        mg = sum(r[direction]["matched_gold"] for r in rows)
        ngold = sum(r[direction]["n_gold"] for r in rows)
        p = mp / npred if npred else None
        rc = mg / ngold if ngold else None
        out[direction] = {
            "n": len(nz),
            "precision": mean(x["precision"] for x in nz),
            "recall": mean(x["recall"] for x in nz),
            "f1": mean(x["f1"] for x in nz),
            "micro_precision": p,
            "micro_recall": rc,
            "micro_f1": (2 * p * rc / (p + rc)) if p and rc else 0.0,
            "gold_edges": ngold,
            "predicted_edges": npred,
        }
    lk = [r for r in rows if "lookup_rank" in r]
    if lk:
        out["lookup_acc@1"] = mean(float(r["lookup_rank"] == 1) for r in lk)
        out["lookup_acc@10"] = mean(float(r["lookup_rank"] is not None and r["lookup_rank"] <= 10) for r in lk)
    secs = [s for r in rows for s in r["seconds"] if s is not None]
    out["query_s_median"] = median(secs)
    out["query_s_p95"] = p95(secs)
    return out


def score_s3(published: dict | None = None) -> dict:
    published = published or {}
    pub_suite = published.get("per_suite", {})
    pub_rows = published.get("rows", {})
    pub_groups = published.get("groups", {})
    pub_gold = published.get("gold", {})
    per_suite: dict = {}
    pooled: dict = defaultdict(lambda: defaultdict(list))
    agg_carried: dict = defaultdict(set)  # arm -> groups carried as aggregates (no rows)
    rows_out: dict = defaultdict(dict)
    carried: list[str] = []
    gold_info: dict = {}
    failures: list[dict] = []
    micro_by_group: dict[str, str] = {}
    for name, spec, micro in suites():
        gp = gold_path(name)
        grp = lang_group(spec["language"])
        if not gp.exists() or not any((RUNS_DIR / "s3" / a / f"{safe_name(name)}.json").exists() for a in ARM_NAMES):
            # nothing to score here: keep the published set, if any
            if name in pub_gold and any(name in pub_suite.get(a, {}) for a in ARM_NAMES):
                gold_info[name] = dict(pub_gold[name], carried=True)
                if micro:
                    micro_by_group.setdefault(grp, name)
                for arm in ARM_NAMES:
                    res = pub_suite.get(arm, {}).get(name)
                    if res is None:
                        continue
                    per_suite.setdefault(arm, {})[name] = res
                    carried.append(f"{arm}:{name}")
                    failures += [f for f in published.get("failures", []) if f.get("arm") == arm and f.get("suite") == name]
                    rows = pub_rows.get(arm, {}).get(name)
                    if rows is not None:
                        rows_out[arm][name] = rows
                    if not micro:
                        if rows is not None:
                            pooled[arm][grp].extend(rows)
                        else:
                            agg_carried[arm].add(grp)
                continue
            if not gp.exists():
                gold_info[name] = {"missing": True, "language": spec["language"], "repo": spec.get("repo"), "micro": micro, "title": spec.get("title", name)}
                continue
        gold = read_json(gp)
        is_micro = gold.get("suite") in MICRO_SUITES
        if is_micro:
            micro_by_group.setdefault(lang_group(spec["language"]), name)
            gold_info[name] = {
                "micro": True,
                "title": spec.get("title", name),
                "language": spec["language"],
                "snippets": len(gold["snippets"]),
                "functions": sum(len(g["sample"]) for g in gold["snippets"].values()),
                "edges": sum(len(v) for g in gold["snippets"].values() for v in g["callees"].values()),
                "commit": gold.get("commit"),
                "source": gold.get("source"),
            }
            if "phpstan_agreement" in gold:
                gold_info[name]["phpstan_agreement"] = {k: v for k, v in gold["phpstan_agreement"].items() if k != "per_snippet"}
            if "crosscheck" in gold:
                gold_info[name]["crosscheck"] = {k: v for k, v in gold["crosscheck"].items() if k != "per_snippet"}
        else:
            gold_info[name] = {
                "repo": gold["repo"],
                "language": gold["language"],
                "commit": gold["commit"],
                "indexer": gold["indexer"],
                "method": gold.get("suite", "scip"),
                "callables": len(gold["callables"]),
                "edges": gold["stats"].get("edges"),
                "sample": len(gold["sample"]),
                "stats": gold["stats"],
            }
        for arm in ARM_NAMES:
            rp = RUNS_DIR / "s3" / arm / f"{safe_name(name)}.json"
            if not rp.exists():
                res = pub_suite.get(arm, {}).get(name)
                if res is not None and pub_gold.get(name, {}).get("commit") == gold.get("commit"):
                    per_suite.setdefault(arm, {})[name] = res
                    carried.append(f"{arm}:{name}")
                    rows = pub_rows.get(arm, {}).get(name)
                    if rows is not None:
                        rows_out[arm][name] = rows
                    if not is_micro:
                        if rows is not None:
                            pooled[arm][grp].extend(rows)
                        else:
                            agg_carried[arm].add(grp)
                continue
            rec = read_json(rp)
            if is_micro:
                rows = []
                idx = []
                meta = {"suite": gold["suite"], "language": gold.get("language", spec["language"])}
                for sname, g in sorted(gold["snippets"].items()):
                    if not g["sample"]:
                        continue
                    srec = rec.get("snippets", {}).get(sname)
                    if srec is None or srec.get("status") != "ok":
                        failures.append({"arm": arm, "suite": name, "snippet": sname, "error": (srec or {}).get("error", "missing")})
                        answers = [{"fid": q, "resolve": {"ok": False}} for q in g["sample"]]
                    else:
                        answers = srec["answers"]
                        idx.append(srec.get("index_seconds"))
                    rows += _score_answers(answers, g, meta)
                rows_out[arm][name] = rows
                res = _agg_s3(rows)
                res["index_s_median"] = median(idx)
                res["snippets_failed"] = sum(1 for f in failures if f["arm"] == arm and f["suite"] == name)
            else:
                if rec.get("status") != "ok":
                    failures.append({"arm": arm, "suite": name, "error": rec.get("error")})
                    answers = [{"fid": q, "resolve": {"ok": False}} for q in gold["sample"]]
                else:
                    answers = rec["answers"]
                rows = _score_answers(answers, gold, gold)
                rows_out[arm][name] = rows
                res = _agg_s3(rows)
                res["index_seconds"] = rec.get("index_seconds")
                res["status"] = rec.get("status")
                pooled[arm][lang_group(gold["language"])].extend(rows)
            per_suite.setdefault(arm, {})[name] = res
    arms_with = set(pooled) | set(agg_carried)
    overall_groups = common_groups({arm: set(pooled.get(arm, {})) | agg_carried.get(arm, set()) for arm in arms_with})
    groups = {}
    for arm in arms_with:
        d = pooled.get(arm, {})
        groups[arm] = {g: _agg_s3(rows) for g, rows in d.items()}
        for g in agg_carried.get(arm, ()):
            # published before per-question rows were stored: keep the aggregate
            if g in pub_groups.get(arm, {}):
                groups[arm][g] = pub_groups[arm][g]
        if any(g in agg_carried.get(arm, ()) for g in overall_groups):
            same = list(published.get("overall_groups") or []) == overall_groups
            groups[arm][OVERALL] = pub_groups.get(arm, {}).get(OVERALL) if same else {"n": 0}
        else:
            groups[arm][OVERALL] = _agg_s3([r for g in overall_groups for r in d.get(g, [])])
    return {
        "per_suite": per_suite,
        "groups": groups,
        "gold": gold_info,
        "failures": failures,
        "overall_groups": overall_groups,
        "micro_by_group": micro_by_group,
        "carried": carried,
        "rows": {a: dict(v) for a, v in rows_out.items()},
    }


# --- markdown -----------------------------------------------------------------------


def _f(x, pct: bool = True, nd: int = 1) -> str:
    if x is None:
        return "—"
    if pct:
        return f"{100 * x:.{nd}f}"
    return f"{x:.2f}" if x < 100 else f"{x:.0f}"


def _arm_cols(status: dict) -> list[str]:
    return list(ARM_NAMES)


def _bold_best(vals: list[str]) -> list[str]:
    nums = []
    for v in vals:
        try:
            nums.append(float(v))
        except ValueError:
            nums.append(None)
    present = [n for n in nums if n is not None]
    if len(present) < 2:
        return vals
    best = max(present)
    return [f"**{v}**" if n is not None and n == best else v for v, n in zip(vals, nums)]


S1_METRICS = [
    ("File Acc@1", "file_acc@1"),
    ("File Acc@3", "file_acc@3"),
    ("File Acc@5", "file_acc@5"),
    ("File Recall@5", "file_recall@5"),
    ("File Recall@10", "file_recall@10"),
    ("File MRR", "file_mrr"),
    ("Func Acc@5", "func_acc@5"),
    ("Func Acc@10", "func_acc@10"),
    ("Func Recall@10", "func_recall@10"),
    ("Func MRR", "func_mrr"),
    ("Ctx-file Recall@10 (ContextBench)", "ctx_recall@10"),
]
S3_COLS = ["definition found", "callers P", "callers R", "callers F1", "callees P", "callees R", "callees F1", "micro P (callees)", "micro R (callees)", "lookup Acc@1", "lookup Acc@10", "query s (median)"]

# Parity table: (column title, getter(s1, s3, arm, group) -> value or None)
def _s1v(variant: str, key: str):
    return lambda s1, s3, arm, grp: (s1["table"].get(arm, {}).get(variant, {}).get(grp) or {}).get(key)


def _s3v(*path: str, micro: bool = False):
    def get(s1, s3, arm, grp):
        if micro:
            name = s3["micro_by_group"].get(grp)
            v = s3["per_suite"].get(arm, {}).get(name) if name else None
        else:
            v = s3["groups"].get(arm, {}).get(grp)
        for k in path:
            v = v.get(k) if isinstance(v, dict) else None
        return v

    return get


# Columns shown in the parity table (key metrics) ...
PARITY_KEYS = [
    ("S1 kw file Acc@5", _s1v("kw", "file_acc@5")),
    ("S1 kw func Acc@10", _s1v("kw", "func_acc@10")),
    ("S1 raw file Acc@5", _s1v("raw", "file_acc@5")),
    ("S1 raw func Acc@10", _s1v("raw", "func_acc@10")),
    ("S3 definition found", _s3v("definition_found")),
    ("S3 callers F1", _s3v("callers", "f1")),
    ("S3 callees F1", _s3v("callees", "f1")),
    ("S3 micro callees F1", _s3v("callees", "f1", micro=True)),
]
# ... and every metric of the delta view, which decides the status.
GATE_METRICS = [
    _s1v("kw", "file_acc@1"),
    _s1v("kw", "file_acc@5"),
    _s1v("kw", "func_acc@10"),
    _s1v("raw", "file_acc@5"),
    _s1v("raw", "func_acc@10"),
    _s3v("definition_found"),
    _s3v("callers", "f1"),
    _s3v("callees", "f1"),
    _s3v("callees", "micro_recall"),
    _s3v("lookup_acc@1"),
    _s3v("definition_found", micro=True),
    _s3v("callers", "f1", micro=True),
    _s3v("callees", "f1", micro=True),
]


def _s3row(label: str, t: dict | None) -> str:
    if not t or not t.get("n"):
        return f"| {label} | pending |" + " |" * len(S3_COLS)
    cr, ce = t["callers"], t["callees"]
    vals = [
        _f(t["definition_found"]),
        _f(cr["precision"]),
        _f(cr["recall"]),
        _f(cr["f1"]),
        _f(ce["precision"]),
        _f(ce["recall"]),
        _f(ce["f1"]),
        _f(ce["micro_precision"]),
        _f(ce["micro_recall"]),
        _f(t.get("lookup_acc@1")),
        _f(t.get("lookup_acc@10")),
        _f(t.get("query_s_median"), False),
    ]
    return f"| {label} | {t['n']} | " + " | ".join(vals) + " |"


def _gold_set_row(name: str, gi: dict) -> str:
    if gi.get("missing"):
        what = gi.get("repo") or gi.get("title") or name
        return f"| {what} | {gi.get('language', '')} | | gold missing | | | |"
    if gi.get("micro"):
        commit = f"`{gi['commit'][:10]}`" if gi.get("commit") else f"`bench/{gi['source']}`"
        return f"| {gi['title']} | {gi['language']} | {commit} | hand-written | {gi['functions']} | {gi['edges']} | {gi['functions']} ({gi['snippets']} snippets) |"
    return f"| {gi['repo']} | {gi['language']} | `{gi['commit'][:10]}` | {gi['indexer']} | {gi['callables']} | {gi['edges']} | {gi['sample']} |"


def _noise_band(n: int | None) -> float | None:
    """Points within which a gap is noise for a rate measured on n items:
    one standard error of a proportion at p = 0.5 (50 / sqrt(n))."""
    return 50.0 / n**0.5 if n else None


def _cell_delta(r, c, band: float | None) -> tuple[str, bool | None]:
    if r is None or c is None:
        return ("pending" if r is None else f"pending / {_f(c)}"), None
    d = 100 * (r - c)
    ok = d >= -1e-9 or (band is not None and d >= -band)
    mark = "✅" if d >= -1e-9 else ("≈" if ok else "❌")
    return f"{_f(r)} / {_f(c)} / {d:+.1f} {mark}", ok


def _render_rfc_parity(L: list[str], s1: dict, s3: dict, data: dict) -> None:
    """Parity rows in the RFC 0003 §3.2 format (one per language, one per
    data format), next to the per-metric view above."""
    man = load_manifest()
    L.append("## Parity rows (RFC 0003 §3.2)")
    L.append("")
    L.append(
        "One row per language: Rueca / cbm v0.12.0 / Δ (points), S1 on the keyword query. A cell is ✅ when Δ ≥ 0, ≈ when the gap is "
        "within the noise band printed for its sample size (one standard error of a proportion at 50 %, i.e. 50/√n points), ❌ otherwise; "
        "*At parity* needs every cell ✅ or ≈. *Status* is the release-gate status of RFC 0003 §3.1, kept in the manifest "
        "(`[languages.<family>] status`): `supported`, `preview` or `text` (no pack on main)."
    )
    L.append("")
    L.append("| Language | Status | S1 n | File Acc@5 R / cbm / Δ | Func Acc@10 R / cbm / Δ | S3 gold | S3 n | Def. found R / cbm / Δ | Callers F1 R / cbm / Δ | Callees F1 R / cbm / Δ | Index s (median) R / cbm | At parity |")
    L.append("|---|---|---:|---|---|---|---:|---|---|---|---|---|")
    for key, fam in families().items():
        spec = man["languages"][key]
        kw = {a: s1["table"].get(a, {}).get("kw", {}).get(fam.label) for a in ("rueca", "cbm")}
        g3 = {a: s3["groups"].get(a, {}).get(fam.label) for a in ("rueca", "cbm")}
        mname = s3["micro_by_group"].get(fam.label)
        m3 = {a: s3["per_suite"].get(a, {}).get(mname) if mname else None for a in ("rueca", "cbm")}
        n1 = (kw["cbm"] or kw["rueca"] or {}).get("n")
        nf = (kw["cbm"] or kw["rueca"] or {}).get("n_func")
        if not n1 and not (g3["cbm"] or {}).get("n") and not (m3["cbm"] or {}).get("n"):
            continue
        # S3: repository gold when it exists, else the micro-benchmark
        golds = [gi for gi in s3["gold"].values() if gi.get("language") in fam.members and not gi.get("missing")]
        src = g3 if any(not gi.get("micro") for gi in golds) else m3
        n3 = (src["cbm"] or src["rueca"] or {}).get("n")
        gdesc = "; ".join(
            sorted({f"{gi['indexer']} ({', '.join(sorted(x['repo'].split('/')[1] for x in golds if not x.get('micro') and x.get('indexer') == gi['indexer']))})" for gi in golds if not gi.get("micro")})
        )
        if src is m3:
            gdesc = (gdesc + "; " if gdesc else "") + "micro-benchmark only"
        cells, oks = [], []
        for get, n in (
            (lambda a: (kw[a] or {}).get("file_acc@5"), n1),
            (lambda a: (kw[a] or {}).get("func_acc@10"), nf),
            (lambda a: (src[a] or {}).get("definition_found"), n3),
            (lambda a: ((src[a] or {}).get("callers") or {}).get("f1"), ((src["cbm"] or {}).get("callers") or {}).get("n")),
            (lambda a: ((src[a] or {}).get("callees") or {}).get("f1"), ((src["cbm"] or {}).get("callees") or {}).get("n")),
        ):
            txt, ok = _cell_delta(get("rueca"), get("cbm"), _noise_band(n))
            cells.append(txt)
            oks.append(ok)
        idx = f"{_f((kw['rueca'] or {}).get('index_s_median'), False) if kw['rueca'] else 'pending'} / {_f((kw['cbm'] or {}).get('index_s_median'), False)}"
        if any(o is None for o in oks[:1]) and kw["rueca"] is None:
            verdict = "pending (no Rueca results)"
        elif all(o is not False for o in oks):
            verdict = "✅"
        else:
            verdict = "❌ (" + ", ".join(t for t, o in zip(("S1 file", "S1 func", "S3 def", "S3 callers", "S3 callees"), oks) if o is False) + ")"
        L.append(
            f"| {fam.label} | {spec.get('status', '—')} | {n1 or '—'} | {cells[0]} | {cells[1]} | {gdesc or '—'} | {n3 or '—'} | {cells[2]} | {cells[3]} | {cells[4]} | {idx} | {verdict} |"
        )
    L.append("")
    if data.get("formats"):
        L.append("Data formats (RFC 0003 §3.3; details in *Data formats* below). Key recall and span accuracy are Rueca / cbm; bridge P / R pool every corpus repository (env reads and writes).")
        L.append("")
        L.append("| Format | Status | Key recall vs reference (R / cbm) | Span line accuracy (R / cbm) | Kinds covered (R / cbm) | Bridge P / R (Rueca) | Bridge P / R (cbm) | At parity |")
        L.append("|---|---|---|---|---|---|---|---|")
        status = man.get("data", {}).get("status", {})
        br = data.get("bridge", {})

        def pr(arm):
            a = (br.get(arm) or {}).get("all")
            return f"{_f(a['precision'])} / {_f(a['recall'])}" if a else "pending"

        for fmt, arms in sorted(data["formats"].items()):
            r, c = arms.get("rueca"), arms.get("cbm")
            kinds = sorted(k for k, v in data.get("kinds", {}).items() if any(x.get("format") == fmt for x in v.values()))
            kc = []
            for k in kinds:
                v = data["kinds"][k]
                rr = v.get("rueca", {})
                cc = v.get("cbm", {})
                kc.append(f"{k} {rr.get('with_kind_nodes', '–') if rr else '–'}/{cc.get('with_kind_nodes', '–')} of {(cc or rr).get('files')}")
            bridge_fmt = fmt == "yaml"
            if r is None:
                verdict = "pending (no Rueca results)"
            else:
                ok = (r.get("recall") or 0) >= (c.get("recall") or 0) if c else True
                if bridge_fmt and br.get("rueca") and br.get("cbm"):
                    ok = ok and (br["rueca"]["all"]["f1"] >= br["cbm"]["all"]["f1"])
                verdict = "✅" if ok else "❌"
            L.append(
                f"| {fmt} | {status.get(fmt, 'text')} | {_f((r or {}).get('recall')) if r else 'pending'} / {_f((c or {}).get('recall'))} "
                f"| {_f((r or {}).get('span_accuracy')) if r else 'pending'} / {_f((c or {}).get('span_accuracy'))} | {'; '.join(kc) or '—'} "
                f"| {pr('rueca') if bridge_fmt else '—'} | {pr('cbm') if bridge_fmt else '—'} | {verdict} |"
            )
        L.append("")


def _render_data(L: list[str], data: dict, meta: dict) -> None:
    L.append("## Data formats (RFC 0003 §3.3)")
    L.append("")
    L.append(
        "Reference keys = leaf keys of every in-scope data file from a reference parser (JSON grammar, PyYAML, tomllib, python-hcl2; HCL attributes "
        "and labelled blocks). A key is covered when the arm has a node in that file with the same name starting inside the key's span; span "
        "accuracy = covered keys whose node starts on the key's line. Bridge labels: `bench/labels/bridge/` (hand-checked). Percentages."
    )
    L.append("")
    vers = sorted({(a, r.get("arm_version")) for d in data["repos"].values() for a, r in d.items() if r.get("arm_version")})
    for a, v in vers:
        L.append(f"- Arm `{a}` on this suite: {v}.")
    if any(a == "rueca" for a, _ in vers):
        L.append(
            "- The Rueca data binary is the data branch alone: it has the data pack and the Python / TS / PHP bridge facts but not the Go and "
            "Java packs, so Go and Java env reads (most of Online Boutique's services) count as misses until the packs are merged."
        )
    L.append("")
    L.append("| Corpus | Commit | Data files (skipped) | Reference keys | Kinds | Bridge labels (reads / writes) |")
    L.append("|---|---|---:|---|---|---:|")
    for name, gi in data["gold"].items():
        if gi.get("missing"):
            L.append(f"| {name} | | gold missing | | | |")
            continue
        sk = sum(gi["skipped"].values())
        L.append(
            f"| {gi['repo']} | `{gi['commit'][:10]}` | {gi['data_files']} ({sk}: {', '.join(f'{k} {v}' for k, v in gi['skipped'].items()) or '—'}) "
            f"| {', '.join(f'{k} {v}' for k, v in gi['reference_keys'].items())} | {', '.join(gi['kinds']) or '—'} | {gi['bridge_reads']} / {gi['bridge_writes']} |"
        )
    L.append("")
    L.append("| Format | Arm | Files | Reference keys | Covered | Key recall | Span line accuracy |")
    L.append("|---|---|---:|---:|---:|---:|---:|")
    for fmt, arms in sorted(data["formats"].items()):
        for arm in ("cbm", "rueca"):
            a = arms.get(arm)
            if not a:
                L.append(f"| {fmt} | {arm} | pending | | | | |")
                continue
            L.append(f"| {fmt} | {arm} | {a['files']} | {a['keys']} | {a['covered']} | {_f(a['recall'])} | {_f(a['span_accuracy'])} |")
    L.append("")
    if data.get("kinds"):
        L.append("Kinds covered: files of each kind on which the arm emitted a kind node (`Resource`, `Image`, `EnvVar`, `Route`).")
        L.append("")
        L.append("| Kind | Format | Files | cbm | rueca |")
        L.append("|---|---|---:|---:|---:|")
        for kind, v in sorted(data["kinds"].items()):
            any_ = next(iter(v.values()))
            L.append(f"| {kind} | {any_['format']} | {any_['files']} | {v.get('cbm', {}).get('with_kind_nodes', 'pending')} | {v.get('rueca', {}).get('with_kind_nodes', 'pending')} |")
        L.append("")
    L.append("Env bridge (reads: `(file, function, variable)`; writes: `(manifest, variable, line ±1)`):")
    L.append("")
    L.append("| Corpus | Arm | Reads P | Reads R | Writes P | Writes R | All P | All R | All F1 |")
    L.append("|---|---|---:|---:|---:|---:|---:|---:|---:|")
    rows = [(n, a, r.get("bridge")) for n, d in data["repos"].items() for a, r in d.items()]
    for n, a, b in sorted(rows, key=lambda x: (x[0], ("grep", "cbm", "rueca").index(x[1]))):
        if not b:
            continue
        L.append(
            f"| {n} | {a} | {_f(b['reads']['precision'])} | {_f(b['reads']['recall'])} | {_f(b['writes']['precision'])} | {_f(b['writes']['recall'])} "
            f"| {_f(b['all']['precision'])} | {_f(b['all']['recall'])} | {_f(b['all']['f1'])} |"
        )
    for a in ("grep", "cbm", "rueca"):
        b = data["bridge"].get(a)
        if b:
            L.append(
                f"| **all** | {a} | {_f(b['reads']['precision'])} | {_f(b['reads']['recall'])} | {_f(b['writes']['precision'])} | {_f(b['writes']['recall'])} "
                f"| {_f(b['all']['precision'])} | {_f(b['all']['recall'])} | {_f(b['all']['f1'])} |"
            )
        else:
            L.append(f"| **all** | {a} | pending | | | | | | |")
    L.append("")
    for f in data.get("failures", []):
        L.append(f"- Data `{f['arm']}` {f['repo']}: {f['error'][:200]}")
    if data.get("failures"):
        L.append("")


def render(meta: dict, s1: dict, s3: dict, data: dict | None = None) -> str:
    arms = _arm_cols(meta["arms"])
    head_arms = []
    for a in arms:
        st = meta["arms"][a]
        head_arms.append(a if st["available"] or a in s1["table"] else f"{a} (pending)")
    s1_groups = [g for g in language_groups() if any(g in s1["table"].get(a, {}).get(v, {}) for a in arms for v in VARIANTS)]
    s3_groups = [g for g in language_groups() if any(g in s3["groups"].get(a, {}) for a in arms)]
    groups_s1 = s1_groups + [OVERALL]
    L: list[str] = []
    L.append("# rueca-bench — alpha results")
    L.append("")
    L.append(f"Generated by `rueca-bench report` on {meta['date']}. Do not edit by hand; see `bench/README.md` for methodology.")
    L.append("")
    L.append("## Run metadata")
    L.append("")
    m = meta["machine"]
    L.append(f"- Machine: {m['cpu']} ({m['cpus']} CPUs, {m['memory_gb']} GB), {m['platform']}; Python {m['python']}")
    for a in arms:
        st = meta["arms"][a]
        ver = st["version"] if st["available"] else f"pending — {st['reason']}"
        L.append(f"- Arm `{a}`: {ver}")
    L.append(f"- Manifest sha256: `{meta['manifest_sha256'][:16]}…`")
    L.append("- Every arm sees the same worktree and the same query string; ranked outputs are post-filtered identically (source extension of the instance language, non-test path).")
    if s1.get("overall_groups") and s1["overall_groups"] != s1_groups:
        scope = overall_scope()
        if scope is not None and set(s1_groups) - scope:
            L.append(
                f"- `overall` pools {', '.join(s1['overall_groups'])} (manifest `[bench] overall`: the alpha languages, for which every arm has results); "
                "languages added since are reported in their own columns and parity rows."
            )
        else:
            L.append(
                f"- `overall` pools the languages for which every arm has results ({', '.join(s1['overall_groups'])}); "
                "a language where an arm is still pending is reported in its own column only."
            )
    if meta.get("carried_groups"):
        cf = meta.get("carried_from") or {}
        mc = cf.get("machine") or {}
        L.append(
            f"- Carried forward from the published results of {cf.get('date', '?')} ({mc.get('cpu', '?')}): raw outputs not in this run's cache "
            f"for {', '.join(meta['carried_groups'])} (`bench/results/*.json` keeps per-instance rows; see `bench/README.md`, *Additive results*)."
        )
    L.append("")

    # ---- S1
    g = s1["gold"]
    L.append("## S1 — Localization")
    L.append("")
    L.append(
        f"{g['instances']} instances ({', '.join(f'{k}: {v}' for k, v in sorted(g['by_language'].items()))}); "
        f"function gold from {', '.join(f'{k}: {v}' for k, v in sorted(g['function_source'].items()))}; "
        f"{g['with_function_gold']} instances have non-empty function gold, {g['with_context_gold']} have ContextBench gold context. "
        f"Tree-sitter labeler vs assembled labels: mean Jaccard {_f(g['labeler_agreement_jaccard_mean'])}% over {g['labeler_agreement_n']} instances."
    )
    L.append("")
    # one table per chunk of languages: the `overall` languages with the
    # overall column first, then the others four at a time
    scope = overall_scope()
    first = [g for g in groups_s1 if g == OVERALL or scope is None or g in scope]
    rest = [g for g in groups_s1 if g not in first]
    chunks = [first] + [rest[i : i + 4] for i in range(0, len(rest), 4)]
    for variant, title in (("kw", "keyword query"), ("raw", "raw issue text")):
        L.append(f"### S1, {title} (`{variant}`) — percentages")
        L.append("")
        for ci, chunk in enumerate(chunks):
            if ci:
                L.append("")
            cols = [(a, grp) for grp in chunk for a in arms]
            L.append("| Metric | " + " | ".join(f"{grp}: {head_arms[arms.index(a)]}" for a, grp in cols) + " |")
            L.append("|---|" + "---:|" * len(cols))
            ns = []
            for a, grp in cols:
                t = s1["table"].get(a, {}).get(variant, {}).get(grp, {})
                ns.append(str(t.get("n", "—")) if t else "pending")
            L.append("| n instances | " + " | ".join(ns) + " |")
            for label, key in S1_METRICS:
                cells = []
                for grp in chunk:
                    vals = []
                    for a in arms:
                        t = s1["table"].get(a, {}).get(variant, {}).get(grp)
                        vals.append(_f(t.get(key)) if t else "pending")
                    cells += _bold_best(vals)
                L.append(f"| {label} | " + " | ".join(cells) + " |")
        L.append("")
    L.append("### S1 cost (seconds)")
    L.append("")
    L.append("| Arm | Group | n | index median | index p95 | query median (kw) | query p95 (kw) | query median (raw) | failed (kw/raw) |")
    L.append("|---|---|---:|---:|---:|---:|---:|---:|---:|")
    for a in arms:
        for grp in groups_s1:
            t = s1["table"].get(a, {}).get("kw", {}).get(grp)
            tr = s1["table"].get(a, {}).get("raw", {}).get(grp)
            if not t:
                L.append(f"| {a} | {grp} | pending | | | | | | |")
                continue
            L.append(
                f"| {a} | {grp} | {t['n']} | {_f(t['index_s_median'], False)} | {_f(t['index_s_p95'], False)} | "
                f"{_f(t['query_s_median'], False)} | {_f(t['query_s_p95'], False)} | {_f(tr['query_s_median'], False) if tr else '—'} | "
                f"{t['n_failed']}/{tr['n_failed'] if tr else '—'} |"
            )
    L.append("")
    L.append("### S1 per repository (`kw`, File Acc@5 / Func Acc@10)")
    L.append("")
    repos = sorted({k[5:] for a in s1["table"] for k in s1["table"][a].get("kw", {}) if k.startswith("repo:")})
    L.append("| Repository | n | " + " | ".join(arms) + " |")
    L.append("|---|---:|" + "---:|" * len(arms))
    for repo in repos:
        cells = []
        n = None
        for a in arms:
            t = s1["table"].get(a, {}).get("kw", {}).get(f"repo:{repo}")
            if not t:
                cells.append("pending")
                continue
            n = t["n"]
            cells.append(f"{_f(t['file_acc@5'])} / {_f(t['func_acc@10'])}")
        L.append(f"| {repo} | {n} | " + " | ".join(cells) + " |")
    L.append("")
    if "grep" in s1["table"]:
        t = s1["table"]["grep"]["kw"][OVERALL]
        if "func_acc@10_name_only" in t:
            L.append(
                f"grep function ranking uses BM25 over function bodies; the name-only BM25 variant scores Func Acc@5 "
                f"{_f(t['func_acc@5_name_only'])}% / Acc@10 {_f(t['func_acc@10_name_only'])}% (overall, `kw`)."
            )
            L.append("")

    # ---- S3
    L.append("## S3 — Structural questions (depth-1 callers / callees, symbol lookup)")
    L.append("")
    L.append("| Gold set | Language | Commit | Indexer | Callables | Gold call edges | Questions |")
    L.append("|---|---|---|---|---:|---:|---:|")
    for name, gi in s3["gold"].items():
        L.append(_gold_set_row(name, gi))
    L.append("")
    agreements = [(gi["title"], gi["phpstan_agreement"]) for gi in s3["gold"].values() if gi.get("phpstan_agreement")]
    for title, ag in agreements:
        if ag.get("available"):
            L.append(
                f"{title}: answer key vs the PHPStan extractor ({ag['tool']}) — {ag['agree']} of {ag['edges_key']} hand-labelled edges "
                f"found by PHPStan (recall {_f(ag['recall'])}%), {ag['edges_phpstan'] - ag['agree']} extra PHPStan edges "
                f"(precision {_f(ag['precision'])}%); {ag['snippets_identical']} snippets identical."
            )
        else:
            L.append(f"{title}: PHPStan cross-check not run ({ag.get('reason')}).")
        L.append("")
    for gi in s3["gold"].values():
        cc = gi.get("crosscheck")
        if not cc:
            continue
        if cc.get("available"):
            L.append(
                f"{gi['title']}: answer key vs {cc['tool']} (same edge derivation as the repository gold) — {cc['agree']} of {cc['edges_key']} "
                f"hand-labelled edges found by SCIP (recall {_f(cc['recall'])}%), {cc['edges_scip'] - cc['agree']} extra SCIP edges "
                f"(precision {_f(cc['precision'])}%); {cc['snippets_identical']} snippets identical."
            )
        else:
            L.append(f"{gi['title']}: SCIP cross-check not run ({cc.get('reason')}).")
        L.append("")
    L.append("Macro P/R/F1 average over questions whose gold set for that direction is non-empty; micro P/R pool all predicted/gold edges (so spurious edges on functions with no gold edges count against precision). Percentages.")
    L.append("")
    L.append("| Set / arm | n | " + " | ".join(S3_COLS) + " |")
    L.append("|---|---:|" + "---:|" * len(S3_COLS))
    for grp in s3_groups + [OVERALL]:
        for a in arms:
            L.append(_s3row(f"**{grp}** · {a}", s3["groups"].get(a, {}).get(grp)))
    for name in s3["gold"]:
        for a in arms:
            L.append(_s3row(f"{name} · {a}", s3["per_suite"].get(a, {}).get(name)))
    L.append("")
    if s3.get("overall_groups") is not None and s3["overall_groups"] != s3_groups:
        L.append(f"S3 `overall` pools {', '.join(s3['overall_groups'])} (languages with repository gold and results for every arm); micro-benchmarks are reported per set.")
        L.append("")

    # ---- per-language summaries
    fams = [f for f in families().values() if f.label in s1_groups or f.label in s3_groups or f.label in s3["micro_by_group"]]
    L.append("## Per language")
    L.append("")
    L.append("Key numbers per language (percentages; `kw` = keyword query). Full tables above.")
    L.append("")
    for fam in fams:
        spec = load_manifest()["languages"][fam.key]
        L.append(f"### {fam.label}")
        L.append("")
        n_inst = s1["gold"].get("by_group", {}).get(fam.label, 0)
        repos_n = {r["repo"] for r in s1["per_instance"] if r["group"] == fam.label}
        L.append(f"- S1: {n_inst} instances over {len(repos_n)} repositories; sources: {spec.get('s1_sources', 'see bench/README.md')}.")
        L.append(f"- S3 gold: {spec.get('s3_gold', 'see bench/README.md')}.")
        vers = meta.get("versions_by_group", {}).get(fam.label, {})
        if vers.get("rueca"):
            L.append(f"- Rueca binary for this language: {'; '.join(vers['rueca'])}.")
        if spec.get("notes"):
            L.append(f"- Notes: {spec['notes']}")
        L.append("")
        L.append("| Arm | S1 kw file Acc@1 | S1 kw file Acc@5 | S1 kw func Acc@10 | S1 raw file Acc@5 | S1 raw func Acc@10 | S1 index s (median) | S1 query s (median) | S3 def found | S3 callers F1 | S3 callees F1 | S3 micro callers F1 | S3 micro callees F1 |")
        L.append("|---|" + "---:|" * 12)
        for a in arms:
            kw = s1["table"].get(a, {}).get("kw", {}).get(fam.label)
            raw = s1["table"].get(a, {}).get("raw", {}).get(fam.label)
            rg = s3["groups"].get(a, {}).get(fam.label)
            mname = s3["micro_by_group"].get(fam.label)
            mg = s3["per_suite"].get(a, {}).get(mname) if mname else None
            if not kw and not rg and not mg:
                L.append(f"| {a} | pending |" + " |" * 11)
                continue
            vals = [
                _f(kw.get("file_acc@1")) if kw else "pending",
                _f(kw.get("file_acc@5")) if kw else "pending",
                _f(kw.get("func_acc@10")) if kw else "pending",
                _f(raw.get("file_acc@5")) if raw else "pending",
                _f(raw.get("func_acc@10")) if raw else "pending",
                _f(kw.get("index_s_median"), False) if kw else "pending",
                _f(kw.get("query_s_median"), False) if kw else "pending",
                _f(rg["definition_found"]) if rg and rg.get("n") else "—",
                _f(rg["callers"]["f1"]) if rg and rg.get("n") else "—",
                _f(rg["callees"]["f1"]) if rg and rg.get("n") else "—",
                _f(mg["callers"]["f1"]) if mg and mg.get("n") else "—",
                _f(mg["callees"]["f1"]) if mg and mg.get("n") else "—",
            ]
            L.append(f"| {a} | " + " | ".join(vals) + " |")
        L.append("")

    # ---- parity table per language
    L.append("## Parity table per language")
    L.append("")
    L.append(
        "Gate (README, RFC 0001 §15): a language is supported when Rueca matches or beats codebase-memory-mcp v0.12.0. "
        "Each cell is rueca / cbm / grep (percentages). Status: ✅ when Rueca ≥ cbm on every metric of the delta view below "
        "(13 metrics where data exists; count met in brackets), ❌ otherwise, *pending* without Rueca results. "
        "Samples are small, so a miss of a few points is within noise but still counts. Suite 4 (agent end-to-end) is not part of the alpha."
    )
    L.append("")
    L.append("| Language | " + " | ".join(t for t, _ in PARITY_KEYS) + " | Status |")
    L.append("|---|" + "---|" * len(PARITY_KEYS) + "---|")
    for fam in fams:
        cells = []
        rueca_any = False
        for _, get in PARITY_KEYS:
            r, c, gp = (get(s1, s3, arm, fam.label) for arm in ("rueca", "cbm", "grep"))
            if r is None and c is None and gp is None:
                cells.append("—")
                continue
            cells.append(f"{_f(r) if r is not None else 'pending'} / {_f(c)} / {_f(gp)}")
            rueca_any = rueca_any or r is not None
        met = total = 0
        for get in GATE_METRICS:
            r, c = (get(s1, s3, arm, fam.label) for arm in ("rueca", "cbm"))
            if r is not None and c is not None:
                total += 1
                met += int(r - c >= -1e-9)
        if not rueca_any:
            status = "pending (no Rueca results)"
        elif total and met == total:
            status = f"✅ ({met}/{total})"
        else:
            status = f"❌ ({met}/{total})"
        L.append(f"| {fam.label} | " + " | ".join(cells) + f" | {status} |")
    L.append("")

    # ---- parity view (deltas)
    pgroups = [g for g in language_groups() if g in s1_groups or g in s3_groups]
    L.append("## Parity view: Rueca vs codebase-memory-mcp v0.12.0")
    L.append("")
    sizes_s1 = " / ".join(f"{s1['gold'].get('by_group', {}).get(gp, 0)} {gp}" for gp in pgroups)
    sizes_s3 = " / ".join(f"{(s3['groups'].get('cbm', {}).get(gp) or {}).get('n', 0)} {gp}" for gp in pgroups)
    L.append(f"README parity contract: per language, Rueca must match or beat cbm. Δ = Rueca − cbm in percentage points; ✅ Rueca ≥ cbm, ❌ Rueca < cbm. Samples are small (S1: {sizes_s1} instances; S3 repository gold: {sizes_s3} questions), so gaps of a few points are within noise.")
    L.append("")
    L.append("| Suite | Metric | " + " | ".join(f"{gp} (rueca / cbm / Δ)" for gp in pgroups) + " |")
    L.append("|---|---|" + "---|" * len(pgroups))

    def cell(t_val, c_val) -> str:
        if t_val is None or c_val is None:
            return "pending"
        d = 100 * (t_val - c_val)
        mark = "✅" if d >= -1e-9 else "❌"
        return f"{_f(t_val)} / {_f(c_val)} / {d:+.1f} {mark}"

    parity_s1 = [("S1 kw", "kw", "file_acc@1"), ("S1 kw", "kw", "file_acc@5"), ("S1 kw", "kw", "func_acc@10"),
                 ("S1 raw", "raw", "file_acc@5"), ("S1 raw", "raw", "func_acc@10")]
    for suite, variant, key in parity_s1:
        cells = []
        for gp in pgroups:
            t = s1["table"].get("rueca", {}).get(variant, {}).get(gp, {})
            c = s1["table"].get("cbm", {}).get(variant, {}).get(gp, {})
            cells.append(cell(t.get(key), c.get(key)))
        L.append(f"| {suite} | {key} | " + " | ".join(cells) + " |")
    parity_s3 = [("definition found", ("definition_found",)), ("callers F1", ("callers", "f1")),
                 ("callees F1", ("callees", "f1")), ("callees micro R", ("callees", "micro_recall")),
                 ("lookup Acc@1", ("lookup_acc@1",))]
    for micro in (False, True):
        rows = parity_s3 if not micro else parity_s3[:3]
        for label, path in rows:
            cells = []
            for gp in pgroups:
                if micro and gp not in s3["micro_by_group"]:
                    cells.append("—")
                    continue
                vals = [_s3v(*path, micro=micro)(s1, s3, arm, gp) for arm in ("rueca", "cbm")]
                cells.append(cell(*vals))
            L.append(f"| {'S3 (micro)' if micro else 'S3 (repo gold)'} | {label} | " + " | ".join(cells) + " |")
    L.append("")
    L.append(
        "S3 repo gold = SCIP (scip-python, scip-typescript, scip-go, scip-clang) or the PHPStan extractor (PHP); "
        "S3 micro = PyCG (Python) or the rueca-bench micro-benchmarks (PHP, Go, Java, C, C++)."
    )
    L.append("")
    _render_rfc_parity(L, s1, s3, data or {})
    if data and data.get("gold"):
        _render_data(L, data, meta)

    # ---- failures
    L.append("## Failures and gaps")
    L.append("")
    if not s1["failures"] and not s3["failures"]:
        L.append("No arm failures recorded.")
    fails = Counter((f["arm"], f["variant"], str(f["error"])[:160]) for f in s1["failures"])
    for (a, v, e), n in sorted(fails.items()):
        L.append(f"- S1 `{a}` `{v}`: {n}× {e}")
    for f in s3["failures"][:40]:
        L.append(f"- S3 `{f['arm']}` {f['suite']} {f.get('snippet', '')}: {str(f.get('error'))[:200]}")
    for a in arms:
        st = meta["arms"][a]
        if not st["available"] and a not in s1["table"]:
            L.append(f"- Arm `{a}` pending: {st['reason']}.")
    for gp in language_groups():
        missing = [a for a in arms if gp in s1_groups and gp not in s1["table"].get(a, {}).get("kw", {})]
        for a in missing:
            L.append(f"- Arm `{a}` pending for {gp}.")
    L.append("")
    return "\n".join(L) + "\n"


def report() -> None:
    published = load_published()
    s1 = score_s1(published["s1"])
    s3 = score_s3(published["s3"])
    data = data_mod.score()
    carried_groups = {k.split(":", 1)[1] for k in s1.get("carried", {})}
    for k in s3.get("carried", []):
        name = k.split(":", 1)[1]
        spec = next((sp for n, sp, _ in suites() if n == name), None)
        if spec:
            carried_groups.add(lang_group(spec["language"]))
    arms = arm_status()
    pub_arms = published["meta"].get("arms", {})
    for a, st in arms.items():
        if carried_groups and pub_arms.get(a):
            vers = sorted(set(st["recorded_versions"]) | set(pub_arms[a].get("recorded_versions", [])))
            st.update(recorded_versions=vers, version="; ".join(vers) if vers else st["version"], available=st["available"] or bool(vers))
    meta = {
        "date": dt.datetime.now(dt.timezone.utc).strftime("%Y-%m-%d %H:%M UTC"),
        "machine": machine_info(),
        "arms": arms,
        "versions_by_group": versions_by_group(published["meta"], carried_groups),
        "manifest_sha256": sha256_file(MANIFEST_PATH),
        "manifest_tools": load_manifest()["tools"],
        "carried_groups": sorted(carried_groups),
        "carried_from": {k: published["meta"].get(k) for k in ("date", "machine")} if carried_groups else None,
    }
    RESULTS_DIR.mkdir(parents=True, exist_ok=True)
    write_json(RESULTS_DIR / "meta.json", meta)
    write_json(RESULTS_DIR / "s1.json", s1)
    write_json(RESULTS_DIR / "s3.json", s3)
    write_json(RESULTS_DIR / "data.json", data)
    DOCS_DIR.mkdir(parents=True, exist_ok=True)
    (DOCS_DIR / "alpha-results.md").write_text(render(meta, s1, s3, data))
    log(f"wrote {RESULTS_DIR}/{{meta,s1,s3,data}}.json and {DOCS_DIR / 'alpha-results.md'}")
