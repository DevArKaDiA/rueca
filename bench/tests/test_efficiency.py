"""Tokens per correct answer (efficiency.py)."""

from rueca_bench.common import count_tokens
from rueca_bench.efficiency import json_prefix_tokens, search_aggregate, search_row, text_prefix_tokens, trace_aggregate

TREE = "results: 3  (cols: qn label file lines rank)\n  a.f Function a.c 1-2 -3.1\n  a.g Function a.c 4-9 -2.0\n  b.h Function b.c 1-2 -1.0\ntotal: 3\n"


def test_text_prefix_counts_headers_and_item_lines():
    prefix, approx = text_prefix_tokens(TREE, 3)
    assert not approx
    lines = TREE.splitlines(keepends=True)
    assert prefix[0] == count_tokens("".join(lines[:2]))
    assert prefix[1] == prefix[0] + count_tokens(lines[2])
    assert prefix[2] == prefix[1] + count_tokens(lines[3])
    assert prefix[2] < count_tokens(TREE)  # trailing summary not read before the hit


def test_text_prefix_falls_back_to_proportional():
    prefix, approx = text_prefix_tokens("no items here\n", 2)
    assert approx and len(prefix) == 2 and prefix[1] == count_tokens("no items here\n")


def test_rueca_grouped_text():
    txt = "acme/billing@7696364aef89 (main)\nsrc/a.py\n  Function f  L1-2  def f()\n  Function g  L4-5  def g()\nsrc/b.py\n  Method C.h  L3-9  def h(self)\n"
    prefix, approx = text_prefix_tokens(txt, 3)
    assert not approx
    lines = txt.splitlines(keepends=True)
    assert prefix[2] == count_tokens("".join(lines[:4])) + count_tokens("".join(lines[4:6]))


def test_json_prefix_head_first():
    items = [{"id": "x", "name": "f"}, {"id": "y", "name": "g"}]
    raw = '{"results":[{"id":"x","name":"f"},{"id":"y","name":"g"}],"truncated":false}'
    prefix, _ = json_prefix_tokens(raw, items)
    assert prefix[-1] == count_tokens(raw)
    assert prefix[0] < prefix[1]


def test_search_row_and_aggregate():
    items = [{"path": "x.c", "name": "a", "start_line": 1}, {"path": "gold.c", "name": "b", "start_line": 5}, {"path": "gold.c", "name": "c", "start_line": 9}]
    row = search_row(items, [10, 20, 30], 40, lambda it: it["path"] == "gold.c", lambda it: it["name"] == "c")
    assert (row["fh"], row["pf"], row["nh"]) == (2, 20, 2)
    assert (row["ff"], row["pff"], row["nf"]) == (3, 30, 1)
    miss = search_row(items, [10, 20, 30], 40, lambda it: False, lambda it: False)
    assert (miss["fh"], miss["pf"], miss["nh"]) == (0, 40, 0)
    agg = search_aggregate([row, miss])
    # a miss costs its whole answer and adds no correct answer
    assert agg["to_first_hit"] == (20 + 40) / 1
    assert agg["per_hit"] == (40 + 40) / 2
    assert agg["hit_rate"] == 0.5


def test_trace_aggregate():
    a = trace_aggregate([100, 50, 30], [4, 0, 1])
    assert a["per_true_edge"] == 180 / 5 and a["true_edges"] == 5
    assert trace_aggregate([10], [0])["per_true_edge"] is None
