"""S3 — structural questions (callers / callees at depth 1, symbol lookup).

Gold sources:
- SCIP (scip-python / scip-typescript) at pinned commits, call edges derived
  per RFC 0002 §4.2 (see scip.py), ~50 functions per repo stratified by
  fan-in / fan-out.
- PyCG micro-benchmark (Apache-2.0): hand-written call graphs of small Python
  programs; every function of every snippet is a question.

Raw arm outputs are cached in .cache/runs/s3/<arm>/<suite>.json.
"""

from __future__ import annotations

import json
import shutil
import traceback
from pathlib import Path

from .arms import make_arm
from .arms.base import Arm, Handle
from .common import GOLD_DIR, RUNS_DIR, SCIP_DIR, WORK_DIR, load_manifest, log, read_json, safe_name, write_json
from .common import run as sh
from .corpus import add_worktree, remove_worktree, worktree
from .scip import derive_call_graph, fid, in_scope, read_index, sample_functions
from .treesitter import extract_functions

PYCG_KEY = "pycg"


def repo_name(spec: dict) -> str:
    return spec["repo"].split("/")[1]


def gold_path(name: str) -> Path:
    return GOLD_DIR / "s3" / f"{name}.json"


def run_path(arm: str, name: str) -> Path:
    return RUNS_DIR / "s3" / arm / f"{safe_name(name)}.json"


# --- gold: SCIP -----------------------------------------------------------------


def _scip_command(spec: dict, root: Path, out: Path) -> list[str]:
    tool = load_manifest()["tools"][spec["indexer"]]
    return ["npx", "--yes", f"{tool['npm']}@{tool['version']}", *spec["args"], "--cwd", str(root), "--output", str(out)]


def build_scip_gold(spec: dict, force: bool = False) -> dict:
    name = repo_name(spec)
    gp = gold_path(name)
    if gp.exists() and not force:
        return read_json(gp)
    man = load_manifest()["s3"]
    scip_out = SCIP_DIR / f"{name}.scip"
    with worktree(spec["repo"], spec["commit"], f"s3-{name}") as root:
        indexer_seconds = None
        if not scip_out.exists() or force:
            SCIP_DIR.mkdir(parents=True, exist_ok=True)
            cmd = _scip_command(spec, root, scip_out)
            log(f"scip {name}: {' '.join(cmd[2:])}")
            # npx runs outside the worktree so the repository's .npmrc (e.g.
            # express: min-release-age) does not govern the pinned indexer.
            res = sh(cmd, cwd=SCIP_DIR, timeout=3600)
            indexer_seconds = res.seconds
            if res.rc != 0 or not scip_out.exists():
                raise RuntimeError(f"SCIP indexer failed for {name} rc={res.rc}: {res.stderr[-1500:]} {res.stdout[-500:]}")
        docs = read_index(scip_out)
        graph = derive_call_graph(root, docs, spec["include"])
    sample = sample_functions(graph, man["sample_per_repo"], man["seed"], man["min_lines"])
    gold = {
        "suite": "scip",
        "name": name,
        "repo": spec["repo"],
        "language": spec["language"],
        "commit": spec["commit"],
        "include": spec["include"],
        "indexer": f"{load_manifest()['tools'][spec['indexer']]['npm']}@{load_manifest()['tools'][spec['indexer']]['version']}",
        "indexer_seconds": indexer_seconds,
        "stats": graph["stats"],
        "callables": graph["callables"],
        "callers": graph["callers"],
        "callees": graph["callees"],
        "sample": sample,
    }
    write_json(gp, gold)
    log(f"s3 gold {name}: {graph['stats'].get('edges')} edges, {len(graph['callables'])} callables, sample {len(sample)}")
    return gold


# --- gold: PyCG micro-benchmark ---------------------------------------------------


def pycg_root() -> Path:
    return WORK_DIR / "pycg-snippets"


def _module_name(rel: str) -> str:
    mod = rel[: -len(".py")].replace("/", ".")
    return mod[: -len(".__init__")] if mod.endswith(".__init__") else mod


def _git_snapshot(path: Path) -> None:
    """Each snippet becomes a tiny git repo (fixed identity and date) so every
    arm sees an ordinary working tree rooted at the snippet."""
    env = {
        "GIT_AUTHOR_NAME": "telar-bench",
        "GIT_AUTHOR_EMAIL": "bench@invalid",
        "GIT_COMMITTER_NAME": "telar-bench",
        "GIT_COMMITTER_EMAIL": "bench@invalid",
        "GIT_AUTHOR_DATE": "2026-01-01T00:00:00Z",
        "GIT_COMMITTER_DATE": "2026-01-01T00:00:00Z",
    }
    sh(["git", "init", "-q"], cwd=path, check=True)
    sh(["git", "add", "-A"], cwd=path, check=True)
    sh(["git", "-c", "commit.gpgsign=false", "commit", "-q", "-m", "snapshot"], cwd=path, env=env, check=True)


def pycg_graph(snip: Path) -> dict:
    cg = json.loads((snip / "callgraph.json").read_text())
    files = sorted(str(p.relative_to(snip)) for p in snip.rglob("*.py") if ".git" not in p.parts)
    modules = {_module_name(rel): rel for rel in files}
    funcs: dict[str, list[dict]] = {}
    callables: dict[str, dict] = {}
    for rel in files:
        lst = []
        for f in extract_functions(rel, (snip / rel).read_bytes()):
            d = f.as_dict()
            d["path"] = rel
            lst.append(d)
            callables[fid(rel, d)] = d
        funcs[rel] = lst
    stats = {"names": 0, "external_or_builtin": 0, "module_nodes": 0, "unmapped": 0}

    def resolve(name: str) -> str | None:
        stats["names"] += 1
        best = None
        for m in modules:
            if (name == m or name.startswith(m + ".")) and (best is None or len(m) > len(best)):
                best = m
        if best is None:
            stats["external_or_builtin"] += 1
            return None
        if name == best:
            stats["module_nodes"] += 1
            return None
        qual = name[len(best) + 1 :]
        rel = modules[best]
        hits = [d for d in funcs[rel] if d["qualname"] == qual]
        if not hits:
            stats["unmapped"] += 1
            return None
        return fid(rel, hits[0])

    callers: dict[str, set[str]] = {}
    callees: dict[str, set[str]] = {}
    questions: set[str] = set()
    for src, dsts in sorted(cg.items()):
        s = resolve(src)
        if s is None:
            continue
        questions.add(s)
        for dst in dsts:
            d = resolve(dst)
            if d is None:
                continue
            callees.setdefault(s, set()).add(d)
            callers.setdefault(d, set()).add(s)
    return {
        "callables": callables,
        "callers": {k: sorted(v) for k, v in callers.items()},
        "callees": {k: sorted(v) for k, v in callees.items()},
        "sample": sorted(questions),
        "stats": stats,
    }


def build_pycg_gold(force: bool = False) -> dict:
    gp = gold_path(PYCG_KEY)
    if gp.exists() and not force:
        return read_json(gp)
    spec = load_manifest()["s3"]["pycg"]
    dest = pycg_root()
    if dest.exists():
        shutil.rmtree(dest)
    dest.mkdir(parents=True)
    src_root = add_worktree(spec["repo"], spec["commit"], "pycg-src")
    try:
        base = src_root / spec["path"]
        snippets = sorted(p.parent for p in base.glob("*/*/callgraph.json"))
        for s in snippets:
            name = f"{s.parent.name}__{s.name}"
            shutil.copytree(s, dest / name)
    finally:
        remove_worktree(spec["repo"], src_root)
    gold: dict = {"suite": "pycg", "name": PYCG_KEY, "repo": spec["repo"], "commit": spec["commit"], "language": "python", "snippets": {}}
    for snip in sorted(dest.iterdir()):
        g = pycg_graph(snip)
        # the answer key must not be visible to the arms
        for extra in ("callgraph.json", "README.md"):
            (snip / extra).unlink(missing_ok=True)
        _git_snapshot(snip)
        gold["snippets"][snip.name] = g
    n_q = sum(len(g["sample"]) for g in gold["snippets"].values())
    n_e = sum(len(v) for g in gold["snippets"].values() for v in g["callees"].values())
    log(f"s3 gold pycg: {len(gold['snippets'])} snippets, {n_q} functions, {n_e} edges")
    write_json(gp, gold)
    return gold


def ensure_pycg_tree() -> None:
    """Re-create the snippet repos if the work dir was cleaned after setup."""
    if pycg_root().exists() and any(pycg_root().iterdir()):
        return
    build_pycg_gold(force=True)


# --- run ------------------------------------------------------------------------------


def _brief(h: dict) -> dict:
    return {k: h.get(k) for k in ("path", "name", "label", "start_line", "end_line")}


def _ask(arm: Arm, h: Handle, graph: dict, questions: list[str], do_lookup: bool) -> list[dict]:
    out = []
    for q in questions:
        func = graph["callables"][q]
        rec: dict = {"fid": q}
        r = arm.resolve(h, func)
        rec["resolve"] = {"ok": r.ok, "seconds": r.seconds, "error": r.error}
        if r.ok:
            ref = r.extra.get("ref")
            for direction in ("callers", "callees"):
                n = arm.neighbors(h, ref, direction)
                rec[direction] = {
                    "ok": n.ok,
                    "seconds": n.seconds,
                    "error": n.error,
                    "hits": [_brief(x) for x in n.hits],
                }
        if do_lookup:
            lk = arm.lookup(h, func["name"], 10)
            rec["lookup"] = {"ok": lk.ok, "seconds": lk.seconds, "error": lk.error, "hits": [_brief(x) for x in lk.hits]}
        out.append(rec)
    return out


def _run_scip_repo(arm: Arm, spec: dict) -> dict:
    name = repo_name(spec)
    gold = read_json(gold_path(name))
    rec: dict = {"arm": arm.name, "suite": "scip", "name": name, "repo": spec["repo"], "language": spec["language"]}
    with worktree(spec["repo"], spec["commit"], f"s3-{name}") as root:
        h = arm.index(root, f"s3-{name}", spec["language"])
        rec["index_seconds"] = h.index_seconds
        rec["index_info"] = h.info
        if not h.ok:
            rec.update(status="index_failed", error=h.error)
            arm.cleanup(h)
            return rec
        try:
            rec["answers"] = _ask(arm, h, gold, gold["sample"], do_lookup=True)
        finally:
            arm.cleanup(h)
    rec["status"] = "ok"
    return rec


def _run_pycg(arm: Arm) -> dict:
    gold = read_json(gold_path(PYCG_KEY))
    ensure_pycg_tree()
    rec: dict = {"arm": arm.name, "suite": "pycg", "name": PYCG_KEY, "language": "python", "snippets": {}}
    for sname, g in sorted(gold["snippets"].items()):
        if not g["sample"]:
            continue
        root = pycg_root() / sname
        h = arm.index(root, f"pycg-{sname}", "python")
        srec: dict = {"index_seconds": h.index_seconds}
        if not h.ok:
            srec.update(status="index_failed", error=h.error)
        else:
            try:
                srec["answers"] = _ask(arm, h, g, g["sample"], do_lookup=False)
                srec["status"] = "ok"
            except Exception as exc:
                srec.update(status="error", error=str(exc))
        arm.cleanup(h)
        rec["snippets"][sname] = srec
    rec["status"] = "ok"
    return rec


def suites() -> list[tuple[str, dict | None]]:
    man = load_manifest()["s3"]
    out: list[tuple[str, dict | None]] = [(repo_name(s), s) for s in man["repos"]]
    if "pycg" in man:
        out.append((PYCG_KEY, None))
    return out


def run(arm_names: list[str], only: list[str] | None = None, force: bool = False) -> None:
    for name, spec in suites():
        if only and name not in only:
            continue
        if not gold_path(name).exists():
            log(f"s3 {name}: gold missing (run `telar-bench setup`); skipped")
            continue
        for arm_name in arm_names:
            arm = make_arm(arm_name)
            ok, _ = arm.available()
            if not ok:
                continue
            p = run_path(arm_name, name)
            if p.exists() and not force and read_json(p).get("status") in ("ok", "index_failed"):
                continue
            log(f"s3 {arm_name:5s} {name}")
            try:
                rec = _run_pycg(arm) if spec is None else _run_scip_repo(arm, spec)
            except Exception as exc:
                rec = {"arm": arm_name, "name": name, "status": "error", "error": f"{exc}\n{traceback.format_exc()[-1500:]}"}
            rec["arm_version"] = arm.version()
            write_json(p, rec)


def prediction_in_scope(path: str | None, gold: dict) -> bool:
    if not path:
        return False
    if gold.get("suite") == "pycg":
        return path.endswith(".py") and not path.startswith("<")
    return in_scope(path, gold["include"])
