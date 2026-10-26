"""Org-path scenarios (RFC 0005 §5, §6, §8): several repositories in one
store, and one repository at two commits.

**Multi-repository** (manifest `[org]`). The first gated S1 instance of the
first `repos_per_family` gated S1 repositories of every language family
(manifest order) — each repository at that instance's base commit — are
indexed into one store (Rueca: one `--store`, one root per repository; cbm:
one project per repository, its own cache). Each instance's keyword query is
then asked of the whole organization:

- Rueca **native**: one `search_symbols` with `repo: "*"` (RFC 0005 §5: rank
  fusion across the locally indexed roots); pending when the binary rejects
  `"*"`;
- Rueca and cbm **per repository**: one search per repository (`repo:` id /
  `project`), merged by the bench with reciprocal rank fusion (k = 60; one
  item per repository at each rank, ties broken by the raw per-repository
  score) — what an agent without an org query has to do; cbm has no
  cross-project search.

Localization is scored like S1 (File Acc@5, Function Acc@10) with paths
qualified by repository and the candidate filter of every language, and
compared, per instance, with the same arm's single-repository S1 answer.
Tokens: the efficiency query (top 10) of the org view against the
single-repository one; the per-repository mode costs the sum of its calls.

**Multi-ref** (manifest `[multiref]`). One S1 instance's repository at its
base commit and at a fix commit (the base plus the instance's gold patch,
committed with a fixed identity and date, so the commit id is deterministic),
both indexed into one store. Gold (`collapse_gold`): per symbol of the
patched language, whether it is identical in both refs (its file blob is the
same: one item, both snapshots), changed (content differs: `changed: true`),
added, removed, or unchanged inside a changed file. Queries: the names of
the changed / added / removed functions plus a deterministic sample of
unchanged ones, each a `search_symbols` over both refs; `score_collapse`
checks collapse, `in` lists and `changed` flags. The single-ref answers
(one per root) are measured today as the uncollapsed baseline; the
multi-ref answer is reported **pending** while the local catalog holds one
snapshot per root (RFC 0005 §9).

Raw outputs: .cache/runs/org/{multirepo,multiref}-<arm>.json.
"""

from __future__ import annotations

import json
import shutil
import traceback
from pathlib import Path
from typing import Any

from .arms import make_arm
from .arms.base import same_def
from .arms.rueca import flat_results
from .common import CACHE, RUNS_DIR, candidate_path, count_tokens, families, lang_family, load_manifest, log, read_json, run, safe_name, write_json
from .corpus import add_worktree, git_show, remove_worktree
from .datasets import instance_row
from .gold import parse_patch
from .s1 import EFF_LIMIT, queries_for, s1_gold
from .s1 import instances as s1_instances
from .treesitter import extract_functions, grammar_for

RRF_K = 60
ORG_LIMIT = 100
MULTIREF_SAMPLE = 20


# --- selection ---------------------------------------------------------------------


def org_instances() -> list[dict]:
    man = load_manifest()
    cfg = man.get("org", {})
    per = int(cfg.get("repos_per_family", 3))
    gated = [r for r in man["s1"]["repos"] if r.get("split", "gated") == "gated"]
    first: dict[str, dict] = {}
    for inst in s1_instances():
        first.setdefault(inst["repo"], inst)
    out: list[dict] = []
    for fam in families():
        repos = [r["repo"] for r in gated if lang_family(r["language"]) == fam and r["repo"] in first][:per]
        out += [first[r] for r in repos]
    return out


def run_path(kind: str, arm: str) -> Path:
    return RUNS_DIR / "org" / f"{kind}-{arm}.json"


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


# --- multi-repository ------------------------------------------------------------------


def _norm_cbm_score(hits: list[dict]) -> list[dict]:
    # cbm's `rank` is a negative BM25 (lower = better); flip it to a score
    return [dict(h, score=-(h.get("rank") or 0.0)) for h in hits]


def run_multirepo(arm_names: list[str], force: bool = False) -> None:
    insts = org_instances()
    if not insts:
        return
    for arm_name in arm_names:
        if arm_name not in ("rueca", "cbm"):
            continue
        arm = make_arm(arm_name)
        if not arm.available()[0]:
            continue
        p = run_path("multirepo", arm_name)
        if p.exists() and not force and read_json(p).get("status") == "ok":
            continue
        log(f"org multirepo {arm_name}: {len(insts)} repositories")
        try:
            rec = _multirepo_arm(arm, insts)
        except Exception as exc:
            rec = {"status": "error", "error": f"{exc}\n{traceback.format_exc()[-1500:]}"}
        rec["arm_version"] = arm.version()
        write_json(p, rec)


def _items(hits: list[dict], repo: str | None = None) -> list[list]:
    return [[h.get("repo") or repo, h.get("path"), h.get("name"), h.get("start_line"), h.get("label")] for h in hits]


def _multirepo_arm(arm, insts: list[dict]) -> dict:
    store = CACHE / "org" / f"{arm.name}-store"
    shutil.rmtree(store, ignore_errors=True)
    store.mkdir(parents=True, exist_ok=True)
    handles: dict[str, Any] = {}
    index: dict[str, dict] = {}
    for inst in insts:
        repo = inst["repo"]
        root = add_worktree(repo, inst["base_commit"], f"org-{repo}")
        try:
            if arm.name == "rueca":
                res = run([arm.bin, "index", str(root), "--store", str(store), "--json"], timeout=arm.index_timeout)
                ok = res.rc == 0
                rid = None
                if ok:
                    try:
                        rid = json.loads(res.stdout)["segment"]["snapshot"]["repo"]
                    except (json.JSONDecodeError, KeyError):
                        ok = False
                index[repo] = {"ok": ok, "seconds": res.seconds, "repo_id": rid, "error": None if ok else res.stderr[-300:]}
            else:
                h = arm.index(root, f"org-{repo}", inst["language"])
                handles[repo] = h
                index[repo] = {"ok": h.ok, "seconds": h.index_seconds, "error": h.error}
        finally:
            remove_worktree(repo, root)
    answers: dict[str, dict] = {}
    for inst in insts:
        q = queries_for(inst)["kw"]
        a: dict = {}
        if arm.name == "rueca":
            data, secs, err, raw = arm.call("search_symbols", {"query": q, "limit": ORG_LIMIT, "repo": "*"}, None, store)
            if err:
                a["native"] = {"ok": False, "error": err}
            else:
                hits = flat_results(data)
                eff = arm.search_eff(None, q, EFF_LIMIT, root=None, store=store, repo="*")
                a["native"] = {"ok": True, "seconds": secs, "items": _items(hits)[:50], "eff": _eff(eff)}
            per: list[list[dict]] = []
            tokens = 0
            errors = []
            for other in insts:
                rid = index.get(other["repo"], {}).get("repo_id")
                if not rid:
                    continue
                d2, _, e2, _ = arm.call("search_symbols", {"query": q, "limit": ORG_LIMIT, "repo": rid}, None, store)
                if e2:
                    errors.append(e2)
                    continue
                per.append([dict(h, repo=other["repo"]) for h in flat_results(d2)])
                ef = arm.search_eff(None, q, EFF_LIMIT, root=None, store=store, repo=rid)
                if ef.ok:
                    tokens += ef.extra.get("tokens") or 0
            merged = rrf_merge(per)
            a["per_repo"] = {"ok": not errors or bool(per), "errors": errors[:3], "items": _items(merged)[:50], "tokens": tokens, "calls": len(per)}
        else:
            per = []
            tokens = 0
            for other in insts:
                h = handles.get(other["repo"])
                if h is None or not h.ok:
                    continue
                res = arm._search(h, {"query": q}, ORG_LIMIT, text=False)
                if not res.ok:
                    continue
                per.append([dict(x, repo=other["repo"]) for x in _norm_cbm_score(res.hits)])
                txt = arm.agent_text("search_graph", {"project": h.state["project"], "query": q, "limit": EFF_LIMIT})
                if txt is not None:
                    tokens += count_tokens(txt) or 0
            merged = rrf_merge(per)
            a["per_repo"] = {"ok": bool(per), "items": _items(merged)[:50], "tokens": tokens, "calls": len(per)}
        answers[inst["id"]] = a
    for h in handles.values():
        arm.cleanup(h)
    shutil.rmtree(store, ignore_errors=True)
    return {"status": "ok", "instances": [i["id"] for i in insts], "index": index, "answers": answers}


def _eff(res) -> dict:
    if not res.ok:
        return {"ok": False, "error": res.error}
    return {"ok": True, "tokens": res.extra.get("tokens"), "prefix": res.extra.get("prefix") or [], "format": res.extra.get("format"), "items": _items(res.hits)}


def _acc(items: list[list], repo: str, gold: dict) -> dict:
    """File Acc@5 and Function Acc@10 of an org ranking (repository-qualified)."""
    files: list[tuple] = []
    funcs: list[dict] = []
    for r, path, name, line, label in items:
        if not any_candidate(path):
            continue
        if (r, path) not in files:
            files.append((r, path))
        if label in ("Function", "Method", "Test", "Constructor") or (label or "").endswith(("Function", "Method")):
            funcs.append({"repo": r, "path": path, "name": name, "start_line": line})
    gfiles = set(gold["files"])

    def own(r: str | None) -> bool:
        return r is not None and (r == repo or r.endswith("/" + repo) or r.endswith(repo))

    f5 = any(own(r) and p in gfiles for r, p in files[:5])
    seen: list[dict] = []
    for f in funcs:
        k = (f["repo"], f["path"], f["name"], f["start_line"])
        if k not in [(x["repo"], x["path"], x["name"], x["start_line"]) for x in seen]:
            seen.append(f)
    fn10 = any(own(f["repo"]) and any(same_def(f, g) for g in gold["functions"]) for f in seen[:10]) if gold["functions"] else None
    top10 = items[:10]
    other = sum(1 for it in top10 if not own(it[0]))
    return {"file_acc@5": float(f5), "func_acc@10": None if fn10 is None else float(fn10), "other_repo_top10": other / len(top10) if top10 else None}


def score_multirepo(s1: dict) -> dict:
    """Per arm and mode: accuracy under the org view vs the same instances'
    single-repository S1 answers (kw), and tokens."""
    insts = {i["id"]: i for i in org_instances()}
    single = {(r["arm"], r["id"]): r for r in s1["per_instance"] if r["variant"] == "kw"}
    out: dict = {"instances": len(insts), "repos": sorted({i["repo"] for i in insts.values()}), "arms": {}}
    for arm in ("rueca", "cbm"):
        p = run_path("multirepo", arm)
        if not p.exists():
            continue
        rec = read_json(p)
        if rec.get("status") != "ok":
            out["arms"][arm] = {"status": rec.get("status"), "error": str(rec.get("error"))[:300]}
            continue
        modes: dict = {}
        for mode in ("native", "per_repo"):
            rows = []
            pending = None
            for iid, a in rec["answers"].items():
                m = a.get(mode)
                if m is None:
                    continue
                if not m.get("ok"):
                    pending = str(m.get("error")).split("; indexed:")[0]
                    continue
                gp = CACHE / "gold" / "s1" / f"{safe_name(iid)}.json"
                gold = read_json(gp) if gp.exists() else None
                if gold is None or iid not in insts:
                    continue
                acc = _acc(m["items"], insts[iid]["repo"], gold)
                srow = single.get((arm, iid)) or {}
                se = srow.get("e") or {}
                tokens = (m.get("eff") or {}).get("tokens") if mode == "native" else m.get("tokens")
                rows.append({
                    "id": iid,
                    **acc,
                    "single_file_acc@5": (srow.get("file") or {}).get("acc@5"),
                    "single_func_acc@10": (srow.get("function") or {}).get("acc@10") if srow.get("function") else None,
                    "tokens": tokens,
                    "single_tokens": se.get("t"),
                })
            if not rows and pending:
                modes[mode] = {"status": "pending", "reason": str(pending)[:300]}
                continue
            if not rows:
                continue
            fa = [r["file_acc@5"] for r in rows]
            sa = [r["single_file_acc@5"] for r in rows if r["single_file_acc@5"] is not None]
            fn = [r["func_acc@10"] for r in rows if r["func_acc@10"] is not None]
            sfn = [r["single_func_acc@10"] for r in rows if r["single_func_acc@10"] is not None]
            tk = [r["tokens"] for r in rows if r["tokens"] is not None]
            st = [r["single_tokens"] for r in rows if r["single_tokens"] is not None]
            ot = [r["other_repo_top10"] for r in rows if r["other_repo_top10"] is not None]
            med = lambda xs: sorted(xs)[len(xs) // 2] if xs else None
            modes[mode] = {
                "status": "ok",
                "n": len(rows),
                "file_acc@5": sum(fa) / len(fa),
                "single_file_acc@5": sum(sa) / len(sa) if sa else None,
                "func_acc@10": sum(fn) / len(fn) if fn else None,
                "single_func_acc@10": sum(sfn) / len(sfn) if sfn else None,
                "n_func": len(fn),
                "other_repo_top10": sum(ot) / len(ot) if ot else None,
                "tokens_median": med(tk),
                "single_tokens_median": med(st),
                "rows": rows,
            }
        out["arms"][arm] = {"status": "ok", "modes": modes, "index_seconds": sum((v.get("seconds") or 0) for v in rec["index"].values()), "version": rec.get("arm_version")}
    return out


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
    [base], changed versions one ref each)."""
    by_key: dict[tuple, list[dict]] = {}
    for it in items:
        by_key.setdefault((it.get("path"), it.get("qn")), []).append(it)
    unchanged = set(gold["unchanged_files"])
    sy = {(s["path"], s["qn"]): s for s in gold["symbols"].values()}
    c = {"identical": 0, "identical_ok": 0, "changed_tp": 0, "changed_fp": 0, "changed_fn": 0, "in_total": 0, "in_ok": 0, "unknown": 0}
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
            c["changed_fp"] += int(flagged)
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
        idx = {}
        for name, root in (("base", base_root), ("fix", fix_root)):
            r = run([arm.bin, "index", str(root), "--store", str(store), "--json"], timeout=arm.index_timeout)
            idx[name] = {"ok": r.rc == 0, "seconds": r.seconds}
        queries = []
        for q in names + sorted(set(sample)):
            entry: dict = {"query": q}
            singles = {}
            for name, root in (("base", base_root), ("fix", fix_root)):
                data, _, err, raw = arm.call("search_symbols", {"query": q, "limit": EFF_LIMIT}, root, store, mode="agent")
                tokens, _ = arm.agent_tokens(None, "search_symbols", {"query": q, "limit": EFF_LIMIT}, raw, root=root, store=store, mode="agent") if not err else (None, None)
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
        out["reason"] = "local catalog holds one snapshot per root (RFC 0005 §9)"
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


def run_all(arm_names: list[str], force: bool = False) -> None:
    run_multirepo(arm_names, force)
    run_multiref(arm_names, force)
