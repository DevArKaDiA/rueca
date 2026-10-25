"""Health (S5, RFC 0004 §5): parse-error rate and index throughput per
language, from Rueca's own IndexReport.

Population: every file of the language (source extensions of its family,
tests included) in each corpus repository — the S3 gold repositories at
their pinned tags and every S1 repository at the base commit of its first
instance (manifest order). Each repository is indexed once per language on
a hard-linked scratch tree that holds only that language's files, so
`files_partial / files_extracted` is that language's rate (`.h` follows the
repository: C++ when it has C++ sources). Query latency p95 comes from the
S1 / S3 runs (report.py).

Raw outputs: .cache/runs/health/rueca/<name>.json.
"""

from __future__ import annotations

import traceback

from .arms import make_arm
from .common import RUNS_DIR, family_of, is_code_file, lang_family, load_manifest, log, read_json, safe_name, split_of, write_json
from .corpus import list_files, prune_clone, worktree
from .s1 import instances


def targets(languages: set[str] | None = None) -> list[dict]:
    """(name, repo, commit, language, source) of every corpus repository."""
    man = load_manifest()
    out: list[dict] = []
    for spec in man["s3"]["repos"]:
        out.append({"name": f"s3-{spec['repo'].split('/')[1]}", "repo": spec["repo"], "commit": spec["commit"], "language": spec["language"], "source": "s3", "split": split_of(spec)})
    split_by_repo = {r["repo"]: split_of(r) for r in man["s1"]["repos"]}
    seen: set[str] = set()
    for inst in instances():
        if inst["repo"] in seen:
            continue
        seen.add(inst["repo"])
        out.append({"name": f"s1-{inst['repo'].replace('/', '__')}", "repo": inst["repo"], "commit": inst["base_commit"], "language": inst["language"], "source": "s1", "split": split_by_repo.get(inst["repo"], "gated")})
    if languages:
        out = [t for t in out if lang_family(t["language"]) in languages]
    return out


def run_path(name: str):
    return RUNS_DIR / "health" / "rueca" / f"{safe_name(name)}.json"


def run(languages: set[str] | None = None, force: bool = False, only: list[str] | None = None, prune: bool = False) -> None:
    arm = make_arm("rueca")
    ok, why = arm.available()
    if not ok:
        log(f"health: rueca unavailable ({why})")
        return
    for t in targets(languages):
        if only and t["name"] not in only and t["repo"] not in only:
            continue
        p = run_path(t["name"])
        if p.exists() and not force and read_json(p).get("ok"):
            continue
        fam = family_of(t["language"])
        log(f"health {t['name']} ({fam.label})")
        try:
            with worktree(t["repo"], t["commit"], f"health-{t['name']}") as root:
                files = [f for f in list_files(root) if is_code_file(f, t["language"])]
                rec = arm.health(root, t["name"], files)
        except Exception as exc:
            rec = {"ok": False, "error": f"{exc}\n{traceback.format_exc()[-1000:]}"}
        rec.update({k: t[k] for k in ("name", "repo", "commit", "language", "source", "split")})
        rec["arm_version"] = arm.version()
        write_json(p, rec)
        if prune:
            prune_clone(t["repo"])


def score() -> dict:
    """Per family label: pooled files with parse errors over extracted files
    (gated repositories), throughput, and per-repository rows."""
    rows = []
    for t in targets():
        p = run_path(t["name"])
        if not p.exists():
            continue
        rec = read_json(p)
        if not rec.get("ok"):
            rows.append({**t, "ok": False, "error": rec.get("error")})
            continue
        rows.append(
            {
                **t,
                "ok": True,
                "files_extracted": rec.get("files_extracted") or 0,
                "files_partial": rec.get("files_partial") or 0,
                "duration_ms": rec.get("duration_ms"),
            }
        )
    return {"rows": rows}
