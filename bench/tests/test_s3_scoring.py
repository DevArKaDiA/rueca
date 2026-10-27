"""S3 gold derivation and scoring: SCIP equivalence classes, recursion,
constructions by declaration, one-to-one hit matching."""

import subprocess

from rueca_bench.arms.grep import GrepArm
from rueca_bench.arms.rueca import trace_neighbours
from rueca_bench.match import CallableIndex, UnionFind, qn_agreement, qn_segments
from rueca_bench.phpstan import edges_from_calls
from rueca_bench.report import agg_s3, score_answers
from rueca_bench.scip import Doc, Occ, derive_call_graph, gold_neighbours, questions

CPP = """struct Token {
  Token(int k) {}
};
int copy(int a) { return a; }
int copy(long a) { return 2; }
int use() {
  Token t(1);
  return copy(3) + use();
}
#ifdef _WIN32
int plat() { return 1; }
#else
int plat() { return 2; }
#endif
"""


def _occ(symbol: str, line0: int, sc: int, ec: int, definition: bool = False) -> Occ:
    return Occ(symbol, 1 if definition else 0, (line0, sc, line0, ec), None)


def _cpp_graph(tmp_path) -> dict:
    (tmp_path / "a.cpp").write_text(CPP)
    doc = Doc(path="a.cpp", position_encoding=1, occurrences=[
        _occ("ctor", 1, 2, 7, True),
        _occ("copy", 3, 4, 8, True),  # one SCIP symbol for two definitions (SFINAE-like)
        _occ("copy", 4, 4, 8, True),
        _occ("use", 5, 4, 7, True),
        _occ("plat", 12, 4, 8, True),  # the #else branch only: line 11's plat is not compiled
        _occ("ctor", 6, 8, 9),  # `Token t(1);`: scip-clang puts the constructor on `t`
        _occ("copy", 7, 9, 13),
        _occ("use", 7, 19, 22),  # recursion
    ])
    return derive_call_graph(tmp_path, [doc], [""], "cpp")


def test_shared_symbol_and_unbound_alternative_are_classes(tmp_path):
    g = _cpp_graph(tmp_path)
    assert sorted(g["callables"]) == ["a.cpp#Token::Token@2", "a.cpp#copy@4", "a.cpp#copy@5", "a.cpp#plat@11", "a.cpp#plat@13", "a.cpp#use@6"]
    assert g["classes"] == [["a.cpp#copy@4", "a.cpp#copy@5"], ["a.cpp#plat@11", "a.cpp#plat@13"]]
    assert g["stats"]["classes"] == 2 and g["stats"]["classes_by_symbol"] == 1 and g["stats"]["class_members_unbound"] == 1
    assert g["stats"]["class_edges"] == 1  # use -> copy


def test_construction_by_declaration_and_recursion_are_edges(tmp_path):
    g = _cpp_graph(tmp_path)
    assert g["callees"]["a.cpp#use@6"] == ["a.cpp#Token::Token@2", "a.cpp#copy@4", "a.cpp#use@6"]
    assert "a.cpp#use@6" in gold_neighbours(g, "a.cpp#use@6", "callers")
    assert "a.cpp#use@6" in questions(g, 1)["callers"]


def test_construction_needs_a_body_declaration(tmp_path):
    # a prototype in a class body is a declaration, not a construction
    (tmp_path / "b.cpp").write_text("struct P {\n  P(int k);\n};\nP::P(int k) {}\n")
    doc = Doc(path="b.cpp", position_encoding=1, occurrences=[_occ("ctor", 3, 3, 4, True), _occ("ctor", 1, 2, 3)])
    g = derive_call_graph(tmp_path, [doc], [""], "cpp")
    assert g["stats"].get("call_sites", 0) == 0


def test_qualified_names():
    assert qn_segments("fmt::v12::detail::copy(InputIt, OutputIt/*#2*/)") == ["fmt", "v12", "detail", "copy"]
    assert qn_segments("src/x.py:Class.method") == ["src", "x", "py", "Class", "method"]
    assert qn_segments("formatter<std::string>::parse") == ["formatter<std::string>", "parse"]
    assert qn_agreement("fmt::v12::detail::copy(int)", "detail::copy") == 2
    assert qn_agreement("num_bits<int128_opt>()", "num_bits<int128_opt>") == 2
    assert qn_agreement("num_bits<int128_opt>()", "num_bits") == 1  # equal without template arguments
    assert qn_agreement("a::Point::Point(int)", "Line::Point") == 0
    assert qn_agreement(None, "f") == 0


def _f(path, qn, line, end=None):
    return {"path": path, "name": qn.split("::")[-1].split(".")[-1], "qualname": qn, "start": line, "end": end or line, "def_line": line, "outer_start": line}


def test_hit_resolves_to_one_callable():
    callables = {
        "m.cpp#Point::Point@3": _f("m.cpp", "Point::Point", 3),
        "m.cpp#Point::Point@4": _f("m.cpp", "Point::Point", 4),
        "m.cpp#Line::Point@5": _f("m.cpp", "Line::Point", 5),
    }
    idx = CallableIndex(callables)
    # nearest line, not every definition within ±2
    assert idx.resolve({"path": "m.cpp", "name": "Point", "start_line": 4}) == "m.cpp#Point::Point@4"
    assert idx.resolve({"path": "m.cpp", "name": "Point", "start_line": 3}) == "m.cpp#Point::Point@3"
    # the arm's qualified name wins over the nearest line
    assert idx.resolve({"path": "m.cpp", "name": "Point", "start_line": 5, "qualified_name": "geo::Point::Point(int, int)"}) == "m.cpp#Point::Point@4"
    assert idx.resolve({"path": "m.cpp", "name": "Point", "start_line": 9}) is None
    assert idx.resolve({"path": "m.cpp", "name": "Line", "start_line": 5}) is None


def _graph() -> dict:
    callables = {k: _f("a.cpp", k.split("#")[1].split("@")[0], int(k.split("@")[1]), int(k.split("@")[1]) + 3) for k in (
        "a.cpp#copy@10", "a.cpp#copy@20", "a.cpp#copy@30", "a.cpp#main@40", "a.cpp#other@50", "a.cpp#helper@60",
    )}
    return {
        "callables": callables,
        # gold binds the shared symbol to copy@10
        "callees": {"a.cpp#main@40": ["a.cpp#copy@10", "a.cpp#helper@60"], "a.cpp#other@50": ["a.cpp#copy@10"]},
        "callers": {"a.cpp#copy@10": ["a.cpp#main@40", "a.cpp#other@50"], "a.cpp#helper@60": ["a.cpp#main@40"]},
        "classes": [["a.cpp#copy@10", "a.cpp#copy@20", "a.cpp#copy@30"]],
    }


def _hit(k: str, conf=None) -> dict:
    name, line = k.split("#")[1].split("@")
    return {"path": "a.cpp", "name": name, "label": "Function", "start_line": int(line), "confidence": conf}


META = {"suite": "scip", "language": "cpp", "include": [""], "exclude": []}


def test_class_aware_callees_count_a_class_once():
    g = _graph()
    # the arm names two members of the class (the bound one is not among them) and helper
    ans = [{"fid": "a.cpp#main@40", "resolve": {"ok": True}, "neighbors": {"ok": True, "callers": [], "callees": [_hit("a.cpp#copy@20"), _hit("a.cpp#copy@30"), _hit("a.cpp#helper@60")]}}]
    row = score_answers(ans, g, META, 0.6)[0]
    assert row["ce"] == [2, 2, 2, 2]  # {class, helper}: no double credit
    assert row["ces"] == [3, 1, 2, 1]  # strict: copy@20 and copy@30 are wrong, copy@10 missed


def test_class_aware_callers_ask_every_member():
    g = _graph()
    ans = [{
        "fid": "a.cpp#copy@10", "resolve": {"ok": True},
        "neighbors": {"ok": True, "callers": [_hit("a.cpp#main@40")], "callees": []},
        "members": [{"fid": "a.cpp#copy@20", "ok": True, "callers": [_hit("a.cpp#other@50")]}, {"fid": "a.cpp#copy@30", "ok": False, "callers": []}],
    }]
    row = score_answers(ans, g, META, 0.6)[0]
    assert row["cr"] == [2, 2, 2, 2]
    assert row["crs"] == [1, 1, 2, 1]


def test_class_member_without_own_callers_is_not_a_second_callers_question():
    g = _graph()
    ans = [{"fid": "a.cpp#copy@20", "resolve": {"ok": True}, "neighbors": {"ok": True, "callers": [_hit("a.cpp#main@40")], "callees": []},
            "members": [{"fid": "a.cpp#copy@10", "ok": True, "callers": [_hit("a.cpp#other@50")]}]}]
    row = score_answers(ans, g, META, 0.6)[0]
    assert row["cr"][2] == 0 and row["crs"][2] == 0


def test_strict_rows_aggregate_as_diagnostic():
    g = _graph()
    ans = [{"fid": "a.cpp#main@40", "resolve": {"ok": True}, "neighbors": {"ok": True, "callers": [], "callees": [_hit("a.cpp#copy@20"), _hit("a.cpp#helper@60")]}}]
    out = agg_s3(score_answers(ans, g, META, 0.6), "t")
    assert out["callees"]["precision"] == 1.0 and out["callees@strict"]["precision"] == 0.5


def test_trace_self_edge_is_caller_and_callee():
    root = "local/x@abc:a.py#f"
    other = "local/x@abc:a.py#g"
    v2 = {
        "nodes": [
            {"id": root, "qn": "f", "label": "Function", "path": "a.py", "lines": [1, 5], "d": 0},
            {"id": other, "qn": "g", "label": "Function", "path": "a.py", "lines": [7, 9], "d": 1, "dir": "out"},
        ],
        "edges": [[0, 0, "CALLS", 0.9, "lexical", 3], [0, 1, "CALLS", 1.0, "lexical", 4]],
    }
    out, _ = trace_neighbours(v2, root)
    assert [(x["name"], x["confidence"]) for x in out["callers"]] == [("f", 0.9)]
    assert [x["name"] for x in out["callees"]] == ["g", "f"]
    alpha = {"root": {"id": root, "name": "f", "path": "a.py", "start_line": 1}, "nodes": [], "edges": [{"src": root, "dst": root, "confidence": 1.0}]}
    out, _ = trace_neighbours(alpha, root)
    assert [x["name"] for x in out["callers"]] == ["f"] and [x["name"] for x in out["callees"]] == ["f"]


def test_grep_arm_reports_recursion(tmp_path):
    (tmp_path / "a.py").write_text("def f(n):\n    return f(n - 1) + g()\n\n\ndef g():\n    return 1\n")
    subprocess.run(["git", "init", "-q"], cwd=tmp_path, check=True)
    subprocess.run(["git", "add", "-A"], cwd=tmp_path, check=True)
    arm = GrepArm()
    h = arm.index(tmp_path, "t", "python")
    ref = arm.resolve(h, {"path": "a.py", "name": "f", "start": 1}).extra["ref"]
    n = arm.neighbors(h, ref)
    assert sorted(x["name"] for x in n.extra["callees"]) == ["f", "g"]
    assert [x["name"] for x in n.extra["callers"]] == ["f"]


def test_phpstan_ambiguous_target_is_a_class():
    by_file = {"src/f.php": [
        {"path": "src/f.php", "name": "helper", "qualname": "helper", "start": 3, "end": 5, "def_line": 3, "outer_start": 3},
        {"path": "src/f.php", "name": "helper", "qualname": "helper", "start": 8, "end": 10, "def_line": 8, "outer_start": 8},
        {"path": "src/f.php", "name": "main", "qualname": "main", "start": 12, "end": 15, "def_line": 12, "outer_start": 12},
    ]}
    calls = {"src/f.php": [{"file": "/src/src/f.php", "line": 13, "targets": [{"kind": "function", "name": "helper", "file": "/src/src/f.php"}]}]}
    uf = UnionFind()
    _, callees, stats = edges_from_calls(calls, "/src/", by_file, lambda rel: True, uf)
    assert callees["src/f.php#main@12"] == {"src/f.php#helper@3"}
    assert stats["targets_ambiguous"] == 1 and uf.groups() == [["src/f.php#helper@3", "src/f.php#helper@8"]]
