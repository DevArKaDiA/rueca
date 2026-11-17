"""Statistics, scorecard block and report rendering of the organization
scenario (orgstats.py, report.py), on synthetic raw outputs."""

import math

import pytest
from rueca_bench import crossrepo, org, report
from rueca_bench.orgstats import (
    absolute_status,
    crossrepo_pools,
    mde,
    org_block,
    org_stats,
    paired_comparison,
    paired_join,
    relative_status,
)

RESAMPLES, SEED = 2000, 7


def test_mde_formula_and_the_old_population():
    assert mde(0.2, 100) == pytest.approx((1.96 + 0.84) * math.sqrt(0.002))
    assert mde(0.2, 100) == pytest.approx(0.12522, abs=1e-5)
    # same discordance, the 21 instances of the previous scenario: √(100 / 21) times larger
    assert mde(0.2, 21) / mde(0.2, 100) == pytest.approx(math.sqrt(100 / 21))
    assert mde(0.0, 50) == 0.0 and mde(0.3, 0) is None


def test_paired_join_keeps_instances_both_answered_in_a_order():
    a = [{"id": "x", "m": 1.0}, {"id": "y", "m": None}, {"id": "z", "m": 0.0}, {"id": "w", "m": 1.0}]
    b = [{"id": "z", "m": 1.0}, {"id": "x", "m": 1.0}, {"id": "y", "m": 0.0}]
    assert paired_join(a, b, "m") == (["x", "z"], [1.0, 0.0], [1.0, 1.0])


def test_paired_comparison_discordance_status_and_mde():
    # 40 instances: A hits 30, B hits 20; they disagree on 14 (12 A-only, 2 B-only)
    a = [1.0] * 18 + [1.0] * 12 + [0.0] * 2 + [0.0] * 8
    b = [1.0] * 18 + [0.0] * 12 + [1.0] * 2 + [0.0] * 8
    ra = [{"id": str(i), "k": v} for i, v in enumerate(a)]
    rb = [{"id": str(i), "k": v} for i, v in enumerate(b)]
    p = paired_comparison(ra, rb, "k", RESAMPLES, SEED)
    assert (p["n"], p["discordant"]) == (40, 14) and p["discordance"] == pytest.approx(0.35)
    assert p["diff"] == pytest.approx(0.25) and p["a"] == 0.75 and p["b"] == 0.5
    assert p["ci"][0] > 0 and p["status"] == "✅"
    assert p["half_width"] == pytest.approx((p["ci"][1] - p["ci"][0]) / 2)
    assert p["mde"] == pytest.approx(2.8 * math.sqrt(0.35 / 40)) and p["mde_old_n"] == pytest.approx(2.8 * math.sqrt(0.35 / 21))
    assert paired_comparison(ra, [], "k", RESAMPLES, SEED) == {"n": 0, "status": "pending"}


def test_statuses():
    assert relative_status([0.0, 0.1]) == "✅" and relative_status([-0.05, 0.1]) == "⚠" and relative_status([-0.2, -0.01]) == "❌"
    assert relative_status(None) == "pending"
    assert absolute_status(0.9, [0.8, 0.95], 0.85) == "⚠" and absolute_status(0.9, [0.86, 0.95], 0.85) == "✅"
    assert absolute_status(0.8, None, 0.85) == "❌" and absolute_status(0.5, None, 2.0, lower=True) == "✅"


def _lib(pred, corr, gold, found):
    return {"repo": "lib", "score": {"imports": {"predicted": 0, "correct": 0, "gold": 0, "found": 0}, "uses": {"predicted": pred, "correct": corr, "gold": gold, "found": found}}}


def test_crossrepo_pools_split_rust_from_the_rest():
    xr = {
        "status": "ok",
        "groups": [
            {"consumer": "a", "language": "python", "status": "ok", "libraries": [_lib(100, 99, 50, 48)]},
            {"consumer": "b", "language": "rust", "status": "ok", "libraries": [_lib(50, 45, 100, 40), _lib(10, 10, 10, 10)]},
            {"consumer": "c", "language": "java", "status": "error", "libraries": []},
        ],
    }
    pools = crossrepo_pools(xr)
    assert pools["all"]["pairs"] == 3 and pools["all"]["counts"] == {"predicted": 160, "correct": 154, "gold": 160, "found": 98}
    assert pools["without_rust"]["recall"]["value"] == 0.96 and pools["without_rust"]["recall"]["status"] == "✅"
    assert pools["rust"]["recall"]["value"] == pytest.approx(50 / 110) and pools["rust"]["recall"]["status"] == "❌"
    assert pools["rust"]["precision"]["floor"] == 0.9 and pools["rust"]["precision"]["ci"][0] < pools["rust"]["precision"]["value"]
    assert crossrepo_pools({}) == {}


# --- end to end: raw outputs → scores → statistics → scorecard block → markdown -----------------


def _inst(i: int, lang: str, kinds: list[str]) -> dict:
    gold = {"files": [f"src/f{i}.py"], "functions": [{"path": f"src/f{i}.py", "name": "f", "start": 1, "def_line": 1, "outer_start": 1}], "patch_at_pin": "base" if i % 2 else "applies"}
    rec = {"id": f"r{i}", "repo": f"acme/r{i % 3}", "language": lang, "kind": kinds[0], "kinds": kinds, "pin": "P", "query": "q", "gold": gold}
    if "impact" in kinds:
        rec["impact"] = {"consumers": [{"repo": "acme/consumer", "path": "src/use.py", "symbols": ["X"]}], "symbols": []}
    return rec


def _raw(arm: str, hits: set[int], n: int = 30) -> dict:
    insts = [_inst(i, "python" if i % 2 else "go", ["issue", "impact"] if i < 4 else ["issue"]) for i in range(n)]
    answers = {}
    for inst in insts:
        i = int(inst["id"][1:])
        own = [f"github.com/{inst['repo']}", f"src/f{i}.py", "f", 1, "Function"]
        miss = ["github.com/acme/other", "src/zzz.py", "z", 1, "Function"]
        cons = ["github.com/acme/consumer", "src/use.py", "u", 1, "Function"]
        items = ([own] if i in hits else [miss]) + ([cons] if arm == "rueca" else [])
        ans = {"per_repo": {"ok": True, "items": [miss], "tokens": 900}, "single": {"ok": True, "items": [own], "eff": {"ok": True, "tokens": 100, "prefix": [100], "items": [own]}}}
        if arm != "cbm":
            ans["native"] = {"ok": True, "items": items, "eff": {"ok": True, "tokens": 200, "prefix": [200, 300], "items": items}}
        answers[inst["id"]] = ans
    members = [{"repo": f"acme/r{k}", "language": "python", "commit": "P", "split": "gated"} for k in range(3)]
    index = {m["repo"]: {"ok": True, "seconds": 1.0, "org_ms": 100, "repo_ms": 1000} for m in members}
    inc = {"ok": True, "repo": "acme/r0", "docs": 10, "repo_ms": 1000, "first_repo_ms": 1200, "org_ms": 300, "org_repos": 3} if arm == "rueca" else None
    return {"status": "ok", "split": "gated", "arm": arm, "members": members, "instances": insts, "index": index, "incremental": inc, "answers": answers}


@pytest.fixture()
def raw_outputs(tmp_path, monkeypatch):
    runs = tmp_path / "runs"
    monkeypatch.setattr(org, "run_path", lambda kind, arm: runs / f"{kind}-{arm}.json")
    monkeypatch.setattr(crossrepo, "org_run_path", lambda kind, arm: runs / f"{kind}-{arm}.json")
    org.write_json(runs / "multirepo-gated-rueca.json", _raw("rueca", set(range(24))))
    org.write_json(runs / "multirepo-gated-grep.json", _raw("grep", set(range(12))))
    return runs


def test_stats_scorecard_block_and_markdown(raw_outputs):
    orgs = report.score_orgs({})
    mr, st = orgs["multirepo"], orgs["stats"]["gated"]
    assert mr["instances"] == 30 and mr["impact_instances"] == 4
    assert mr["counts"]["python"]["issue"]["patch_at_pin"] == {"base": 15} and mr["counts"]["go"]["issue"]["n"] == 15
    r = st["arms"]["rueca"]["native"]
    assert r["file_acc@5"]["value"] == 0.8 and r["file_acc@5"]["n"] == 30 and r["file_acc@5"]["ci"][0] < 0.8 < r["file_acc@5"]["ci"][1]
    assert r["file_acc@5_ratio"]["value"] == 0.8 and r["tokens_per_correct_ratio"]["ci"] is not None
    assert st["arms"]["cbm"]["status"] == "pending"
    rg = next(p for p in st["paired"] if p["kind"] == "issue" and p["metric"] == "file_acc@5" and p["a"] == "rueca native" and p["b"] == "grep native")
    assert (rg["n"], rg["discordant"], rg["status"]) == (30, 12, "✅") and rg["diff"] == pytest.approx(0.4)
    assert rg["mde"] == pytest.approx(2.8 * math.sqrt(0.4 / 30))
    imp = next(p for p in st["paired"] if p["kind"] == "impact" and p["metric"] == "consumer_hit@10" and p["b"] == "grep native")
    assert (imp["n"], imp["diff"]) == (4, 1.0)
    assert next(p for p in st["paired"] if p["b"] == "cbm per_repo")["status"] == "pending"
    rows = {x["key"]: x for x in orgs["rfc0006"]}
    assert rows["file_acc@5_ratio"]["status"] == "❌" and rows["incremental_update"]["ratio"] == 0.3 and rows["incremental_update"]["status"] == "✅"

    card = {"generated": "g", "languages": {"python": {"metrics": {"file_acc@5": {"value": 0.5}}}}, "config": {}, "settings": {}, "rueca": "v"}
    before = {k: repr(v) for k, v in card.items()}
    report.attach_org(card, orgs)
    assert list(card) == ["generated", "languages", "config", "settings", "rueca", "org"]
    assert all(repr(card[k]) == v for k, v in before.items())
    blk = card["org"]
    assert blk["status"] == "ok" and blk["instances"]["issue"] == 30 and blk["instances"]["impact"] == 4
    assert "rows" not in blk["arms"]["rueca"]["native"] and blk["settings"]["mde_old_n"] == 21 and len(blk["settings"]["pins_sha256"]) == 64

    L: list[str] = []
    report._render_org(L, orgs)
    md = "\n".join(L)
    assert "| Rueca | organization `--org` | 30 | 80.0 (" in md
    assert "| BM25 (grep) | one BM25 over the union | 30 | 40.0 (" in md
    assert "| cbm | — | — | pending | | | | | | | |" in md
    assert "| issue | rueca native − grep native | File Acc@5 | 30 | 80.0 | 40.0 | +40.0 (" in md and "| 12 (40.0) |" in md
    assert f"| {100 * 2.8 * math.sqrt(0.4 / 30):.1f} | {100 * 2.8 * math.sqrt(0.4 / 21):.1f} |" in md
    assert "**Impact instances** (4)" in md and "| Rueca | organization `--org` | 4 | " in md
    assert "Calibration split — never used for gating" in md and "not comparable with the 21-instance" in md
    assert "| python | 3 | 15 | 2 | 15 / 0 / 0 |" in md.replace("| gated ", "")


def test_old_scenario_is_never_carried_same_population_is(raw_outputs):
    old = {"multirepo": {"instances": 21, "arms": {"rueca": {"status": "ok", "modes": {}}, "cbm": {"status": "ok", "modes": {}}}}}
    (raw_outputs / "multirepo-gated-rueca.json").unlink()
    (raw_outputs / "multirepo-gated-grep.json").unlink()
    orgs = report.score_orgs(old)
    assert not orgs["multirepo"].get("arms") and orgs["stats"]["gated"]["arms"]["rueca"]["status"] == "pending"
    L: list[str] = []
    report._render_org(L, orgs)
    assert "Pending: no organization output" in "\n".join(L) and report.attach_org({}, orgs)["org"]["status"] == "pending"
    # a published output of the same population: carried per arm
    org.write_json(raw_outputs / "multirepo-gated-grep.json", _raw("grep", set(range(12))))
    published = report.score_orgs({})["multirepo"]
    org.write_json(raw_outputs / "multirepo-gated-rueca.json", _raw("rueca", set(range(24))))
    (raw_outputs / "multirepo-gated-grep.json").unlink()
    mr = report.score_orgs({"multirepo": published})["multirepo"]
    assert mr["arms"]["grep"].get("carried") and not mr["arms"]["rueca"].get("carried")
    # another population (an instance more): not carried
    other = _raw("rueca", set(range(24)), n=31)
    org.write_json(raw_outputs / "multirepo-gated-rueca.json", other)
    assert "grep" not in report.score_orgs({"multirepo": published})["multirepo"]["arms"]


def test_org_block_pending_keeps_cross_repository_pools():
    blk = org_block({}, org_stats({}, "gated", RESAMPLES, SEED), {})
    assert blk["status"] == "pending" and blk["crossrepo"] == {} and blk["settings"]["members"] >= 1
