"""`rueca-bench report`: score cached arm outputs against gold.

Writes bench/results/{meta,s1,s3}.json and docs/bench/alpha-results.md.
Failures count as misses (zero scores) and are listed, never dropped.
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
from .metrics import mean, median, p95, prf, score_files, score_functions
from .s1 import instances as s1_instances
from .s3 import MICRO_SUITES, gold_path, prediction_in_scope, suites

VARIANTS = ("kw", "raw")
OVERALL = "overall"
CTX_KS = (5, 10)


def language_groups() -> list[str]:
    """Report columns, one per language family, in manifest order."""
    return [f.label for f in families().values()]


def common_groups(rows_by_arm: dict[str, set[str]]) -> list[str]:
    """Groups in which every arm that produced any result has results: the
    `overall` column pools only these, so an arm that is still pending for a
    language does not make the overall numbers incomparable."""
    present = [g for g in rows_by_arm.values() if g]
    if not present:
        return []
    shared = set.intersection(*present)
    return [g for g in language_groups() if g in shared]


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
    for suite in ("s1", "s3"):
        for p in sorted((RUNS_DIR / suite / name).glob("*.json")):
            v = read_json(p).get("arm_version")
            if v:
                vers.add(v)
    return sorted(vers)


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


def score_s1() -> dict:
    insts = s1_instances()
    per: list[dict] = []
    for inst in insts:
        gp = GOLD_DIR / "s1" / f"{safe_name(inst['id'])}.json"
        if not gp.exists():
            continue
        gold = read_json(gp)
        for arm in ARM_NAMES:
            rp = RUNS_DIR / "s1" / arm / f"{safe_name(inst['id'])}.json"
            if not rp.exists():
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

    golds = [read_json(GOLD_DIR / "s1" / f"{safe_name(i['id'])}.json") for i in insts if (GOLD_DIR / "s1" / f"{safe_name(i['id'])}.json").exists()]
    agree = [g["labeler_agreement"]["jaccard"] for g in golds if "labeler_agreement" in g]
    gold_stats = {
        "instances": len(insts),
        "by_language": dict(Counter(i["language"] for i in insts)),
        "by_group": dict(Counter(lang_group(i["language"]) for i in insts)),
        "by_repo": dict(Counter(i["repo"] for i in insts)),
        "by_dataset": dict(Counter(i["dataset"] for i in insts)),
        "function_source": dict(Counter(g["function_source"] for g in golds)),
        "with_function_gold": sum(1 for g in golds if g["functions"]),
        "with_context_gold": sum(1 for g in golds if g.get("context_files")),
        "labeler_agreement_jaccard_mean": mean(agree),
        "labeler_agreement_n": len(agree),
        "assembled_unmapped": sum(len(g.get("assembled_unmapped", [])) for g in golds),
        "assembled_labels": sum(g.get("assembled_labels", 0) for g in golds),
        "mean_gold_files": mean(len(g["files"]) for g in golds),
        "mean_gold_functions": mean(len(g["functions"]) for g in golds if g["functions"]),
    }
    failures = [
        {k: r[k] for k in ("arm", "variant", "id", "error")} for r in per if not r["ok"]
    ]
    return {"table": table, "per_instance": per, "gold": gold_stats, "failures": failures, "overall_groups": overall_groups}


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


def score_s3() -> dict:
    per_suite: dict = {}
    pooled: dict = defaultdict(lambda: defaultdict(list))
    gold_info: dict = {}
    failures: list[dict] = []
    micro_by_group: dict[str, str] = {}
    for name, spec, micro in suites():
        gp = gold_path(name)
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
                res = _agg_s3(rows)
                res["index_seconds"] = rec.get("index_seconds")
                res["status"] = rec.get("status")
                pooled[arm][lang_group(gold["language"])].extend(rows)
            per_suite.setdefault(arm, {})[name] = res
    overall_groups = common_groups({arm: set(d) for arm, d in pooled.items()})
    groups = {}
    for arm, d in pooled.items():
        groups[arm] = {g: _agg_s3(rows) for g, rows in d.items()}
        groups[arm][OVERALL] = _agg_s3([r for g in overall_groups for r in d.get(g, [])])
    return {
        "per_suite": per_suite,
        "groups": groups,
        "gold": gold_info,
        "failures": failures,
        "overall_groups": overall_groups,
        "micro_by_group": micro_by_group,
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


PARITY_KEYS = [
    ("S1 kw file Acc@5", _s1v("kw", "file_acc@5")),
    ("S1 kw func Acc@10", _s1v("kw", "func_acc@10")),
    ("S1 raw file Acc@5", _s1v("raw", "file_acc@5")),
    ("S3 definition found", _s3v("definition_found")),
    ("S3 callers F1", _s3v("callers", "f1")),
    ("S3 callees F1", _s3v("callees", "f1")),
    ("S3 micro callees F1", _s3v("callees", "f1", micro=True)),
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


def render(meta: dict, s1: dict, s3: dict) -> str:
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
        L.append(
            f"- `overall` pools the languages for which every arm has results ({', '.join(s1['overall_groups'])}); "
            "a language where an arm is still pending is reported in its own column only."
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
    for variant, title in (("kw", "keyword query"), ("raw", "raw issue text")):
        L.append(f"### S1, {title} (`{variant}`) — percentages")
        L.append("")
        cols = [(a, grp) for grp in groups_s1 for a in arms]
        L.append("| Metric | " + " | ".join(f"{grp}: {head_arms[arms.index(a)]}" for a, grp in cols) + " |")
        L.append("|---|" + "---:|" * len(cols))
        ns = []
        for a, grp in cols:
            t = s1["table"].get(a, {}).get(variant, {}).get(grp, {})
            ns.append(str(t.get("n", "—")) if t else "pending")
        L.append("| n instances | " + " | ".join(ns) + " |")
        for label, key in S1_METRICS:
            cells = []
            for grp in groups_s1:
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
        "Each cell is rueca / cbm / grep (percentages); status ✅ when Rueca ≥ cbm on every key metric with data, ❌ otherwise "
        "(count of metrics met in brackets), *pending* without Rueca results. Suite 4 (agent end-to-end) is not part of the alpha."
    )
    L.append("")
    L.append("| Language | " + " | ".join(t for t, _ in PARITY_KEYS) + " | Status |")
    L.append("|---|" + "---|" * len(PARITY_KEYS) + "---|")
    for fam in fams:
        cells = []
        met = total = 0
        rueca_any = False
        for _, get in PARITY_KEYS:
            r, c, gp = (get(s1, s3, arm, fam.label) for arm in ("rueca", "cbm", "grep"))
            if r is None and c is None and gp is None:
                cells.append("—")
                continue
            cells.append(f"{_f(r) if r is not None else 'pending'} / {_f(c)} / {_f(gp)}")
            if r is not None:
                rueca_any = True
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
    L.append("S3 repo gold = SCIP (Python, TS/JS) or the PHPStan extractor (PHP); S3 micro = PyCG (Python) or the PHP micro-benchmark.")
    L.append("")

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
    meta = {
        "date": dt.datetime.now(dt.timezone.utc).strftime("%Y-%m-%d %H:%M UTC"),
        "machine": machine_info(),
        "arms": arm_status(),
        "manifest_sha256": sha256_file(MANIFEST_PATH),
        "manifest_tools": load_manifest()["tools"],
    }
    s1 = score_s1()
    s3 = score_s3()
    RESULTS_DIR.mkdir(parents=True, exist_ok=True)
    write_json(RESULTS_DIR / "meta.json", meta)
    write_json(RESULTS_DIR / "s1.json", s1)
    write_json(RESULTS_DIR / "s3.json", s3)
    DOCS_DIR.mkdir(parents=True, exist_ok=True)
    (DOCS_DIR / "alpha-results.md").write_text(render(meta, s1, s3))
    log(f"wrote {RESULTS_DIR}/{{meta,s1,s3}}.json and {DOCS_DIR / 'alpha-results.md'}")
