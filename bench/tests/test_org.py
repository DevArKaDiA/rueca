"""Org scenarios: rank fusion, the organization file and RFC 0006 metrics,
instance selection and gold at the pin, the organization BM25 ranking,
multi-repository scoring, and the multi-ref collapse scorer (org.py)."""

import tomllib

import pytest
from rueca_bench import org
from rueca_bench.arms.base import Handle
from rueca_bench.bm25 import BM25, union_search
from rueca_bench.common import load_manifest
from rueca_bench.org import (
    collapse_gold,
    copy_store,
    derive_pins,
    eff_hit,
    grep_answer,
    grep_member_stats,
    org_instances,
    org_layer_stats,
    parse_only,
    rrf_merge,
    score_arm,
    score_collapse,
    write_org_file,
)
from rueca_bench.s1 import instances as s1_instances

BASE = "def f():\n    return 1\n\n\ndef g():\n    return 2\n\n\ndef gone():\n    pass\n"
FIX = "def f():\n    return 10\n\n\ndef g():\n    return 2\n\n\ndef new():\n    pass\n"


def test_rrf_interleaves_by_rank_and_breaks_ties_by_score():
    a = [{"id": "a1", "score": 5.0}, {"id": "a2", "score": 4.0}]
    b = [{"id": "b1", "score": 9.0}, {"id": "b2", "score": 1.0}]
    assert [x["id"] for x in rrf_merge([a, b])] == ["b1", "a1", "a2", "b2"]


def test_collapse_gold_classes():
    gold = collapse_gold({"m.py": (BASE, FIX)}, "python", ["other.py"])
    cls = {s["qn"]: s["class"] for s in gold["symbols"].values()}
    assert cls == {"f": "changed", "g": "same_in_changed_file", "gone": "removed", "new": "added"}


def test_score_collapse_perfect_and_broken():
    gold = collapse_gold({"m.py": (BASE, FIX)}, "python", ["other.py"])
    good = [
        {"path": "other.py", "qn": "x", "in": [0, 1], "changed": False},
        {"path": "m.py", "qn": "f", "in": [0], "changed": True},
        {"path": "m.py", "qn": "f", "in": [1], "changed": True},
        {"path": "m.py", "qn": "g", "in": [0, 1], "changed": False},
        {"path": "m.py", "qn": "new", "in": [1], "changed": False},
        {"path": "m.py", "qn": "gone", "in": [0], "changed": False},
    ]
    c = score_collapse(good, gold, 0, 1)
    assert (c["identical_ok"], c["identical"]) == (1, 1)
    assert (c["changed_tp"], c["changed_fp"], c["changed_fn"]) == (1, 0, 0)
    assert c["in_ok"] == c["in_total"] == 5
    bad = [
        {"path": "other.py", "qn": "x", "in": [0], "changed": False},
        {"path": "other.py", "qn": "x", "in": [1], "changed": False},
        {"path": "m.py", "qn": "f", "in": [0, 1], "changed": False},
        {"path": "m.py", "qn": "new", "in": [0, 1], "changed": True},
    ]
    c = score_collapse(bad, gold, 0, 1)
    assert (c["identical_ok"], c["identical"]) == (0, 1)
    assert (c["changed_tp"], c["changed_fp"], c["changed_fn"]) == (0, 1, 1)
    assert c["in_ok"] == 0
    # blob-level versions: `g` is unchanged inside a changed file, so Rueca
    # lists it once per ref, flagged; counted apart, not as a false positive
    by_blob = [
        {"path": "m.py", "qn": "g", "in": [0], "changed": True},
        {"path": "m.py", "qn": "g", "in": [1], "changed": True},
    ]
    c = score_collapse(by_blob, gold, 0, 1)
    assert (c["changed_fp"], c["changed_same_file"], c["in_ok"], c["in_total"]) == (0, 1, 2, 2)


def test_org_file_lists_every_root(tmp_path):
    f = tmp_path / "org.toml"
    write_org_file(f, [tmp_path / "a b", tmp_path / 'q"x'])
    cfg = tomllib.loads(f.read_text())
    assert cfg["name"] == "bench"
    assert [r["path"] for r in cfg["repo"]] == [str(tmp_path / "a b"), str(tmp_path / 'q"x')]


def test_eff_hit_counts_own_repository_gold_files():
    gold = {"files": ["src/a.py"], "functions": []}
    eff = {"ok": True, "tokens": 90, "prefix": [10, 25, 40], "items": [
        ["github.com/x/other", "src/a.py", "f", 1, "Function"],
        ["github.com/acme/repo", "src/b.py", "g", 1, "Function"],
        ["github.com/acme/repo", "src/a.py", "h", 1, "Function"],
    ]}
    assert eff_hit(eff, "acme/repo", gold) == (3, 40)
    assert eff_hit(dict(eff, items=eff["items"][:2]), "acme/repo", gold) == (0, 90)
    assert eff_hit({"ok": False}, "acme/repo", gold) == (0, None)


def test_org_layer_stats_ratios():
    index = {
        "a": {"ok": True, "repo_ms": 1000, "org_ms": 20},
        "b": {"ok": True, "repo_ms": 200, "org_ms": 40},
        "c": {"ok": False},
    }
    inc = {"ok": True, "repo": "a", "repo_ms": 500, "first_repo_ms": 1000, "org_ms": 25, "docs": 7, "org_repos": 2}
    s = org_layer_stats(index, inc)
    assert s["steps"] == 2 and s["org_ms_max"] == 40
    assert abs(s["ratio_max"] - 0.2) < 1e-9
    assert s["replace"]["ratio"] == 0.05 and s["replace"]["ratio_first"] == 0.025
    assert org_layer_stats({}, None) is None


# --- members, pins, selection ---------------------------------------------------------


def test_manifest_org_repos_follow_the_pin_rule():
    man = load_manifest()
    want = derive_pins(man["s1"]["repos"], s1_instances())
    have = [r for r in man["org"]["repos"] if any(w["repo"] == r["repo"] for w in want)]
    assert [(r["repo"], r["language"], r["commit"], r["split"]) for r in have] == [(w["repo"], w["language"], w["commit"], w["split"]) for w in want]
    assert all(r.get("pin") and r["split"] in ("gated", "calibration") and len(r["commit"]) == 40 for r in man["org"]["repos"])
    assert man["org"]["max_instances_per_repo"] >= 1


def test_parse_only_splits_and_repositories():
    assert parse_only(None) == (["gated", "calibration"], None)
    assert parse_only(["calibration"]) == (["calibration"], None)
    assert parse_only(["psf/requests", "gated"]) == (["gated"], {"psf/requests"})


BASE_SRC = "class Session:\n    def request(self):\n        return 1\n"
PIN_SRC = "import x\n\n\nclass Session:\n    def other(self):\n        pass\n\n    def request(self):\n        return 2\n"


def _fn(path, qn, start):
    name = qn.rsplit(".", 1)[-1]
    return {"path": path, "name": name, "qualname": qn, "label": "Method", "start": start, "end": start + 1, "def_line": start, "outer_start": start, "indent": 4}


@pytest.fixture
def fake_corpus(monkeypatch, tmp_path):
    """One gated member (acme/a at PIN) with six instances: a present (file
    and function at the pin), b (gold file gone), c (function gone), d (file
    gold only), e (own base = pin), f (never examined: cap reached)."""
    insts = [{"id": f"acme__a-{k}", "repo": "acme/a", "language": "python", "base_commit": "PIN" if k == "e" else f"B{k}", "dataset": "x"} for k in "abcdef"]
    golds = {
        "acme__a-a": {"files": ["s.py"], "functions": [_fn("s.py", "Session.request", 2)], "function_source": "locagent"},
        "acme__a-b": {"files": ["gone.py"], "functions": []},
        "acme__a-c": {"files": ["s.py"], "functions": [_fn("s.py", "Session.vanished", 2)]},
        "acme__a-d": {"files": ["s.py"], "functions": []},
        "acme__a-e": {"files": ["s.py"], "functions": [_fn("s.py", "Session.request", 8)]},
        "acme__a-f": {"files": ["s.py"], "functions": []},
    }
    man = {"org": {"max_instances_per_repo": 3, "repos": [{"repo": "acme/a", "language": "python", "commit": "PIN", "split": "gated", "pin": "test"}]}}
    calls = []
    monkeypatch.setattr(org, "load_manifest", lambda: man)
    monkeypatch.setattr(org, "s1_instances", lambda: insts)
    monkeypatch.setattr(org, "s1_gold", lambda inst: calls.append(inst["id"]) or golds[inst["id"]])
    monkeypatch.setattr(org, "ensure_commit", lambda repo, commit: None)
    monkeypatch.setattr(org, "file_exists_at", lambda repo, commit, path: path == "s.py")
    monkeypatch.setattr(org, "git_show", lambda repo, commit, path: PIN_SRC if commit == "PIN" else BASE_SRC)
    monkeypatch.setattr(org, "blob_id", lambda repo, commit, path: "same" if commit == "PIN" or commit == "Bd" else "old")
    monkeypatch.setattr(org, "GOLD_DIR", tmp_path / "gold")
    monkeypatch.setattr(org, "instance_row", lambda inst: {"patch": "p"})
    monkeypatch.setattr(org, "patch_state", lambda repo, commit, patch: "neither")
    return calls


def test_selection_keeps_the_first_instances_whose_gold_is_at_the_pin(fake_corpus):
    sel = org_instances("gated", workers=1)
    assert [i["id"] for i in sel] == ["acme__a-a", "acme__a-d", "acme__a-e"]
    assert "acme__a-f" not in fake_corpus  # cap reached before it
    a = sel[0]
    assert a["kind"] == "issue" and a["pin"] == "PIN"
    # function re-resolved at the pin: Session.request moved from line 2 to 8
    assert [(f["qualname"], f["start"], f["path"]) for f in a["gold"]["functions"]] == [("Session.request", 8, "s.py")]
    assert a["gold"]["files_same_blob"] == 0 and a["gold"]["patch_at_pin"] == "neither"
    assert sel[1]["gold"]["files_same_blob"] == 1
    # own base commit = pin: the S1 gold as it is
    assert sel[2]["gold"]["functions"] == [_fn("s.py", "Session.request", 8)] and sel[2]["gold"]["patch_at_pin"] == "base"


def test_selection_skips_instances_whose_fix_is_already_at_the_pin(fake_corpus, monkeypatch):
    # a's fix is already in the pin: stale question, skipped; the cap then reaches f
    monkeypatch.setattr(org, "instance_row", lambda inst: {"patch": inst["id"]})
    monkeypatch.setattr(org, "patch_state", lambda repo, commit, patch: "reverse" if patch == "acme__a-a" else "neither")
    sel = org_instances("gated", workers=1)
    assert [i["id"] for i in sel] == ["acme__a-d", "acme__a-e", "acme__a-f"]


def test_calibration_split_has_its_own_cap(fake_corpus, monkeypatch):
    man = org.load_manifest()
    man["org"]["calibration_max_instances_per_repo"] = 1
    man["org"]["repos"][0]["split"] = "calibration"
    assert [i["id"] for i in org_instances("calibration", workers=1)] == ["acme__a-a"]


def test_selection_records_reasons(fake_corpus):
    org_instances("gated", workers=1)
    reasons = {iid: org.read_json(org._org_gold_path(iid)).get("reason") for iid in ("acme__a-b", "acme__a-c")}
    assert reasons["acme__a-b"].startswith("gold file missing at the pin") and reasons["acme__a-c"] == "no gold function at the pin"


def test_selection_is_deterministic_and_cached(fake_corpus):
    first = org_instances("gated", workers=1)
    n = len(fake_corpus)
    assert org_instances("gated", workers=2) == first
    assert len(fake_corpus) == n  # second pass read .cache/gold/org


# --- organization BM25 ------------------------------------------------------------------

DOCS_A = [("a/x.py", ["parse", "header", "value"]), ("a/y.py", ["send", "request", "header", "header"]), ("a/z.py", ["noise"])]
DOCS_B = [("b/x.py", ["request", "retry"]), ("b/w.py", ["header", "cookie", "request", "request"])]


def test_union_of_one_index_ranks_exactly_like_bm25():
    q = ["request", "header", "header", "missing"]
    bm = BM25(DOCS_A)
    st = bm.restrict(set(q))
    assert [(k, s) for _, k, s in union_search([("a", st)], q, 10)] == bm.search(q, 10)
    assert st.keys == ["a/x.py", "a/y.py"]  # documents the query terms reach


def test_union_ranks_like_one_bm25_over_the_concatenation():
    q = ["request", "header", "cookie"]
    whole = BM25([(f"a\x00{k}", t) for k, t in DOCS_A] + [(f"b\x00{k}", t) for k, t in DOCS_B])
    parts = [("a", BM25(DOCS_A).restrict(set(q))), ("b", BM25(DOCS_B).restrict(set(q)))]
    got = [(f"{r}\x00{k}", s) for r, k, s in union_search(parts, q, 10)]
    want = whole.search(q, 10)
    assert [k for k, _ in got] == [k for k, _ in want]
    assert all(abs(a - b) < 1e-12 for (_, a), (_, b) in zip(got, want))
    assert union_search(parts, q, 2) == union_search(parts, q, 10)[:2]


def _handle(docs, funcs):
    h = Handle("grep", "k", None, "python")
    h.state = {"files": BM25(docs), "bodies": BM25([(fid, toks) for fid, (_, toks) in funcs.items()]), "funcs": {fid: f for fid, (f, _) in funcs.items()}}
    return h


def test_grep_answer_is_repository_qualified_and_single_is_own_repository():
    fa = {"x.py#parse@1": ({"path": "x.py", "name": "parse", "start_line": 1, "label": "Function"}, ["parse", "header"])}
    fb = {"w.py#cookie@3": ({"path": "w.py", "name": "cookie", "start_line": 3, "label": "Function"}, ["header", "cookie", "request"])}
    q = ["header", "cookie"]
    stats = {
        "acme/a": grep_member_stats(_handle([("x.py", ["parse", "header"])], fa), set(q)),
        "acme/b": grep_member_stats(_handle([("w.py", ["header", "cookie"]), ("v.py", ["zzz"])], fb), set(q)),
    }
    members = [{"repo": "acme/a"}, {"repo": "acme/b"}]
    a = grep_answer(q, "acme/a", members, stats)
    assert a["native"]["files"] == [["acme/b", "w.py"], ["acme/a", "x.py"]]
    assert [it[:3] for it in a["native"]["items"]] == [["acme/b", "w.py", "cookie"], ["acme/a", "x.py", "parse"]]
    assert a["single"]["files"] == [["acme/a", "x.py"]] and [it[0] for it in a["single"]["items"]] == ["acme/a"]
    assert sorted(map(tuple, a["per_repo"]["files"])) == [("acme/a", "x.py"), ("acme/b", "w.py")]
    assert a["per_repo"]["tokens"] is None and a["per_repo"]["calls"] == 2


# --- scoring ------------------------------------------------------------------------------


def test_score_arm_rows_and_aggregates():
    gold = {"files": ["src/a.py"], "functions": [{"path": "src/a.py", "name": "f", "start": 10, "def_line": 10, "outer_start": 10}]}
    inst = {"id": "i1", "repo": "acme/repo", "language": "python", "kind": "issue", "gold": gold}
    inst2 = {"id": "i2", "repo": "acme/repo", "language": "python", "kind": "issue", "gold": {"files": ["src/z.py"], "functions": []}}
    own = ["github.com/acme/repo", "src/a.py", "f", 10, "Function"]
    other = ["github.com/x/other", "src/a.py", "f", 10, "Function"]
    eff = {"ok": True, "tokens": 100, "prefix": [40, 70, 100], "items": [other, own]}
    seff = {"ok": True, "tokens": 50, "prefix": [50], "items": [["acme/repo", "src/a.py", "f", 10, "Function"]]}
    rec = {
        "instances": [inst, inst2],
        "answers": {
            "i1": {
                "native": {"ok": True, "items": [other, own], "eff": eff},
                "per_repo": {"ok": True, "items": [own], "tokens": 300},
                "single": {"ok": True, "items": [own], "eff": seff},
            },
            "i2": {
                "native": {"ok": True, "items": [other], "eff": dict(eff, items=[other])},
                "per_repo": {"ok": False, "error": "boom"},
                "single": {"ok": True, "items": [own], "eff": dict(seff, items=[])},
            },
        },
    }
    modes = score_arm(rec)
    n = modes["native"]
    r1, r2 = n["rows"]
    assert (r1["file_acc@5"], r1["func_acc@10"], r1["other_repo_top10"]) == (1.0, 1.0, 0.5)
    assert (r1["fh"], r1["pf"], r1["single_fh"], r1["single_pf"], r1["tokens"], r1["single_tokens"]) == (2, 70, 1, 50, 100, 50)
    assert (r2["file_acc@5"], r2["func_acc@10"], r2["single_file_acc@5"]) == (0.0, None, 0.0)
    assert r1["top10"] == [other, own] and r1["kind"] == "issue" and r1["language"] == "python"
    assert (n["n"], n["file_acc@5"], n["n_func"], n["found_top10"], n["single_found_top10"]) == (2, 0.5, 1, 1, 1)
    assert n["tokens_per_correct"] == 200 and n["single_tokens_per_correct"] == 100 and n["tokens_per_correct_ratio"] == 2
    p = modes["per_repo"]
    assert p["n"] == 1 and p["rows"][0]["tokens"] == 300 and p["rows"][0]["fh"] is None and p["found_top10"] is None


def test_score_arm_reads_a_separate_file_ranking():
    gold = {"files": ["src/a.py"], "functions": []}
    inst = {"id": "g", "repo": "acme/repo", "language": "python", "gold": gold}
    files = [["x/o", "src/b.py"]] * 1 + [["acme/repo", "src/a.py"]]
    rec = {"instances": [inst], "answers": {"g": {
        "native": {"ok": True, "items": [["x/o", "src/q.py", "q", 1, "Function"]], "files": files},
        "single": {"ok": True, "items": [], "files": [["acme/repo", "src/a.py"]], "eff": None},
    }}}
    row = score_arm(rec)["native"]["rows"][0]
    assert (row["file_acc@5"], row["single_file_acc@5"], row["other_repo_top10"]) == (1.0, 1.0, 0.5)
    assert row["tokens"] is None and row["single_tokens"] is None and row["top10_files"] == files


def test_copy_store_is_an_independent_copy(tmp_path):
    src = tmp_path / "store"
    (src / "seg").mkdir(parents=True)
    (src / "seg" / "a.bin").write_bytes(b"abc")
    (src / "catalog.json").write_text("{}")
    dst = tmp_path / "copy"
    (dst / "stale").mkdir(parents=True)
    copy_store(src, dst)
    assert (dst / "seg" / "a.bin").read_bytes() == b"abc" and (dst / "catalog.json").read_text() == "{}"
    assert not (dst / "stale").exists()
    (dst / "seg" / "a.bin").write_bytes(b"changed")
    assert (src / "seg" / "a.bin").read_bytes() == b"abc"


def test_patch_state_tells_whether_the_fix_is_at_a_commit(monkeypatch, tmp_path):
    repo = tmp_path / "r"
    repo.mkdir()
    env = {"GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@invalid", "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@invalid"}

    def git(*args):
        return org.run(["git", "-c", "commit.gpgsign=false", *args], cwd=repo, env=env, check=True).stdout.strip()

    git("init", "-q")
    (repo / "m.py").write_text("x = 1\n")
    git("add", "m.py")
    git("commit", "-qm", "base")
    base = git("rev-parse", "HEAD")
    (repo / "m.py").write_text("x = 2\n")
    git("commit", "-qam", "fix")
    fixed = git("rev-parse", "HEAD")
    (repo / "m.py").write_text("y = 3\n")
    git("commit", "-qam", "moved on")
    moved = git("rev-parse", "HEAD")
    patch = "diff --git a/m.py b/m.py\n--- a/m.py\n+++ b/m.py\n@@ -1 +1 @@\n-x = 1\n+x = 2\n"
    monkeypatch.setattr(org, "repo_dir", lambda name: repo)
    assert [org.patch_state("acme/r", c, patch) for c in (base, fixed, moved)] == ["applies", "reverse", "neither"]
