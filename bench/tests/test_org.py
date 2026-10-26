"""Org scenarios: rank fusion and the multi-ref collapse scorer (org.py)."""

from rueca_bench.org import collapse_gold, rrf_merge, score_collapse

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
