"""Per-file view of the held-out SQL health corpus (manifest
`[[health.repos]]`, language sql): which files keep a strict parse error,
under the dialect each file was given *a priori* (docs/decisions.md,
2026-11-25) and the dialect the pack detected.

The gated number is the harness's (`rueca-bench run --suite health`, the
IndexReport of the `rueca` binary); this script only says which files and
which statements, with the `dialects` and `unparsed` examples of
`rueca-lang-sql` built from the same source:

    uv run --project bench python bench/datasets/heldout/sql_by_dialect.py \
        <target/release/examples> <out-dir>

Writes <out-dir>/sql_files.json (committed as sql_heldout_files.json) and
<out-dir>/unparsed/<repo>.txt (not committed: source text of the
repositories).
"""

from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

from rueca_bench.common import is_code_file, load_manifest
from rueca_bench.corpus import list_files, worktree
from rueca_bench.health import exclusion_reason

# Repositories whose files are labelled by a path word; every other
# repository's files carry the repository's label.
BY_PATH = {
    "apache/dolphinscheduler",
    "temporalio/temporal",
    "diesel-rs/diesel",
    "authelia/authelia",
    "camunda/camunda-bpm-platform",
    "atuinsh/atuin",
}
WORDS = [
    ("MySQL / MariaDB", {"mysql", "mariadb"}),
    ("PostgreSQL", {"postgres", "postgresql", "pg", "cockroach"}),
    ("SQLite", {"sqlite", "sqlite3"}),
    ("T-SQL", {"mssql", "sqlserver"}),
    ("Oracle", {"oracle"}),
    ("DB2", {"db2"}),
    ("H2", {"h2"}),
]
REPO_LABEL = {
    "supabase/postgres": "PostgreSQL",
    "matrix-org/synapse": "PostgreSQL",
    "supabase/auth": "PostgreSQL",
    "pgvector/pgvector": "PostgreSQL",
    "apache/age": "PostgreSQL",
    "apache/cloudstack": "MySQL / MariaDB",
    "vitessio/vitess": "MySQL / MariaDB",
    "github/gh-ost": "MySQL / MariaDB",
    "actualbudget/actual": "SQLite",
    "microsoft/sql-server-samples": "T-SQL",
    "marcingminski/sqlwatch": "T-SQL",
    "GoogleCloudPlatform/cortex-data-foundation": "BigQuery",
    "blockchain-etl/ethereum-etl-airflow": "BigQuery",
    "Velir/dbt-ga4": "dbt Jinja",
    "Snowflake-Labs/sf-samples": "Snowflake",
    "dbt-labs/dbt-adapters": "dbt Jinja",
    "rittmananalytics/ra_data_warehouse": "dbt Jinja",
    "oracle-samples/db-sample-schemas": "Oracle",
    "pljson/pljson": "Oracle",
}
# Without a path word: atuin's client migrations are SQLite, the other
# five have no default.
PATH_DEFAULT = {"atuinsh/atuin": "SQLite"}


def label(repo: str, path: str) -> str:
    if repo not in BY_PATH:
        return REPO_LABEL[repo]
    words = set(re.split(r"[^a-z0-9]+", path.lower()))
    for name, keys in WORDS:
        if words & keys:
            return name
    return PATH_DEFAULT.get(repo, "unlabelled")


def main() -> None:
    bins, out = Path(sys.argv[1]), Path(sys.argv[2])
    (out / "unparsed").mkdir(parents=True, exist_ok=True)
    repos = []
    for spec in load_manifest()["health"]["repos"]:
        if spec["language"] != "sql":
            continue
        repo = spec["repo"]
        with worktree(repo, spec["commit"], f"heldout-{repo.replace('/', '__')}") as root, tempfile.TemporaryDirectory() as tmp:
            tree = Path(tmp) / "tree"
            files, excluded, large = [], {}, []
            for f in list_files(root):
                if not is_code_file(f, "sql"):
                    continue
                src = root / f
                if src.is_symlink() or not src.is_file():
                    continue
                why = exclusion_reason(f, "sql", repo, root)
                if why:
                    excluded[f] = why
                    continue
                if src.stat().st_size > 2 * 1024 * 1024:
                    large.append(f)  # the examples skip files over 2 MiB
                files.append(f)
                dst = tree / f
                dst.parent.mkdir(parents=True, exist_ok=True)
                try:
                    os.link(src, dst)
                except OSError:
                    shutil.copy2(src, dst)
            res = subprocess.run([bins / "dialects", str(tree), "--files", "--heads"], capture_output=True, text=True, check=True)
            failing, detected, heads = {}, {}, {}
            for line in res.stdout.splitlines():
                m = re.match(r"^(\S+) (.*) strict (\d+)/(\d+) lenient (\d+)$", line)
                if m:
                    failing[m.group(2)] = {"detected": m.group(1), "strict": int(m.group(3)), "statements": int(m.group(4)), "lenient": int(m.group(5))}
                    continue
                cols = line.split("\t")
                if cols[0] == "head-only":
                    heads[cols[2]] = int(cols[1])
                elif len(cols) > 2 and cols[1].startswith("files "):
                    detected[cols[0]] = {"files": int(cols[1].split()[1]), "strict": int(re.search(r"files with errors (\d+)", cols[2]).group(1)), "lenient": int(re.search(r"files (\d+)", cols[3]).group(1))}
            un = subprocess.run([bins / "unparsed", str(tree), "--full"], capture_output=True, text=True, check=True)
            (out / "unparsed" / f"{repo.replace('/', '__')}.txt").write_text(un.stdout)
            causes: dict[str, dict[str, int]] = {}
            # Statements no dialect parses: "### <parser error>", "<file> [<line, column>]", text.
            for block in re.split(r"(?m)^### ", un.stdout)[1:]:
                lines = block.split("\n")
                head, sep, loc = lines[1].rpartition(" [")
                rel = head if sep and loc.endswith("]") and "]" not in loc[:-1] else lines[1]
                first = " ".join(" ".join(lines[2:]).split()[:3])[:60]
                key = f"error | {lines[0].removeprefix('sql parser error: ')[:80]} | {first}"
                causes.setdefault(rel, {})
                causes[rel][key] = causes[rel].get(key, 0) + 1
            # Statements accepted by their head alone: "<file>: <statement>".
            ho = subprocess.run([bins / "dialects", str(tree), "--heads-full", ""], capture_output=True, text=True, check=True)
            for line in ho.stdout.splitlines():
                rel = next((f for f in failing if line.startswith(f + ": ")), None)
                if rel is None:
                    continue
                key = "head-only | " + " ".join(line[len(rel) + 2 :].split()[:2]).upper()[:60]
                causes.setdefault(rel, {})
                causes[rel][key] = causes[rel].get(key, 0) + 1
        by_label: dict[str, list[int]] = {}
        for f in files:
            e = by_label.setdefault(label(repo, f), [0, 0])
            e[0] += 1
            e[1] += f in failing
        repos.append(
            {
                "repo": repo,
                "commit": spec["commit"],
                "files": len(files),
                "excluded": excluded,
                "over_2mib": large,
                "by_label": {k: {"files": v[0], "strict": v[1]} for k, v in sorted(by_label.items())},
                "detected": detected,
                "head_only": heads,
                "failing": {f: {**v, "label": label(repo, f), "causes": causes.get(f, {})} for f, v in sorted(failing.items())},
            }
        )
        print(repo, len(files), len(failing), flush=True)
    (out / "sql_files.json").write_text(json.dumps({"repos": repos}, indent=1, sort_keys=True) + "\n")


if __name__ == "__main__":
    main()
