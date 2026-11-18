"""Localization scoring with a localization-only arm: all-gold metrics
(metrics.py), the S1 tables and the scorecard with a `vector` column
(report.py), the SWE-bench Lite diagnostic's scoring and rendering (lite.py)."""

import pytest

import rueca_bench.lite as lite_mod
import rueca_bench.report as report
import rueca_bench.s1 as s1_mod
from rueca_bench.common import safe_name, write_json
from rueca_bench.metrics import score_files, score_functions

GOLD_FILES = ["src/a.py", "src/b.py"]
GOLD_FUNCS = [
    {"path": "src/a.py", "name": "f", "qualname": "f", "start": 10, "def_line": 10},
    {"path": "src/b.py", "name": "g", "qualname": "g", "start": 3, "def_line": 3},
]


def _hit(path: str, name: str, line: int) -> dict:
    return {"path": path, "name": name, "start_line": line}


def test_all_gold_needs_every_gold_item_in_the_top_k():
    some = score_files(["src/a.py", "x.py", "y.py", "z.py", "w.py", "src/b.py"], GOLD_FILES)
    assert some["acc@5"] == 1.0 and some["all_acc@5"] == 0.0
    both = score_files(["src/b.py", "x.py", "src/a.py"], GOLD_FILES)
    assert both["all_acc@5"] == 1.0
    assert score_files(["x.py"], [])["all_acc@5"] == 0.0
    fn = score_functions([_hit("src/a.py", "f", 11)] + [_hit("o.py", f"h{i}", i) for i in range(9)] + [_hit("src/b.py", "g", 3)], GOLD_FUNCS)
    assert fn["acc@10"] == 1.0 and fn["all_acc@10"] == 0.0
    fn = score_functions([_hit("src/b.py", "g", 3), _hit("src/a.py", "f", 9)], GOLD_FUNCS)
    assert fn["all_acc@10"] == 1.0


def _run_record(arm: str, iid: str, files: list[str], funcs: list[dict]) -> dict:
    q = {"ok": True, "seconds": 0.01, "files": files, "functions": funcs}
    return {"arm": arm, "id": iid, "status": "ok", "index_seconds": 0.1, "queries": {"kw": q, "raw": q}, "arm_version": "test"}


@pytest.fixture
def cache(tmp_path, monkeypatch):
    for k in ("PRESET", "DIMS", "MAX_TOKENS", "INSTRUCTION", "FILE_TEXT"):
        monkeypatch.delenv(f"RUECA_BENCH_VECTOR_{k}", raising=False)
    monkeypatch.setattr(s1_mod, "RUNS_DIR", tmp_path / "runs")
    monkeypatch.setattr(report, "GOLD_DIR", tmp_path / "gold")
    monkeypatch.setattr(lite_mod, "GOLD_DIR", tmp_path / "gold")
    return tmp_path


def _write_gold(root, iid: str) -> None:
    write_json(root / "gold" / "s1" / f"{safe_name(iid)}.json", {"id": iid, "files": GOLD_FILES, "functions": GOLD_FUNCS, "function_source": "treesitter"})


def test_s1_tables_and_scorecard_gain_a_vector_column(cache):
    splits = report.s1_splits()
    inst = next(i for i in s1_mod.instances() if i["language"] == "python" and splits.get(i["repo"], "gated") == "gated")
    _write_gold(cache, inst["id"])
    write_json(s1_mod.run_path("grep", inst["id"]), _run_record("grep", inst["id"], ["src/a.py"], [_hit("src/a.py", "f", 10)]))
    write_json(s1_mod.run_path("vector", inst["id"]), _run_record("vector", inst["id"], ["src/b.py", "src/a.py"], [_hit("src/b.py", "g", 3), _hit("src/a.py", "f", 10)]))
    assert "vector/" in str(s1_mod.run_path("vector", inst["id"]))
    s1 = report.score_s1({})
    vec = s1["table"]["vector"]["kw"]["python"]
    assert vec["file_all_acc@5"] == 1.0 and vec["func_all_acc@10"] == 1.0
    assert s1["table"]["grep"]["kw"]["python"]["file_all_acc@5"] == 0.0
    assert report.s1_arms(s1) == ["grep", "cbm", "rueca", "vector"]
    L: list[str] = []
    report._render_s1(L, s1, report.s1_arms(s1))
    text = "\n".join(L)
    assert "python: vector" in text and "File Acc@5, all gold (diagnostic)" in text
    card = report.build_scorecard(s1, {"groups": {}, "calibration": {}, "gold": {}, "rows": {}}, {"rows": []}, {})
    assert card["languages"]["python"]["metrics"]["file_acc@5"]["vector"] == 1.0
    L = []
    report.render_scorecard(L, card, {"gold": {}}, {"versions_by_group": {}}, s1)
    text = "\n".join(L)
    assert "| Metric | Rueca (95% CI) | Floor | Target | Status | BM25 | cbm | vector |" in text
    assert "setting `qwen3-0.6b-q8-dfull-t512-idefault-f0-x1`" in text


def test_carried_rows_without_all_gold_still_aggregate():
    rows = [{"ok": True, "file": {"acc@1": 1.0, "acc@3": 1.0, "acc@5": 1.0, "recall@5": 1.0, "recall@10": 1.0, "mrr": 1.0}, "function": None, "context": {}, "index_seconds": 1, "query_seconds": 1}]
    out = report.agg_s1(rows, "t")
    assert out["file_acc@5"] == 1.0 and out["file_all_acc@5"] is None


def test_lite_scores_any_and_all_gold(cache):
    insts = [{"id": "x__y-1", "repo": "x/y", "language": "python", "base_commit": "0" * 40, "dataset": "swe_bench_lite"}]
    _write_gold(cache, "x__y-1")
    write_json(s1_mod.run_path("vector", "x__y-1", "lite"), _run_record("vector", "x__y-1", ["src/a.py"], [_hit("src/b.py", "g", 3), _hit("src/a.py", "f", 10)]))
    lite = lite_mod.score(insts)
    t = lite["table"]["vector"]["raw"]
    assert (t["file_acc@5"], t["file_all_acc@5"], t["func_acc@10"], t["func_all_acc@10"]) == (1.0, 0.0, 1.0, 1.0)
    L: list[str] = []
    lite_mod.render(L, lite)
    assert "| vector | `raw` | 1 | 100.0" in "\n".join(L)
