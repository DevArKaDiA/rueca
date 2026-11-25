"""Final datasets: every eligible row of the repositories named in
s1_repos.toml, per language, sorted by instance_id. Prints sha256.
Usage: combine.py <s1_repos.toml> <per_repo dir> <out dir>"""
import hashlib, json, sys, tomllib
from pathlib import Path
spec = tomllib.loads(Path(sys.argv[1]).read_text())["s1"]["repos"]
per, out = Path(sys.argv[2]), Path(sys.argv[3])
# GitHub reports NOASSERTION; licence read from the repository's licence file
LICENSE_OVERRIDES = {"BrentOzarULTD/SQL-Server-First-Responder-Kit": "MIT", "pgpartman/pg_partman": "PostgreSQL"}
for lang in ("shell", "sql"):
    rows = []
    for s in spec:
        if s["language"] != lang: continue
        f = per / lang / (s["repo"].replace("/", "__") + ".jsonl")
        new = [json.loads(l) for l in f.read_text(encoding="utf-8").split("\n") if l]
        for r in new:
            r["license"] = LICENSE_OVERRIDES.get(r["repo"], r["license"])
        rows += new
    rows.sort(key=lambda r: r["instance_id"])
    assert len({r["instance_id"] for r in rows}) == len(rows)
    p = out / f"rueca_mined_{lang}.jsonl"
    p.write_text("".join(json.dumps(r, ensure_ascii=True, sort_keys=True) + "\n" for r in rows), encoding="utf-8")
    print(lang, len(rows), "rows", p.stat().st_size, "bytes sha256", hashlib.sha256(p.read_bytes()).hexdigest())
