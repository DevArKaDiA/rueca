"""Extract the multi-step suite's gold inputs from the bench's existing gold
into `bench/multistep/inputs.json` (committed, so the derivation of every
answer is reproducible without the corpora; `derive.py` documents the
fields). Needs the S3 gold of the three repositories (`rueca-bench setup`),
the corpora's checkouts (`corpora.prepare`) and the S2 gold of the ollama
group (`python -m rueca_bench.contracts gold --only ollama`, run here when
missing).
"""

from __future__ import annotations

from pathlib import Path

from ..common import BENCH_DIR, read_json, write_json
from ..contracts import run_gold
from ..corpus import git_show
from ..crossrepo import label_python, make_libs
from ..datasets import instance_row
from ..gold import parse_patch
from ..org import any_candidate, collapse_gold
from ..org import run_path as org_run_path
from ..s3 import gold_path
from . import corpora

INPUTS_PATH = BENCH_DIR / "multistep" / "inputs.json"


def s3_inputs(name: str) -> dict:
    g = read_json(gold_path(name))
    edges = sorted([a, b] for a, bs in g["callees"].items() for b in bs)
    return {
        "repo": g["repo"],
        "commit": g["commit"],
        "include": g["include"],
        "callables": {k: v["label"] for k, v in sorted(g["callables"].items())},
        "edges": edges,
    }


def crossrepo_inputs(roots: dict[str, Path]) -> dict:
    g = corpora.crossrepo_group()
    libs = make_libs("python", roots, g["libraries"])
    _imports, uses, _anc, _named = label_python(roots[g["consumer"]], libs)
    return {"consumer": g["consumer"], "uses": sorted([list(u) for u in uses])}


def contract_inputs() -> dict:
    p = org_run_path("contracts", "gold")
    doc = read_json(p) if p.exists() else {}
    group = next((x for x in doc.get("groups", []) if x["name"] == "ollama"), None)
    if group is None:
        group = next(x for x in run_gold(["ollama"])["groups"] if x["name"] == "ollama")
    pairs = []
    for x in group["gold"]["pairs"]:
        (sr, sp, ss), (dr, dp, ds) = x["src"], x["dst"]
        pairs.append({"kind": x["kind"], "key": x["key"], "src_repo": sr, "src_path": sp, "src_symbol": ss, "dst_repo": dr, "dst_path": dp, "dst_symbol": ds})
    pairs.sort(key=lambda d: tuple(d.values()))
    return {"group": "ollama", "repos": group["repos"], "pairs": pairs}


def multiref_inputs(fix: str) -> dict:
    inst = corpora.multiref_inst()
    patch = instance_row(inst)["patch"]
    changed: dict[str, tuple[str | None, str | None]] = {}
    for fp in parse_patch(patch):
        for path in {fp.old_path, fp.new_path} - {None}:
            if any_candidate(path):
                old = git_show(inst["repo"], inst["base_commit"], fp.old_path) if fp.old_path else None
                new = git_show(inst["repo"], fix, fp.new_path) if fp.new_path else None
                changed[path] = (old, new)
    gold = collapse_gold(changed, inst["language"], [])
    symbols = sorted([s["path"], s["qn"], s["class"]] for s in gold["symbols"].values())
    return {"instance": inst["id"], "repo": inst["repo"], "base": inst["base_commit"], "fix": fix, "symbols": symbols}


def org_deps() -> dict[str, list[str]]:
    g = corpora.crossrepo_group()
    return {g["consumer"]: sorted(x["repo"] for x in g["libraries"])}


def extract(binary: str) -> dict:
    xr = corpora.prepare("xr-flask", binary)
    mr = corpora.prepare(corpora.MULTIREF, binary)
    doc = {
        "s3": {name: s3_inputs(name) for name in corpora.SINGLE},
        "crossrepo": crossrepo_inputs(xr.roots),
        "contracts": contract_inputs(),
        "multiref": multiref_inputs(mr.refs["fix"]),
        "org_deps": org_deps(),
    }
    write_json(INPUTS_PATH, doc, compact=True)
    return doc


def load() -> dict:
    return read_json(INPUTS_PATH)
