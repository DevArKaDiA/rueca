"""Multi-step suite (RFC 0009 phase 2): every task has its three references
(plan, JavaScript, Starlark) and a cbm plan or a reason; the gold answers
are re-derived from the committed inputs; the derivations, the text parsers
and the scoring behave. Nothing here needs the corpora or a binary, except
the re-extraction check, which runs only when the bench cache holds the
gold it reads."""

import json
import re

import pytest

from rueca_bench.common import BENCH_DIR, GOLD_DIR
from rueca_bench.multistep import cbm_plans, derive, inputs, plans, report, suite
from rueca_bench.multistep.cli import GOLD_PATH, gold_answers
from rueca_bench.multistep.run import correct, script_path
from rueca_bench.multistep.tasks import BY_ID, TASKS

TOOL_NAMES = ("search_symbols", "search_code", "get_symbol", "get_snippet", "trace", "get_file_outline", "list_repositories", "list_contracts", "query_graph", "rueca.")


def test_task_ids_and_counts():
    ids = [t.id for t in TASKS]
    assert ids == sorted(set(ids)) and 20 <= len(ids) <= 30
    assert 3 <= sum(t.expect != "win" for t in TASKS) <= 5
    assert len({t.kind for t in TASKS}) >= 15


@pytest.mark.parametrize("task", TASKS, ids=lambda t: t.id)
def test_every_task_has_three_references_and_a_cbm_entry(task):
    assert task.id in plans.PLANS
    for lang in ("js", "starlark"):
        src = script_path(task.id, lang).read_text()
        assert src.strip() and len(src.encode()) < 16 * 1024
    assert "return " in script_path(task.id, "js").read_text()
    assert re.search(r"^result = ", script_path(task.id, "starlark").read_text(), re.MULTILINE)
    assert (task.id in cbm_plans.CBM_PLANS) != (task.id in cbm_plans.CBM_SKIP)


@pytest.mark.parametrize("task", TASKS, ids=lambda t: t.id)
def test_statements_are_self_contained(task):
    text = task.statement
    assert "Return " in text
    assert not any(name in text for name in TOOL_NAMES), "a statement must not name the tools (no solution steps)"
    assert "/Users/" not in text


def test_no_stray_scripts():
    names = {p.name for p in (BENCH_DIR / "multistep" / "scripts").iterdir()}
    want = {f"{t.id}.{ext}" for t in TASKS for ext in ("js", "star")}
    assert names == want


def test_gold_is_rederived_from_committed_inputs():
    committed = json.loads(GOLD_PATH.read_text())
    assert gold_answers(inputs.load()) == committed
    for t in TASKS:
        assert correct(t, committed[t.id], committed[t.id])


@pytest.mark.skipif(not all((GOLD_DIR / "s3" / f"{n}.json").exists() for n in ("flask", "requests", "hono")), reason="S3 gold not in the bench cache")
def test_s3_inputs_match_the_cached_gold():
    committed = inputs.load()["s3"]
    for name in ("flask", "requests", "hono"):
        assert inputs.s3_inputs(name) == committed[name]


# --- derivations --------------------------------------------------------------------------------

S3 = {
    "callables": {f"a.py#{n}@{i}": "Function" for i, n in enumerate("abcdef", 1)},
    "edges": [["a.py#a@1", "a.py#c@3"], ["a.py#b@2", "a.py#c@3"], ["a.py#b@2", "a.py#d@4"], ["a.py#e@5", "a.py#b@2"], ["a.py#f@6", "a.py#a@1"], ["a.py#a@1", "a.py#d@4"]],
}


def test_graph_derivations():
    g = derive.Graph.from_inputs(S3)
    assert derive.callers_of(g, "a.py#c") == ["a.py#a", "a.py#b"]
    assert derive.two_hop_callers(g, "a.py#c") == ["a.py#a", "a.py#b", "a.py#e", "a.py#f"]
    assert derive.exactly_two_hop_count(g, "a.py#c") == 2
    assert derive.common_callers(g, "a.py#c", "a.py#d") == ["a.py#a", "a.py#b"]
    assert derive.common_callees(g, "a.py#a", "a.py#b") == ["a.py#c", "a.py#d"]
    assert derive.leaf_callees(g, "a.py#b") == ["a.py#c", "a.py#d"]
    assert derive.top_by_callers(g, g.in_file("a.py"), 2) == [["a.py#c", 2], ["a.py#d", 2]]
    assert derive.uncalled(g, g.in_file("a.py")) == ["a.py#e", "a.py#f"]
    assert derive.caller_counts(g, ["a.py#b", "a.py#f"]) == {"a.py#b": 1, "a.py#f": 0}


def test_other_derivations():
    uses = [["src/x/a.py", "lib", "lib.f"], ["src/x/b.py", "lib", "lib.f"], ["tests/t.py", "lib", "lib.f"], ["src/x/a.py", "lib", "lib.g"]]
    assert derive.files_using(uses, "lib.f", "src/x/") == ["src/x/a.py", "src/x/b.py"]
    assert derive.files_using_each(uses, ["lib.g", "lib.h"], "src/") == {"lib.g": ["src/x/a.py"], "lib.h": []}
    pairs = [
        {"kind": "http-code", "key": "GET /a", "src_repo": "py", "dst_path": "s.go", "dst_symbol": "S.A"},
        {"kind": "http-code", "key": "GET /a", "src_repo": "js", "dst_path": "u.go", "dst_symbol": "S.P"},
        {"kind": "http-spec", "key": "GET /a", "src_repo": "js", "dst_path": "api.yaml", "dst_symbol": "GET /a"},
    ]
    assert derive.contract_keys(pairs, "http-code", "py") == {"GET /a"}
    assert derive.contract_dsts(pairs, "http-code", "GET /a") == ["s.go#S.A", "u.go#S.P"]
    assert derive.spec_ops_count(pairs, "js") == 1
    syms = [["f.py", "A.x", "changed"], ["f.py", "A.y", "same_in_changed_file"], ["f.py", "z", "added"], ["g.py", "w", "removed"]]
    assert derive.differing(syms, "f.py") == ["f.py#A.x", "f.py#z"]
    assert derive.bare("p.py#A.b@12") == "p.py#A.b" and derive.bare("p.py#x") == "p.py#x"


# --- text parsers -------------------------------------------------------------------------------

TRACE = """github.com/psf/requests@6e83187b8feb (HEAD)
0 Method Session.request  src/requests/sessions.py L557-653
1 in d1 Function request  src/requests/api.py L24-71  →0 CALLS 0.5 heuristic @71
2 in d2 Function get  src/requests/api.py L74-87  →1 CALLS 0.9 same_module @87
3 out d1 Class Thing  src/requests/x.py L1-9  ←0 CALLS 0.9 import_resolved @600
4 in d1 Function helper  tests/test_x.py L1-3  →0 CALLS 0.9 same_module @2
(19 tests excluded; `include_tests: true` lists them)
"""

ORG_TRACE = """[0] github.com/pallets/click@934813e4d421 (HEAD)
[1] github.com/pallets/flask@22d924701a6a (HEAD)
0 [0] Function echo  src/click/utils.py L219-319
1 [0] in d1 Function secho  src/click/termui.py L603-634  →0 CALLS 0.95 import_resolved @634
2 [1] in d1 Method FlaskGroup.cmd  src/flask/cli.py L10-20  →0 CALLS 0.9 import_resolved @12
"""


def test_trace_parser():
    nodes = plans.trace_nodes(TRACE)
    assert [n.idx for n in nodes] == [0, 1, 2, 3, 4]
    assert nodes[0].depth == 0 and nodes[0].dir is None and nodes[0].fid == "src/requests/sessions.py#Session.request"
    assert nodes[2].depth == 2 and nodes[2].target == 1
    assert plans.fids(plans.neighbours(TRACE, "in", "src/requests/")) == {"src/requests/api.py#request"}
    assert plans.neighbours(TRACE, "out", "src/") == []  # a Class is not a function
    assert plans.flask_files(ORG_TRACE) == ["src/flask/cli.py"]
    assert plans.snapshots(ORG_TRACE) == {0: "github.com/pallets/click", 1: "github.com/pallets/flask"}
    one = plans.trace_nodes("3 [1] both d2 Method A.b c  src/a.py L4-9  →0 CALLS 0.9\nx Function f  a.py L1\n1 Function f  a b.py L1")
    assert one == [plans.Node(3, 1, "both", 2, "Method", "A.b c", "src/a.py", 0)]


def test_outline_and_contract_parsers():
    outline = "r@1 (HEAD)\nf.py (python, 9 lines) [0] changed\n  Function a L1-2  def a() changed\n  Class C L3-9  class C\n    Method C.m L4-5  def m(self)\nversion [1] changed: f.py (python, 9 lines)\n  Function a L1-3  def a() changed\n"
    v = plans.outline_versions(outline)
    assert [s.qn for s in v[0]] == ["a", "C", "C.m"] and [(s.qn, s.end) for s in v[1]] == [("a", 3)]
    assert [s.qn for s in v[0] if s.changed] == ["a"] and v[1][0].changed
    rows = "o@b8 (HEAD)\nGET /api/tags  provides  gin  github.com/o/o:server/routes.go#Server.ListHandler L2407  (7)\nGET /  consumes  js-http  github.com/o/js:test/u.test.ts#get Function Header Tests > x L20  (3)\n"
    es = plans.contracts(rows)
    assert es[0].key == "GET /api/tags" and es[0].symbol == "Server.ListHandler" and es[0].path == "server/routes.go"
    assert plans.is_test_file(es[1].path) and not plans.is_test_file("src/flask/app.py")
    assert plans.contracts("GET /  provides  gin  r:p.go L1  (2)\nGET /  uses  gin  r:p.go L1  (2)\nGET /  provides  gin  r:p.go L1  2")[0].symbol == ""
    assert len(plans.contracts("GET /  provides  gin  r:p.go L1  (2)\nGET /  uses  gin  r:p.go L1  (2)")) == 1


def test_cbm_rows_and_names():
    text = "rows_refs: 1  (cols: id prefix)\n  0 tb-p.src.flask.app.Flask.\nrows_ref_rule: @N+suffix=prefix+suffix\nrows: 2  (cols: a.qualified_name a.file_path n)\n  @0+run src/flask/app.py \"3\"\n  tb-p.src.flask.cli.main src/flask/cli.py \"0\"\ntotal: 2\n"
    assert cbm_plans.rows(text) == [["tb-p.src.flask.app.Flask.run", "src/flask/app.py", "3"], ["tb-p.src.flask.cli.main", "src/flask/cli.py", "0"]]
    assert cbm_plans.fid("tb-p.src.flask.app.Flask.run", "src/flask/app.py") == "src/flask/app.py#Flask.run"
    assert cbm_plans.local("tb-h.src.utils.jwt.jwt.verify", "src/utils/jwt/jwt.ts") == "verify"


# --- scoring, aggregation, report ---------------------------------------------------------------


def test_scoring_by_answer_shape():
    assert correct(BY_ID["ms02"], ["b", "a"], ["a", "b"]) and not correct(BY_ID["ms02"], ["a"], ["a", "b"])
    assert correct(BY_ID["ms09"], 7, 7) and not correct(BY_ID["ms09"], True, 1) and not correct(BY_ID["ms09"], "7", 7)
    assert correct(BY_ID["ms07"], [["a", 2]], [["a", 2]]) and not correct(BY_ID["ms07"], [["a", 2], ["b", 1]], [["b", 1], ["a", 2]])
    assert correct(BY_ID["ms18"], {"k": ["b", "a"]}, {"k": ["a", "b"]}) and not correct(BY_ID["ms18"], {"k": []}, {"k": ["a"]})
    assert correct(BY_ID["ms05"], {"a": 1}, {"a": 1}) and not correct(BY_ID["ms05"], {"a": 1, "b": 0}, {"a": 1})


def _rec(task, arm, ok, read, calls=1, **kw):
    return {"task": task, "arm": arm, "correct": ok, "read": read, "written": 10, "calls": calls, "seconds": 0.1, **kw}


def test_gate_and_report():
    recs = [
        _rec("ms02", "plan", True, 1000, 2),
        _rec("ms02", "js", True, 200, total_us=3000, engine_us=1000),
        _rec("ms04", "plan", True, 100),
        _rec("ms04", "js", False, 300, total_us=2000, engine_us=1500, error="js runtime error"),
    ]
    summary = {a: suite.arm_summary([r for r in recs if r["arm"] == a], 400 if a == "js" else 0, 2) for a in ("plan", "js")}
    assert summary["plan"]["read_per_correct"] == 550 and summary["js"]["read_per_correct"] == 500
    assert summary["js"]["read_per_correct_amortised"] == 900 and summary["js"]["overhead_ms_median"] == 1.25
    by_task = {"ms02": {r["arm"]: r for r in recs[:2]}, "ms04": {r["arm"]: r for r in recs[2:]}}
    g = suite.gate(summary, by_task, "js")
    assert g["regressed"] == ["ms04"] and not g["pass"] and g["checks"]["overhead"]
    doc = {
        "meta": {"binary": "rueca test", "cbm": None, "tasks": 2},
        "summary": summary,
        "gates": {"js": g},
        "describe_api_tokens": {"js": 400},
        "by_expect": {"win": {"plan": summary["plan"]}},
        "records": recs,
        "cbm_skipped": {},
    }
    md = report.render(doc)
    assert "| js (`describe_api` 400 tokens) |" in md and "**fail**" in md and "ms04" in md


def _probe(task, lang, ok, tokens, attempt=1):
    return {"task": task, "language": lang, "correct": ok, "error": None, "script_tokens": tokens, "result_tokens": 20, "calls": 2, "try": attempt}


def test_probe_report():
    committed = report.load_probe()
    first = [r for r in committed if r["try"] == 1]
    assert len(first) == 20 and {r["language"] for r in first} == {"js", "starlark"} and all(r["task"] in BY_ID for r in committed)
    for r in committed:
        assert (BENCH_DIR / "multistep" / "probe" / "scripts" / f"{r['task']}-{r['language']}.{'js' if r['language'] == 'js' else 'star'}").is_file()
    assert report.probe_lines([]) == ["Not run."]
    md = "\n".join(report.probe_lines([_probe("ms02", "js", False, 300), _probe("ms02", "js", True, 310, 2)], [_rec("ms02", "plan", True, 1000), _rec("ms02", "js", True, 200)]))
    assert "| ms02 | callers-intersection | ✗ 300 / 2 → ✓ 310 / 2 | — |" in md
    assert "JavaScript: first try 0/1; after one repair 1/1 (repairs run: 1 of 1)" in md and "Starlark: not run." in md
    assert "first-try scripts —, reference scripts 200, tool-call plans 1,000." in md
