"""The committed (mined) S1 datasets and the "issue names a gold file" flag."""

import hashlib
import json

from rueca_bench import datasets
from rueca_bench.common import BENCH_DIR, load_manifest
from rueca_bench.gold import issue_names_gold_file


def test_local_datasets_match_their_pin_and_columns():
    man = load_manifest()
    local = {k: c for k, c in man["datasets"].items() if "local" in c}
    assert "rueca_mined_shell" in local
    for key, cfg in local.items():
        path = BENCH_DIR / cfg["local"]
        assert hashlib.sha256(path.read_bytes()).hexdigest() == cfg["sha256"]
        rows = datasets.load_rows(key)
        assert rows and all({"instance_id", "repo", "base_commit", "patch", "problem_statement", "language"} <= set(r) for r in rows)
        assert all(len(r["base_commit"]) == 40 for r in rows)
        # one object per `\n`-terminated line, whatever separators an issue body carries
        assert len(rows) == path.read_bytes().count(b"\n")
        ids = {i["id"] for i in man["s1"]["instances"] if i["dataset"] == key}
        assert ids and ids <= {r["instance_id"] for r in rows}


def test_local_rows_are_read_by_newline_only(tmp_path, monkeypatch):
    text = json.dumps({"instance_id": "a__b-1", "repo": "a/b", "base_commit": "0" * 40, "patch": "", "problem_statement": "x y", "language": "shell", "extra": 1}, ensure_ascii=False)
    path = tmp_path / "d.jsonl"
    path.write_text(text + "\n", encoding="utf-8")
    monkeypatch.setattr(datasets, "_local_dataset", lambda key, cfg: path)
    rows = datasets._local_jsonl_rows("d", {"local": "d.jsonl"})
    assert len(rows) == 1 and rows[0]["problem_statement"] == "x y" and "extra" not in rows[0]


def test_issue_names_gold_file():
    files = ["plugins/git/git.plugin.zsh", "lib/cli.zsh"]
    assert issue_names_gold_file("see git.plugin.zsh", files)
    assert issue_names_gold_file("the `cli` command fails", files)  # name without its last extension
    assert not issue_names_gold_file("the prompt is slow", files)
    assert not issue_names_gold_file("anything", [])
