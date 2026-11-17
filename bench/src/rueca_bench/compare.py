"""`rueca-bench compare`: decision tables over cached S1 runs (RFC 0007
phases 0 and 1).

Reads the cached outputs of one split (`<cache>/runs/s1/<arm>/<id>.json`,
or `<cache>/runs/s1/<arm>/<tag>/<id>.json` for arms with sweep settings:
`vector`, `rueca-hybrid`, `rueca-semantic`) and the gold
(`<cache>/gold/s1/<id>.json`), scores them with the report's scorer
(`metrics.score_files` / `score_functions`; a failed query is a miss) and
builds, per column (an arm, or an arm and one of its tags) and query
variant:

- the table per language and pooled: any-gold and all-gold File Acc@5 and
  Function Acc@10, file and function MRR;
- the paired bootstrap of column − base per instance (`stats.py`, the
  manifest's resamples and seed, 95%), pooled and per language;
- RFC 0007 §7.6's objective: the macro over languages of ½ (File Acc@5 +
  Function Acc@10), per variant and averaged over `kw` and `raw`;
- for every pair of non-base columns, the top-10 overlap: the mean over
  common instances of the Jaccard index of the two top-10 file lists and of
  the two top-10 function lists (RFC 0007 §3.3 and §6.3 read it).

Columns are compared on the instances each has; pairs only on instances
both have, so a sweep still running yields honest, smaller n.
"""

from __future__ import annotations

import itertools
from dataclasses import dataclass
from pathlib import Path

from .common import families, lang_group, load_manifest, read_json, safe_name, scorecard_spec, split_of
from .metrics import mean, score_files, score_functions
from .stats import paired_bootstrap, seeded

VARIANTS = ("kw", "raw")
POOLED = "pooled"
TOP = 10
# (row key, part of the score, metric of that part)
METRICS = (
    ("file_acc@5", "file", "acc@5"),
    ("func_acc@10", "function", "acc@10"),
    ("file_all@5", "file", "all_acc@5"),
    ("func_all@10", "function", "all_acc@10"),
    ("file_mrr", "file", "mrr"),
    ("func_mrr", "function", "mrr"),
)
PAIRED = ("file_acc@5", "func_acc@10", "file_all@5", "func_all@10")
LABELS = {
    "file_acc@5": "File Acc@5",
    "func_acc@10": "Fn Acc@10",
    "file_all@5": "File Acc@5 all-gold",
    "func_all@10": "Fn Acc@10 all-gold",
    "file_mrr": "File MRR",
    "func_mrr": "Fn MRR",
}


# --- columns and instances --------------------------------------------------------------


@dataclass(frozen=True)
class Column:
    arm: str
    tag: str | None
    path: Path

    @property
    def label(self) -> str:
        return f"{self.arm}:{self.tag}" if self.tag else self.arm


def _has_runs(d: Path) -> bool:
    return d.is_dir() and any(d.glob("*.json"))


def columns_for(runs: Path, spec: str) -> list[Column]:
    """`arm` (its outputs, or every tag directory under it), or `arm:tag`.
    An arm without outputs yields one empty column (reported as n = 0)."""
    arm, _, tag = spec.partition(":")
    base = runs / arm
    if tag:
        return [Column(arm, tag, base / tag)]
    if _has_runs(base) or not base.is_dir():
        return [Column(arm, None, base)]
    tags = [Column(arm, d.name, d) for d in sorted(base.iterdir()) if _has_runs(d)]
    return tags or [Column(arm, None, base)]


def split_instances(split: str) -> list[dict]:
    """The manifest's S1 instances of one split (`all`: every split)."""
    s1 = load_manifest()["s1"]
    splits = {r["repo"]: split_of(r) for r in s1["repos"]}
    return [i for i in s1.get("instances", []) if split == "all" or splits.get(i["repo"], "gated") == split]


def group_order() -> list[str]:
    return [f.label for f in families().values()]


# --- scoring ----------------------------------------------------------------------------


def _distinct(items: list, limit: int) -> list:
    out: list = []
    for x in items:
        if x not in out:
            out.append(x)
        if len(out) >= limit:
            break
    return out


def _fn_key(h: dict) -> tuple:
    return h.get("path"), h.get("name"), h.get("start_line")


def score_query(rec: dict, variant: str, gold: dict) -> dict:
    """One instance and variant: every metric, and the top-10 lists."""
    q = (rec.get("queries") or {}).get(variant) or {}
    ok = bool(rec.get("status") == "ok" and q.get("ok"))
    files = q.get("files", []) if ok else []
    funcs = q.get("functions", []) if ok else []
    parts = {"file": score_files(files, gold["files"]), "function": score_functions(funcs, gold["functions"]) if gold["functions"] else None}
    row: dict = {"ok": ok}
    for key, part, metric in METRICS:
        row[key] = None if parts[part] is None else parts[part][metric]
    row["top_files"] = _distinct(files, TOP)
    row["top_funcs"] = _distinct([_fn_key(h) for h in funcs], TOP)
    return row


def load_golds(gold_dir: Path, insts: list[dict]) -> dict[str, dict]:
    out = {}
    for inst in insts:
        p = gold_dir / f"{safe_name(inst['id'])}.json"
        if p.exists():
            out[inst["id"]] = read_json(p)
    return out


def load_column(col: Column, insts: list[dict], golds: dict[str, dict]) -> dict[str, dict]:
    """{instance id: {variant: row}} for the instances with an output and gold."""
    out: dict[str, dict] = {}
    for inst in insts:
        p = col.path / f"{safe_name(inst['id'])}.json"
        gold = golds.get(inst["id"])
        if gold is None or not p.exists():
            continue
        rec = read_json(p)
        out[inst["id"]] = {v: score_query(rec, v, gold) for v in VARIANTS}
    return out


# --- aggregation ------------------------------------------------------------------------


def half(file_acc: float | None, func_acc: float | None) -> float | None:
    """½ (File Acc@5 + Function Acc@10) (RFC 0007 §7.6)."""
    if file_acc is None or func_acc is None:
        return None
    return (file_acc + func_acc) / 2


def aggregate(rows: list[dict]) -> dict:
    out: dict = {"n": len(rows), "n_func": sum(1 for r in rows if r["func_acc@10"] is not None), "n_failed": sum(1 for r in rows if not r["ok"])}
    for key, _, _ in METRICS:
        out[key] = mean(r[key] for r in rows)
    out["objective"] = half(out["file_acc@5"], out["func_acc@10"])
    return out


def _groups_of(insts: list[dict]) -> dict[str, str]:
    return {i["id"]: lang_group(i["language"]) for i in insts}


def column_table(rows: dict[str, dict], groups: dict[str, str], order: list[str]) -> dict:
    """{variant: {group | pooled: aggregate}}."""
    out: dict = {}
    for v in VARIANTS:
        t: dict = {}
        for g in order:
            sel = [rows[i][v] for i in rows if groups[i] == g]
            if sel:
                t[g] = aggregate(sel)
        t[POOLED] = aggregate([rows[i][v] for i in rows])
        out[v] = t
    return out


def macro_objective(table: dict, variant: str, order: list[str], skip: str | None = None) -> float | None:
    """Macro over languages of ½ (File Acc@5 + Function Acc@10); `skip` leaves
    one language out."""
    vals = [table[variant][g]["objective"] for g in order if g != skip and g in table[variant]]
    return mean(vals)


def objectives(table: dict, order: list[str]) -> dict:
    kw = macro_objective(table, "kw", order)
    raw = macro_objective(table, "raw", order)
    return {"kw": kw, "raw": raw, "both": mean([kw, raw]), "languages": sorted(g for g in order if g in table["kw"])}


# --- paired bootstrap and overlap -------------------------------------------------------


def _bootstrap() -> tuple[int, int]:
    sc = scorecard_spec()
    return int(sc.get("bootstrap_resamples", 10000)), int(sc.get("bootstrap_seed", 20261025))


def paired_metric(a_rows: dict, b_rows: dict, ids: list[str], variant: str, key: str, seed_parts: tuple[str, ...]) -> dict | None:
    a, b = [], []
    for iid in ids:
        ra, rb = a_rows[iid][variant][key], b_rows[iid][variant][key]
        if ra is not None and rb is not None:
            a.append(ra)
            b.append(rb)
    if not a:
        return None
    resamples, seed = _bootstrap()
    res = paired_bootstrap(a, b, resamples, seeded(seed, "compare", *seed_parts))
    if res and res.get("ci"):
        res["ci"] = list(res["ci"])
    return res


def paired_table(col: Column, rows: dict, base: Column, base_rows: dict, groups: dict[str, str], order: list[str]) -> dict:
    """{variant: {group | pooled: {metric: {diff, ci, n}}}} of column − base."""
    common = sorted(set(rows) & set(base_rows))
    out: dict = {}
    for v in VARIANTS:
        t: dict = {}
        for g in [*order, POOLED]:
            ids = common if g == POOLED else [i for i in common if groups[i] == g]
            if ids:
                t[g] = {k: paired_metric(rows, base_rows, ids, v, k, (col.label, base.label, v, g, k)) for k in PAIRED}
        out[v] = t
    return out


def jaccard(a: list, b: list) -> float:
    """|A ∩ B| / |A ∪ B| (1.0 when both are empty)."""
    sa, sb = set(a), set(b)
    union = sa | sb
    return len(sa & sb) / len(union) if union else 1.0


def overlap(a_rows: dict, b_rows: dict, variant: str) -> dict:
    ids = sorted(set(a_rows) & set(b_rows))
    files = [jaccard(a_rows[i][variant]["top_files"], b_rows[i][variant]["top_files"]) for i in ids]
    funcs = [jaccard(a_rows[i][variant]["top_funcs"], b_rows[i][variant]["top_funcs"]) for i in ids]
    return {"n": len(ids), "files": mean(files), "functions": mean(funcs)}


# --- the comparison ---------------------------------------------------------------------


@dataclass
class Comparison:
    cache: Path
    split: str
    base: str
    arms: list[str]

    def run(self) -> dict:
        runs = self.cache / "runs" / "s1"
        insts = split_instances(self.split)
        golds = load_golds(self.cache / "gold" / "s1", insts)
        groups = _groups_of(insts)
        present = {groups[i] for i in golds}
        order = [g for g in group_order() if g in present]
        base = columns_for(runs, self.base)[0]
        cols = _unique([c for spec in self.arms for c in columns_for(runs, spec) if c.label != base.label])
        rows = {c.label: load_column(c, insts, golds) for c in [base, *cols]}
        out: dict = {
            "suite": "s1",
            "split": self.split,
            "cache": str(self.cache),
            "instances": len(insts),
            "with_gold": len(golds),
            "groups": order,
            "base": base.label,
            "bootstrap": dict(zip(("resamples", "seed"), _bootstrap(), strict=True)),
            "columns": {},
            "paired": {},
            "overlap": [],
        }
        for c in [base, *cols]:
            table = column_table(rows[c.label], groups, order)
            out["columns"][c.label] = {"arm": c.arm, "tag": c.tag, "n": len(rows[c.label]), "table": table, "objective": objectives(table, order)}
        for c in cols:
            out["paired"][c.label] = paired_table(c, rows[c.label], base, rows[base.label], groups, order)
        for a, b in itertools.combinations(cols, 2):
            for v in VARIANTS:
                out["overlap"].append({"a": a.label, "b": b.label, "variant": v, **overlap(rows[a.label], rows[b.label], v)})
        return out


def _unique(cols: list[Column]) -> list[Column]:
    seen: set[str] = set()
    out = []
    for c in cols:
        if c.label not in seen:
            seen.add(c.label)
            out.append(c)
    return out


# --- markdown ---------------------------------------------------------------------------


def pct(x: float | None) -> str:
    return "—" if x is None else f"{100 * x:.1f}"


def delta(p: dict | None) -> str:
    if not p:
        return "—"
    ci = p.get("ci")
    rng = f" ({100 * ci[0]:+.1f} … {100 * ci[1]:+.1f})" if ci else ""
    return f"{100 * p['diff']:+.1f}{rng}"


def _variants(choice: str) -> list[str]:
    return list(VARIANTS) if choice == "both" else [choice]


def _pooled_table(res: dict, variants: list[str]) -> list[str]:
    keys = [k for k, _, _ in METRICS]
    L = ["| Column | Variant | n | n fn | failed | " + " | ".join(LABELS[k] for k in keys) + " | ½ (File + Fn) |", "|---|---|---:|---:|---:|" + "---:|" * (len(keys) + 1)]
    for label, col in res["columns"].items():
        for v in variants:
            t = col["table"][v][POOLED]
            L.append(f"| {label} | {v} | {t['n']} | {t['n_func']} | {t['n_failed']} | " + " | ".join(pct(t[k]) for k in keys) + f" | {pct(t['objective'])} |")
    return L


def _language_table(res: dict, variant: str) -> list[str]:
    groups = res["groups"]
    L = ["| Column | " + " | ".join(f"{g} (n)" for g in groups) + " |", "|---|" + "---:|" * len(groups)]
    for label, col in res["columns"].items():
        cells = []
        for g in groups:
            t = col["table"][variant].get(g)
            cells.append(f"{pct(t['file_acc@5'])} / {pct(t['func_acc@10'])} ({t['n']})" if t else "—")
        L.append(f"| {label} | " + " | ".join(cells) + " |")
    return L


def _paired_section(res: dict, variant: str) -> list[str]:
    head = [LABELS[k] for k in PAIRED]
    L = ["| Column | Group | n | " + " | ".join(f"Δ {h}" for h in head) + " |", "|---|---|---:|" + "---:|" * len(PAIRED)]
    for label, by_v in res["paired"].items():
        for g in [POOLED, *res["groups"]]:
            p = by_v[variant].get(g)
            if not p:
                continue
            n = (p.get("file_acc@5") or {}).get("n", 0)
            L.append(f"| {label} | {g} | {n} | " + " | ".join(delta(p[k]) for k in PAIRED) + " |")
    return L


def _objective_table(res: dict) -> list[str]:
    base = res["columns"][res["base"]]["objective"]
    L = ["| Column | kw | raw | mean (kw, raw) | Δ mean vs base | languages |", "|---|---:|---:|---:|---:|---|"]
    for label, col in res["columns"].items():
        o = col["objective"]
        d = None if o["both"] is None or base["both"] is None else o["both"] - base["both"]
        dtxt = "—" if d is None else f"{100 * d:+.1f}"
        L.append(f"| {label} | {pct(o['kw'])} | {pct(o['raw'])} | {pct(o['both'])} | {dtxt} | {len(o['languages'])} of {len(res['groups'])} |")
    return L


def _overlap_table(res: dict, variants: list[str]) -> list[str]:
    L = ["| A | B | Variant | n | top-10 files Jaccard | top-10 functions Jaccard |", "|---|---|---|---:|---:|---:|"]
    for o in res["overlap"]:
        if o["variant"] in variants:
            L.append(f"| {o['a']} | {o['b']} | {o['variant']} | {o['n']} | {_ratio(o['files'])} | {_ratio(o['functions'])} |")
    return L


def _ratio(x: float | None) -> str:
    return "—" if x is None else f"{x:.3f}"


def _instance_line(res: dict) -> str:
    n = res["columns"][res["base"]]["table"]["kw"][POOLED]["n"]
    return f"One pooled instance = {100 / n:.2f} points (n = {n} for the base); the RFC 0007 §3.3 / §3.4 / §6.3 rules read \"within one calibration instance\" against it. " if n else ""


def render(res: dict, variant: str = "both") -> str:
    variants = _variants(variant)
    b = res["bootstrap"]
    L = [
        f"# S1 compare — split `{res['split']}`, base `{res['base']}`",
        "",
        f"Cache `{res['cache']}`; {res['instances']} instances in the split, {res['with_gold']} with gold. Percentages; failed queries count as misses. "
        f"Paired bootstrap of column − base over the instances both have, {b['resamples']:,} resamples, seed {b['seed']}, 95% intervals. "
        f"{_instance_line(res)}Objective (RFC 0007 §7.6): macro over languages of ½ (File Acc@5 + Function Acc@10).",
        "",
        "## Pooled",
        "",
        *_pooled_table(res, variants),
        "",
        "## Objective",
        "",
        *_objective_table(res),
        "",
    ]
    for v in variants:
        L += [f"## Per language, `{v}` — File Acc@5 / Fn Acc@10 (n)", "", *_language_table(res, v), ""]
        L += [f"## Paired vs `{res['base']}`, `{v}`", "", *_paired_section(res, v), ""]
    if res["overlap"]:
        L += ["## Top-10 overlap between columns", "", *_overlap_table(res, variants), ""]
    return "\n".join(L) + "\n"
