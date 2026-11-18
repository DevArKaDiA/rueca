"""RFC 0007 phases 2–3 measurement wiring: second-stage knobs of the Rueca
arm, `rueca query --batch` (prefetch), the organization scenario in hybrid
mode on a tiny fixture organization (`index --org --embed`, impact hit@10
with and without expansion), the phase 2 sweeps (one batch per instance and
environment, the cheapest-within-one-instance choice, the reranker gate),
the latency rows, the vector arm's CPU knob, and the §11.2 table."""

import contextlib
import json

import pytest

import rueca_bench.latency as latency
import rueca_bench.org as org
import rueca_bench.s1 as s1_mod
import rueca_bench.sweep as sweep_mod
from conftest import git_repo
from rueca_bench import check, orgstats, phase2, rfc0007
from rueca_bench.arms import make_arm, run_label
from rueca_bench.arms.rueca import STAGE2_ENV, RuecaArm, RuecaConfig
from rueca_bench.arms.vector import VectorArm, VectorConfig
from rueca_bench.embed import RuecaEmbedder

KNOBS = ("RUECA_BENCH_RUECA_MODE", "RUECA_BENCH_VECTOR_PRESET", "RUECA_FUSION_ALPHA", "RUECA_FUSION_N", "RUECA_FUSION_NORM", "RUECA_BENCH_VECTOR_CPU", *STAGE2_ENV)
LIB = {"lib/parse.py": "def parse(text):\n    return text\n", "lib/other.py": "def other():\n    return 0\n"}
APP = {**{f"app/a{i:02d}.py": f"def a{i:02d}():\n    return {i}\n" for i in range(12)}, "app/main.py": "from lib.parse import parse\n\n\ndef main():\n    return parse('x')\n"}


@pytest.fixture
def env(monkeypatch, fake_rueca, tmp_path):
    for k in KNOBS:
        monkeypatch.delenv(k, raising=False)
    for preset in ("qwen3-0.6b-q8", "qwen3-reranker-0.6b-q8"):
        (tmp_path / "models" / preset).mkdir(parents=True)
        (tmp_path / "models" / preset / "weights.gguf").write_text("w")
    monkeypatch.setenv("RUECA_BIN", str(fake_rueca))
    monkeypatch.setenv("RUECA_MODEL_DIR", str(tmp_path / "models"))
    monkeypatch.setenv("RUECA_BENCH_RUECA_VECTOR_DIR", str(tmp_path / "rvec"))
    monkeypatch.setenv("FAKE_RUECA_LOG", str(tmp_path / "calls.jsonl"))
    return monkeypatch


def calls(tmp_path) -> list[list[str]]:
    p = tmp_path / "calls.jsonl"
    return [json.loads(x) for x in p.read_text().splitlines()] if p.exists() else []


def queries(tmp_path) -> list[list[str]]:
    return [c for c in calls(tmp_path) if c[0] == "query" and "--help" not in c]


# --- configuration ------------------------------------------------------------------------


def test_second_stage_tags_labels_and_environment(env):
    assert RuecaConfig().plain and RuecaConfig().env() == {} and RuecaConfig().run_label() == "rueca"
    lex = RuecaConfig(expand="calls", decay=0.25)
    assert lex.arm_name == "rueca-lexical" and lex.run_label() == "rueca-lexical/lexical-xcalls-d0.25"
    assert lex.env()["RUECA_EXPAND"] == "calls" and lex.env()["RUECA_RERANK"] == "0" and "RUECA_FUSION_ALPHA" not in lex.env()
    hyb = RuecaConfig(mode="hybrid", beta=0.5, rerank=True, rerank_k=20)
    assert hyb.tag() == "qwen3-0.6b-q8-a0.5-n100-minmax-b0.5-r20" and hyb.label() == f"rueca-hybrid:{hyb.tag()}"
    assert hyb.query_args() == {"mode": "hybrid", "rerank": True, "rerank_k": 20}
    # phase 1 tags are unchanged: cached runs keep their directories
    assert RuecaConfig(mode="hybrid").run_label() == "rueca-hybrid/qwen3-0.6b-q8-a0.5-n100-minmax"
    # today's lexical arm turns a knob of the caller's environment off explicitly
    env.setenv("RUECA_EXPAND", "calls")
    assert RuecaConfig().env() == {"RUECA_EXPAND": "off"}
    assert RuecaConfig.from_env().arm_name == "rueca-lexical" and run_label("rueca-lexical").startswith("rueca-lexical/lexical-xcalls")
    env.delenv("RUECA_EXPAND")
    with pytest.raises(ValueError, match="second-stage knob"):
        make_arm("rueca-lexical")
    with pytest.raises(ValueError):
        RuecaConfig(expand="imports")


def test_reranked_preflight_needs_reranker_weights(env):
    RuecaArm(config=RuecaConfig(rerank=True)).preflight()
    with pytest.raises(RuntimeError, match="reranker weights"):
        RuecaArm(config=RuecaConfig(rerank=True, rerank_preset="missing-reranker")).preflight()


# --- batch ---------------------------------------------------------------------------------


def test_prefetch_answers_calls_from_one_batch(env, tmp_path):
    root = git_repo(tmp_path / "repo", {"a/one.py": "def one(): pass\n", "b/two.py": "def two(): pass\n"})
    arm = RuecaArm(config=RuecaConfig(mode="hybrid", alpha=1.0))
    h = arm.index(root, "x", "python")
    assert arm.supports_batch
    res = arm.prefetch([("search_symbols", arm.search_args(q, 10), "scoring") for q in ("one", "two")], h.root, h.state["store"])
    assert len(res.lines) == 2 and res.first_s is not None and len(res.warm_seconds()) == 1
    before = len(queries(tmp_path))
    assert arm.localize(h, "two", 10).extra["files"][0] == "b/two.py"  # from the memo
    assert len(queries(tmp_path)) == before
    batch = [c for c in queries(tmp_path) if "--batch" in c]
    assert len(batch) == 1 and "--store" in batch[0]
    arm.localize(h, "two", 10)  # popped: a process again
    assert len(queries(tmp_path)) == before + 1


def test_no_batch_falls_back_to_one_process_per_call(env, tmp_path):
    env.setenv("FAKE_RUECA_NO_BATCH", "1")
    root = git_repo(tmp_path / "repo", {"a/one.py": "def one(): pass\n"})
    arm = RuecaArm()
    h = arm.index(root, "x", "python")
    assert arm.prefetch([("search_symbols", arm.search_args("one", 10), "scoring")], h.root, h.state["store"]) is None
    assert arm.localize(h, "one", 10).ok


# --- organization in hybrid mode ------------------------------------------------------------


@pytest.fixture
def tiny_org(env, tmp_path):
    """Two members, a library and its consumer; one impact question on the
    library whose consumer file is outside the top 10 without expansion."""
    roots = {"acme/lib": git_repo(tmp_path / "wt" / "org-acme__lib", LIB), "acme/app": git_repo(tmp_path / "wt" / "org-acme__app", APP)}
    env.setattr(org, "add_worktree", lambda repo, commit, name: roots[repo])
    env.setattr(org, "remove_worktree", lambda repo, root: None)
    env.setattr(org, "CACHE", tmp_path / "cache")
    env.setattr(org, "run_path", lambda kind, arm: tmp_path / "runs" / f"{kind}-{arm}.json")
    members = [{"repo": "acme/lib", "language": "python", "commit": "L", "split": "gated"}, {"repo": "acme/app", "language": "python", "commit": "A", "split": "gated"}]
    inst = {
        "id": "acme__lib-1",
        "repo": "acme/lib",
        "language": "python",
        "kind": "impact",
        "kinds": ["impact"],
        "query": "parse",
        "gold": {"files": ["lib/parse.py"], "functions": [], "patch_at_pin": "neither"},
        "impact": {"consumers": [{"repo": "acme/app", "path": "app/main.py", "symbols": ["parse"]}], "symbols": [{"path": "lib/parse.py", "names": ["parse"]}]},
    }
    return members, inst


def _grep_record(members: list[dict], inst: dict) -> dict:
    items = [["acme/lib", "lib/parse.py", "parse", 1, "Function"]] + [["acme/app", f"app/a{i:02d}.py", f"a{i:02d}", 1, "Function"] for i in range(12)]
    answer = {"native": {"ok": True, "items": items}, "per_repo": {"ok": True, "items": items}, "single": {"ok": True, "items": items[:1]}}
    return org._record(members, [inst], {}, {inst["id"]: answer}, 0, 0) | {"arm": "grep"}


def test_org_hybrid_run_embeds_members_and_scores_impact(tiny_org, tmp_path):
    members, inst = tiny_org
    arm = RuecaArm(config=RuecaConfig(mode="hybrid", alpha=1.0, expand="calls"))
    out = org._multirepo_rueca(arm, ["gated"], {"gated": [inst]}, members, [])
    rec = out["gated"]
    idx = [c for c in calls(tmp_path) if c[:2] == ["index", "--org"]]
    assert idx and all("--embed=qwen3-0.6b-q8" in c for c in idx)
    assert rec["index"]["acme/lib"]["vectors"]["embedded"] == 2
    assert any("--batch" in c and "--org" in c for c in queries(tmp_path))  # one batch per question and process kind
    native = rec["answers"][inst["id"]]["native"]
    assert native["ok"] and [x[1] for x in native["items"][:2]] == ["lib/parse.py", "app/main.py"]
    assert org.org_label(arm) == f"rueca-hybrid--{arm.config.tag()}"
    # the same members again: every vector comes from the shared vector cache
    again = org._multirepo_rueca(arm, ["gated"], {"gated": [inst]}, members, [])
    assert again["gated"]["index"]["acme/app"]["vectors"]["cached"] == 13
    # scored next to BM25 through the report's path
    rec.update(arm=arm.name, setting=arm.config.as_dict())
    org.write_json(org.run_path("multirepo-gated", org.org_label(arm)), rec)
    org.write_json(org.run_path("multirepo-gated", "grep"), _grep_record(members, inst))
    split = org.score_split("gated")
    key = arm.config.label()
    assert key in split["arms"] and split["arms"][key]["impact"]["native"]["consumer_hit@10"]["value"] == 1.0
    st = orgstats.org_stats(split, "gated", resamples=200, seed=1)
    p = next(x for x in st["paired"] if x["kind"] == "impact" and x["metric"] == "consumer_hit@10" and x["a"] == f"{key} native" and x["b"] == "grep native")
    assert p["n"] == 1 and p["diff"] == 1.0
    row = rfc0007.org_impact_row(st, key)
    assert row["status"] in ("✅", "⚠") and "+100.0" in row["today"]


def test_impact_sweep_indexes_once_and_reads_expansion(tiny_org, tmp_path, monkeypatch):
    members, inst = tiny_org
    monkeypatch.setattr(org, "impact_questions", lambda split, repos=None: [dict(inst)])
    monkeypatch.setattr(org, "org_members", lambda split: members)
    sw = sweep_mod.Sweep(mode="lexical", expands=["off", "calls"], decays=[0.5, 0.25], suite="org", split="gated")
    configs = sw.configs()
    assert [c.label() for c in configs] == ["rueca", "rueca-lexical:lexical-xcalls-d0.5", "rueca-lexical:lexical-xcalls-d0.25"]
    org.run_impact_sweep(RuecaArm(config=configs[0]), configs, "gated")
    assert sum(1 for c in calls(tmp_path) if c[:2] == ["index", "--org"]) == 2  # two members, once
    org.write_json(org.run_path("multirepo-gated", "grep"), _grep_record(members, inst))
    text, res = sweep_mod.report(sw)
    hits = {r["setting"]: r["consumer_hit@10"] for r in res["impact"]["settings"]}
    assert hits == {"rueca": 0.0, "rueca-lexical:lexical-xcalls-d0.5": 1.0, "rueca-lexical:lexical-xcalls-d0.25": 1.0}
    # one instance of one question is the whole range: everything is within it, the cheapest (no edges) wins
    assert res["choice"]["tolerance"] == 1.0 and res["choice"]["setting"] == "rueca"
    paired = {r["setting"]: r["paired"]["diff"] for r in res["impact"]["settings"]}
    assert paired["rueca-lexical:lexical-xcalls-d0.5"] == 1.0 and paired["rueca"] == 0.0
    assert "Organization impact sweep" in text and "gated population" in text


# --- S1 sweeps ------------------------------------------------------------------------------


def test_s1_sweep_one_batch_per_instance_and_environment(env, tmp_path, monkeypatch):
    root = git_repo(tmp_path / "repo", {"a/one.py": "def one(): pass\n", "b/two.py": "def two(): pass\n", "c/three.py": "def three(): pass\n"})
    monkeypatch.setattr(sweep_mod, "RUNS_DIR", tmp_path / "runs")
    monkeypatch.setattr(s1_mod, "s1_gold", lambda inst: {})
    monkeypatch.setattr(s1_mod, "queries_for", lambda inst: {"kw": "one", "raw": "the one function"})
    monkeypatch.setattr(sweep_mod, "worktree", lambda *a: contextlib.nullcontext(root))
    sw = sweep_mod.Sweep(mode="hybrid", alphas=[1.0], norms=["minmax"], reranks=[False, True], rerank_ks=[20, 50], expands=["off", "calls"])
    configs = sw.configs()
    assert len(configs) == 6 and phase2.varies_stage2(configs)
    inst = {"id": "o__r-1", "repo": "o/r", "language": "python", "base_commit": "0" * 40}
    arm = RuecaArm(config=configs[0])
    sweep_mod.sweep_instance(arm, inst, configs, 10, force=False)
    assert sum(1 for c in calls(tmp_path) if c[0] == "index") == 1
    assert [c for c in queries(tmp_path) if "--batch" not in c] == []  # every answer came from a batch
    assert sum(1 for c in queries(tmp_path) if "--batch" in c) == 2  # expansion off / calls: two environments
    recs = {c.label(): json.loads(sweep_mod.output_path(c, inst["id"]).read_text()) for c in configs}
    plain, k20 = recs[configs[0].label()], recs[configs[1].label()]
    assert plain["queries"]["kw"]["files"][0] == "a/one.py" and k20["queries"]["kw"]["files"][0] == "c/three.py"
    assert k20["setting"]["rerank_k"] == 20 and "-r20" in configs[1].tag()


def _rows(file_acc: list[float], func_acc: list[float]) -> dict:
    return {f"i{i}": {v: {"file_acc@5": f, "func_acc@10": g} for v in ("kw", "raw")} for i, (f, g) in enumerate(zip(file_acc, func_acc, strict=True))}


def test_reranker_gate_holds_and_fails():
    off, on = RuecaConfig(mode="hybrid"), RuecaConfig(mode="hybrid", rerank=True, rerank_k=20)
    n = 40
    base = _rows([1.0] * 20 + [0.0] * 20, [0.0] * n)
    lifted = _rows([1.0] * 19 + [0.0] * 21, [1.0] * 2 + [0.0] * (n - 2))  # Fn +5, File −1 instance
    g = phase2.rerank_gates({off.label(): base, on.label(): lifted}, [off, on])[0]
    assert g["func_ok"] and g["file_ok"] and g["status"] == "✅ holds" and g["one_instance"] == pytest.approx(1 / 40)
    dropped = _rows([1.0] * 18 + [0.0] * 22, [1.0] * 2 + [0.0] * (n - 2))  # File −2 instances
    g = phase2.rerank_gates({off.label(): base, on.label(): dropped}, [off, on])[0]
    assert g["func_ok"] and not g["file_ok"] and g["status"] == "❌ fails"
    text = phase2.render_gates([g], "both")
    assert "## Reranker gate (RFC 0007 §7.5)" in text and "**❌ fails**" in text


def test_cheapest_within_one_instance():
    cs = [RuecaConfig(), RuecaConfig(expand="calls"), RuecaConfig(expand="calls+contains"), RuecaConfig(beta=0.5), RuecaConfig(rerank=True)]
    vals = {cs[0].label(): 0.50, cs[1].label(): 0.58, cs[2].label(): 0.60, cs[3].label(): 0.59, cs[4].label(): 0.61}
    c = phase2.cheapest_within(vals, cs, 20)  # one instance = 0.05
    assert c["best"]["setting"] == cs[4].label() and c["setting"] == cs[3].label()  # reranker off, then no edges
    assert set(c["within"]) == {x.label() for x in cs[1:]}
    assert phase2.cheapest_within({}, cs, 20) is None


# --- latency --------------------------------------------------------------------------------


def test_latency_rows_warm_cold_and_reranker(env, tmp_path, monkeypatch):
    env.setenv("FAKE_RUECA_QUERY_CPU", "1")
    root = git_repo(tmp_path / "repo", {"a/one.py": "def one(): pass\n", "b/two.py": "def two(): pass\n"})
    monkeypatch.setattr(latency, "target_queries", lambda t: {"kw": [("one", 10), ("two", 10), ("one two", 10)], "raw": [("the one function", 100)]})
    monkeypatch.setattr(latency, "RUNS_DIR", tmp_path / "runs")
    arm = RuecaArm(config=latency.config())
    t = {"name": "s1-o__r", "repo": "o/r", "commit": "c", "language": "python", "source": "s1", "split": "gated"}
    rec = latency.measure(arm, t, root, rerank_sample=2)
    assert rec["ok"] and rec["batch"] and rec["warm"]["kw"][0] is None and all(x is not None for x in rec["warm"]["kw"][1:])
    assert len(rec["warm"]["raw"]) == 1 and len(rec["cold"]["kw"]) == 3
    assert rec["cpu_switch"] == ["--cpu"] and len(rec["rerank"]["cpu"]["20"]) == 2 and rec["rerank"]["default"]["ms50"] == [75.0, 75.0]
    assert any("--cpu" in c and "--batch" in c for c in queries(tmp_path))
    latency.write_json(latency.run_path(t["name"]), {**rec, **t})
    sc = latency.score()
    assert sc["status"] == "ok" and sc["warm_kw"]["n"] == 2 and sc["cold_kw"]["n"] == 3 and set(sc["rerank"]) == {"default", "cpu"}
    assert rfc0007.latency_row(sc)["status"] == "✅"


def test_latency_without_a_cpu_switch_says_so(env, tmp_path):
    arm = RuecaArm(config=latency.config())
    assert arm.cpu_switch() is None
    txt = rfc0007._rerank_txt({"status": "ok", "rerank": {"default": {"20": {"value": 10.0, "n": 1}}}, "cpu_switch": None})
    assert "exposes no CPU switch" in txt


# --- vector arm on the CPU ------------------------------------------------------------------


def test_vector_cpu_knob_has_its_own_tag_and_cache(env, tmp_path):
    env.setenv("RUECA_BENCH_VECTOR_CPU", "1")
    cfg = VectorConfig.from_env()
    assert cfg.cpu and cfg.tag().endswith("-cpu") and not cfg.backend_twin().tag().endswith("-cpu")
    arm = VectorArm(vector_root=tmp_path / "vec")
    assert isinstance(arm.embedder, RuecaEmbedder) and arm.embedder.cpu
    assert arm.embedder.namespace(False).split("/")[0].endswith("-cpu")
    assert RuecaEmbedder(arm.embedder.binary, cpu=True).namespace(False).split("/")[0] == "fake-qwen3-0.6b-q8"  # CoIR keeps its cache
    root = git_repo(tmp_path / "repo", {"m.py": "def f():\n    return 1\n"})
    h = arm.index(root, "x", "python")
    assert h.ok and any(c[0] == "embed" and "--cpu" in c for c in calls(tmp_path))


# --- the §11.2 table --------------------------------------------------------------------------


def test_rfc0007_table_rows_render_and_check(env, tmp_path, monkeypatch):
    monkeypatch.setattr(latency, "score", lambda cfg=None: {"status": "pending"})
    coir = tmp_path / "coir"
    coir.mkdir()
    (coir / "qwen3-0.6b-q8-native-512.json").write_text(json.dumps({"summary": {"mean_published_tasks": 68.34, "published_mean": 69.57, "delta": -1.23, "within_tolerance": False}}))
    t = rfc0007.table({"per_instance": [], "table": {}}, {"stats": {"gated": {}}}, [0.01] * 50 + [0.05] * 5, cache=tmp_path / "cache", coir_dir=coir)
    keys = [r["key"] for r in t["rows"]]
    assert len(keys) == 15 and keys[0] == "s1_file5_hybrid_bm25_raw" and keys[-1] == "backend_overlap"
    by = {r["key"]: r for r in t["rows"]}
    assert by["coir_cheap"]["status"] == "❌" and "Δ -1.23" in by["coir_cheap"]["today"]
    assert by["lexical_latency_p95_ms"]["status"] == "✅" and by["hybrid_latency_p95_ms"]["status"] == "pending"
    assert by["reindex_one_file"]["status"] == "reported" and by["org_impact_hit10"]["status"] == "pending"
    assert set(t["metrics"]) == {"coir_cheap", "lexical_latency_p95_ms"}
    L: list[str] = []
    rfc0007.render(L, t)
    md = "\n".join(L)
    assert "## RFC 0007 §11.2 acceptance table" in md and "| Metric | Today | Floor | Target | Status |" in md and md.count("\n| ") >= 16
    # check: a §11.2 row the current run did not measure is a note, a drop is a regression
    base = {"rfc0007": {"metrics": {"org_impact_hit10": {"value": 0.2, "ci": [0.1, 0.3], "floor": "interval not below 0", "gated": True}}}}
    fails, notes = check.compare(base, {"rfc0007": {"metrics": {}}})
    assert not fails and any("org_impact_hit10" in n for n in notes)
    fails, _ = check.compare(base, {"rfc0007": {"metrics": {"org_impact_hit10": {"value": 0.0}}}})
    assert fails
