"""Rueca's vector modes (RFC 0007 phase 1): the arm's configuration, the
hybrid arm against the fake binary (index with `--embed`, a vector cache
shared across instances, `mode` and fusion knobs passed through), the
sweep (one index per instance, one run directory per setting), the §7.6
plateau choice, and the report's hybrid column and §11.2 gate rows."""

import contextlib
import json

import pytest

import rueca_bench.report as report
import rueca_bench.s1 as s1_mod
import rueca_bench.sweep as sweep_mod
from conftest import git_repo
from rueca_bench import semgates
from rueca_bench.arms import canonical_arm, make_arm, run_label
from rueca_bench.arms.rueca import RuecaArm, RuecaConfig
from rueca_bench.common import safe_name, write_json

REPO = {"a/one.py": "def one():\n    return 1\n", "b/two.py": "def two():\n    return 2\n", "c/three.py": "def three():\n    return 3\n"}
KNOBS = ("RUECA_BENCH_RUECA_MODE", "RUECA_BENCH_VECTOR_PRESET", "RUECA_FUSION_ALPHA", "RUECA_FUSION_N", "RUECA_FUSION_NORM")


@pytest.fixture
def clean_env(monkeypatch):
    for k in KNOBS:
        monkeypatch.delenv(k, raising=False)
    return monkeypatch


@pytest.fixture
def hybrid_env(clean_env, fake_rueca, tmp_path):
    models = tmp_path / "models" / "qwen3-0.6b-q8"
    models.mkdir(parents=True)
    (models / "weights.gguf").write_text("w")
    clean_env.setenv("RUECA_BIN", str(fake_rueca))
    clean_env.setenv("RUECA_MODEL_DIR", str(tmp_path / "models"))
    clean_env.setenv("RUECA_BENCH_RUECA_VECTOR_DIR", str(tmp_path / "rvec"))
    clean_env.setenv("FAKE_RUECA_LOG", str(tmp_path / "calls.jsonl"))
    return tmp_path


def _calls(tmp_path) -> list[list[str]]:
    return [json.loads(x) for x in (tmp_path / "calls.jsonl").read_text().splitlines()]


def test_config_from_env_tag_and_arm_names(clean_env):
    assert RuecaConfig.from_env().run_label() == "rueca" and RuecaConfig().env() == {}
    assert canonical_arm("rueca") == "rueca" and run_label("rueca") == "rueca"
    clean_env.setenv("RUECA_BENCH_RUECA_MODE", "hybrid")
    clean_env.setenv("RUECA_FUSION_ALPHA", "0.3")
    clean_env.setenv("RUECA_FUSION_NORM", "theoretical")
    cfg = RuecaConfig.from_env()
    assert cfg.run_label() == "rueca-hybrid/qwen3-0.6b-q8-a0.3-n100-theoretical"
    assert cfg.env()["RUECA_FUSION_ALPHA"] == "0.3" and cfg.env()["RUECA_FUSION_N"] == "100"
    assert canonical_arm("rueca") == "rueca-hybrid" and canonical_arm("grep") == "grep"
    assert run_label("rueca-hybrid") == cfg.run_label() and run_label("rueca-semantic").startswith("rueca-semantic/")
    assert make_arm("rueca-hybrid").name == "rueca-hybrid"
    with pytest.raises(ValueError):
        RuecaConfig(mode="hybrid", norm="zscore")
    with pytest.raises(ValueError):
        RuecaConfig(mode="hybrid", alpha=1.5)


def test_hybrid_arm_through_the_fake_binary(hybrid_env):
    root = git_repo(hybrid_env / "repo", REPO)
    arm = RuecaArm(config=RuecaConfig(mode="hybrid", alpha=1.0))
    arm.preflight()
    h = arm.index(root, "s1-x", "python")
    assert h.ok and h.info["vectors"]["embedded"] == 3
    lexical_order = arm.localize(h, "one", 10)
    assert lexical_order.extra["files"] == ["a/one.py", "b/two.py", "c/three.py"]
    assert [x[0] for x in lexical_order.extra["scores"]["files"]] == lexical_order.extra["files"]
    dense = arm.with_config(RuecaConfig(mode="hybrid", alpha=0.0)).localize(h, "one", 10)
    assert dense.extra["files"] == ["c/three.py", "b/two.py", "a/one.py"]
    assert "semantic" in arm.with_config(RuecaConfig(mode="semantic")).version()
    # a second instance (another store) reuses the shared vector cache
    h2 = arm.index(root, "s1-y", "python")
    assert h2.info["vectors"]["cached"] == 3 and h2.info["vectors"]["embedded"] == 0
    idx = [c for c in _calls(hybrid_env) if c[0] == "index" and "--help" not in c]
    assert all("--embed=qwen3-0.6b-q8" in c for c in idx)
    queries = [json.loads(c[2]) for c in _calls(hybrid_env) if c[:2] == ["query", "search_symbols"]]
    assert queries and all(q["mode"] in ("hybrid", "semantic") for q in queries)


def test_lexical_arm_is_unchanged(hybrid_env):
    root = git_repo(hybrid_env / "repo", REPO)
    arm = RuecaArm()
    h = arm.index(root, "s1-x", "python")
    res = arm.localize(h, "one", 10)
    assert res.ok and "scores" not in res.extra and "vectors" not in h.info
    calls = _calls(hybrid_env)
    assert not any(a.startswith("--embed") for c in calls for a in c)
    assert all("mode" not in json.loads(c[2]) for c in calls if c[:2] == ["query", "search_symbols"])


def test_preflight_needs_weights(hybrid_env, clean_env):
    clean_env.setenv("RUECA_MODEL_DIR", str(hybrid_env / "nowhere"))
    with pytest.raises(RuntimeError, match="rueca models pull"):
        RuecaArm(config=RuecaConfig(mode="hybrid")).preflight()


def test_parse_alphas():
    assert sweep_mod.parse_alphas("0,0.1,...,1") == [round(i / 10, 10) for i in range(11)]
    assert sweep_mod.parse_alphas("0.2, 0.8") == [0.2, 0.8]
    with pytest.raises(ValueError):
        sweep_mod.parse_alphas("0,...,1")


def test_sweep_indexes_once_per_instance(hybrid_env, monkeypatch):
    root = git_repo(hybrid_env / "repo", REPO)
    monkeypatch.setattr(sweep_mod, "RUNS_DIR", hybrid_env / "runs")
    monkeypatch.setattr(s1_mod, "s1_gold", lambda inst: {})
    monkeypatch.setattr(s1_mod, "queries_for", lambda inst: {"kw": "one", "raw": "the one function"})
    monkeypatch.setattr(sweep_mod, "worktree", lambda *a: contextlib.nullcontext(root))
    sw = sweep_mod.Sweep(alphas=[0.0, 1.0], norms=["minmax", "theoretical"])
    configs = sw.configs()
    inst = {"id": "o__r-1", "repo": "o/r", "language": "python", "base_commit": "0" * 40}
    arm = RuecaArm(config=configs[0])
    sweep_mod.sweep_instance(arm, inst, configs, 10, force=False)
    assert sum(1 for c in _calls(hybrid_env) if c[0] == "index") == 1
    recs = {c.tag(): json.loads(sweep_mod.output_path(c, inst["id"]).read_text()) for c in configs}
    assert len(recs) == 4 and all(r["status"] == "ok" and r["arm"] == "rueca-hybrid" for r in recs.values())
    assert recs["qwen3-0.6b-q8-a1-n100-minmax"]["queries"]["kw"]["files"][0] == "a/one.py"
    assert recs["qwen3-0.6b-q8-a0-n100-minmax"]["queries"]["kw"]["files"][0] == "c/three.py"
    assert "eff" not in recs["qwen3-0.6b-q8-a0-n100-minmax"]
    sweep_mod.sweep_instance(arm, inst, configs, 10, force=False)  # everything cached: no work
    assert sum(1 for c in _calls(hybrid_env) if c[0] == "index") == 1
    assert sw.columns()[0] == "rueca-hybrid:qwen3-0.6b-q8-a0-n100-minmax"
    assert sweep_mod.Sweep(mode="semantic", norms=["minmax"]).configs() == [RuecaConfig("semantic", "qwen3-0.6b-q8", 0.0, 100, "minmax")]


# --- plateau --------------------------------------------------------------------------


def _res(curves: dict[str, list[float]], alphas: list[float], n_per_group: int = 10) -> tuple[dict, list[RuecaConfig]]:
    """A compare result whose two languages both score `v` per setting."""
    cols, configs = {}, []
    for norm, vals in curves.items():
        for a, v in zip(alphas, vals, strict=True):
            c = RuecaConfig("hybrid", "qwen3-0.6b-q8", a, 100, norm)
            configs.append(c)
            g = {"objective": v, "n": n_per_group}
            t = {"python": g, "go": g, "pooled": {"n": 2 * n_per_group}}
            cols[f"{c.arm_name}:{c.tag()}"] = {"table": {"kw": t, "raw": t}}
    return {"groups": ["python", "go"], "columns": cols}, configs


def test_plateau_takes_the_centre_not_the_argmax():
    alphas = [0.0, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6]
    res, configs = _res({"minmax": [0.5, 0.6, 0.70, 0.70, 0.70, 0.72, 0.4], "theoretical": [0.3] * 7}, alphas)
    p = sweep_mod.plateau(res, configs, "both")
    c = p["choice"]
    assert c["best"]["objective"] == 0.72 and c["tolerance"] == pytest.approx(0.05)
    assert c["plateau"]["alphas"] == [0.2, 0.3, 0.4, 0.5] and c["alpha"] == 0.3 and c["norm"] == "minmax"
    assert [r["stable"] for r in p["leave_one_out"]] == [True, True]
    text = sweep_mod.render_plateau(p)
    assert "**Choice: α = 0.3, minmax, N 100**" in text and "| python | 10 |" in text


def test_plateau_without_results():
    res, configs = _res({"minmax": [None, None]}, [0.0, 1.0])
    assert sweep_mod.plateau(res, configs)["choice"] is None


# --- report ---------------------------------------------------------------------------

GOLD = {"files": ["src/a.py"], "functions": [{"path": "src/a.py", "name": "f", "qualname": "f", "start": 10, "def_line": 10}], "function_source": "treesitter"}


def _rec(arm: str, iid: str, hit: bool) -> dict:
    files = ["src/a.py"] if hit else ["src/z.py"]
    funcs = [{"path": files[0], "name": "f" if hit else "z", "start_line": 10}]
    q = {"ok": True, "seconds": 0.01, "files": files, "functions": funcs}
    return {"arm": arm, "id": iid, "status": "ok", "index_seconds": 0.1, "queries": {"kw": q, "raw": q}, "arm_version": "test"}


def test_report_gains_hybrid_column_and_gate_rows(clean_env, tmp_path, monkeypatch):
    monkeypatch.setattr(s1_mod, "RUNS_DIR", tmp_path / "runs")
    monkeypatch.setattr(report, "GOLD_DIR", tmp_path / "gold")
    splits = report.s1_splits()
    insts = [i for i in s1_mod.instances() if i["language"] == "python" and splits.get(i["repo"], "gated") == "gated"][:3]
    for inst in insts:
        write_json(tmp_path / "gold" / "s1" / f"{safe_name(inst['id'])}.json", GOLD)
        write_json(s1_mod.run_path("grep", inst["id"]), _rec("grep", inst["id"], False))
        write_json(s1_mod.run_path("vector", inst["id"]), _rec("vector", inst["id"], False))
        write_json(s1_mod.run_path("rueca-hybrid", inst["id"]), _rec("rueca-hybrid", inst["id"], True))
    assert "rueca-hybrid/qwen3-0.6b-q8-a0.5-n100-minmax/" in str(s1_mod.run_path("rueca-hybrid", insts[0]["id"]))
    s1 = report.score_s1({})
    assert "rueca-hybrid" in report.s1_arms(s1) and s1["run_labels"]["rueca-hybrid"].endswith("minmax")
    card = report.build_scorecard(s1, {"groups": {}, "calibration": {}, "gold": {}, "rows": {}}, {"rows": []}, {})
    assert card["languages"]["python"]["metrics"]["file_acc@5"]["hybrid"] == 1.0
    L: list[str] = []
    report.render_scorecard(L, card, {"gold": {}}, {"versions_by_group": {}}, s1)
    assert "| Status | BM25 | cbm | vector | hybrid |" in "\n".join(L)
    g = semgates.gates(s1, "gated")
    first = g["pooled"][0]
    assert first["paired"]["diff"] == 1.0 and first["paired"]["n"] == 3 and first["status"] == "✅"
    assert g["languages"][0]["group"] == "python" and g["languages"][0]["better"] == "grep"
    assert semgates.gates(s1, "calibration") is None
    L = []
    semgates.render(L, semgates.all_gates(s1))
    assert "## Semantic search gates (RFC 0007 §11.2)" in "\n".join(L)
    # rows of another sweep setting are not carried into this one
    pub = {"per_instance": [dict(r, split="gated") for r in s1["per_instance"]], "run_labels": dict(s1["run_labels"], **{"rueca-hybrid": "rueca-hybrid/other"})}
    for inst in insts:
        s1_mod.run_path("rueca-hybrid", inst["id"]).unlink()
    assert "rueca-hybrid" not in report.score_s1(pub)["table"]
