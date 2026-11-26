"""Instances the harness would pick from the mined rows: per `[[s1.repos]]`
entry the first `quota` rows by sha256(instance_id) (datasets.py:
select_instances; mined rows carry no assembled gold).
Usage: select_s1.py <s1_repos.toml> <rows.jsonl>... -> prints `split repo id` lines"""
import hashlib, json, sys, tomllib
from pathlib import Path
spec = tomllib.loads(Path(sys.argv[1]).read_text())["s1"]["repos"]
rows = [json.loads(l) for f in sys.argv[2:] for l in Path(f).read_text(encoding="utf-8").split("\n") if l]
for s in spec:
    c = sorted((r for r in rows if r["repo"] == s["repo"] and r["language"] == s["language"]), key=lambda r: hashlib.sha256(r["instance_id"].encode()).hexdigest())
    for r in c[: s["quota"]]:
        print(s.get("split", "gated"), s["repo"], r["instance_id"])
