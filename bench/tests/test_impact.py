"""Impact instances (impact.py, org.py): a library's issue scored on the
consumer files that use what its fix changes; both kinds from one answer."""

from pathlib import Path

from rueca_bench import impact, org
from rueca_bench.impact import gold_symbols, impact_consumers, impact_record, label_uses, owner_candidates, use_candidates, use_hits
from rueca_bench.org import consumer_metrics, org_questions, score_arm

GOLD = {
    "files": ["src/Carbon/Traits/Date.php", "src/Carbon/CarbonInterface.php"],
    "functions": [{"path": "src/Carbon/Traits/Date.php", "name": "mutateIfMutable", "qualname": "Date::mutateIfMutable", "start": 10}],
}
PATHS = {
    "Carbon\\Carbon": "src/Carbon/Carbon.php",
    "Carbon\\Traits\\Date": "src/Carbon/Traits/Date.php",
    "Carbon\\CarbonInterface": "src/Carbon/CarbonInterface.php",
    "Carbon\\CarbonInterval": "src/Carbon/CarbonInterval.php",
    "Carbon\\Traits\\Units": "src/Carbon/Traits/Units.php",
}


class FakeLib:
    """Stands for PhpLib: `symbol_path` reads `classes`."""

    classes = PATHS


def fake_labeler(consumer: Path, libs: list, only=None):
    uses = {
        ("src/Cache/Repository.php", "briannesbitt/carbon", "Carbon\\Carbon"),  # reaches the Date trait (ancestor)
        ("src/Support/Interval.php", "briannesbitt/carbon", "Carbon\\CarbonInterval"),  # reaches nothing changed
        ("src/Support/Iface.php", "briannesbitt/carbon", "Carbon\\CarbonInterface::now"),  # member of a file without function gold
        ("tests/CarbonTest.php", "briannesbitt/carbon", "Carbon\\Carbon"),  # test file: filtered by `keep`
        ("src/Log/Writer.php", "Seldaek/monolog", "Monolog\\Logger"),  # another library
    }
    ancestors = {
        ("briannesbitt/carbon", "Carbon\\Carbon"): {"Carbon\\Traits\\Date", "Carbon\\CarbonInterface"},
        ("briannesbitt/carbon", "Carbon\\CarbonInterval"): {"Carbon\\Traits\\Units"},
    }
    return set(), uses, ancestors, {}


def _uses(monkeypatch) -> list[dict]:
    monkeypatch.setitem(impact.LABELERS, "php", fake_labeler)
    return label_uses(Path("/nowhere"), [("briannesbitt/carbon", FakeLib()), ("Seldaek/monolog", FakeLib())], "php")


def test_gold_symbols_are_qualified_name_segments_or_whole_files():
    assert gold_symbols(GOLD) == {"src/Carbon/Traits/Date.php": ["Date", "mutateIfMutable"], "src/Carbon/CarbonInterface.php": None}
    java = {"files": ["S.java"], "functions": [{"path": "S.java", "name": "write", "qualname": "Streams.AppendableWriter.write"}]}
    assert gold_symbols(java) == {"S.java": ["AppendableWriter", "Streams", "write"]}


def test_candidates_follow_the_edge_scorer_acceptance_rule():
    assert owner_candidates("A\\B::m", "php") == ["A\\B::m", "A\\B"]
    assert owner_candidates("com.x.C.D.m", "java") == ["com.x.C.D.m", "com.x.C.D", "com.x.C", "com.x"]
    assert owner_candidates("tokio::sync::broadcast::Sender", "rust") == ["tokio::sync::broadcast::Sender"]
    assert use_candidates("A\\B", {"A\\T", "A\\B"}, "php") == ["A\\B", "A\\T"]


def test_impact_consumers_from_labeler_uses(monkeypatch):
    uses = _uses(monkeypatch)
    by = {u["symbol"]: u for u in uses}
    assert by["Carbon\\Carbon"]["candidates"][1] == ["Carbon\\CarbonInterface", "src/Carbon/CarbonInterface.php"]
    symbols = gold_symbols(GOLD)
    assert use_hits(by["Carbon\\Carbon"]["candidates"], symbols, "php")
    assert not use_hits(by["Carbon\\CarbonInterval"]["candidates"], symbols, "php"), "an ancestor in a file the fix does not touch"
    keep = lambda path: not path.startswith("tests/")  # noqa: E731
    got = impact_consumers("briannesbitt/carbon", "php", symbols, {"laravel/framework": uses}, keep)
    assert got == [
        {"repo": "laravel/framework", "path": "src/Cache/Repository.php", "symbols": ["Carbon\\Carbon"]},
        {"repo": "laravel/framework", "path": "src/Support/Iface.php", "symbols": ["Carbon\\CarbonInterface::now"]},
    ]
    # only the touched trait method's names count in a file with function gold
    narrow = {"src/Carbon/Traits/Date.php": ["somethingElse"]}
    assert impact_consumers("briannesbitt/carbon", "php", narrow, {"laravel/framework": uses}, keep) == []


def test_impact_record_is_cached_and_empty_side_dropped(monkeypatch, tmp_path):
    monkeypatch.setattr(impact, "GOLD_DIR", tmp_path)
    uses = {"laravel/framework": _uses(monkeypatch)}
    inst = {"id": "briannesbitt__carbon-1", "repo": "briannesbitt/carbon", "language": "php", "pin": "P", "base_commit": "B", "query": "q", "gold": GOLD}
    rec = impact_record(inst, uses, lambda p: True)
    assert rec["kind"] == "impact" and len(rec["consumers"]) == 3 and rec["symbols"][1] == {"path": "src/Carbon/CarbonInterface.php", "names": None}
    assert (tmp_path / "org" / "impact" / "briannesbitt__carbon-1.json").exists()
    assert impact_record(dict(inst, gold={"files": ["nothing.php"], "functions": []}), uses, lambda p: True) is None


def test_consumer_metrics_ranks_distinct_candidate_files():
    consumers = [{"repo": "laravel/framework", "path": "src/A.php"}, {"repo": "laravel/framework", "path": "src/B.php"}]
    own = ["github.com/briannesbitt/carbon", "src/Carbon/Carbon.php", "f", 1, "Method"]
    items = [own] * 3 + [["github.com/x/other", f"src/O{k}.php", "o", 1, "Method"] for k in range(12)] + [["github.com/laravel/framework", "src/B.php", "b", 1, "Method"]]
    m = consumer_metrics(items, consumers)
    assert (m["consumer_hit@10"], m["consumer_hit@20"], m["consumer_recall@20"], m["consumer_files"]) == (0.0, 1.0, 0.5, 2)
    files = [["laravel/framework", "src/A.php"], ["acme/x", "src/A.php"]]
    assert consumer_metrics([], consumers, files)["consumer_hit@10"] == 1.0, "grep: the separate file ranking"
    assert consumer_metrics(items, [])["consumer_recall@20"] is None


def test_both_kinds_from_one_answer(monkeypatch):
    gold = {"files": ["src/Carbon/Carbon.php"], "functions": []}
    payload = {"consumers": [{"repo": "laravel/framework", "path": "src/A.php", "symbols": ["Carbon\\Carbon"]}], "symbols": []}
    issues = [{"id": "c-1", "repo": "briannesbitt/carbon", "language": "php", "kind": "issue", "pin": "P", "gold": gold}]
    impacts = [
        {"id": "c-1", "repo": "briannesbitt/carbon", "language": "php", "kind": "impact", "pin": "P", "gold": gold, "version": 1, **payload},
        {"id": "c-9", "repo": "briannesbitt/carbon", "language": "php", "kind": "impact", "pin": "P", "gold": gold, "version": 1, **payload},
    ]
    monkeypatch.setattr(org, "org_instances", lambda split, repos=None, workers=1: [dict(i) for i in issues])
    monkeypatch.setattr(org, "impact_instances", lambda split, repos=None: impacts)
    qs = org_questions("gated")
    assert [(q["id"], q["kinds"]) for q in qs] == [("c-1", ["issue", "impact"]), ("c-9", ["impact"])]
    assert qs[0]["kind"] == "issue" and qs[0]["impact"] == payload and "consumers" not in qs[1] and qs[1]["impact"] == payload
    own = ["github.com/briannesbitt/carbon", "src/Carbon/Carbon.php", "Carbon", 1, "Class"]
    cons = ["github.com/laravel/framework", "src/A.php", "a", 1, "Method"]
    ans = {"native": {"ok": True, "items": [own, cons]}, "per_repo": {"ok": True, "items": [cons], "tokens": 5}, "single": {"ok": True, "items": [own]}}
    rec = {"instances": [org._inst_record(q) for q in qs], "answers": {"c-1": ans, "c-9": ans}}
    issue = score_arm(rec)
    assert [r["id"] for r in issue["native"]["rows"]] == ["c-1"]
    imp = score_arm(rec, "impact")
    n, p = imp["native"], imp["per_repo"]
    assert [r["id"] for r in n["rows"]] == ["c-1", "c-9"] and all(r["kind"] == "impact" for r in n["rows"])
    assert n["rows"][0]["file_acc@5"] == issue["native"]["rows"][0]["file_acc@5"] == 1.0, "same answer, same own-side score"
    assert n["consumer_hit@10"]["value"] == 1.0 and n["consumer_hit@10"]["n"] == 2 and n["consumer_hit@10"]["ci"][1] == 1.0
    assert p["file_acc@5"]["value"] == 0.0 and p["consumer_recall@20"] == 1.0
    # an output written before the impact kind: every instance is an issue
    old = {"instances": [{k: v for k, v in rec["instances"][0].items() if k not in ("kinds", "impact")}], "answers": {"c-1": ans}}
    assert score_arm(old)["native"]["n"] == 1 and score_arm(old, "impact") == {}
