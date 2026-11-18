"""CoIR runner (coir.py): profiles, published comparison, instruction policy
and file naming without mteb; a dry run through mteb's own retrieval
evaluator on its in-memory mock task (no network) when the optional `coir`
group is installed."""

import json

import pytest

from rueca_bench import coir
from rueca_bench.embed import CachedEmbedder, FakeEmbedder, RuecaEmbedder


def test_profiles_and_published_numbers():
    assert len(coir.PROFILES["cheap"]) == 7 and len(coir.PROFILES["full"]) == 10
    assert set(coir.PROFILES["cheap"]) <= set(coir.PUBLISHED["qwen3-0.6b"])
    assert coir.model_family("qwen3-0.6b-q4") == "qwen3-0.6b"
    assert coir.model_family("qwen3-4b-q4") == "qwen3-4b"
    assert coir.model_family("coderank-137m") is None
    cheap_mean = sum(coir.PUBLISHED["qwen3-0.6b"][t] for t in coir.PROFILES["cheap"]) / 7
    assert round(cheap_mean, 2) == 69.57


def test_compare_against_published_mean():
    out = coir.compare({"CosQA": 36.0, "AppsRetrieval": 76.0, "Custom": 10.0}, {"CosQA": 36.48, "AppsRetrieval": 75.34})
    assert out["tasks_with_published"] == ["CosQA", "AppsRetrieval"]
    assert out["delta"] == pytest.approx(0.09, abs=0.01) and out["within_tolerance"]
    assert coir.compare({"Custom": 10.0}, {})["mean"] == 10.0


def test_instruction_policy_and_result_names(tmp_path):
    task = coir.InstructionPolicy("task", "qwen3-0.6b-q8")
    assert task.for_task("CosQA") == coir.QWEN3_TASK_PROMPTS["CosQA"] and task.tag() == ""
    assert coir.InstructionPolicy("task", "coderank-137m").for_task("CosQA") is None
    assert coir.InstructionPolicy("preset").for_task("CosQA") is None
    literal = coir.InstructionPolicy("Find code")
    assert literal.for_task("CosQA") == "Find code" and literal.tag().startswith("-i")
    run = coir.CoirRun(dims=256, out_dir=tmp_path)
    assert run.out_path().name == "qwen3-0.6b-q8-256-512.json"
    assert coir.CoirRun(profile="full", limit_corpus=50, out_dir=tmp_path).out_path().name == "qwen3-0.6b-q8-native-512-full-smoke50.json"


def test_coir_dry_run_on_the_mteb_mock_task(tmp_path):
    pytest.importorskip("mteb")
    mock = pytest.importorskip("mteb.mocks.mock_tasks.retrieval")
    fake = FakeEmbedder(32)
    run = coir.CoirRun(preset="qwen3-0.6b-q8", tasks=["MockRetrievalTask"], out_dir=tmp_path / "coir")
    result = run.run(CachedEmbedder(fake, tmp_path / "vectors"), [mock.MockRetrievalTask()])
    row = result["tasks"]["MockRetrievalTask"]
    assert 0 <= row["ndcg@10"] <= 100 and row["published"] is None and row["delta"] is None
    assert row["docs"] > 0 and row["queries"] > 0
    assert result["summary"]["mean"] == row["ndcg@10"]
    written = json.loads(run.out_path().read_text())
    assert written["tasks"]["MockRetrievalTask"]["ndcg@10"] == row["ndcg@10"]
    assert written["model"]["model_key"] == "fake-32"


def test_coir_model_sends_task_instructions_to_the_binary(tmp_path, fake_rueca, monkeypatch):
    pytest.importorskip("mteb")
    mock = pytest.importorskip("mteb.mocks.mock_tasks.retrieval")
    log = tmp_path / "argv.jsonl"
    monkeypatch.setenv("FAKE_RUECA_LOG", str(log))
    monkeypatch.setenv("RUECA_BENCH_VECTOR_DIR", str(tmp_path / "vectors"))
    run = coir.CoirRun(tasks=["MockRetrievalTask"], instruction="Find the code", limit_corpus=1, out_dir=tmp_path / "coir")
    result = run.run(RuecaEmbedder(str(fake_rueca)), [mock.MockRetrievalTask()])
    assert result["tasks"]["MockRetrievalTask"]["instruction"] == "Find the code"
    assert result["tasks"]["MockRetrievalTask"]["docs"] <= 2  # one per split
    calls = [json.loads(x) for x in log.read_text().splitlines()]
    sent = [c[c.index("--instruction") + 1] for c in calls if "--instruction" in c]
    assert sent and set(sent) == {"Find the code"}
    assert run.out_path().name.endswith("-smoke1.json")
