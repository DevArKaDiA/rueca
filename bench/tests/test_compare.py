"""`rueca-bench compare` (compare.py) on synthetic cached outputs: columns
per tag, scoring, paired bootstrap, the RFC 0007 §7.6 objective, top-10
overlap, Markdown and JSON."""

import json

import pytest

from rueca_bench import cli
from rueca_bench.common import safe_name, write_json
from rueca_bench.compare import Comparison, columns_for, jaccard, render, split_instances

GOLD_FILES = ["src/a.py"]
GOLD_FUNCS = [{"path": "src/a.py", "name": "f", "qualname": "f", "start": 10, "def_line": 10}]
HIT = {"path": "src/a.py", "name": "f", "start_line": 10}
MISS = {"path": "src/z.py", "name": "z", "start_line": 1}
TAG_A = "qwen3-0.6b-q8-dfull-t512-idefault-f0-x1"
TAG_B = "qwen3-0.6b-q8-d256-t512-idefault-f0-x1"


def _record(files: list[str], funcs: list[dict], status: str = "ok") -> dict:
    q = {"ok": True, "seconds": 0.01, "files": files, "functions": funcs}
    return {"status": status, "queries": {"kw": q, "raw": q}}


def _two_languages() -> list[dict]:
    insts = split_instances("calibration")
    first = insts[0]
    other = next(i for i in insts if i["language"] != first["language"])
    return [first, other]


@pytest.fixture
def cache(tmp_path):
    insts = _two_languages()
    for inst in insts:
        write_json(tmp_path / "gold" / "s1" / f"{safe_name(inst['id'])}.json", {"files": GOLD_FILES, "functions": GOLD_FUNCS})
    hit, miss = insts
    runs = tmp_path / "runs" / "s1"
    write_json(runs / "grep" / f"{safe_name(hit['id'])}.json", _record(["src/a.py"], [HIT]))
    write_json(runs / "grep" / f"{safe_name(miss['id'])}.json", _record(["src/z.py"], [MISS]))
    for tag in (TAG_A, TAG_B):
        write_json(runs / "vector" / tag / f"{safe_name(hit['id'])}.json", _record(["src/a.py", "src/z.py"], [HIT, MISS]))
    # TAG_A also has the second instance (a hit); TAG_B is still running there
    write_json(runs / "vector" / TAG_A / f"{safe_name(miss['id'])}.json", _record(["src/a.py"], [HIT]))
    return tmp_path


def test_columns_expand_tags(cache):
    runs = cache / "runs" / "s1"
    assert [c.label for c in columns_for(runs, "grep")] == ["grep"]
    assert [c.tag for c in columns_for(runs, "vector")] == sorted([TAG_A, TAG_B])
    assert [c.label for c in columns_for(runs, f"vector:{TAG_B}")] == [f"vector:{TAG_B}"]
    assert columns_for(runs, "rueca-hybrid")[0].label == "rueca-hybrid"


def test_scores_pairs_objective_and_overlap(cache):
    res = Comparison(cache, "calibration", "grep", ["vector"]).run()
    grep = res["columns"]["grep"]
    a, b = res["columns"][f"vector:{TAG_A}"], res["columns"][f"vector:{TAG_B}"]
    assert grep["table"]["kw"]["pooled"]["file_acc@5"] == 0.5 and grep["n"] == 2
    assert a["table"]["kw"]["pooled"]["file_acc@5"] == 1.0 and a["table"]["raw"]["pooled"]["func_all@10"] == 1.0
    assert b["n"] == 1
    # objective: macro over the two languages of ½ (File + Fn)
    assert grep["objective"]["kw"] == 0.5 and a["objective"]["both"] == 1.0
    p = res["paired"][f"vector:{TAG_A}"]["kw"]["pooled"]["file_acc@5"]
    assert p["diff"] == 0.5 and p["n"] == 2 and len(p["ci"]) == 2
    assert res["paired"][f"vector:{TAG_B}"]["kw"]["pooled"]["file_acc@5"]["n"] == 1
    ov = [o for o in res["overlap"] if o["variant"] == "kw"]
    assert ov == [{"a": f"vector:{TAG_B}", "b": f"vector:{TAG_A}", "variant": "kw", "n": 1, "files": 1.0, "functions": 1.0}]
    md = render(res, "both")
    assert "## Paired vs `grep`, `raw`" in md and "Top-10 overlap" in md and "| grep | kw | 2 |" in md


def test_failed_queries_are_misses(cache):
    inst = _two_languages()[0]
    write_json(cache / "runs" / "s1" / "grep" / f"{safe_name(inst['id'])}.json", _record(["src/a.py"], [HIT], status="error"))
    res = Comparison(cache, "calibration", "grep", [f"vector:{TAG_A}"]).run()
    t = res["columns"]["grep"]["table"]["kw"]["pooled"]
    assert t["file_acc@5"] == 0.0 and t["n_failed"] == 1


def test_jaccard():
    assert jaccard(["a", "b"], ["b", "c"]) == pytest.approx(1 / 3)
    assert jaccard([], []) == 1.0


def test_cli_writes_json_and_markdown(cache, tmp_path, capsys):
    out = tmp_path / "cmp.json"
    assert cli.main(["compare", "--split", "calibration", "--arms", "vector", "--variant", "raw", "--cache", str(cache), "--json", str(out)]) == 0
    text = capsys.readouterr().out
    assert "# S1 compare" in text and "`kw`" not in text.split("## Objective")[1]
    data = json.loads(out.read_text())
    assert data["base"] == "grep" and set(data["columns"]) == {"grep", f"vector:{TAG_A}", f"vector:{TAG_B}"}
