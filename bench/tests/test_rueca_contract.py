"""Rueca arm normalizers: alpha and RFC 0005 responses give the same hits."""

from rueca_bench.arms.rueca import flat_results, local_name, parse_id, trace_graph, trace_neighbours, unwrap

ID_F = "github.com/acme/billing@7696364aef89:src/a.py#mod.f"
ID_G = "github.com/acme/billing@7696364aef89:src/b.py#mod.C.g"
ID_H = "github.com/acme/billing@7696364aef89:src/b.py#mod.h"


def test_parse_id():
    p = parse_id("local/shop@7696364aef89+:src/x.py#f")
    assert p == {"repo": "local/shop", "commit": "7696364aef89", "path": "src/x.py", "qn": "f"}


def test_search_alpha_and_rfc0005_agree():
    alpha = {"results": [
        {"id": ID_F, "name": "f", "qualified_name": "mod.f", "label": "Function", "path": "src/a.py", "start_line": 1, "end_line": 3, "score": 2.0},
        {"id": ID_G, "name": "g", "qualified_name": "mod.C.g", "label": "Method", "path": "src/b.py", "start_line": 5, "end_line": 9, "score": 1.0},
    ]}
    v2 = {"snapshots": [{"s": 0, "repo": "github.com/acme/billing", "ref": "main", "commit": "7696364aef89" + "0" * 28}], "results": [
        {"s": 0, "path": "src/a.py", "symbols": [{"id": ID_F, "name": "f", "qn": "mod.f", "label": "Function", "lines": [1, 3]}]},
        {"s": 0, "path": "src/b.py", "symbols": [{"id": ID_G, "qn": "mod.C.g", "label": "Method", "lines": [5, 9], "sig": "def g(self)"}]},
    ]}
    key = lambda xs: [(x["path"], x["name"], x["label"], x["start_line"], x["end_line"], x["id"]) for x in xs]
    assert key(flat_results(alpha)) == key(flat_results(v2))
    assert flat_results(v2)[0]["repo"] == "github.com/acme/billing"


def test_trace_alpha_and_rfc0005_agree():
    root = {"id": ID_G, "name": "g", "qualified_name": "mod.C.g", "label": "Method", "path": "src/b.py", "start_line": 5, "end_line": 9}
    alpha = {
        "root": root,
        "nodes": [
            {"id": ID_F, "name": "f", "qualified_name": "mod.f", "label": "Function", "path": "src/a.py", "start_line": 1, "end_line": 3, "depth": 1, "direction": "callers"},
            {"id": ID_H, "name": "h", "qualified_name": "mod.h", "label": "Function", "path": "src/b.py", "start_line": 11, "end_line": 12, "depth": 1, "direction": "callees"},
        ],
        "edges": [
            {"src": ID_F, "dst": ID_G, "kind": "CALLS", "confidence": 0.9, "strategy": "type_resolved", "call_site": {"path": "src/a.py", "line": 2}},
            {"src": ID_G, "dst": ID_H, "kind": "CALLS", "confidence": 0.5, "strategy": "unique_name", "call_site": {"path": "src/b.py", "line": 7}},
        ],
    }
    v2 = {
        "snapshots": [{"s": 0, "repo": "github.com/acme/billing"}],
        "nodes": [
            {"id": ID_G, "qn": "mod.C.g", "label": "Method", "path": "src/b.py", "lines": [5, 9], "d": 0},
            {"id": ID_F, "qn": "mod.f", "label": "Function", "path": "src/a.py", "lines": [1, 3], "d": 1, "dir": "in"},
            {"id": ID_H, "qn": "mod.h", "label": "Function", "path": "src/b.py", "lines": [11, 12], "d": 1, "dir": "out"},
        ],
        "edges": [[1, 0, "CALLS", 0.9, "type_resolved", 2], [0, 2, "CALLS", 0.5, "unique_name", 7]],
    }
    a, _ = trace_neighbours(alpha, ID_G)
    b, _ = trace_neighbours(v2, ID_G)
    key = lambda d: {k: [(x["path"], x["name"], x["start_line"], x["confidence"]) for x in v] for k, v in d.items()}
    assert key(a) == key(b)
    assert key(a)["callers"] == [("src/a.py", "f", 1, 0.9)]
    g = trace_graph(v2)
    assert g["root"] == ID_G and g["edges"][0]["call_site"] == {"path": "src/a.py", "line": 2}


def test_trace_v2_without_node_directions_uses_edges():
    v2 = {
        "root": 0,
        "nodes": [
            {"id": ID_G, "qn": "mod.C.g", "label": "Method", "path": "src/b.py", "lines": [5, 9]},
            {"id": ID_F, "qn": "mod.f", "label": "Function", "path": "src/a.py", "lines": [1, 3]},
        ],
        "edges": [[1, 0, "CALLS", 1.0, "same_module", 2], [0, 1, "CALLS", 1.0, "same_module", 6]],
    }
    out, _ = trace_neighbours(v2, ID_G)
    assert [x["name"] for x in out["callers"]] == ["f"] and [x["name"] for x in out["callees"]] == ["f"]


def test_local_name():
    assert local_name("geo::scale(double, double)") == "scale"
    assert local_name("src/util/index:Class.method") == "method"
    assert local_name("App\\Models\\User::save") == "save"
    assert local_name("Engine.ServeHTTP") == "ServeHTTP"
    assert local_name("f") == "f"


def test_unwrap_rfc0005_object_results():
    v2 = {"snapshots": [{"s": 0}], "results": {"s": 0, "symbol": {"id": ID_F}}, "truncated": False}
    assert unwrap(v2)["symbol"]["id"] == ID_F and unwrap(v2)["snapshots"] == [{"s": 0}]
    alpha = {"symbol": {"id": ID_F}}
    assert unwrap(alpha) is alpha
