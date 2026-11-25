"""Org-path scenarios (RFC 0005 §5, §6, §8; RFC 0006 §8): several
repositories as one organization, and one repository at two commits.

**Multi-repository** (manifest `[org]`, `[[org.repos]]`).

*Members.* Every `[[org.repos]]` entry is one repository pinned to one
commit (pin rule in the manifest; `derive_pins` recomputes it). The gated
organization is the `split = "gated"` members; the calibration organization
is the gated one plus the `split = "calibration"` members (RFC 0004 §7.7).
A member without S1 instances is indexed like the others (a distractor).

*Instances* (`org_instances(split)`), deterministic: per member of that
split, its S1 instances in manifest order whose S1 gold is present at the
pin — every gold file exists there (`file_exists_at`) and, when the instance
has function gold, at least one gold function exists there by qualified
name (tree-sitter over the file at the pin) and whose fix is not already
at the pin (`patch_at_pin` != "reverse") — the first
`max_instances_per_repo` (`calibration_max_instances_per_repo` for the
calibration split, which is only ever tuned on). The gold at the pin (`gold_at_pin`, cached in
.cache/gold/org/<id>.json) is the S1 gold's files and its functions
re-resolved at the pin (start / end / name lines at the pin, which
`same_def` compares), plus `files_same_blob` (gold files whose blob at the
pin equals the base commit's: a drift signal when the pin is not the
instance's own base). Kind `issue`: the query is the S1 keyword query
derived from the issue text (`queries_for(inst)["kw"]`). Kind `impact`
(`impact_instances`, `impact.py`): every S1 instance of a library member
(no cap) whose consumer members use what its fix changes, with the consumer
files as a second gold side. The run asks `org_questions(split)`: the issue
instances plus the impact instances that are not issue instances (one query
per id; an id of both kinds is scored as both from the same answer).

*Arms* (all built one repository at a time from worktrees at the pins,
removed after indexing):

- **rueca** — one organization (`rueca index --org <file> <root>` per
  member, RFC 0006); **native** = one `search_symbols` served with `--org`
  and no `repo` (limit 100; efficiency: the same request at limit 10, the
  text an agent reads); **per_repo** = one `repo:`-scoped search per member
  in the same store, merged by reciprocal rank fusion (k = 60);
  **single** = the instance's own repository's list of that sweep. After the
  gated members, the member with the most symbol documents is re-indexed
  (RFC 0006 §8 incremental update). The calibration organization is a copy
  of the gated store (`copy_store`) with the calibration members added by
  `index --org`.
- **grep** (BM25, `arms/grep.py` per member): **native** = one BM25 ranking
  over the union of the members (file documents for files, function-body
  documents for functions; document count, lengths and frequencies of the
  union: `bm25.union_search`); **per_repo** = each member's own BM25 ranking
  merged by RRF; **single** = the own member's ranking. Each member's index
  is restricted to the terms of every query of the run (`BM25.restrict`)
  and dropped, so memory holds one full index at a time. grep has no
  rendered answer, so no tokens (`null`), as in S1.
- **cbm** — one project per member; **per_repo** = one `search_graph` per
  project merged by RRF; **single** = the own project's list. No native mode
  (cbm has no cross-project search).

*Scoring* (`score_multirepo`): File Acc@5 and Function Acc@10 over
repository-qualified items with the candidate filter of every language
family (`any_candidate`), for each mode and for the single answer of the
same arm in the same run; grep's file accuracy reads its file ranking.
Tokens per correct answer (RFC 0006 §8): Σ tokens of the top-10 answers /
queries whose top 10 holds a gold file of the instance's own repository.

*Raw outputs*: .cache/runs/org/multirepo-<split>-<arm>.json:

    {status, split, arm, arm_version, filter (repositories of an `--only`
     run, else null), members: [{repo, language, commit, split}],
     instances: [{id, repo, language, kind, kinds, pin, base_commit, query,
                  gold: {files, functions, files_same_blob, patch_at_pin},
                  impact?: {consumers: [{repo, path, symbols}],
                            symbols: [{path, names}]}}],
     index: {repo: {ok, seconds, error, ...; rueca: repo_id, docs, repo_ms,
            org_ms, org_repos, generation, added, replaced, bytes_copied;
            grep: files, functions}} (calibration: only the members added
            to the gated organization),
     incremental (rueca, gated) | null, store: {copied_from, seconds} | null,
     answers: {id: {native?, per_repo, single}},
     seconds: {index, queries}, max_rss_mb: {bench, children}}

    native   rueca {ok, seconds, items, eff}; grep {ok, items, files}
    per_repo {ok, items, tokens (Σ per-member top-10 answers; grep null),
              calls, errors?; grep: files}
    single   {ok, items, eff (rueca, cbm: the own member's top-10 answer);
              grep: files}
    items    [[repo, path, name, start_line, label], ...] (top 50)
    files    [[repo, path], ...] (top 50)
    eff      {ok, tokens, prefix, format, items}

`score_multirepo()` returns the gated split at top level and the calibration
split under `calibration`, each: {split, instances (issue), impact_instances,
repos (members), instance_repos, arms: {arm: {status, version,
index_seconds, modes: {native|per_repo: aggregates + rows} (issue),
impact: {native|per_repo: `impact_aggregate`} (when the output has impact
instances), org_layer (rueca)}}}. Impact rows add consumer_hit@10 /
consumer_hit@20 (a consumer gold file among the first 10 / 20 distinct
candidate files) and consumer_recall@20. One issue row per
instance: id, repo, language, kind, file_acc@5, func_acc@10 (None without
function gold), other_repo_top10, single_file_acc@5, single_func_acc@10,
tokens, single_tokens, fh / pf (first gold-file rank in the top-10 answer
and the tokens read until it; native only), single_fh / single_pf, top10
(items) and top10_files (grep).

**Incremental update** (RFC 0006 §8, Rueca): each `index --org <file>
<root>` reports the repository's own index time and the organization-layer
update time (`org_ms`, adding one repository to the ones already there);
after the gated members, the repository with the most symbol documents is
re-indexed the same way (its part of the organization replaced).

**Multi-ref** (manifest `[multiref]`). One S1 instance's repository at its
base commit and at a fix commit (the base plus the instance's gold patch,
committed with a fixed identity and date, so the commit id is deterministic):
the base worktree is indexed, then the fix commit as a ref snapshot of the
same repository (`rueca index <base> --ref <fix commit>`, RFC 0006 §9.3).
Gold (`collapse_gold`): per symbol of the patched language, whether it is
identical in both refs (its file blob is the same: one item, both
snapshots), changed (content differs: `changed: true`), added, removed, or
unchanged inside a changed file. Queries: the names of the changed / added /
removed functions plus a deterministic sample of unchanged ones, each a
`search_symbols` over both refs (`ref: [base, fix]`) against the two
single-ref answers (`ref: base`, `ref: fix`, the uncollapsed baseline);
`score_collapse` checks collapse, `in` lists and `changed` flags.

Raw output: .cache/runs/org/multiref-rueca.json.
"""

from __future__ import annotations

import hashlib
import json
import os
import resource
import shutil
import sys
import tempfile
import time
import traceback
from collections.abc import Callable
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from typing import Any

from .arms import make_arm
from .arms.base import same_def
from .arms.rueca import flat_results
from .bm25 import TermStats, union_search
from .common import CACHE, GOLD_DIR, candidate_path, count_tokens, families, lang_group, load_manifest, log, read_json, run, safe_name, write_json, write_org_file
from .common import org_run_path as run_path
from .corpus import add_worktree, blob_id, ensure_commit, file_exists_at, git_show, remove_worktree, repo_dir
from .crossrepo import run_crossrepo
from .datasets import instance_row
from .efficiency import _median
from .gold import parse_patch
from .impact import consumer_uses, impact_record, library_consumers
from .queries import tokenize
from .s1 import EFF_LIMIT, queries_for, s1_gold
from .s1 import instances as s1_instances
from .spanning import spanning_pairs
from .stats import wilson
from .treesitter import extract_functions, grammar_for

RRF_K = 60
ORG_LIMIT = 100
KEEP = 50  # items kept per answer (a merged top 50 needs at most each list's top 50)
MULTIREF_SAMPLE = 20
SPLITS = ("gated", "calibration")
ORG_ARMS = ("rueca", "grep", "cbm")
SELECT_WORKERS = 4


# --- members and pins ------------------------------------------------------------------


def derive_pins(s1_repos: list[dict], s1_insts: list[dict]) -> list[dict]:
    """The manifest's pin rule: every gated S1 repository at the base commit of
    its first `[[s1.instances]]` entry, then every calibration one (manifest
    order; repositories without instances are left out)."""
    first: dict[str, dict] = {}
    for inst in s1_insts:
        first.setdefault(inst["repo"], inst)
    out = []
    for split in SPLITS:
        for r in s1_repos:
            if r.get("split", "gated") == split and r["repo"] in first:
                out.append({"repo": r["repo"], "language": r["language"], "commit": first[r["repo"]]["base_commit"], "split": split, "instance": first[r["repo"]]["id"]})
    return out


def org_repos(split: str | None = None) -> list[dict]:
    """`[[org.repos]]` entries (of one split, or all), manifest order."""
    reps = load_manifest().get("org", {}).get("repos", [])
    return [dict(r) for r in reps if split is None or r.get("split", "gated") == split]


def org_members(split: str) -> list[dict]:
    """Members of an organization: gated = the gated entries; calibration =
    the gated entries plus the calibration ones."""
    if split == "gated":
        return org_repos("gated")
    return org_repos("gated") + org_repos("calibration")


def parse_only(only: list[str] | None) -> tuple[list[str], set[str] | None]:
    """`--only` of the org suite: split names (`gated`, `calibration`) select
    splits, anything else restricts members and instances to those
    repositories (smoke runs)."""
    only = only or []
    splits = [s for s in SPLITS if s in only] or list(SPLITS)
    repos = {x for x in only if x not in SPLITS}
    return splits, (repos or None)


def _filtered(members: list[dict], repos: set[str] | None) -> list[dict]:
    return [m for m in members if repos is None or m["repo"] in repos]


# --- instance selection and gold at the pin ----------------------------------------------


def _org_gold_path(iid: str) -> Path:
    return GOLD_DIR / "org" / f"{safe_name(iid)}.json"


def resolve_gold_at_pin(gold: dict, inst: dict, pin: str) -> dict:
    """The S1 gold of `inst` (files + functions at its base commit) carried to
    the member's pin. `present` = every gold file exists at the pin and, when
    there is function gold, at least one gold function exists there by
    qualified name; functions are re-resolved at the pin (nearest start line
    to the base one among same-name definitions)."""
    repo, lang, base = inst["repo"], inst["language"], inst["base_commit"]
    files = list(gold.get("files") or [])
    funcs = list(gold.get("functions") or [])
    out: dict = {
        "id": inst["id"],
        "repo": repo,
        "base_commit": base,
        "pin": pin,
        "present": False,
        "reason": None,
        "files": files,
        "functions": [],
        "functions_base": len(funcs),
        "files_same_blob": None,
        "source": f"S1 gold (functions: {gold.get('function_source', 'treesitter')})"
        + (" at the instance's own base commit (pin = base)" if pin == base else ", files checked and functions re-resolved at the pin"),
    }
    if not files:
        out["reason"] = "no file gold"
        return out
    if pin == base:
        out.update(present=True, functions=funcs, files_same_blob=len(files))
        return out
    ensure_commit(repo, pin)
    missing = [f for f in files if not file_exists_at(repo, pin, f)]
    if missing:
        out["reason"] = f"gold file missing at the pin: {missing[0]}"
        return out
    at_pin: dict[str, list] = {}
    resolved: dict[tuple, dict] = {}
    for g in funcs:
        path = g["path"]
        if path not in at_pin:
            src = git_show(repo, pin, path)
            at_pin[path] = [] if src is None or grammar_for(path, lang) is None else extract_functions(path, src.encode("utf-8"), lang)
        cands = [f for f in at_pin[path] if f.qualname == g.get("qualname")]
        if not cands:
            continue
        f = min(cands, key=lambda f: (abs(f.start - (g.get("start") or 0)), f.start))
        d = f.as_dict()
        d["path"] = path
        resolved[(path, f.start, f.qualname)] = d
    out["functions"] = sorted(resolved.values(), key=lambda d: (d["path"], d["start"]))
    if funcs and not out["functions"]:
        out["reason"] = "no gold function at the pin"
        return out
    out["files_same_blob"] = sum(1 for f in files if blob_id(repo, pin, f) == blob_id(repo, base, f))
    out["present"] = True
    return out


def patch_state(repo: str, commit: str, patch: str) -> str:
    """Whether a patch applies at a commit (`applies`: the fix is not there),
    applies in reverse (`reverse`: the fix is already there) or neither
    (`neither`: the code moved on). Checked against a scratch index, no
    worktree."""
    with tempfile.TemporaryDirectory(prefix="rueca-bench-patch-") as d:
        pf = Path(d) / "fix.patch"
        pf.write_text(patch)
        env = {"GIT_INDEX_FILE": str(Path(d) / "index")}
        if run(["git", "read-tree", commit], cwd=repo_dir(repo), env=env, timeout=300).rc != 0:
            return "neither"
        for state, extra in (("applies", []), ("reverse", ["-R"])):
            if run(["git", "apply", "--cached", "--check", *extra, str(pf)], cwd=repo_dir(repo), env=env, timeout=300).rc == 0:
                return state
    return "neither"


def gold_at_pin(inst: dict, pin: str) -> dict:
    """`resolve_gold_at_pin` of the instance's S1 gold, cached per (instance,
    pin) in .cache/gold/org/<id>.json. For a present instance whose pin is
    not its own base commit, `patch_at_pin` records whether the fix is
    already at the pin (`patch_state`; `reverse` instances are skipped by
    `select_for_member`, the other states are reported)."""
    p = _org_gold_path(inst["id"])
    if p.exists():
        cached = read_json(p)
        if cached.get("pin") == pin and "patch_at_pin" in cached:
            return cached
    g = resolve_gold_at_pin(s1_gold(inst), inst, pin)
    g["patch_at_pin"] = None
    if g["present"]:
        g["patch_at_pin"] = "base" if pin == inst["base_commit"] else patch_state(inst["repo"], pin, instance_row(inst)["patch"])
    write_json(p, g)
    return g


def select_for_member(member: dict, insts: list[dict], cap: int, gold_fn: Callable[[dict, str], dict] = gold_at_pin) -> list[dict]:
    """The member's first `cap` instances (manifest order) whose gold is
    present at its pin and whose fix is not already there (`patch_at_pin`
    != "reverse": a question about a bug the pin has fixed is stale)."""
    out: list[dict] = []
    for inst in insts:
        if len(out) >= cap:
            break
        g = gold_fn(inst, member["commit"])
        if g.get("present") and g.get("patch_at_pin") != "reverse":
            gold = {"files": g["files"], "functions": g["functions"], "files_same_blob": g.get("files_same_blob"), "patch_at_pin": g.get("patch_at_pin")}
            out.append({**inst, "kind": "issue", "pin": member["commit"], "gold": gold})
    return out


def org_instances(split: str = "gated", repos: set[str] | None = None, workers: int = SELECT_WORKERS) -> list[dict]:
    """Instances of one split (module docstring): per member of the split, its
    first `[org] max_instances_per_repo` (calibration:
    `calibration_max_instances_per_repo`) S1 instances whose gold is present
    at the pin and whose fix is not already there. Deterministic (manifest order); members are examined in
    parallel, the result keeps member order."""
    cfg = load_manifest().get("org", {})
    cap = int(cfg.get("calibration_max_instances_per_repo" if split == "calibration" else "max_instances_per_repo", 3))
    members = _filtered(org_repos(split), repos)
    by_repo: dict[str, list[dict]] = {}
    for inst in s1_instances():
        by_repo.setdefault(inst["repo"], []).append(inst)

    def pick(m: dict) -> list[dict]:
        sel = select_for_member(m, by_repo.get(m["repo"], []), cap)
        log(f"org {split}: {m['repo']} {len(sel)} instance(s) at {m['commit'][:10]}")
        return sel

    with ThreadPoolExecutor(max_workers=max(1, workers)) as ex:
        picked = list(ex.map(pick, members))
    return [i for sel in picked for i in sel]


def impact_instances(split: str = "gated", repos: set[str] | None = None) -> list[dict]:
    """Impact instances of one split (`impact.py`): per library member of the
    split (manifest order), every S1 instance (manifest order) whose gold is
    present at the pin and whose fix is not already there — the issue rule
    without the cap — with a non-empty consumer side; consumers are labelled
    at the organization pins (`[[org.repos]]`). [] when the split has no
    library member (calibration)."""
    libs = library_consumers(split)
    members = [m for m in _filtered(org_repos(split), repos) if m["repo"] in libs]
    if not members:
        return []
    pins = {m["repo"]: m["commit"] for m in org_members(split)}
    pairs = spanning_pairs(split)
    by_repo: dict[str, list[dict]] = {}
    for inst in s1_instances():
        by_repo.setdefault(inst["repo"], []).append(inst)
    out: list[dict] = []
    for m in members:
        cands = by_repo.get(m["repo"], [])
        sel = select_for_member(m, cands, len(cands))
        uses = {c: consumer_uses(c, pairs[c], pins) for c in libs[m["repo"]]}
        kept = 0
        for inst in sel:
            rec = impact_record({**inst, "query": queries_for(inst)["kw"]}, uses, any_candidate)
            if rec is not None:
                out.append({**inst, **rec})
                kept += 1
        log(f"org {split}: {m['repo']} {kept} impact instance(s) of {len(sel)} at the pin")
    return out


def org_questions(split: str = "gated", repos: set[str] | None = None, workers: int = SELECT_WORKERS) -> list[dict]:
    """What the multi-repository run asks: the issue instances
    (`org_instances`), then the impact instances (`impact_instances`) that
    are not issue instances. Every record carries `kinds`; an impact
    instance also carries `impact` = {consumers, symbols}. One query per id:
    an id of both kinds is asked once and scored as both."""
    out = [dict(i, kinds=["issue"]) for i in org_instances(split, repos, workers)]
    at = {i["id"]: k for k, i in enumerate(out)}
    for imp in impact_instances(split, repos):
        payload = {"consumers": imp["consumers"], "symbols": imp["symbols"]}
        if imp["id"] in at:
            rec = out[at[imp["id"]]]
            rec["kinds"].append("impact")
            rec["impact"] = payload
        else:
            out.append({k: v for k, v in imp.items() if k not in ("consumers", "symbols", "version")} | {"kind": "impact", "kinds": ["impact"], "impact": payload})
    return out


def calibration_instances() -> list[dict]:
    """Instances of the calibration split (routing calibration)."""
    return org_instances("calibration")


def any_candidate(path: str | None) -> bool:
    """Candidate filter of the org view: a non-test source file of any
    language family (the query does not say which repository or language)."""
    if not path:
        return False
    for fam in families().values():
        if candidate_path(path, fam.members[0]):
            return True
    return False


# --- rank fusion ---------------------------------------------------------------------


def rrf_merge(lists: list[list[dict]], k: int = RRF_K) -> list[dict]:
    """Reciprocal rank fusion of per-repository rankings; every item belongs
    to one list, so items of equal rank tie and are ordered by their raw
    `score` (higher first), then by list order."""
    scored = []
    for li, lst in enumerate(lists):
        for rank, it in enumerate(lst, 1):
            scored.append((1.0 / (k + rank), it.get("score") or 0.0, -li, -rank, it))
    scored.sort(key=lambda x: (x[0], x[1], x[2], x[3]), reverse=True)
    return [x[4] for x in scored]


# --- stores --------------------------------------------------------------------------


def copy_store(src: Path, dst: Path) -> float:
    """Copy a store directory (replacing `dst`); copy-on-write clones on macOS
    (`cp -c`, APFS), a plain copy elsewhere. Seconds taken."""
    t0 = time.perf_counter()
    shutil.rmtree(dst, ignore_errors=True)
    dst.parent.mkdir(parents=True, exist_ok=True)
    if sys.platform == "darwin" and run(["cp", "-c", "-R", str(src), str(dst)], timeout=1800).rc == 0:
        return time.perf_counter() - t0
    shutil.rmtree(dst, ignore_errors=True)
    shutil.copytree(src, dst, symlinks=True)
    return time.perf_counter() - t0


def _org_index(arm, org_file: Path, root: Path, store: Path) -> dict:
    """`rueca index --org <file> <root>`: one repository indexed, then the
    organization layer updated. Repository id, both times, documents."""
    res = run([arm.bin, "index", "--org", str(org_file), str(root), "--store", str(store), "--json"], timeout=arm.index_timeout)
    out: dict = {"ok": res.rc == 0, "seconds": res.seconds, "repo_id": None, "error": None if res.rc == 0 else res.stderr[-300:]}
    if res.rc != 0:
        return out
    try:
        rep = json.loads(res.stdout)
        seg = rep["indexed"][0]["segment"]
        up = rep["update"]
        out.update(
            repo_id=seg["snapshot"]["repo"],
            docs=seg["node_count"],
            repo_ms=rep["indexed"][0]["duration_ms"],
            org_ms=rep["org_ms"],
            org_repos=up["repos"],
            generation=up["generation"],
            added=up["added"],
            replaced=up["replaced"],
            bytes_copied=up["bytes_copied"],
        )
    except (json.JSONDecodeError, KeyError, IndexError) as exc:
        out.update(ok=False, error=f"unexpected index report: {exc}")
    return out


def add_org_members(arm, members: list[dict], store: Path, org_file: Path, roots: list[Path], index: dict, prefix: str = "org", name: str = "bench", skip: Callable[[str], bool] | None = None, on_step: Callable[[], None] | None = None) -> None:
    """Grow a Rueca organization one member at a time: worktree at the pin,
    appended to the organization file, `index --org`, worktree removed.
    `roots` and `index` are updated in place."""
    for m in members:
        repo = m["repo"]
        if skip is not None and skip(repo):
            continue
        root = add_worktree(repo, m["commit"], f"{prefix}-{repo}")
        try:
            if root not in roots:
                roots.append(root)
            write_org_file(org_file, roots, name=name)
            index[repo] = _org_index(arm, org_file, root, store)
            log(f"org {arm.name}: {repo} indexed ({index[repo].get('seconds', 0):.1f} s, org layer {index[repo].get('org_ms')} ms)")
        finally:
            remove_worktree(repo, root)
        if on_step is not None:
            on_step()


# --- answers -------------------------------------------------------------------------


def _items(hits: list[dict], repo: str | None = None) -> list[list]:
    return [[h.get("repo") or repo, h.get("path"), h.get("name"), h.get("start_line"), h.get("label")] for h in hits]


def _eff(res, repo: str | None = None) -> dict:
    """Compact efficiency answer; `repo` labels every item (single-repository
    answers)."""
    if not res.ok:
        return {"ok": False, "error": res.error}
    hits = [dict(h, repo=repo) for h in res.hits] if repo else res.hits
    return {"ok": True, "tokens": res.extra.get("tokens"), "prefix": res.extra.get("prefix") or [], "format": res.extra.get("format"), "items": _items(hits)}


def _norm_cbm_score(hits: list[dict]) -> list[dict]:
    # cbm's `rank` is a negative BM25 (lower = better); flip it to a score
    return [dict(h, score=-(h.get("rank") or 0.0)) for h in hits]


def rueca_answer(arm, q: str, own: str, store: Path, org_file: Path, members: list[dict], index: dict) -> dict:
    a: dict = {}
    data, secs, err, _ = arm.call("search_symbols", {"query": q, "limit": ORG_LIMIT}, None, store, org=org_file)
    if err:
        a["native"] = {"ok": False, "error": err}
    else:
        eff = arm.search_eff(None, q, EFF_LIMIT, root=None, store=store, org=org_file)
        a["native"] = {"ok": True, "seconds": secs, "items": _items(flat_results(data))[:KEEP], "eff": _eff(eff)}
    per: list[list[dict]] = []
    tokens = 0
    errors: list[str] = []
    single: dict = {"ok": False, "error": "own repository not indexed"}
    for m in members:
        rid = index.get(m["repo"], {}).get("repo_id")
        if not rid:
            continue
        d2, _, e2, _ = arm.call("search_symbols", {"query": q, "limit": ORG_LIMIT, "repo": rid}, None, store)
        if e2:
            errors.append(e2)
            if m["repo"] == own:
                single = {"ok": False, "error": e2}
            continue
        hits = [dict(h, repo=m["repo"]) for h in flat_results(d2)]
        per.append(hits)
        ef = arm.search_eff(None, q, EFF_LIMIT, root=None, store=store, repo=rid)
        if ef.ok:
            tokens += ef.extra.get("tokens") or 0
        if m["repo"] == own:
            single = {"ok": True, "items": _items(hits)[:KEEP], "eff": _eff(ef, own)}
    a["per_repo"] = {"ok": not errors or bool(per), "errors": errors[:3], "items": _items(rrf_merge(per))[:KEEP], "tokens": tokens, "calls": len(per)}
    a["single"] = single
    return a


def cbm_answer(arm, q: str, own: str, members: list[dict], handles: dict) -> dict:
    per: list[list[dict]] = []
    tokens = 0
    single: dict = {"ok": False, "error": "own repository not indexed"}
    for m in members:
        h = handles.get(m["repo"])
        if h is None or not h.ok:
            continue
        res = arm._search(h, {"query": q}, ORG_LIMIT, text=False)
        if not res.ok:
            if m["repo"] == own:
                single = {"ok": False, "error": res.error}
            continue
        hits = [dict(x, repo=m["repo"]) for x in _norm_cbm_score(res.hits)]
        per.append(hits)
        txt = arm.agent_text("search_graph", {"project": h.state["project"], "query": q, "limit": EFF_LIMIT})
        if txt is not None:
            tokens += count_tokens(txt) or 0
        if m["repo"] == own:
            single = {"ok": True, "items": _items(hits)[:KEEP], "eff": _eff(arm.search_eff(h, q, EFF_LIMIT), own)}
    return {"per_repo": {"ok": bool(per), "items": _items(rrf_merge(per))[:KEEP], "tokens": tokens, "calls": len(per)}, "single": single}


def grep_member_stats(h, terms: set[str]) -> dict:
    """What the organization's BM25 rankings need from one member's grep
    index: its file and function-body indexes restricted to the run's query
    terms, and the brief of every function those terms reach."""
    st = h.state
    bodies = st["bodies"].restrict(terms)
    meta = {}
    for k in bodies.keys:
        f = st["funcs"][k]
        meta[k] = (f["path"], f["name"], f["start_line"], f["label"])
    return {"files": st["files"].restrict(terms), "bodies": bodies, "meta": meta}


def _grep_files(ranked: list[tuple[str, str, float]]) -> list[dict]:
    return [{"repo": r, "path": k, "score": s} for r, k, s in ranked]


def _grep_funcs(ranked: list[tuple[str, str, float]], stats: dict) -> list[dict]:
    out = []
    for r, k, s in ranked:
        path, name, line, label = stats[r]["meta"][k]
        out.append({"repo": r, "path": path, "name": name, "start_line": line, "label": label, "score": s})
    return out


def grep_answer(qtok: list[str], own: str, members: list[dict], stats: dict) -> dict:
    """Organization BM25 (union of the members), per-member BM25 + RRF, and
    the own member's BM25 for one query."""
    names = [m["repo"] for m in members if m["repo"] in stats]
    files_parts: list[tuple[str, TermStats]] = [(r, stats[r]["files"]) for r in names]
    body_parts: list[tuple[str, TermStats]] = [(r, stats[r]["bodies"]) for r in names]
    native = {
        "ok": True,
        "items": _items(_grep_funcs(union_search(body_parts, qtok, KEEP), stats))[:KEEP],
        "files": [[d["repo"], d["path"]] for d in _grep_files(union_search(files_parts, qtok, KEEP))],
    }
    per_files: list[list[dict]] = []
    per_funcs: list[list[dict]] = []
    single: dict = {"ok": False, "error": "own repository not indexed"}
    for (r, fs), (_, bs) in zip(files_parts, body_parts):
        fl = _grep_files(union_search([(r, fs)], qtok, KEEP))
        bl = _grep_funcs(union_search([(r, bs)], qtok, KEEP), stats)
        per_files.append(fl)
        per_funcs.append(bl)
        if r == own:
            single = {"ok": True, "items": _items(bl), "files": [[d["repo"], d["path"]] for d in fl], "eff": None}
    per = {
        "ok": bool(names),
        "items": _items(rrf_merge(per_funcs))[:KEEP],
        "files": [[d["repo"], d["path"]] for d in rrf_merge(per_files)][:KEEP],
        "tokens": None,
        "calls": len(names),
    }
    return {"native": native, "per_repo": per, "single": single}


# --- multi-repository run ----------------------------------------------------------------


def _max_rss_mb() -> dict:
    """Peak resident set of this process and of its finished children (MB)."""
    scale = 1 if sys.platform == "darwin" else 1024  # ru_maxrss: bytes on macOS, KiB on Linux
    me = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss * scale
    ch = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss * scale
    return {"bench": round(me / 2**20), "children": round(ch / 2**20)}


def _inst_record(inst: dict) -> dict:
    rec = {k: inst.get(k) for k in ("id", "repo", "language", "kind", "kinds", "pin", "base_commit", "query", "gold")}
    if inst.get("impact"):
        rec["impact"] = inst["impact"]
    return rec


def _member_record(m: dict) -> dict:
    return {k: m.get(k) for k in ("repo", "language", "commit", "split")}


def _done(split: str, arm: str, repos: set[str] | None) -> bool:
    p = run_path(f"multirepo-{split}", arm)
    if not p.exists():
        return False
    rec = read_json(p)
    return rec.get("status") == "ok" and rec.get("filter") == (sorted(repos) if repos else None)


def run_multirepo(arm_names: list[str], force: bool = False, only: list[str] | None = None) -> None:
    """The multi-repository scenario for every requested arm and split
    (`only`: see `parse_only`). Instances are selected once; each arm builds
    the gated organization, answers the gated instances, then grows it into
    the calibration organization and answers those."""
    splits, repos = parse_only(only)
    arms = []
    for name in ORG_ARMS:
        if name not in arm_names:
            continue
        arm = make_arm(name)
        if not arm.available()[0]:
            continue
        pending = [s for s in splits if force or not _done(s, name, repos)]
        if pending:
            arms.append((arm, pending))
    if not arms:
        return
    wanted = sorted({s for _, p in arms for s in p}, key=SPLITS.index)
    insts = {s: org_questions(s, repos) for s in wanted}
    for s in wanted:
        for inst in insts[s]:
            inst["query"] = queries_for(inst)["kw"]
        kinds = {k: sum(1 for i in insts[s] if k in i["kinds"]) for k in ("issue", "impact")}
        log(f"org multirepo {s}: {len(insts[s])} queries ({kinds['issue']} issue, {kinds['impact']} impact instances), {len(_filtered(org_members(s), repos))} members")
    for arm, pending in arms:
        t0 = time.perf_counter()
        try:
            recs = _multirepo_arm(arm, pending, insts, repos)
        except Exception as exc:
            recs = {s: {"status": "error", "error": f"{exc}\n{traceback.format_exc()[-1500:]}"} for s in pending}
        version = arm.version()
        for s, rec in recs.items():
            rec.update(split=s, arm=arm.name, arm_version=version, filter=sorted(repos) if repos else None)
            rec.setdefault("max_rss_mb", _max_rss_mb())
            write_json(run_path(f"multirepo-{s}", arm.name), rec)
        log(f"org multirepo {arm.name}: done in {time.perf_counter() - t0:.0f} s, peak RSS {_max_rss_mb()}")


def _multirepo_arm(arm, pending: list[str], insts: dict[str, list[dict]], repos: set[str] | None) -> dict[str, dict]:
    gated = _filtered(org_members("gated"), repos)
    extra = _filtered(org_repos("calibration"), repos) if "calibration" in pending else []
    if arm.name == "rueca":
        return _multirepo_rueca(arm, pending, insts, gated, extra)
    if arm.name == "grep":
        return _multirepo_grep(arm, pending, insts, gated, extra)
    return _multirepo_cbm(arm, pending, insts, gated, extra)


def _record(members: list[dict], insts: list[dict], index: dict, answers: dict, index_s: float, query_s: float, **extra: Any) -> dict:
    return {
        "status": "ok",
        "members": [_member_record(m) for m in members],
        "instances": [_inst_record(i) for i in insts],
        "index": index,
        "answers": answers,
        "seconds": {"index": round(index_s, 1), "queries": round(query_s, 1)},
        "max_rss_mb": _max_rss_mb(),
        **extra,
    }


def _multirepo_rueca(arm, pending: list[str], insts: dict[str, list[dict]], gated: list[dict], extra: list[dict]) -> dict[str, dict]:
    base = CACHE / "org"
    store, org_file = base / "rueca-store", base / "rueca-org.toml"
    cstore, corg = base / "rueca-calib-store", base / "rueca-calib-org.toml"
    shutil.rmtree(store, ignore_errors=True)
    store.mkdir(parents=True, exist_ok=True)
    out: dict[str, dict] = {}
    try:
        roots: list[Path] = []
        index: dict[str, dict] = {}
        t0 = time.perf_counter()
        add_org_members(arm, gated, store, org_file, roots, index)
        # re-index one repository of the full organization: the one with the
        # most symbol documents (the largest part to replace)
        incremental = None
        if gated:
            target = max(gated, key=lambda m: index.get(m["repo"], {}).get("docs") or 0)
            root = add_worktree(target["repo"], target["commit"], f"org-{target['repo']}")
            try:
                incremental = {"repo": target["repo"], **_org_index(arm, org_file, root, store)}
                incremental["first_repo_ms"] = index.get(target["repo"], {}).get("repo_ms")
            finally:
                remove_worktree(target["repo"], root)
        index_s = time.perf_counter() - t0
        if "gated" in pending:
            t1 = time.perf_counter()
            answers = {i["id"]: rueca_answer(arm, i["query"], i["repo"], store, org_file, gated, index) for i in insts["gated"]}
            out["gated"] = _record(gated, insts["gated"], index, answers, index_s, time.perf_counter() - t1, incremental=incremental, store=None)
        if "calibration" in pending:
            t0 = time.perf_counter()
            copied = copy_store(store, cstore)
            croots = list(roots)
            cindex: dict[str, dict] = {}
            add_org_members(arm, extra, cstore, corg, croots, cindex)
            index_s = time.perf_counter() - t0
            members = gated + extra
            ids = {**index, **cindex}
            t1 = time.perf_counter()
            answers = {i["id"]: rueca_answer(arm, i["query"], i["repo"], cstore, corg, members, ids) for i in insts["calibration"]}
            out["calibration"] = _record(members, insts["calibration"], cindex, answers, index_s, time.perf_counter() - t1, incremental=None, store={"copied_from": "gated", "seconds": round(copied, 2)})
    finally:
        for p in (store, cstore):
            shutil.rmtree(p, ignore_errors=True)
        for f in (org_file, corg):
            f.unlink(missing_ok=True)
    return out


def _multirepo_grep(arm, pending: list[str], insts: dict[str, list[dict]], gated: list[dict], extra: list[dict]) -> dict[str, dict]:
    qtok = {i["id"]: tokenize(i["query"]) for s in pending for i in insts[s]}
    terms = {t for toks in qtok.values() for t in toks}
    stats: dict[str, dict] = {}
    index: dict[str, dict] = {}
    secs: dict[str, float] = {}
    for group, members in (("gated", gated), ("calibration", extra)):
        t0 = time.perf_counter()
        for m in members:
            repo = m["repo"]
            root = add_worktree(repo, m["commit"], f"org-{repo}")
            try:
                h = arm.index(root, f"org-{repo}", m["language"])
                index[repo] = {"ok": h.ok, "seconds": h.index_seconds, "error": h.error, **h.info}
                if h.ok:
                    stats[repo] = grep_member_stats(h, terms)
                del h
            finally:
                remove_worktree(repo, root)
            log(f"org grep: {repo} indexed ({index[repo]['seconds']:.1f} s, peak RSS {_max_rss_mb()['bench']} MB)")
        secs[group] = time.perf_counter() - t0
    out: dict[str, dict] = {}
    for split in pending:
        members = gated if split == "gated" else gated + extra
        t1 = time.perf_counter()
        answers = {i["id"]: grep_answer(qtok[i["id"]], i["repo"], members, stats) for i in insts[split]}
        idx = {m["repo"]: index[m["repo"]] for m in (gated if split == "gated" else extra) if m["repo"] in index}
        ix_s = secs["gated"] if split == "gated" else secs["calibration"]
        out[split] = _record(members, insts[split], idx, answers, ix_s, time.perf_counter() - t1, incremental=None, store=None)
    return out


def _multirepo_cbm(arm, pending: list[str], insts: dict[str, list[dict]], gated: list[dict], extra: list[dict]) -> dict[str, dict]:
    handles: dict[str, Any] = {}
    out: dict[str, dict] = {}

    def add(members: list[dict]) -> tuple[dict, float]:
        t0 = time.perf_counter()
        idx = {}
        for m in members:
            repo = m["repo"]
            root = add_worktree(repo, m["commit"], f"org-{repo}")
            try:
                h = arm.index(root, f"org-{repo}", m["language"])
                handles[repo] = h
                idx[repo] = {"ok": h.ok, "seconds": h.index_seconds, "error": h.error}
            finally:
                remove_worktree(repo, root)
            log(f"org cbm: {repo} indexed ({idx[repo]['seconds']:.1f} s)")
        return idx, time.perf_counter() - t0

    try:
        index, index_s = add(gated)
        if "gated" in pending:
            t1 = time.perf_counter()
            answers = {i["id"]: cbm_answer(arm, i["query"], i["repo"], gated, handles) for i in insts["gated"]}
            out["gated"] = _record(gated, insts["gated"], index, answers, index_s, time.perf_counter() - t1, incremental=None, store=None)
        if "calibration" in pending:
            cindex, cindex_s = add(extra)
            members = gated + extra
            t1 = time.perf_counter()
            answers = {i["id"]: cbm_answer(arm, i["query"], i["repo"], members, handles) for i in insts["calibration"]}
            out["calibration"] = _record(members, insts["calibration"], cindex, answers, cindex_s, time.perf_counter() - t1, incremental=None, store=None)
    finally:
        for h in handles.values():
            arm.cleanup(h)
    return out


# --- scoring -------------------------------------------------------------------------


def _own(r: str | None, repo: str) -> bool:
    """A hit's repository (`acme/x` or a Rueca id `github.com/acme/x`) is the
    instance's own."""
    return r is not None and r.endswith(repo)


def ranked_files(items: list[list], files: list[list] | None = None) -> list[tuple]:
    """The distinct (repository, path) candidate files of an org ranking, in
    rank order: the files of `items`, or of `files` (a separate file ranking,
    grep) when given."""
    ranked: list[tuple] = []
    seen: set[tuple] = set()
    for r, path, *_ in items if files is None else files:
        if any_candidate(path) and (r, path) not in seen:
            seen.add((r, path))
            ranked.append((r, path))
    return ranked


def _acc(items: list[list], repo: str, gold: dict, files: list[list] | None = None) -> dict:
    """File Acc@5 and Function Acc@10 of an org ranking (repository-qualified).
    `files`: a separate file ranking ([repo, path], grep) scored at file level
    instead of the files of `items`; the other-repository share then reads it
    too."""
    ranked = ranked_files(items, files)
    funcs: list[dict] = []
    for r, path, name, line, label in items:
        if not any_candidate(path):
            continue
        if label in ("Function", "Method", "Test", "Constructor") or (label or "").endswith(("Function", "Method")):
            funcs.append({"repo": r, "path": path, "name": name, "start_line": line})
    gfiles = set(gold["files"])
    f5 = any(_own(r, repo) and p in gfiles for r, p in ranked[:5])
    seen: list[dict] = []
    keys: set[tuple] = set()
    for f in funcs:
        k = (f["repo"], f["path"], f["name"], f["start_line"])
        if k not in keys:
            keys.add(k)
            seen.append(f)
    fn10 = any(_own(f["repo"], repo) and any(same_def(f, g) for g in gold["functions"]) for f in seen[:10]) if gold["functions"] else None
    top10 = [tuple(x) for x in files[:10]] if files is not None else [tuple(it[:2]) for it in items[:10]]
    other = sum(1 for it in top10 if not _own(it[0], repo))
    return {"file_acc@5": float(f5), "func_acc@10": None if fn10 is None else float(fn10), "other_repo_top10": other / len(top10) if top10 else None}


def eff_hit(eff: dict | None, repo: str, gold: dict) -> tuple[int, int | None]:
    """(1-based rank of the first item of an efficiency answer in a gold file
    of the instance's own repository, 0 if none; tokens read until it — the
    whole answer when none)."""
    if not eff or not eff.get("ok"):
        return 0, None
    gfiles = set(gold["files"])
    prefix = eff.get("prefix") or []
    for i, (r, path, *_rest) in enumerate(eff.get("items") or [], 1):
        if r is not None and (r == repo or r.endswith("/" + repo)) and path in gfiles:
            return i, prefix[i - 1] if i - 1 < len(prefix) else eff.get("tokens")
    return 0, eff.get("tokens")


def org_layer_stats(index: dict, incremental: dict | None) -> dict | None:
    """RFC 0006 §8 incremental update: the organization-layer time of every
    `index --org` step (one repository added) and of the re-index of one
    repository, against that repository's own index time."""
    steps = [v for v in index.values() if v.get("ok") and v.get("org_ms") is not None and v.get("repo_ms")]
    if not steps:
        return None
    ratios = [v["org_ms"] / v["repo_ms"] for v in steps]
    out: dict = {
        "steps": len(steps),
        "org_ms_median": _median([v["org_ms"] for v in steps]),
        "org_ms_max": max(v["org_ms"] for v in steps),
        "repo_ms_median": _median([v["repo_ms"] for v in steps]),
        "ratio_median": _median(ratios),
        "ratio_max": max(ratios),
    }
    if incremental and incremental.get("ok"):
        inc = {k: incremental.get(k) for k in ("repo", "docs", "repo_ms", "first_repo_ms", "org_ms", "org_repos", "replaced", "bytes_copied")}
        inc["ratio"] = incremental["org_ms"] / incremental["repo_ms"] if incremental.get("repo_ms") else None
        inc["ratio_first"] = incremental["org_ms"] / incremental["first_repo_ms"] if incremental.get("first_repo_ms") else None
        out["replace"] = inc
    return out


def multirepo_row(inst: dict, mode: str, m: dict, single: dict | None) -> dict:
    """One scored row: an instance's answer in one mode against the same
    arm's single-repository answer (module docstring, *Scoring*)."""
    repo, gold = inst["repo"], inst["gold"]
    acc = _acc(m["items"], repo, gold, m.get("files"))
    single = single if single and single.get("ok") else None
    sacc = _acc(single["items"], repo, gold, single.get("files")) if single else {}
    native = mode == "native"
    tokens = (m.get("eff") or {}).get("tokens") if native else m.get("tokens")
    fh, pf = eff_hit(m.get("eff"), repo, gold) if native and m.get("eff") else (None, None)
    se = (single or {}).get("eff")
    sfh, spf = eff_hit(se, repo, gold) if se else (None, None)
    row = {
        "id": inst["id"],
        "repo": repo,
        "language": inst.get("language"),
        "kind": inst.get("kind", "issue"),
        **acc,
        "single_file_acc@5": sacc.get("file_acc@5"),
        "single_func_acc@10": sacc.get("func_acc@10"),
        "tokens": tokens,
        "single_tokens": se.get("tokens") if se and se.get("ok") else None,
        "fh": fh,
        "pf": pf,
        "single_fh": sfh,
        "single_pf": spf,
        "top10": m["items"][:10],
    }
    if m.get("files") is not None:
        row["top10_files"] = m["files"][:10]
    return row


def multirepo_aggregate(rows: list[dict]) -> dict:
    """Per-mode aggregates over scored rows (the published shape)."""
    fa = [r["file_acc@5"] for r in rows]
    sa = [r["single_file_acc@5"] for r in rows if r["single_file_acc@5"] is not None]
    fn = [r["func_acc@10"] for r in rows if r["func_acc@10"] is not None]
    sfn = [r["single_func_acc@10"] for r in rows if r["single_func_acc@10"] is not None]
    tk = [r["tokens"] for r in rows if r["tokens"] is not None]
    st = [r["single_tokens"] for r in rows if r["single_tokens"] is not None]
    ot = [r["other_repo_top10"] for r in rows if r["other_repo_top10"] is not None]

    def med(xs: list) -> Any:
        return sorted(xs)[len(xs) // 2] if xs else None

    # tokens per correct answer over the queries both answers exist for
    paired = [r for r in rows if r["fh"] is not None and r["tokens"] is not None and r["single_tokens"] is not None and r["single_fh"] is not None]
    found = sum(1 for r in paired if r["fh"])
    sfound = sum(1 for r in paired if r["single_fh"])
    tpc = sum(r["tokens"] for r in paired) / found if found else None
    stpc = sum(r["single_tokens"] for r in paired) / sfound if sfound else None
    fa_org = sum(fa) / len(fa) if fa else None
    fa_single = sum(sa) / len(sa) if sa else None
    return {
        "status": "ok",
        "n": len(rows),
        "file_acc@5": fa_org,
        "single_file_acc@5": fa_single,
        "func_acc@10": sum(fn) / len(fn) if fn else None,
        "single_func_acc@10": sum(sfn) / len(sfn) if sfn else None,
        "n_func": len(fn),
        "other_repo_top10": sum(ot) / len(ot) if ot else None,
        "tokens_median": med(tk),
        "single_tokens_median": med(st),
        "file_acc@5_ratio": fa_org / fa_single if fa_org is not None and fa_single else None,
        "tokens_per_correct": tpc,
        "single_tokens_per_correct": stpc,
        "tokens_per_correct_ratio": tpc / stpc if tpc is not None and stpc else None,
        "found_top10": found if paired else None,
        "single_found_top10": sfound if paired else None,
        "rows": rows,
    }


def consumer_metrics(items: list[list], consumers: list[dict], files: list[list] | None = None) -> dict:
    """Consumer side of an impact instance: whether a consumer gold file
    (repository-qualified) is among the first 10 / 20 distinct candidate
    files of the ranking, and the share of the consumer gold files found in
    the first 20 (denominator capped at 20)."""
    want: dict[str, set[str]] = {}
    for c in consumers:
        want.setdefault(c["repo"], set()).add(c["path"])
    total = sum(len(v) for v in want.values())
    hits = [any(_own(r, repo) and p in paths for repo, paths in want.items()) for r, p in ranked_files(items, files)[:20]]
    return {
        "consumer_hit@10": float(any(hits[:10])),
        "consumer_hit@20": float(any(hits)),
        "consumer_recall@20": sum(hits) / min(total, 20) if total else None,
        "consumer_files": total,
        "consumer_repos": len(want),
    }


def impact_row(inst: dict, mode: str, m: dict, single: dict | None) -> dict:
    """An impact instance's row: the issue row of the same answer (own side)
    plus the consumer metrics (`consumer_metrics`) of the mode's ranking and
    of the single answer (which holds the own repository only, so its
    consumer hits are 0 by construction)."""
    row = multirepo_row(inst, mode, m, single)
    cons = inst["impact"]["consumers"]
    row["kind"] = "impact"
    row.update(consumer_metrics(m["items"], cons, m.get("files")))
    return row


def _rate(values: list[float]) -> dict:
    """Mean of 0/1 outcomes with its Wilson interval and n."""
    n = len(values)
    k = sum(values)
    return {"value": k / n if n else None, "ci": wilson(k, n), "n": n}


def impact_aggregate(rows: list[dict]) -> dict:
    """Per-mode aggregates of impact rows: own side (File Acc@5, Function
    Acc@10) and consumer side (hit@10, hit@20: Wilson intervals; recall@20:
    mean), the other-repository share, consumer gold sizes."""
    fn = [r["func_acc@10"] for r in rows if r["func_acc@10"] is not None]
    rc = [r["consumer_recall@20"] for r in rows if r["consumer_recall@20"] is not None]
    ot = [r["other_repo_top10"] for r in rows if r["other_repo_top10"] is not None]
    out: dict = {"status": "ok", "n": len(rows), "n_func": len(fn)}
    out["file_acc@5"] = _rate([r["file_acc@5"] for r in rows])
    out["func_acc@10"] = _rate(fn)
    out["consumer_hit@10"] = _rate([r["consumer_hit@10"] for r in rows])
    out["consumer_hit@20"] = _rate([r["consumer_hit@20"] for r in rows])
    out["consumer_recall@20"] = sum(rc) / len(rc) if rc else None
    out["other_repo_top10"] = sum(ot) / len(ot) if ot else None
    out["consumer_files_median"] = _median([r["consumer_files"] for r in rows]) if rows else None
    out["rows"] = rows
    return out


def instance_kinds(inst: dict) -> list[str]:
    """Kinds an instance is scored as (`kinds`; older outputs: `kind`)."""
    return inst.get("kinds") or [inst.get("kind") or "issue"]


def score_arm(rec: dict, kind: str = "issue") -> dict:
    """Modes of one arm's raw multi-repository output, over the instances of
    one kind (`issue`: `multirepo_row` / `multirepo_aggregate`; `impact`:
    `impact_row` / `impact_aggregate`)."""
    insts = {i["id"]: i for i in rec["instances"] if kind in instance_kinds(i)}
    row_fn, agg_fn = (impact_row, impact_aggregate) if kind == "impact" else (multirepo_row, multirepo_aggregate)
    modes: dict = {}
    for mode in ("native", "per_repo"):
        rows = []
        pending = None
        for iid, a in rec["answers"].items():
            m = a.get(mode)
            if m is None or iid not in insts:
                continue
            if not m.get("ok"):
                pending = str(m.get("error")).split("; indexed:")[0]
                continue
            rows.append(row_fn(insts[iid], mode, m, a.get("single")))
        if not rows and pending:
            modes[mode] = {"status": "pending", "reason": str(pending)[:300]}
        elif rows:
            modes[mode] = agg_fn(rows)
    return modes


def instance_counts(instances: list[dict], members: list[dict]) -> dict:
    """Language group → {members, issue / impact: {n, patch_at_pin: {state:
    n}}} of a raw output's instances (`patch_at_pin`: `base` = pin is the
    instance's own base commit; `applies` = the fix is not at the pin;
    `neither` = the code moved on, the fix neither applies nor reverts)."""
    out: dict[str, dict] = {}
    for m in members:
        g = out.setdefault(lang_group(m["language"]), {"members": 0})
        g["members"] += 1
    for i in instances:
        g = out.setdefault(lang_group(i["language"]), {"members": 0})
        for k in instance_kinds(i):
            c = g.setdefault(k, {"n": 0, "patch_at_pin": {}})
            c["n"] += 1
            state = (i.get("gold") or {}).get("patch_at_pin") or "unknown"
            c["patch_at_pin"][state] = c["patch_at_pin"].get(state, 0) + 1
    return out


def population(rec: dict) -> str:
    """sha256 of what a raw output asked: its members and pins, and its
    instance ids with their kinds (scores of two outputs with the same
    population are comparable and may be carried across runs)."""
    key = {"members": [[m["repo"], m.get("commit")] for m in rec.get("members") or []], "instances": [[i["id"], instance_kinds(i)] for i in rec.get("instances") or []]}
    return hashlib.sha256(json.dumps(key, sort_keys=True).encode()).hexdigest()


def score_split(split: str) -> dict:
    out: dict = {"split": split, "instances": 0, "repos": [], "instance_repos": [], "arms": {}}
    for arm in ORG_ARMS:
        p = run_path(f"multirepo-{split}", arm)
        if not p.exists():
            continue
        rec = read_json(p)
        if rec.get("status") != "ok":
            out["arms"][arm] = {"status": rec.get("status"), "error": str(rec.get("error"))[:300]}
            continue
        if not out["repos"]:
            issues = [i for i in rec["instances"] if "issue" in instance_kinds(i)]
            out["repos"] = [m["repo"] for m in rec["members"]]
            out["instances"] = len(issues)
            out["impact_instances"] = sum(1 for i in rec["instances"] if "impact" in instance_kinds(i))
            out["instance_repos"] = sorted({i["repo"] for i in issues})
            out["filter"] = rec.get("filter")
            out["counts"] = instance_counts(rec["instances"], rec["members"])
            out["population"] = population(rec)
        elif population(rec) != out["population"]:
            out["arms"][arm] = {"status": "stale", "error": "asked another population than the other arms (re-run it)"}
            continue
        a = {"status": "ok", "modes": score_arm(rec), "index_seconds": sum((v.get("seconds") or 0) for v in rec["index"].values()), "version": rec.get("arm_version")}
        impact = score_arm(rec, "impact")
        if impact:
            a["impact"] = impact
        layer = org_layer_stats(rec["index"], rec.get("incremental"))
        if layer:
            a["org_layer"] = layer
        out["arms"][arm] = a
    return out


def score_multirepo() -> dict:
    """Per split, arm and mode: accuracy under the org view vs the same arm's
    single-repository answer, and tokens. The gated split at top level, the
    calibration split under `calibration`."""
    gated = score_split("gated")
    cal = score_split("calibration")
    return {**gated, "calibration": cal if cal["arms"] else {}}


# --- multi-ref ---------------------------------------------------------------------------

_GIT_ENV = {
    "GIT_AUTHOR_NAME": "rueca-bench",
    "GIT_AUTHOR_EMAIL": "bench@invalid",
    "GIT_COMMITTER_NAME": "rueca-bench",
    "GIT_COMMITTER_EMAIL": "bench@invalid",
    "GIT_AUTHOR_DATE": "2026-01-01T00:00:00Z",
    "GIT_COMMITTER_DATE": "2026-01-01T00:00:00Z",
}


def multiref_instance() -> dict | None:
    iid = load_manifest().get("multiref", {}).get("instance")
    return next((i for i in s1_instances() if i["id"] == iid), None)


def _body(src: str, f: dict) -> str:
    lines = src.splitlines()
    return "\n".join(lines[f["start"] - 1 : f["end"]])


def collapse_gold(files: dict[str, tuple[str | None, str | None]], language: str, unchanged_paths: list[str]) -> dict:
    """Gold of the multi-ref scenario from the sources of the changed files
    (path -> (base text or None, fix text or None)) and the list of unchanged
    files of the language. Symbol key = (path, qualified name); classes:
    `identical` (unchanged file: one item in both refs), `changed` (content
    differs), `added`, `removed`, `same_in_changed_file` (identical text in a
    file whose blob changed)."""
    symbols: dict[str, dict] = {}
    for path, (old, new) in sorted(files.items()):
        if grammar_for(path, language) is None:
            continue
        a = {f.qualname: f.as_dict() for f in extract_functions(path, old.encode(), language)} if old is not None else {}
        b = {f.qualname: f.as_dict() for f in extract_functions(path, new.encode(), language)} if new is not None else {}
        for qn in sorted(set(a) | set(b)):
            if qn in a and qn in b:
                cls = "changed" if _body(old, a[qn]) != _body(new, b[qn]) else "same_in_changed_file"
            elif qn in b:
                cls = "added"
            else:
                cls = "removed"
            f = b.get(qn) or a.get(qn)
            symbols[f"{path}#{qn}"] = {"path": path, "qn": qn, "name": f["name"], "class": cls}
    return {"symbols": symbols, "unchanged_files": sorted(unchanged_paths)}


def score_collapse(items: list[dict], gold: dict, base: int, fix: int) -> dict:
    """Score a multi-ref search answer: items {path, qn, name, in: [snapshot
    indexes], changed} against `collapse_gold`; `base` / `fix` are the
    snapshot indexes of the two refs. Counts:
    identical_ok / identical (one item, both refs, not changed, no duplicate),
    changed_flag tp / fp / fn, in_ok / in_total (added -> [fix], removed ->
    [base], changed versions one ref each). A symbol unchanged inside a
    changed file that is flagged counts as `changed_same_file`, not as a
    false positive: Rueca compared versions by file blob until it compared
    each symbol's own text (RFC 0006 §9.3), so it is 0 from then on."""
    by_key: dict[tuple, list[dict]] = {}
    for it in items:
        by_key.setdefault((it.get("path"), it.get("qn")), []).append(it)
    unchanged = set(gold["unchanged_files"])
    sy = {(s["path"], s["qn"]): s for s in gold["symbols"].values()}
    c = {"identical": 0, "identical_ok": 0, "changed_tp": 0, "changed_fp": 0, "changed_fn": 0, "changed_same_file": 0, "in_total": 0, "in_ok": 0, "unknown": 0}
    for key, its in by_key.items():
        path, qn = key
        g = sy.get(key)
        if g is None and path in unchanged:
            c["identical"] += 1
            ok = len(its) == 1 and sorted(its[0].get("in") or []) == sorted([base, fix]) and not its[0].get("changed")
            c["identical_ok"] += int(ok)
            c["changed_fp"] += sum(1 for i in its if i.get("changed"))
            continue
        if g is None:
            c["unknown"] += 1
            continue
        flagged = any(i.get("changed") for i in its)
        if g["class"] == "changed":
            c["changed_tp" if flagged else "changed_fn"] += 1
            for i in its:
                c["in_total"] += 1
                c["in_ok"] += int(len(i.get("in") or []) == 1)
        else:
            c["changed_same_file" if g["class"] == "same_in_changed_file" else "changed_fp"] += int(flagged)
            want = {"added": [[fix]], "removed": [[base]], "same_in_changed_file": [[base, fix], [base], [fix]]}[g["class"]]
            for i in its:
                c["in_total"] += 1
                c["in_ok"] += int(sorted(i.get("in") or []) in [sorted(w) for w in want])
    return c


def run_multiref(arm_names: list[str], force: bool = False) -> None:
    inst = multiref_instance()
    if inst is None or "rueca" not in arm_names:
        return
    arm = make_arm("rueca")
    if not arm.available()[0]:
        return
    p = run_path("multiref", "rueca")
    if p.exists() and not force and read_json(p).get("status") == "ok":
        return
    log(f"org multiref rueca: {inst['id']}")
    try:
        rec = _multiref(arm, inst)
    except Exception as exc:
        rec = {"status": "error", "error": f"{exc}\n{traceback.format_exc()[-1500:]}"}
    rec["arm_version"] = arm.version()
    write_json(p, rec)


def _multiref(arm, inst: dict) -> dict:
    patch = instance_row(inst)["patch"]
    lang = inst["language"]
    s1_gold(inst)
    base_root = add_worktree(inst["repo"], inst["base_commit"], f"mr-base-{inst['id']}")
    fix_root = add_worktree(inst["repo"], inst["base_commit"], f"mr-fix-{inst['id']}")
    store = CACHE / "org" / "multiref-store"
    shutil.rmtree(store, ignore_errors=True)
    store.mkdir(parents=True, exist_ok=True)
    try:
        pf = fix_root.parent / f"mr-{safe_name(inst['id'])}.patch"
        pf.write_text(patch)
        res = run(["git", "apply", "--whitespace=nowarn", str(pf)], cwd=fix_root, timeout=120)
        pf.unlink(missing_ok=True)
        if res.rc != 0:
            return {"status": "error", "error": f"git apply failed: {res.stderr[-300:]}"}
        run(["git", "add", "-A"], cwd=fix_root, check=True)
        run(["git", "-c", "commit.gpgsign=false", "commit", "-q", "-m", f"rueca-bench fix of {inst['id']}"], cwd=fix_root, env=_GIT_ENV, check=True)
        fix_commit = run(["git", "rev-parse", "HEAD"], cwd=fix_root).stdout.strip()
        changed: dict[str, tuple[str | None, str | None]] = {}
        for fp in parse_patch(patch):
            for path in {fp.old_path, fp.new_path} - {None}:
                if not any_candidate(path):
                    continue
                old = git_show(inst["repo"], inst["base_commit"], fp.old_path) if fp.old_path else None
                newp = fix_root / fp.new_path if fp.new_path else None
                new = newp.read_text(errors="replace") if newp is not None and newp.exists() else None
                changed[path] = (old, new)
        files = run(["git", "ls-files"], cwd=base_root).stdout.split()
        unchanged = [f for f in files if candidate_path(f, lang) and f not in changed]
        gold = collapse_gold(changed, lang, unchanged)
        # queries: every changed / added / removed callable, plus a fixed
        # sample of names defined only in unchanged files (evenly spaced in
        # sorted order)
        names = sorted({s["name"] for s in gold["symbols"].values() if s["class"] in ("changed", "added", "removed")})
        every = sorted({fn.name for f in unchanged for fn in extract_functions(f, (base_root / f).read_bytes(), lang)} - set(names))
        sample = every[:: max(1, len(every) // MULTIREF_SAMPLE)][:MULTIREF_SAMPLE]
        refs = {"base": inst["base_commit"][:12], "fix": fix_commit[:12]}
        idx = {}
        for name, extra in (("base", []), ("fix", ["--ref", fix_commit])):
            r = run([arm.bin, "index", str(base_root), *extra, "--store", str(store), "--json"], timeout=arm.index_timeout)
            idx[name] = {"ok": r.rc == 0, "seconds": r.seconds, **_index_stats(r.stdout)}
            if r.rc != 0:
                return {"status": "error", "error": f"index {name} failed: {r.stderr[-300:]}"}
        queries = []
        for q in names + sorted(set(sample)):
            entry: dict = {"query": q}
            singles = {}
            for name, ref in refs.items():
                args = {"query": q, "limit": EFF_LIMIT, "ref": ref}
                data, _, err, raw = arm.call("search_symbols", args, base_root, store, mode="agent")
                tokens, _ = arm.agent_tokens(None, "search_symbols", args, raw, root=base_root, store=store, mode="agent") if not err else (None, None)
                singles[name] = {"ok": not err, "tokens": tokens}
            entry["single"] = singles
            data, _, err, raw = arm.call(
                "search_symbols", {"query": q, "limit": EFF_LIMIT, "ref": [inst["base_commit"][:12], fix_commit[:12]]}, base_root, store, mode="agent"
            )
            if err:
                entry["multi"] = {"ok": False, "error": err}
            else:
                snaps = [s.get("commit", "") for s in data.get("snapshots") or []]
                b = next((i for i, c in enumerate(snaps) if c.startswith(inst["base_commit"][:12])), 0)
                f_ = next((i for i, c in enumerate(snaps) if c.startswith(fix_commit[:12])), 1)
                items = [{"path": h["path"], "qn": h["qualified_name"], "name": h["name"], "in": h.get("in"), "changed": h.get("changed")} for h in flat_results(data)]
                tokens, _ = arm.agent_tokens(None, "search_symbols", {"query": q, "limit": EFF_LIMIT, "ref": [inst["base_commit"][:12], fix_commit[:12]]}, raw, root=base_root, store=store, mode="agent")
                entry["multi"] = {"ok": True, "tokens": tokens, "score": score_collapse(items, gold, b, f_)}
            queries.append(entry)
        return {
            "status": "ok",
            "instance": inst["id"],
            "repo": inst["repo"],
            "base_commit": inst["base_commit"],
            "fix_commit": fix_commit,
            "index": idx,
            "gold": {"classes": _class_counts(gold), "changed_files": sorted(changed), "unchanged_files": len(unchanged)},
            "queries": queries,
        }
    finally:
        remove_worktree(inst["repo"], base_root)
        remove_worktree(inst["repo"], fix_root)
        shutil.rmtree(store, ignore_errors=True)


def _index_stats(stdout: str) -> dict:
    """Files extracted and facts-cache hits of an `index --json` report (a
    ref report nests the index report under `index`)."""
    try:
        rep = json.loads(stdout)
    except json.JSONDecodeError:
        return {}
    rep = rep.get("index") or rep
    return {k: rep.get(k) for k in ("files_extracted", "facts_cache_hits", "duration_ms")}


def _class_counts(gold: dict) -> dict:
    out: dict[str, int] = {}
    for s in gold["symbols"].values():
        out[s["class"]] = out.get(s["class"], 0) + 1
    return out


def score_multiref() -> dict:
    p = run_path("multiref", "rueca")
    if not p.exists():
        return {}
    rec = read_json(p)
    if rec.get("status") != "ok":
        return {"status": rec.get("status"), "error": str(rec.get("error"))[:300]}
    qs = rec["queries"]
    single = [q["single"]["base"]["tokens"] + q["single"]["fix"]["tokens"] for q in qs if q["single"]["base"].get("tokens") is not None and q["single"]["fix"].get("tokens") is not None]
    multi_ok = [q for q in qs if (q.get("multi") or {}).get("ok")]
    out: dict = {
        "instance": rec["instance"],
        "repo": rec["repo"],
        "base_commit": rec["base_commit"],
        "fix_commit": rec["fix_commit"],
        "gold": rec["gold"],
        "queries": len(qs),
        "single_tokens_sum": sum(single),
        "version": rec.get("arm_version"),
    }
    if not multi_ok:
        errs = sorted({str((q.get("multi") or {}).get("error"))[:200] for q in qs})
        out["status"] = "pending"
        out["reason"] = "the binary rejected the multi-ref query (`ref` list over a ref snapshot, RFC 0006 §9.3)"
        out["errors"] = errs[:3]
        return out
    tot: dict[str, int] = {}
    for q in multi_ok:
        for k, v in q["multi"]["score"].items():
            tot[k] = tot.get(k, 0) + v
    paired_single = [q["single"]["base"]["tokens"] + q["single"]["fix"]["tokens"] for q in multi_ok]
    out.update(
        status="ok",
        counts=tot,
        multi_tokens_sum=sum(q["multi"]["tokens"] or 0 for q in multi_ok),
        paired_single_tokens_sum=sum(paired_single),
    )
    return out


def run_all(arm_names: list[str], force: bool = False, only: list[str] | None = None) -> None:
    """`run --suite org`. `only` (`parse_only`) restricts the multi-repository
    scenario; the multi-ref and cross-repository scenarios run only without it."""
    run_multirepo(arm_names, force, only)
    if only:
        log("org: --only given, multi-ref and cross-repository scenarios skipped")
        return
    run_multiref(arm_names, force)
    run_crossrepo(arm_names, force)


# --- routing calibration (RFC 0004 §7.7) ---------------------------------------------------

CALIBRATION_ROUTES = [
    "off",
    "prior,alpha=0.5",
    "prior,alpha=1",
    "prior,alpha=2",
    "prior,alpha=3",
    "prior,alpha=2,mu=0",
    "prior,alpha=2,mu=1",
    "prior,alpha=2,mu=0.25",
    "prior,alpha=2,mu=0.75",
    "prior,alpha=2,eps=0.1",
    "prior,alpha=2,eps=0.5",
    "top,top=1",
    "top,top=2,ratio=0.5",
    "top,top=3,ratio=0.3",
]


def run_calibration(routes: list[str] | None = None, force: bool = False) -> dict:
    """Tunes repository routing on the calibration split only (never on the
    gated instances). The organization is the multi-repository scenario's
    calibration organization (`org_members("calibration")`: the gated
    members as distractors — none of their instances is asked — plus the
    calibration members, each at its pin) and the instances are
    `org_instances("calibration")`, scored against their gold at the pin.
    Every instance's keyword query is asked of the organization (`--org`, no
    `repo`) under each routing setting (`RUECA_ORG_ROUTE`), and of its own
    repository alone (`repo`, no `--org`: the repository's own statistics).
    Scored like the multi-repository scenario: File Acc@5 with
    repository-qualified paths, tokens of the top-10 answer, and the
    organization / single ratio of each. The store is kept between sweeps
    (.cache/org/calib-store). Raw output: .cache/runs/org/calibration-rueca.json."""
    arm = make_arm("rueca")
    if not arm.available()[0]:
        raise SystemExit("RUECA_BIN is not set")
    cal = org_instances("calibration")
    members = org_members("calibration")
    store = CACHE / "org" / "calib-store"
    org_file = CACHE / "org" / "calib-org.toml"
    state_path = CACHE / "org" / "calib-index.json"
    if force:
        shutil.rmtree(store, ignore_errors=True)
        state_path.unlink(missing_ok=True)
    store.mkdir(parents=True, exist_ok=True)
    state = read_json(state_path) if state_path.exists() else {"roots": [], "index": {}}
    roots = [Path(r) for r in state["roots"]]

    def save() -> None:
        state["roots"] = [str(r) for r in roots]
        write_json(state_path, state)

    add_org_members(arm, members, store, org_file, roots, state["index"], prefix="calib", name="calib", skip=lambda repo: bool(state["index"].get(repo, {}).get("ok")), on_step=save)
    rids = {repo: v.get("repo_id") for repo, v in state["index"].items()}
    out: dict = {"instances": [i["id"] for i in cal], "members": [m["repo"] for m in members], "single": {}, "routes": {}}
    for inst in cal:
        q = queries_for(inst)["kw"]
        rid = rids.get(inst["repo"])
        data, _, err, _ = arm.call("search_symbols", {"query": q, "limit": ORG_LIMIT, "repo": rid}, None, store)
        items = [] if err else [[inst["repo"], h.get("path"), h.get("name"), h.get("start_line"), h.get("label")] for h in flat_results(data)]
        eff = arm.search_eff(None, q, EFF_LIMIT, root=None, store=store, repo=rid)
        out["single"][inst["id"]] = {"acc": _acc(items[:KEEP], inst["repo"], inst["gold"]), "eff": _eff(eff)}
    saved = os.environ.get("RUECA_ORG_ROUTE")
    try:
        for route in routes or CALIBRATION_ROUTES:
            os.environ["RUECA_ORG_ROUTE"] = route
            rows = {}
            for inst in cal:
                q = queries_for(inst)["kw"]
                data, _, err, _ = arm.call("search_symbols", {"query": q, "limit": ORG_LIMIT}, None, store, org=org_file)
                items = [] if err else _items(flat_results(data))[:KEEP]
                eff = arm.search_eff(None, q, EFF_LIMIT, root=None, store=store, org=org_file)
                rows[inst["id"]] = {"acc": _acc(items, inst["repo"], inst["gold"]), "eff": _eff(eff)}
            out["routes"][route] = rows
            log(f"calibration {route}: {_calibration_summary(out, route, cal)}")
    finally:
        if saved is None:
            os.environ.pop("RUECA_ORG_ROUTE", None)
        else:
            os.environ["RUECA_ORG_ROUTE"] = saved
    out["summary"] = {r: _calibration_summary(out, r, cal) for r in out["routes"]}
    write_json(run_path("calibration", "rueca"), out)
    return out


def _calibration_summary(out: dict, route: str, cal: list[dict]) -> dict:
    insts = {i["id"]: i for i in cal}
    rows = out["routes"][route]
    n = len(rows)
    fa = sum(r["acc"]["file_acc@5"] for r in rows.values()) / n if n else None
    sfa = sum(out["single"][i]["acc"]["file_acc@5"] for i in rows) / n if n else None
    found = tok = sfound = stok = 0
    for iid, r in rows.items():
        gold = insts[iid]["gold"]
        fh, _ = eff_hit(r["eff"], insts[iid]["repo"], gold)
        sfh, _ = eff_hit(out["single"][iid]["eff"], insts[iid]["repo"], gold)
        found += bool(fh)
        sfound += bool(sfh)
        tok += r["eff"].get("tokens") or 0
        stok += out["single"][iid]["eff"].get("tokens") or 0
    other = [r["acc"]["other_repo_top10"] for r in rows.values() if r["acc"]["other_repo_top10"] is not None]
    tpc = tok / found if found else None
    stpc = stok / sfound if sfound else None
    return {
        "n": n,
        "file_acc@5": fa,
        "single_file_acc@5": sfa,
        "ratio": fa / sfa if fa is not None and sfa else None,
        "other_repo_top10": sum(other) / len(other) if other else None,
        "tokens_per_correct": tpc,
        "single_tokens_per_correct": stpc,
        "tokens_ratio": tpc / stpc if tpc and stpc else None,
    }


if __name__ == "__main__":  # pragma: no cover - manual calibration entry point
    if len(sys.argv) >= 2 and sys.argv[1] == "calibrate":
        routes = [a for a in sys.argv[2:] if a != "--force"]
        res = run_calibration(routes or None, force="--force" in sys.argv)
        print(json.dumps(res["summary"], indent=1))
