"""`rueca-bench report`: score cached arm outputs against gold.

Writes bench/results/{meta,s1,s3,data,health,scorecard}.json and
docs/bench/alpha-results.md, which opens with the per-language scorecard of
RFC 0004 §9 (floors and targets from manifest `[scorecard]`, 95% intervals,
statuses `stable` / `preview`). Failures count as misses (zero scores) and
are listed, never dropped.

Results are additive across checkouts: an (arm, instance) or (arm, S3 set)
whose raw output is not in this cache keeps its published entry from
bench/results/*.json (S1 per-instance rows, S3 per-question rows, data-suite
entries), so a Rueca-only run (the CI regression check) or a checkout that
adds a language never re-runs — or silently drops — the others. The report
says which entries were carried.
"""

from __future__ import annotations

import datetime as dt
import platform
import statistics
import subprocess
import sys
from collections import Counter, defaultdict

from . import data as data_mod
from . import health as health_mod
from .arms import ARM_NAMES, make_arm
from .arms.base import is_callable_label, line_match
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
    scorecard_spec,
    sha256_file,
    split_of,
    write_json,
)
from .metrics import mean, median, p95, score_files, score_functions
from .s1 import instances as s1_instances
from .s3 import gold_path, prediction_in_scope, suites
from .scip import gold_neighbours, questions
from .stats import bootstrap_mean, bootstrap_pooled_f1, bootstrap_ratio, paired_bootstrap, quantile_ci, seeded, wilson

VARIANTS = ("kw", "raw")
OVERALL = "overall"
CTX_KS = (5, 10)
S3_SCHEMA = 3
DIRS = ("callers", "callees")


def _sc() -> dict:
    sc = scorecard_spec()
    return {
        "resamples": int(sc.get("bootstrap_resamples", 10000)),
        "seed": int(sc.get("bootstrap_seed", 20261025)),
        "cut": float(sc.get("min_confidence", 0.6)),
        "min_s3": int(sc.get("min_structural_questions", 300)),
        "min_s1": int(sc.get("min_localization_instances", 30)),
        "variant": sc.get("localization_variant", "kw"),
        "metrics": list(sc.get("metrics", [])),
        "config_metrics": list(sc.get("config_metrics", [])),
    }


def _boot(values, *seed_parts) -> list | None:
    sc = _sc()
    ci = bootstrap_mean(list(values), sc["resamples"], seeded(sc["seed"], *seed_parts))
    return list(ci) if ci else None


def _wil(k: float, n: int) -> list | None:
    ci = wilson(k, n)
    return list(ci) if ci else None


def language_groups() -> list[str]:
    """Report columns, one per language family, in manifest order."""
    return [f.label for f in families().values()]


def overall_scope() -> set[str] | None:
    keys = load_manifest().get("bench", {}).get("overall")
    if keys is None:
        return None
    return {families()[k].label for k in keys if k in families()}


def common_groups(rows_by_arm: dict[str, set[str]]) -> list[str]:
    """Groups in which every arm that produced any result has results: the
    `overall` column pools only these (restricted to `[bench] overall`)."""
    present = [g for g in rows_by_arm.values() if g]
    if not present:
        return []
    shared = set.intersection(*present)
    scope = overall_scope()
    return [g for g in language_groups() if g in shared and (scope is None or g in scope)]


# --- published results (carry-forward) ---------------------------------------------


def load_published() -> dict:
    out = {}
    for name in ("meta", "s1", "s3", "data"):
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
    for suite in ("s1", "s3", "data", "health"):
        for p in sorted((RUNS_DIR / suite / name).glob("*.json")):
            v = read_json(p).get("arm_version")
            if v:
                vers.add(v)
    return sorted(vers)


def versions_by_group(published: dict | None = None, carried: set[tuple[str, str]] | None = None) -> dict:
    """{group: {arm: [versions]}} of the outputs behind each language's
    numbers: local runs, plus the published versions of (arm, group) pairs
    whose results were carried."""
    out: dict = defaultdict(lambda: defaultdict(set))
    for g, d in ((published or {}).get("versions_by_group") or {}).items():
        for a, vs in d.items():
            if carried and (a, g) in carried:
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


def s1_splits() -> dict[str, str]:
    return {r["repo"]: split_of(r) for r in load_manifest()["s1"]["repos"]}


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
    return {
        "function_source": row.get("function_source"),
        "n_files": row.get("n_gold_files", 0),
        "n_functions": row.get("n_gold_functions", 0),
        "has_context": bool(row.get("context")),
        "jaccard": None,
        "assembled_unmapped": 0,
        "assembled_labels": 0,
    }


# (key in the aggregate, row getter, kind): kind "p" = 0/1 per instance (Wilson), "m" = mean (bootstrap)
S1_AGG = [
    ("file_acc@1", lambda r: r["file"]["acc@1"], "p", "file"),
    ("file_acc@3", lambda r: r["file"]["acc@3"], "p", "file"),
    ("file_acc@5", lambda r: r["file"]["acc@5"], "p", "file"),
    ("file_recall@5", lambda r: r["file"]["recall@5"], "m", "file"),
    ("file_recall@10", lambda r: r["file"]["recall@10"], "m", "file"),
    ("file_mrr", lambda r: r["file"]["mrr"], "m", "file"),
    ("func_acc@5", lambda r: r["function"]["acc@5"], "p", "func"),
    ("func_acc@10", lambda r: r["function"]["acc@10"], "p", "func"),
    ("func_recall@5", lambda r: r["function"]["recall@5"], "m", "func"),
    ("func_recall@10", lambda r: r["function"]["recall@10"], "m", "func"),
    ("func_mrr", lambda r: r["function"]["mrr"], "m", "func"),
    ("ctx_recall@5", lambda r: r["context"]["ctx_recall@5"], "m", "ctx"),
    ("ctx_recall@10", lambda r: r["context"]["ctx_recall@10"], "m", "ctx"),
]


def agg_s1(rows: list[dict], label: str) -> dict:
    if not rows:
        return {"n": 0}
    pops = {"file": rows, "func": [r for r in rows if r["function"] is not None], "ctx": [r for r in rows if r["context"]]}
    out: dict = {"n": len(rows), "n_failed": sum(1 for r in rows if not r["ok"]), "n_func": len(pops["func"]), "n_ctx": len(pops["ctx"]), "ci": {}}
    for key, get, kind, pop in S1_AGG:
        vals = [get(r) for r in pops[pop]]
        out[key] = mean(vals)
        if vals:
            out["ci"][key] = _wil(sum(vals), len(vals)) if kind == "p" else _boot(vals, "s1", label, key)
    out.update(
        index_s_median=median(r["index_seconds"] for r in rows),
        index_s_p95=p95(r["index_seconds"] for r in rows),
        query_s_median=median(r["query_seconds"] for r in rows),
        query_s_p95=p95(r["query_seconds"] for r in rows),
    )
    nr = [r for r in rows if r.get("function_name_only")]
    if nr:
        out["func_acc@5_name_only"] = mean(r["function_name_only"]["acc@5"] for r in nr)
        out["func_acc@10_name_only"] = mean(r["function_name_only"]["acc@10"] for r in nr)
    return out


def score_s1(published: dict | None = None) -> dict:
    insts = s1_instances()
    splits = s1_splits()
    published = published or {}
    pub_rows = {(r["arm"], r["variant"], r["id"]): r for r in published.get("per_instance", [])}
    pub_gold = published.get("gold_per_instance", {})
    per: list[dict] = []
    summaries: dict[str, dict] = {}
    carried: Counter = Counter()
    for inst in insts:
        split = splits.get(inst["repo"], "gated")
        gp = GOLD_DIR / "s1" / f"{safe_name(inst['id'])}.json"
        gold = read_json(gp) if gp.exists() else None
        if gold is not None:
            summaries[inst["id"]] = _gold_summary(gold)
        for arm in ARM_NAMES:
            rp = RUNS_DIR / "s1" / arm / f"{safe_name(inst['id'])}.json"
            if not rp.exists() or gold is None:
                old = [pub_rows.get((arm, v, inst["id"])) for v in VARIANTS]
                old = [dict(r, group=lang_group(inst["language"]), split=split) for r in old if r]
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
                    "split": split,
                    "ok": bool(ok),
                    "error": None if ok else (q.get("error") or rec.get("error") or rec.get("status")),
                    "index_seconds": rec.get("index_seconds"),
                    "query_seconds": q.get("seconds"),
                    "tokens": q.get("tokens"),
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

    gated = [r for r in per if r["split"] == "gated"]
    overall_groups = common_groups({arm: {r["group"] for r in gated if r["arm"] == arm} for arm in ARM_NAMES})
    table: dict = {}
    for arm in ARM_NAMES:
        for variant in VARIANTS:
            rows = [r for r in per if r["arm"] == arm and r["variant"] == variant]
            if not rows:
                continue
            g_rows = [r for r in rows if r["split"] == "gated"]
            t = table.setdefault(arm, {}).setdefault(variant, {})
            for g in language_groups():
                gr = [r for r in g_rows if r["group"] == g]
                if gr:
                    t[g] = agg_s1(gr, f"{arm}/{variant}/{g}")
                cr = [r for r in rows if r["group"] == g and r["split"] == "calibration"]
                if cr:
                    t[f"calibration:{g}"] = agg_s1(cr, f"{arm}/{variant}/cal/{g}")
            t[OVERALL] = agg_s1([r for r in g_rows if r["group"] in overall_groups], f"{arm}/{variant}/overall")
            for repo in sorted({r["repo"] for r in rows}):
                t[f"repo:{repo}"] = agg_s1([r for r in rows if r["repo"] == repo], f"{arm}/{variant}/{repo}")

    def stats(sel: list[dict], ginsts: list[dict]) -> dict:
        ag = [g["jaccard"] for g in sel if g.get("jaccard") is not None]
        return {
            "instances": len(ginsts),
            "by_language": dict(Counter(i["language"] for i in ginsts)),
            "by_group": dict(Counter(lang_group(i["language"]) for i in ginsts)),
            "by_repo": dict(Counter(i["repo"] for i in ginsts)),
            "by_dataset": dict(Counter(i["dataset"] for i in ginsts)),
            "by_split": dict(Counter(splits.get(i["repo"], "gated") for i in ginsts)),
            "function_source": dict(Counter(g["function_source"] for g in sel)),
            "with_function_gold": sum(1 for g in sel if g["n_functions"]),
            "with_context_gold": sum(1 for g in sel if g["has_context"]),
            "labeler_agreement_jaccard_mean": mean(ag),
            "labeler_agreement_n": len(ag),
            "mean_gold_files": mean(g["n_files"] for g in sel),
            "mean_gold_functions": mean(g["n_functions"] for g in sel if g["n_functions"]),
        }

    gold_stats = stats([summaries[i["id"]] for i in insts if i["id"] in summaries], insts)
    gold_stats["per_group"] = {}
    for grp in language_groups():
        gi = [i for i in insts if lang_group(i["language"]) == grp and splits.get(i["repo"], "gated") == "gated"]
        if gi:
            gold_stats["per_group"][grp] = stats([summaries[i["id"]] for i in gi if i["id"] in summaries], gi)
    failures = [{k: r[k] for k in ("arm", "variant", "id", "error")} for r in per if not r["ok"]]
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
#
# One row per question (compact, committed in bench/results/s3.json):
#   f   question (gold callable id)          ok  definition found (0/1)
#   cr / ce        callers / callees at every confidence: [n_pred, matched_pred, n_gold, matched_gold]
#   cr@ / ce@      the same with edges of confidence < min_confidence dropped
#   lk  lookup rank (0 = not in the top 10; absent = not in the lookup sample)
#   s   seconds of every tool call;  t  tokens of the neighbours answer;  x  out-of-universe predictions dropped


def _universe(graph: dict) -> dict[tuple, list[dict]]:
    idx: dict[tuple, list[dict]] = defaultdict(list)
    for f in graph["callables"].values():
        idx[(f["path"], f["name"])].append(f)
    return idx


def _in_universe(h: dict, idx: dict) -> bool:
    return any(line_match(h.get("start_line"), f) for f in idx.get((h.get("path"), h.get("name")), []))


def _count(preds: list[dict], gold: list[dict]) -> list[int]:
    uniq = {(h.get("path"), h.get("name"), h.get("start_line")): h for h in preds}
    ps = list(uniq.values())
    mp = sum(1 for h in ps if any(h.get("path") == g["path"] and h.get("name") == g["name"] and line_match(h.get("start_line"), g) for g in gold))
    mg = sum(1 for g in gold if any(h.get("path") == g["path"] and h.get("name") == g["name"] and line_match(h.get("start_line"), g) for h in ps))
    return [len(ps), mp, len(gold), mg]


def score_answers(answers: list[dict], graph: dict, meta: dict, cut: float) -> list[dict]:
    idx = _universe(graph)
    rows = []
    for a in answers:
        row: dict = {"f": a["fid"], "ok": int(bool((a.get("resolve") or {}).get("ok"))), "s": [], "x": 0}
        if (a.get("resolve") or {}).get("seconds") is not None:
            row["s"].append(round(a["resolve"]["seconds"], 4))
        nb = a.get("neighbors") or {}
        if nb.get("seconds") is not None:
            row["s"].append(round(nb["seconds"], 4))
        if nb.get("tokens") is not None:
            row["t"] = nb["tokens"]
        for d, short in (("callers", "cr"), ("callees", "ce")):
            gold = [graph["callables"][x] for x in gold_neighbours(graph, a["fid"], d)]
            hits = nb.get(d, []) if nb.get("ok") else []
            scoped = [h for h in hits if prediction_in_scope(h.get("path"), meta) and is_callable_label(h.get("label"))]
            preds = [h for h in scoped if _in_universe(h, idx)]
            row["x"] += len(scoped) - len(preds)
            row[short] = _count(preds, gold)
            row[short + "@"] = _count([h for h in preds if h.get("confidence") is None or h["confidence"] >= cut], gold)
        if "lookup" in a:
            lk = a["lookup"]
            hits = lk.get("hits", []) if lk.get("ok") else []
            hits = [h for h in hits if prediction_in_scope(h.get("path"), meta) and is_callable_label(h.get("label"))]
            f = graph["callables"][a["fid"]]
            rank = next((i + 1 for i, h in enumerate(hits) if h.get("path") == f["path"] and h.get("name") == f["name"] and line_match(h.get("start_line"), f)), 0)
            row["lk"] = rank
            if lk.get("seconds") is not None:
                row["s"].append(round(lk["seconds"], 4))
            if lk.get("tokens") is not None:
                row["lt"] = lk["tokens"]
        rows.append(row)
    return rows


def agg_s3(rows: list[dict], label: str, micro: bool = False) -> dict:
    """Macro precision / recall over the questions of each direction (gold
    neighbours ≥ 1; precision over those with ≥ 1 prediction), with bootstrap
    intervals; definition found and lookup with Wilson intervals; pooled
    (micro) P / R; for micro-benchmarks also the pooled callee F1 over every
    callable (spurious edges on callables without gold count)."""
    if not rows:
        return {"n": 0}
    n = len(rows)
    found = sum(r["ok"] for r in rows)
    out: dict = {"n": n, "definition_found": found / n, "ci": {"definition_found": _wil(found, n)}, "out_of_universe": sum(r.get("x", 0) for r in rows)}
    for d, short in (("callers", "cr"), ("callees", "ce")):
        for suffix in ("", "@"):
            key = d + ("@conf" if suffix else "")
            qs = [r[short + suffix] for r in rows if r[short + suffix][2] > 0]
            prec = [mp / np_ for np_, mp, _, _ in qs if np_ > 0]
            rec = [mg / ng for _, _, ng, mg in qs]
            f1s = []
            for np_, mp, ng, mg in qs:
                p = mp / np_ if np_ else 0.0
                r_ = mg / ng
                f1s.append(2 * p * r_ / (p + r_) if p + r_ else 0.0)
            all_ = [r[short + suffix] for r in rows]
            mp_s = sum(x[1] for x in all_)
            np_s = sum(x[0] for x in all_)
            mg_s = sum(x[3] for x in all_)
            ng_s = sum(x[2] for x in all_)
            mpp = mp_s / np_s if np_s else None
            mrr = mg_s / ng_s if ng_s else None
            o = {
                "n": len(qs),
                "n_precision": len(prec),
                "precision": mean(prec),
                "recall": mean(rec),
                "f1": mean(f1s),
                "micro_precision": mpp,
                "micro_recall": mrr,
                "micro_f1": (2 * mpp * mrr / (mpp + mrr)) if mpp and mrr else 0.0,
                "gold_edges": ng_s,
                "predicted_edges": np_s,
                "ci": {
                    "precision": _boot(prec, label, key, "p") if prec else None,
                    "recall": _boot(rec, label, key, "r") if rec else None,
                },
            }
            if micro:
                sc = _sc()
                ci = bootstrap_pooled_f1([x[1] for x in all_], [x[0] for x in all_], [x[3] for x in all_], [x[2] for x in all_], sc["resamples"], seeded(sc["seed"], label, key, "f1"))
                o["ci"]["micro_f1"] = list(ci) if ci else None
                for k_, num, den in (("micro_precision", 1, 0), ("micro_recall", 3, 2)):
                    ci = bootstrap_ratio([x[num] for x in all_], [x[den] for x in all_], sc["resamples"], seeded(sc["seed"], label, key, k_))
                    o["ci"][k_] = list(ci) if ci else None
            out[key] = o
    lk = [r for r in rows if "lk" in r]
    if lk:
        a1 = sum(1 for r in lk if r["lk"] == 1)
        a10 = sum(1 for r in lk if 1 <= r["lk"] <= 10)
        out.update({"lookup_n": len(lk), "lookup_acc@1": a1 / len(lk), "lookup_acc@10": a10 / len(lk)})
        out["ci"].update({"lookup_acc@1": _wil(a1, len(lk)), "lookup_acc@10": _wil(a10, len(lk))})
    secs = [s for r in rows for s in r["s"] if s is not None]
    out["query_s_median"] = median(secs)
    out["query_s_p95"] = p95(secs)
    toks = [r["t"] for r in rows if r.get("t") is not None]
    out["tokens_median"] = median(toks)
    out["tokens_mean"] = mean(toks)
    return out


def _gold_info(name: str, spec: dict, micro: bool, gold: dict) -> dict:
    if micro:
        gi = {
            "micro": True,
            "title": spec.get("title", name),
            "language": spec["language"],
            "split": split_of(spec),
            "snippets": len(gold["snippets"]),
            "functions": sum(len(g["sample"]) for g in gold["snippets"].values()),
            "edges": sum(len(v) for g in gold["snippets"].values() for v in g["callees"].values()),
            "commit": gold.get("commit"),
            "source": gold.get("source"),
        }
        for k in ("phpstan_agreement", "crosscheck"):
            if k in gold:
                gi[k] = {kk: v for kk, v in gold[k].items() if kk != "per_snippet"}
        return gi
    qs = questions(gold, load_manifest()["s3"]["min_lines"])
    return {
        "repo": gold["repo"],
        "language": gold["language"],
        "split": split_of(spec),
        "commit": gold["commit"],
        "tag": spec.get("tag"),
        "license": spec.get("license"),
        "indexer": gold["indexer"],
        "method": gold.get("suite", "scip"),
        "callables": len(gold["callables"]),
        "edges": gold["stats"].get("edges"),
        "questions_callers": len(qs["callers"]),
        "questions_callees": len(qs["callees"]),
        "questions": len(qs["all"]),
        "lookup_sample": len(gold["sample"]),
        "stats": gold["stats"],
    }


def score_s3(published: dict | None = None) -> dict:
    published = published or {}
    same_schema = published.get("schema") == S3_SCHEMA
    pub_rows = published.get("rows", {}) if same_schema else {}
    pub_gold = published.get("gold", {}) if same_schema else {}
    cut = _sc()["cut"]
    per_suite: dict = defaultdict(dict)
    rows_out: dict = defaultdict(dict)
    carried: list[str] = []
    gold_info: dict = {}
    failures: list[dict] = []
    for name, spec, micro in suites():
        gp = gold_path(name)
        gold = read_json(gp) if gp.exists() else None
        if gold is not None:
            gold_info[name] = _gold_info(name, spec, micro, gold)
        elif name in pub_gold:
            gold_info[name] = dict(pub_gold[name], carried=True)
        else:
            gold_info[name] = {"missing": True, "language": spec["language"], "repo": spec.get("repo"), "micro": micro, "title": spec.get("title", name), "split": split_of(spec)}
        for arm in ARM_NAMES:
            rp = RUNS_DIR / "s3" / arm / f"{safe_name(name)}.json"
            if gold is None or not rp.exists():
                rows = pub_rows.get(arm, {}).get(name)
                if rows is not None and (gold is None or pub_gold.get(name, {}).get("commit") == gold.get("commit")):
                    rows_out[arm][name] = rows
                    carried.append(f"{arm}:{name}")
                continue
            rec = read_json(rp)
            meta = {"suite": gold["suite"], "language": gold.get("language", spec["language"]), "include": gold.get("include"), "exclude": gold.get("exclude", [])}
            if micro:
                rows = []
                for sname, g in sorted(gold["snippets"].items()):
                    if not g["sample"]:
                        continue
                    srec = rec.get("snippets", {}).get(sname)
                    if srec is None or srec.get("status") != "ok":
                        failures.append({"arm": arm, "suite": name, "snippet": sname, "error": (srec or {}).get("error", "missing")})
                        answers = [{"fid": q, "resolve": {"ok": False}} for q in g["sample"]]
                    else:
                        answers = srec["answers"]
                    for r in score_answers(answers, g, meta, cut):
                        r["f"] = f"{sname}/{r['f']}"
                        rows.append(r)
            else:
                qs = questions(gold, load_manifest()["s3"]["min_lines"])["all"]
                if rec.get("status") != "ok":
                    failures.append({"arm": arm, "suite": name, "error": str(rec.get("error"))[:300]})
                    answers = [{"fid": q, "resolve": {"ok": False}} for q in qs]
                else:
                    answers = rec["answers"]
                rows = score_answers(answers, gold, meta, cut)
            rows_out[arm][name] = rows
    for arm, sets in rows_out.items():
        for name, rows in sets.items():
            micro = bool(gold_info.get(name, {}).get("micro"))
            per_suite[arm][name] = agg_s3(rows, f"{arm}/{name}", micro)
    # pooled per language: gated repository sets; calibration and diagnostic sets separately
    groups: dict = defaultdict(dict)
    split_groups: dict = defaultdict(dict)
    micro_by_group: dict[str, list[str]] = defaultdict(list)
    for name, spec, micro in suites():
        if micro and split_of(spec) == "gated":
            micro_by_group[lang_group(spec["language"])].append(name)
    for arm, sets in rows_out.items():
        pools: dict = defaultdict(list)
        mpools: dict = defaultdict(list)
        cal: dict = defaultdict(list)
        for name, spec, micro in suites():
            if name not in sets:
                continue
            g = lang_group(spec["language"])
            sp = split_of(spec)
            if micro and sp == "gated":
                mpools[g].extend(sets[name])
            elif not micro and sp == "gated":
                pools[g].extend(sets[name])
            elif not micro and sp == "calibration":
                cal[g].extend(sets[name])
        for g, rows in pools.items():
            groups[arm][g] = agg_s3(rows, f"{arm}/pool/{g}")
        for g, rows in mpools.items():
            groups[arm][f"micro:{g}"] = agg_s3(rows, f"{arm}/micro/{g}", micro=True)
        for g, rows in cal.items():
            split_groups[arm][f"calibration:{g}"] = agg_s3(rows, f"{arm}/cal/{g}")
    return {
        "schema": S3_SCHEMA,
        "per_suite": {a: dict(v) for a, v in per_suite.items()},
        "groups": {a: dict(v) for a, v in groups.items()},
        "calibration": {a: dict(v) for a, v in split_groups.items()},
        "gold": gold_info,
        "failures": failures,
        "micro_by_group": dict(micro_by_group),
        "carried": carried,
        "rows": {a: dict(v) for a, v in rows_out.items()},
    }


# --- scorecard (RFC 0004 §6, §9) -----------------------------------------------------


def _paired_rows(s1: dict, arm_a: str, arm_b: str, group: str, variant: str, key: str) -> tuple[list[float], list[float]]:
    by: dict = defaultdict(dict)
    for r in s1["per_instance"]:
        if r["variant"] != variant or r["group"] != group or r["split"] != "gated":
            continue
        if key == "func" and r["function"] is None:
            continue
        by[r["id"]][r["arm"]] = r
    a, b = [], []
    for iid in sorted(by):
        ra, rb = by[iid].get(arm_a), by[iid].get(arm_b)
        if ra is None or rb is None:
            continue
        if key == "file":
            a.append(ra["file"]["acc@5"])
            b.append(rb["file"]["acc@5"])
        else:
            a.append(ra["function"]["acc@10"])
            b.append(rb["function"]["acc@10"])
    return a, b


def _latencies(s1: dict, s3: dict, arm: str, group: str) -> list[float]:
    out = [r["query_seconds"] for r in s1["per_instance"] if r["arm"] == arm and r["group"] == group and r["split"] == "gated" and r.get("query_seconds") is not None]
    for name, spec, micro in suites():
        if micro or split_of(spec) != "gated" or lang_group(spec["language"]) != group:
            continue
        for r in s3["rows"].get(arm, {}).get(name, []):
            out += [x for x in r["s"] if x is not None]
    return out


def _paired_tokens(s3: dict, group: str, field: str) -> tuple[list[float], list[float]]:
    a, b = [], []
    for name, spec, micro in suites():
        if micro or split_of(spec) != "gated" or lang_group(spec["language"]) != group:
            continue
        rr = {r["f"]: r for r in s3["rows"].get("rueca", {}).get(name, [])}
        cc = {r["f"]: r for r in s3["rows"].get("cbm", {}).get(name, [])}
        for f in sorted(set(rr) & set(cc)):
            if rr[f].get(field) is not None and cc[f].get(field) is not None:
                a.append(rr[f][field])
                b.append(cc[f][field])
    return a, b


def _judge(spec: dict, value, ci, n, min_n: int | None, paired: dict | None = None) -> str:
    floor = spec.get("floor")
    if floor is None:
        return "—"
    if value is None:
        return "pending"
    if min_n and (n or 0) < min_n:
        return "❌"
    if floor == "bm25":
        if not paired or not paired.get("ci"):
            return "pending"
        lo, hi = paired["ci"]
        return "❌" if hi < 0 else ("⚠" if lo < 0 else "✅")
    lower = spec.get("better") == "lower"
    met = value <= floor if lower else value >= floor
    if not met:
        return "❌"
    if ci and None not in ci and ((ci[1] > floor) if lower else (ci[0] < floor)):
        return "⚠"
    return "✅"


def _metric_values(key: str, fam_label: str, s1: dict, s3: dict, health: dict) -> dict:
    """value, ci, n, bm25, cbm (and paired difference) of one scorecard metric."""
    sc = _sc()
    v = sc["variant"]
    t = lambda arm: s1["table"].get(arm, {}).get(v, {}).get(fam_label) or {}
    g3 = lambda arm: s3["groups"].get(arm, {}).get(fam_label) or {}
    out: dict = {}
    if key in ("file_acc@5", "func_acc@10"):
        r = t("rueca")
        out = {"value": r.get(key), "ci": (r.get("ci") or {}).get(key), "n": r.get("n") if key.startswith("file") else r.get("n_func"), "bm25": t("grep").get(key), "cbm": t("cbm").get(key)}
        a, b = _paired_rows(s1, "rueca", "grep", fam_label, v, "file" if key.startswith("file") else "func")
        out["paired"] = paired_bootstrap(a, b, sc["resamples"], seeded(sc["seed"], "paired", fam_label, key)) if a else None
        if out["paired"] and out["paired"].get("ci"):
            out["paired"]["ci"] = list(out["paired"]["ci"])
    elif key.startswith(("callers_", "callees_")):
        d, _, m = key.partition("_")
        conf = m.endswith("@conf")
        m = m.replace("@conf", "")
        dk = d + ("@conf" if conf else "")
        r = g3("rueca").get(dk) or {}
        c = g3("cbm").get(dk) or {}
        out = {"value": r.get(m), "ci": (r.get("ci") or {}).get(m), "n": r.get("n"), "cbm": c.get(m)}
    elif key == "definition_found":
        r, c = g3("rueca"), g3("cbm")
        out = {"value": r.get("definition_found"), "ci": (r.get("ci") or {}).get("definition_found"), "n": r.get("n"), "cbm": c.get("definition_found")}
    elif key == "micro_callee_f1":
        r = (s3["groups"].get("rueca", {}).get(f"micro:{fam_label}") or {}).get("callees") or {}
        c = (s3["groups"].get("cbm", {}).get(f"micro:{fam_label}") or {}).get("callees") or {}
        out = {
            "value": r.get("micro_f1") if r else None,
            "ci": (r.get("ci") or {}).get("micro_f1"),
            "n": None,
            "cbm": c.get("micro_f1") if c else None,
            "detail": {"precision": r.get("micro_precision"), "recall": r.get("micro_recall"), "edges": r.get("gold_edges")},
        }
        out["n_callables"] = (s3["groups"].get("rueca", {}).get(f"micro:{fam_label}") or {}).get("n")
    elif key == "parse_error_rate":
        rows = [x for x in health["rows"] if x.get("ok") and x["split"] == "gated" and lang_group(x["language"]) == fam_label]
        ext = sum(x["files_extracted"] for x in rows)
        par = sum(x["files_partial"] for x in rows)
        out = {"value": par / ext if ext else None, "ci": _wil(par, ext) if ext else None, "n": ext, "repos": len(rows), "partial": par}
    elif key == "latency_p95_ms":
        lat = _latencies(s1, s3, "rueca", fam_label)
        q = quantile_ci(lat) if lat else None
        cl = _latencies(s1, s3, "cbm", fam_label)
        cq = quantile_ci(cl) if cl else None
        out = {"value": q[0] * 1000 if q else None, "ci": [q[1] * 1000, q[2] * 1000] if q else None, "n": len(lat), "cbm": cq[0] * 1000 if cq else None}
    elif key in ("tokens_per_answer", "tokens_per_search"):
        a, b = _paired_tokens(s3, fam_label, "t" if key == "tokens_per_answer" else "lt")
        q = quantile_ci(a, 0.5) if a else None
        out = {"value": q[0] if q else None, "ci": [q[1], q[2]] if q else None, "n": len(a), "cbm": statistics.median(b) if b else None}
    return out


def build_scorecard(s1: dict, s3: dict, health: dict, data: dict) -> dict:
    sc = _sc()
    langs: dict = {}
    for fam in families().values():
        metrics: dict = {}
        failing: list[str] = []
        for spec in sc["metrics"]:
            key = spec["key"]
            vals = _metric_values(key, fam.label, s1, s3, health)
            min_n = sc["min_s1"] if spec["family"] == "Localization" else (sc["min_s3"] if spec["family"] == "Structure" else None)
            status = _judge(spec, vals.get("value"), vals.get("ci"), vals.get("n"), min_n, vals.get("paired"))
            target = spec.get("target")
            target_met = None
            if isinstance(target, dict) and "cbm_ratio" in target:
                if vals.get("value") is not None and vals.get("cbm"):
                    target_met = vals["value"] <= target["cbm_ratio"] * vals["cbm"]
            elif target is not None and vals.get("value") is not None:
                target_met = vals["value"] <= target if spec.get("better") == "lower" else vals["value"] >= target
            gated = spec.get("gated", True) and spec.get("floor") is not None
            m = {
                **vals,
                "label": spec["label"],
                "family": spec["family"],
                "floor": spec.get("floor"),
                "target": target,
                "better": spec.get("better", "higher"),
                "gated": gated,
                "status": status,
                "target_met": target_met,
                "min_n": min_n,
            }
            metrics[key] = m
            if gated and status not in ("✅", "⚠"):
                failing.append(key)
        langs[fam.key] = {"label": fam.label, "status": "stable" if not failing else "preview", "failing": failing, "metrics": metrics}
    # configuration (RFC 0003 §3.3), one card for the data formats
    cm: dict = {}
    cfailing: list[str] = []
    for spec in sc["config_metrics"]:
        key = spec["key"]
        entries = []
        if key in ("key_recall", "span_accuracy"):
            for fmt, arms in sorted((data.get("formats") or {}).items()):
                r, c = arms.get("rueca"), arms.get("cbm")
                if key == "key_recall":
                    val = (r["covered"] / r["keys"]) if r and r.get("keys") else None
                    ci = _wil(r["covered"], r["keys"]) if r and r.get("keys") else None
                    n = r.get("keys") if r else None
                    cv = (c["covered"] / c["keys"]) if c and c.get("keys") else None
                else:
                    val = (r["exact"] / r["covered"]) if r and r.get("covered") else None
                    ci = _wil(r["exact"], r["covered"]) if r and r.get("covered") else None
                    n = r.get("covered") if r else None
                    cv = (c["exact"] / c["covered"]) if c and c.get("covered") else None
                entries.append((f"{key}:{fmt}", f"{spec['label']} — {fmt}", val, ci, n, cv, None))
        else:
            br = data.get("bridge") or {}
            r = (br.get("rueca") or {}).get("all")
            c = (br.get("cbm") or {}).get("all")
            g = (br.get("grep") or {}).get("all")
            if key == "bridge_precision":
                get = lambda x: (x["tp"] / x["pred"]) if x and x.get("pred") else None
                ci = _wil(r["tp"], r["pred"]) if r and r.get("pred") else None
                n = r.get("pred") if r else None
            else:
                get = lambda x: (x["tp"] / x["gold"]) if x and x.get("gold") else None
                ci = _wil(r["tp"], r["gold"]) if r and r.get("gold") else None
                n = r.get("gold") if r else None
            entries.append((key, spec["label"], get(r), ci, n, get(c), get(g)))
        for k, label, val, ci, n, cv, gv in entries:
            status = _judge(spec, val, ci, n, None)
            cm[k] = {"label": label, "value": val, "ci": ci, "n": n, "cbm": cv, "grep": gv, "floor": spec.get("floor"), "target": spec.get("target"), "better": "higher", "gated": True, "status": status}
            if status not in ("✅", "⚠"):
                cfailing.append(k)
    return {
        "generated": dt.datetime.now(dt.timezone.utc).strftime("%Y-%m-%d %H:%M UTC"),
        "languages": langs,
        "config": {"status": "stable" if not cfailing else "preview", "failing": cfailing, "metrics": cm},
        "settings": {k: sc[k] for k in ("resamples", "seed", "cut", "min_s3", "min_s1", "variant")},
    }


# --- markdown -----------------------------------------------------------------------


def _f(x, pct: bool = True, nd: int = 1) -> str:
    if x is None:
        return "—"
    if pct:
        return f"{100 * x:.{nd}f}"
    return f"{x:.2f}" if x < 100 else f"{x:.0f}"


def _ci(x, ci, pct: bool = True, nd: int = 1) -> str:
    """`66.9 (58.1–74.6)`."""
    if x is None:
        return "—"
    if not ci or None in ci:
        return _f(x, pct, nd)
    if pct:
        return f"{100 * x:.{nd}f} ({100 * ci[0]:.{nd}f}–{100 * ci[1]:.{nd}f})"
    return f"{x:.{nd}f} ({ci[0]:.{nd}f}–{ci[1]:.{nd}f})"


def _num(x, unit: str | None) -> str:
    if x is None:
        return "—"
    if unit == "ms":
        return f"{x:.0f}" if x >= 10 else f"{x:.1f}"
    if unit == "tokens":
        return f"{x:.0f}"
    return _f(x)


def _pct(x: float) -> str:
    """0.9 -> `90`, 0.005 -> `0.5`."""
    v = round(100 * x, 1)
    return f"{v:.0f}" if v == int(v) else f"{v:.1f}"


def _floor_txt(m: dict) -> str:
    fl = m.get("floor")
    if fl is None:
        return "— (reported)"
    if fl == "bm25":
        return "≥ BM25 (paired)"
    unit = m.get("unit")
    v = _num(fl, unit) if unit else _pct(fl)
    return ("≤ " if m.get("better") == "lower" else "≥ ") + v


def _target_txt(m: dict) -> str:
    t = m.get("target")
    if t is None:
        return "—"
    if isinstance(t, dict) and "cbm_ratio" in t:
        return f"≤ {int(t['cbm_ratio'] * 100)}% of cbm"
    unit = m.get("unit")
    v = _num(t, unit) if unit else _pct(t)
    if not unit and t == 1.0:
        return v
    return ("≤ " if m.get("better") == "lower" else "≥ ") + v


def _value_txt(m: dict) -> str:
    unit = m.get("unit")
    v, ci = m.get("value"), m.get("ci")
    if v is None:
        return "pending"
    if unit:
        s = _num(v, unit) + (f" ({_num(ci[0], unit)}–{_num(ci[1], unit)})" if ci else "")
    else:
        s = _ci(v, ci)
    n = m.get("n")
    if m.get("key") == "micro_callee_f1" or m.get("n_callables"):
        n = m.get("n_callables")
    if n is not None:
        s += f", n = {n:,}"
    return s


def _status_txt(m: dict) -> str:
    st = m["status"]
    if m.get("min_n") and (m.get("n") or 0) < m["min_n"] and st == "❌":
        return f"❌ (n < {m['min_n']})"
    if st == "—":
        if m.get("target_met") is True:
            return "— (target met)"
        if m.get("target_met") is False:
            return "— (target not met)"
        return "—"
    if not m["gated"]:
        return f"({st})"
    return st


def _failing_txt(card: dict) -> str:
    out = []
    for k in card["failing"]:
        m = card["metrics"][k]
        if m.get("value") is None:
            out.append(f"{m['label']} (pending)")
            continue
        if m["floor"] == "bm25":
            p = m.get("paired") or {}
            out.append(f"{m['label']} vs BM25 {100 * p.get('diff', 0):+.1f} ({100 * p['ci'][0]:+.1f}–{100 * p['ci'][1]:+.1f})" if p.get("ci") else m["label"])
            continue
        if m.get("min_n") and (m.get("n") or 0) < m["min_n"]:
            out.append(f"{m['label']} (n = {m.get('n') or 0} < {m['min_n']})")
            continue
        out.append(f"{m['label']} {_value_txt(m).split(', n =')[0]} vs {_floor_txt(m)}")
    return "; ".join(out)


def _gold_line(fam_key: str, s3: dict) -> str:
    fam = families()[fam_key]
    parts = []
    for name, gi in s3["gold"].items():
        if gi.get("language") not in fam.members or gi.get("missing"):
            continue
        sp = gi.get("split", "gated")
        if gi.get("micro"):
            cc = gi.get("crosscheck") or gi.get("phpstan_agreement")
            x = ""
            if cc and cc.get("available"):
                x = f", key vs {cc.get('tool')}: {cc['agree']} / {cc['edges_key']} edges"
            parts.append(f"{gi['title']} ({gi['functions']} callables, {gi['edges']} edges{x}){'' if sp == 'gated' else f' [{sp}]'}")
        else:
            parts.append(f"{gi['repo']} {gi.get('tag') or gi['commit'][:10]} ({gi['indexer']}){'' if sp == 'gated' else f' [{sp}]'}")
    return "; ".join(parts)


def render_scorecard(L: list[str], card: dict, s3: dict, meta: dict, s1: dict | None = None) -> None:
    sc = _sc()
    L.append("## Scorecard (RFC 0004 §9)")
    L.append("")
    L.append(
        f"Floors and targets are RFC 0004 §6, read from `bench/manifest.toml` `[scorecard]`. **Rueca (95% CI)**: Wilson intervals for "
        f"proportions (File Acc@5, Function Acc@10, definition found, files with parse errors), percentile bootstrap ({sc['resamples']:,} resamples, "
        f"seed {sc['seed']}) for macro averages (precision / recall over questions) and the pooled micro-benchmark F1, binomial order statistics for "
        "latency p95 and median tokens. **Status**: ✅ floor met with the whole interval on the right side; ⚠ met, but the interval crosses the floor; "
        f"❌ not met, or the population is below the minimum (≥ {sc['min_s3']} structural questions per direction, ≥ {sc['min_s1']} localization instances); "
        "Localization floors are relative: the paired bootstrap of Rueca − BM25 per instance must not lie entirely below zero (§6.1; ✅ interval ≥ 0, ⚠ "
        f"crosses 0). Rows in parentheses are not gated (the confidence ≥ {sc['cut']} rows show what an agent that filters by confidence gets; cbm and "
        "grep have no confidences, so their numbers are the same in both rows). **BM25** = the `grep` arm (localization only); **cbm** = "
        "codebase-memory-mcp v0.12.0 on the same inputs, as reference. A language is `stable` when every gated floor is ✅ or ⚠, else `preview` "
        "(this is the benchmark part of the RFC 0003 / RFC 0004 gate; the other gate items are tracked outside the bench). Calibration and diagnostic "
        "sets are not in the scorecard (see *Calibration split*)."
    )
    L.append("")
    L.append("| Language | Status | Failing floors |")
    L.append("|---|---|---|")
    for key, c in card["languages"].items():
        L.append(f"| {c['label']} | `{c['status']}` | {_failing_txt(c) or '—'} |")
    cfg = card["config"]
    L.append(f"| configuration | `{cfg['status']}` | {'; '.join(cfg['metrics'][k]['label'] + ' ' + _ci(cfg['metrics'][k]['value'], cfg['metrics'][k]['ci']) for k in cfg['failing']) or '—'} |")
    L.append("")
    for key, c in card["languages"].items():
        L.append(f"### {c['label']} — `{c['status']}`")
        L.append("")
        L.append("| Metric | Rueca (95% CI) | Floor | Target | Status | BM25 | cbm |")
        L.append("|---|---|---|---|---|---|---|")
        for mk, m in c["metrics"].items():
            m = dict(m, key=mk)
            unit = next((s.get("unit") for s in sc["metrics"] if s["key"] == mk), None)
            m["unit"] = unit
            label = m["label"] if m["gated"] or m.get("floor") is None else f"({m['label']})"
            bm = _num(m.get("bm25"), unit) if m.get("bm25") is not None else "—"
            if m.get("paired") and m["paired"].get("ci"):
                p = m["paired"]
                bm += f" (Δ {100 * p['diff']:+.1f}, {100 * p['ci'][0]:+.1f}–{100 * p['ci'][1]:+.1f})"
            L.append(f"| {label} | {_value_txt(m)} | {_floor_txt(m)} | {_target_txt(m)} | {_status_txt(m)} | {bm} | {_num(m.get('cbm'), unit)} |")
        L.append("")
        if c["failing"]:
            L.append(f"**Status `preview`** — failing: {_failing_txt(c)}.")
        else:
            L.append("**Status `stable`** — every gated floor met.")
        L.append("")
        mi = c["metrics"].get("micro_callee_f1", {}).get("detail") or {}
        spec = load_manifest()["languages"][key]
        if s1 is not None:
            repos_n = {r["repo"] for r in s1["per_instance"] if r["group"] == c["label"] and r["split"] == "gated"}
            n_inst = (s1["gold"].get("per_group", {}).get(c["label"]) or {}).get("instances", 0)
            L.append(f"- Localization: {n_inst} gated instances over {len(repos_n)} repositories; {spec.get('s1_sources', 'see bench/README.md')}.")
        L.append(f"- Structural gold: {_gold_line(key, s3) or '—'}.")
        if spec.get("notes"):
            L.append(f"- Notes: {spec['notes']}")
        if mi.get("precision") is not None:
            L.append(f"- Micro-benchmark callees, pooled over every callable: precision {_f(mi['precision'])}, recall {_f(mi['recall'])} ({mi['edges']} gold edges).")
        pr = c["metrics"].get("parse_error_rate", {})
        if pr.get("n"):
            L.append(f"- Health: {pr.get('partial')} of {pr['n']:,} files with parse errors over {pr.get('repos')} corpus repositories; latency over every Rueca S1 / S3 repository query (one `rueca query` process per call).")
        vers = meta.get("versions_by_group", {}).get(c["label"], {})
        if vers.get("rueca"):
            L.append(f"- Rueca binary: {'; '.join(vers['rueca'])}.")
        L.append("")
    L.append("### Configuration — `" + cfg["status"] + "`")
    L.append("")
    L.append("Data formats (RFC 0003 §3.3): key recall and span-line accuracy against reference parsers (Wilson over keys), env-var bridge precision / recall against `bench/labels/bridge/` (Wilson over edges). The **grep** column is the regex baseline of the data suite.")
    L.append("")
    L.append("| Metric | Rueca (95% CI) | Floor | Target | Status | grep | cbm |")
    L.append("|---|---|---|---|---|---|---|")
    for mk, m in cfg["metrics"].items():
        n = f", n = {m['n']:,}" if m.get("n") is not None else ""
        L.append(f"| {m['label']} | {_ci(m['value'], m['ci'])}{n} | {_floor_txt(m)} | {_target_txt(m)} | {m['status']} | {_f(m.get('grep'))} | {_f(m.get('cbm'))} |")
    L.append("")


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


def _s3_cells(t: dict | None) -> list[str] | None:
    if not t or not t.get("n"):
        return None
    out = [str(t["n"]), _ci(t["definition_found"], t["ci"].get("definition_found"))]
    for d in DIRS:
        x = t.get(d) or {}
        out += [str(x.get("n", 0)), _ci(x.get("precision"), (x.get("ci") or {}).get("precision")), _ci(x.get("recall"), (x.get("ci") or {}).get("recall")), _f(x.get("f1"))]
    for d in DIRS:
        x = t.get(d + "@conf") or {}
        out += [f"{_f(x.get('precision'))} / {_f(x.get('recall'))}"]
    ce = t.get("callees") or {}
    out += [f"{_f(ce.get('micro_precision'))} / {_f(ce.get('micro_recall'))}", _ci(t.get("lookup_acc@1"), t["ci"].get("lookup_acc@1")), _f(t.get("lookup_acc@10")), _f(t.get("query_s_median"), False), str(t.get("out_of_universe", 0))]
    return out


S3_HEAD = (
    "| Set / arm | questions | definition found | callers n | callers P | callers R | callers F1 | callees n | callees P | callees R | callees F1 "
    "| callers P / R, conf ≥ 0.6 | callees P / R, conf ≥ 0.6 | callees micro P / R | lookup Acc@1 | lookup Acc@10 | query s (median) | dropped (not callable) |"
)


def render(meta: dict, s1: dict, s3: dict, data: dict, health: dict, card: dict) -> str:
    arms = list(ARM_NAMES)
    s1_groups = [g for g in language_groups() if any(g in s1["table"].get(a, {}).get(v, {}) for a in arms for v in VARIANTS)]
    groups_s1 = s1_groups + [OVERALL]
    L: list[str] = []
    L.append("# rueca-bench — results")
    L.append("")
    L.append(f"Generated by `rueca-bench report` on {meta['date']}. Do not edit by hand; methodology, gold sources and known biases: `bench/README.md`; quality bar: RFC 0004.")
    L.append("")
    render_scorecard(L, card, s3, meta, s1)

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
    if meta.get("carried"):
        cf = meta.get("carried_from") or {}
        mc = cf.get("machine") or {}
        L.append(f"- Carried forward from the published results of {cf.get('date', '?')} ({mc.get('cpu', '?')}), raw outputs not in this run's cache: {meta['carried']} (see `bench/README.md`, *Additive results*).")
    L.append("")

    # ---- calibration split
    L.append("## Calibration split (RFC 0004 §7.7)")
    L.append("")
    L.append(
        "Instances and repositories disjoint from the gated ones, declared with `split = \"calibration\"` in the manifest: the only place where ranking "
        "weights and heuristics may be tuned. Never part of the scorecard; changes tuned here say so in their PR."
    )
    L.append("")
    L.append("| Language | Set | n | grep | cbm | rueca |")
    L.append("|---|---|---:|---|---|---|")
    v = _sc()["variant"]
    for g in language_groups():
        key = f"calibration:{g}"
        cells = []
        n = None
        for a in arms:
            t = s1["table"].get(a, {}).get(v, {}).get(key)
            if t:
                n = t["n"]
                cells.append(f"File Acc@5 {_ci(t['file_acc@5'], t['ci'].get('file_acc@5'))} · Func Acc@10 {_ci(t['func_acc@10'], t['ci'].get('func_acc@10'))}")
            else:
                cells.append("pending")
        if n:
            repos = sorted({r["repo"] for r in s1["per_instance"] if r["split"] == "calibration" and r["group"] == g})
            L.append(f"| {g} | S1 `{v}` ({', '.join(repos)}) | {n} | " + " | ".join(cells) + " |")
        cal3 = [s3["calibration"].get(a, {}).get(key) for a in arms]
        if any(cal3):
            names = [gi["repo"] for gi in s3["gold"].values() if gi.get("split") == "calibration" and not gi.get("micro") and lang_group(gi["language"]) == g]
            cells = []
            nn = None
            for t in cal3:
                if not t:
                    cells.append("pending")
                    continue
                nn = t["n"]
                cr, ce = t["callers"], t["callees"]
                cells.append(f"callers P {_f(cr['precision'])} R {_f(cr['recall'])} · callees P {_f(ce['precision'])} R {_f(ce['recall'])}")
            L.append(f"| {g} | S3 ({', '.join(names)}) | {nn} | " + " | ".join(cells) + " |")
    L.append("")

    # ---- S1
    g = s1["gold"]
    L.append("## Localization (S1)")
    L.append("")
    L.append(
        f"{g['instances']} instances ({', '.join(f'{k}: {v}' for k, v in sorted(g['by_language'].items()))}; splits: {', '.join(f'{k} {v}' for k, v in sorted(g['by_split'].items()))}); "
        f"function gold from {', '.join(f'{k}: {v}' for k, v in sorted(g['function_source'].items()))}; "
        f"{g['with_function_gold']} instances have non-empty function gold, {g['with_context_gold']} have ContextBench gold context. "
        f"Tree-sitter labeler vs assembled labels: mean Jaccard {_f(g['labeler_agreement_jaccard_mean'])}% over {g['labeler_agreement_n']} instances. "
        "Gated instances only (the calibration split is above). Each cell: value (95% CI; Wilson for Acc, bootstrap for recall / MRR)."
    )
    L.append("")
    scope = overall_scope()
    first = [x for x in groups_s1 if x == OVERALL or scope is None or x in scope]
    rest = [x for x in groups_s1 if x not in first]
    chunks = [first] + [rest[i : i + 4] for i in range(0, len(rest), 4)]
    for variant, title in (("kw", "keyword query"), ("raw", "raw issue text")):
        L.append(f"### S1, {title} (`{variant}`) — percentages")
        L.append("")
        for ci_, chunk in enumerate(chunks):
            if ci_:
                L.append("")
            cols = [(a, grp) for grp in chunk for a in arms]
            L.append("| Metric | " + " | ".join(f"{grp}: {a}" for a, grp in cols) + " |")
            L.append("|---|" + "---:|" * len(cols))
            ns = []
            for a, grp in cols:
                t = s1["table"].get(a, {}).get(variant, {}).get(grp, {})
                ns.append(str(t.get("n", "—")) if t else "pending")
            L.append("| n instances | " + " | ".join(ns) + " |")
            for label, key in S1_METRICS:
                cells = []
                for grp in chunk:
                    for a in arms:
                        t = s1["table"].get(a, {}).get(variant, {}).get(grp)
                        cells.append(_ci(t.get(key), (t.get("ci") or {}).get(key)) if t else "pending")
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
    splits = s1_splits()
    L.append("| Repository | split | n | " + " | ".join(arms) + " |")
    L.append("|---|---|---:|" + "---:|" * len(arms))
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
        L.append(f"| {repo} | {splits.get(repo, 'gated')} | {n} | " + " | ".join(cells) + " |")
    L.append("")
    L.append("Per-repository samples are small (3–14 instances); their intervals are wide (Wilson at n = 8 spans ±30 points).")
    L.append("")
    gt = s1["table"].get("grep", {}).get("kw", {}).get(OVERALL) or {}
    if "func_acc@10_name_only" in gt:
        L.append(
            f"grep function ranking uses BM25 over function bodies; the name-only BM25 variant scores Func Acc@5 "
            f"{_f(gt['func_acc@5_name_only'])}% / Acc@10 {_f(gt['func_acc@10_name_only'])}% (overall, `kw`)."
        )
        L.append("")

    # ---- S3
    L.append("## Structure (S3): depth-1 callers / callees, symbol lookup")
    L.append("")
    L.append(
        "Questions (RFC 0004 §7.1): every in-scope callable with at least one gold edge in a direction, minus dunders and one-line bodies; "
        "the symbol lookup keeps a stratified sample per set. Micro-benchmarks: every callable of every snippet."
    )
    L.append("")
    L.append("| Gold set | Language | Split | Commit | Gold | Callables | Gold call edges | Questions (callers / callees) | Lookup sample |")
    L.append("|---|---|---|---|---|---:|---:|---|---:|")
    for name, gi in s3["gold"].items():
        if gi.get("missing"):
            L.append(f"| {gi.get('repo') or gi.get('title') or name} | {gi.get('language', '')} | {gi.get('split', '')} | | gold missing | | | | |")
        elif gi.get("micro"):
            L.append(f"| {gi['title']} | {gi['language']} | {gi.get('split', 'gated')} | `bench/{gi.get('source') or ''}` {('`' + gi['commit'][:10] + '`') if gi.get('commit') else ''} | hand-written | {gi['functions']} | {gi['edges']} | {gi['functions']} ({gi['snippets']} snippets) | — |")
        else:
            L.append(
                f"| {gi['repo']} {gi.get('tag') or ''} | {gi['language']} | {gi.get('split', 'gated')} | `{gi['commit'][:10]}` | {gi['indexer']} | {gi['callables']} | {gi['edges']} | "
                f"{gi.get('questions_callers', '—')} / {gi.get('questions_callees', '—')} | {gi.get('lookup_sample', '—')} |"
            )
    L.append("")
    for gi in s3["gold"].values():
        for k, label in (("phpstan_agreement", "PHPStan extractor"), ("crosscheck", None)):
            cc = gi.get(k)
            if not cc:
                continue
            if cc.get("available"):
                extra = cc.get("edges_scip", cc.get("edges_phpstan", 0)) - cc["agree"]
                L.append(
                    f"{gi['title']}: answer key vs {label or cc['tool']} ({cc['tool']}) — {cc['agree']} of {cc['edges_key']} hand-labelled edges "
                    f"found (recall {_f(cc['recall'])}%), {extra} extra edges (precision {_f(cc['precision'])}%); {cc['snippets_identical']} snippets identical"
                    + (f"; {len(cc['snippets_failed'])} snippets failed to index" if cc.get("snippets_failed") else "")
                    + "."
                )
            else:
                L.append(f"{gi['title']}: cross-check not run ({cc.get('reason')}).")
            L.append("")
    L.append(
        "P / R are macro averages over the questions of each direction (precision over questions with at least one prediction), with 95% bootstrap "
        "intervals; *micro P / R* pool every edge. Predictions are restricted to the gold's callable universe (in-scope, non-test definitions the "
        "bench labeler sees as callables: functions and methods with a body), so module / class / variable pseudo-neighbours, C/C++ macros and "
        "prototypes are neither rewarded nor penalised; the last column counts what that rule dropped. Percentages."
    )
    L.append("")
    L.append(S3_HEAD)
    L.append("|---|" + "---:|" * (S3_HEAD.count("|") - 2))

    def s3line(label: str, t: dict | None) -> str:
        cells = _s3_cells(t)
        if cells is None:
            return f"| {label} | pending |" + " |" * (S3_HEAD.count("|") - 3)
        return f"| {label} | " + " | ".join(cells) + " |"

    for grp in language_groups():
        if not any(grp in s3["groups"].get(a, {}) for a in arms):
            continue
        for a in arms:
            L.append(s3line(f"**{grp}** (gated pool) · {a}", s3["groups"].get(a, {}).get(grp)))
    for name, gi in s3["gold"].items():
        for a in arms:
            tag = "" if gi.get("split", "gated") == "gated" else f" [{gi.get('split')}]"
            L.append(s3line(f"{name}{tag} · {a}", s3["per_suite"].get(a, {}).get(name)))
    L.append("")
    L.append("Micro-benchmarks (the scorecard's *Micro* row): callee F1 pooled over every callable (callables without gold edges count their spurious edges).")
    L.append("")
    L.append("| Set / arm | callables | callees micro P (CI) | callees micro R (CI) | callees micro F1 (CI) | callers micro F1 |")
    L.append("|---|---:|---|---|---|---:|")
    for grp in language_groups():
        for a in arms:
            t = s3["groups"].get(a, {}).get(f"micro:{grp}")
            if not t:
                continue
            ce, cr = t["callees"], t["callers"]
            L.append(f"| {grp} · {a} | {t['n']} | {_ci(ce['micro_precision'], ce['ci'].get('micro_precision'))} | {_ci(ce['micro_recall'], ce['ci'].get('micro_recall'))} | {_ci(ce['micro_f1'], ce['ci'].get('micro_f1'))} | {_f(cr['micro_f1'])} |")
    L.append("")

    # ---- health
    L.append("## Health (S5)")
    L.append("")
    L.append(
        "Parse-error rate from Rueca's IndexReport (`files_partial / files_extracted`), one index per repository and language over a hard-linked "
        "tree holding only that language's files (tests included); throughput = extracted files per second of that index (single process, "
        "includes resolution and segment write)."
    )
    L.append("")
    L.append("| Language | Repository | Source | Split | Files extracted | With parse errors | Rate | Files / s |")
    L.append("|---|---|---|---|---:|---:|---:|---:|")
    for r in sorted(health["rows"], key=lambda x: (language_groups().index(lang_group(x["language"])), x["source"], x["repo"])):
        if not r.get("ok"):
            L.append(f"| {lang_group(r['language'])} | {r['repo']} | {r['source']} | {r['split']} | failed: {str(r.get('error'))[:80]} | | | |")
            continue
        ext, par = r["files_extracted"], r["files_partial"]
        tput = ext / (r["duration_ms"] / 1000) if r.get("duration_ms") else None
        L.append(f"| {lang_group(r['language'])} | {r['repo']} | {r['source']} | {r['split']} | {ext} | {par} | {_f(par / ext if ext else None)} | {_f(tput, False) if tput else '—'} |")
    L.append("")

    # ---- efficiency
    L.append("## Efficiency")
    L.append("")
    L.append(
        "Tokens returned per tool answer (tiktoken `cl100k_base` over the JSON the tool printed), Rueca and cbm on the same questions of the gated "
        "repository sets: one depth-1 neighbours answer per question (`trace` direction `both` vs `trace_path` direction `both`) and the lookup "
        "sample's symbol search (limit 10). Reported only (RFC 0004 §6: target ≤ 50% of cbm)."
    )
    L.append("")
    L.append("| Language | Answer | n | Rueca median | cbm median | Rueca / cbm | Rueca mean | cbm mean |")
    L.append("|---|---|---:|---:|---:|---:|---:|---:|")
    for grp in language_groups():
        for field, label in (("t", "trace"), ("lt", "search")):
            a, b = _paired_tokens(s3, grp, field)
            if not a:
                continue
            ma, mb = statistics.median(a), statistics.median(b)
            L.append(f"| {grp} | {label} | {len(a)} | {ma:.0f} | {mb:.0f} | {ma / mb:.2f} | {statistics.mean(a):.0f} | {statistics.mean(b):.0f} |")
    L.append("")

    if data and data.get("gold"):
        _render_data(L, data)

    # ---- failures
    L.append("## Failures and gaps")
    L.append("")
    if not s1["failures"] and not s3["failures"]:
        L.append("No arm failures recorded.")
    fails = Counter((f["arm"], f["variant"], str(f["error"])[:160]) for f in s1["failures"])
    for (a, v_, e), n in sorted(fails.items()):
        L.append(f"- S1 `{a}` `{v_}`: {n}× {e}")
    for f in s3["failures"][:40]:
        L.append(f"- S3 `{f['arm']}` {f['suite']} {f.get('snippet', '')}: {str(f.get('error'))[:200]}")
    for a in arms:
        st = meta["arms"][a]
        if not st["available"] and a not in s1["table"]:
            L.append(f"- Arm `{a}` pending: {st['reason']}.")
    L.append("")
    return "\n".join(L) + "\n"


def _render_data(L: list[str], data: dict) -> None:
    L.append("## Data formats (RFC 0003 §3.3)")
    L.append("")
    L.append(
        "Reference keys = leaf keys of every in-scope data file from a reference parser (JSON grammar, PyYAML, tomllib, python-hcl2; HCL attributes "
        "and labelled blocks). A key is covered when the arm has a node in that file with the same name starting inside the key's span; span "
        "accuracy = covered keys whose node starts on the key's line. Bridge labels: `bench/labels/bridge/` (hand-checked). Percentages with Wilson 95% intervals."
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
    L.append("|---|---|---:|---:|---:|---|---|")
    for fmt, arms in sorted(data["formats"].items()):
        for arm in ("cbm", "rueca"):
            a = arms.get(arm)
            if not a:
                L.append(f"| {fmt} | {arm} | pending | | | | |")
                continue
            L.append(f"| {fmt} | {arm} | {a['files']} | {a['keys']} | {a['covered']} | {_ci(a['recall'], _wil(a['covered'], a['keys']) if a['keys'] else None)} | {_ci(a['span_accuracy'], _wil(a['exact'], a['covered']) if a['covered'] else None)} |")
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
    L.append("|---|---|---|---|---|---|---|---|---:|")

    def pr(x: dict) -> tuple[str, str]:
        p = _ci(x["tp"] / x["pred"], _wil(x["tp"], x["pred"])) if x.get("pred") else "—"
        r = _ci(x["tp"] / x["gold"], _wil(x["tp"], x["gold"])) if x.get("gold") else "—"
        return p, r

    rows = [(n, a, r.get("bridge")) for n, d in data["repos"].items() for a, r in d.items()]
    for n, a, b in sorted(rows, key=lambda x: (x[0], ("grep", "cbm", "rueca").index(x[1]))):
        if not b:
            continue
        L.append(f"| {n} | {a} | {' | '.join(pr(b['reads']))} | {' | '.join(pr(b['writes']))} | {' | '.join(pr(b['all']))} | {_f(b['all']['f1'])} |")
    for a in ("grep", "cbm", "rueca"):
        b = data["bridge"].get(a)
        if b:
            L.append(f"| **all** | {a} | {' | '.join(pr(b['reads']))} | {' | '.join(pr(b['writes']))} | {' | '.join(pr(b['all']))} | {_f(b['all']['f1'])} |")
        else:
            L.append(f"| **all** | {a} | pending | | | | | | |")
    L.append("")
    for f in data.get("failures", []):
        L.append(f"- Data `{f['arm']}` {f['repo']}: {f['error'][:200]}")
    if data.get("failures"):
        L.append("")


def readme_summary(card: dict) -> str:
    """One line per language for the root README (copied by hand)."""
    lines = []
    for c in card["languages"].values():
        lines.append(f"- **{c['label']}** `{c['status']}`" + (f" — {_failing_txt(c)}" if c["failing"] else " — every floor met"))
    cfg = card["config"]
    lines.append(f"- **configuration** `{cfg['status']}`" + (" — " + "; ".join(cfg["failing"]) if cfg["failing"] else " — every floor met"))
    return "\n".join(lines)


def report() -> None:
    published = load_published()
    log("report: scoring S1")
    s1 = score_s1(published["s1"])
    log("report: scoring S3")
    s3 = score_s3(published["s3"])
    data = data_mod.score(published["data"])
    health = health_mod.score()
    log("report: scorecard")
    card = build_scorecard(s1, s3, health, data)
    carried_pairs = {tuple(k.split(":", 1)) for k in s1.get("carried", {})}
    for k in s3.get("carried", []):
        arm, name = k.split(":", 1)
        spec = next((sp for n, sp, _ in suites() if n == name), None)
        if spec:
            carried_pairs.add((arm, lang_group(spec["language"])))
    carried_groups = {g for _, g in carried_pairs}
    carried_arms = {k.split(":", 1)[0] for k in s1.get("carried", {})} | {k.split(":", 1)[0] for k in s3.get("carried", [])}
    arms = arm_status()
    pub_arms = published["meta"].get("arms", {})
    for a, st in arms.items():
        if a in carried_arms and pub_arms.get(a):
            vers = sorted(set(st["recorded_versions"]) | set(pub_arms[a].get("recorded_versions", [])))
            st.update(recorded_versions=vers, version="; ".join(vers) if vers else st["version"], available=st["available"] or bool(vers))
    carried_txt = ", ".join(sorted(f"{k} ({n} instances)" for k, n in s1.get("carried", {}).items()) + sorted(f"S3 {k}" for k in s3.get("carried", [])))
    meta = {
        "date": card["generated"],
        "machine": machine_info(),
        "arms": arms,
        "versions_by_group": versions_by_group(published["meta"], carried_pairs),
        "manifest_sha256": sha256_file(MANIFEST_PATH),
        "manifest_tools": load_manifest()["tools"],
        "carried_groups": sorted(carried_groups),
        "carried": carried_txt,
        "carried_from": {k: published["meta"].get(k) for k in ("date", "machine")} if carried_txt else None,
    }
    card["rueca"] = arms["rueca"]["version"]
    RESULTS_DIR.mkdir(parents=True, exist_ok=True)
    write_json(RESULTS_DIR / "meta.json", meta)
    write_json(RESULTS_DIR / "s1.json", s1)
    write_json(RESULTS_DIR / "s3.json", s3, compact=True)
    write_json(RESULTS_DIR / "data.json", data)
    write_json(RESULTS_DIR / "health.json", health)
    write_json(RESULTS_DIR / "scorecard.json", card)
    DOCS_DIR.mkdir(parents=True, exist_ok=True)
    (DOCS_DIR / "alpha-results.md").write_text(render(meta, s1, s3, data, health, card))
    log(f"wrote {RESULTS_DIR}/{{meta,s1,s3,data,health,scorecard}}.json and {DOCS_DIR / 'alpha-results.md'}")
    print(readme_summary(card))
