"""Org scenarios: rank fusion, the organization file and RFC 0006 metrics,
and the multi-ref collapse scorer (org.py)."""

import tomllib

from rueca_bench.org import collapse_gold, eff_hit, org_layer_stats, rrf_merge, score_collapse, write_org_file

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
