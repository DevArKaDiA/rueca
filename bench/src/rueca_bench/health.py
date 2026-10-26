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

Two rates per repository (manifest `[health]`): **raw** over every file of
the language, and **gated** (the scorecard floor) over the same files minus

- vendored third-party code, by the directory names Rueca's ranking demotes
  (`rueca-segment` priors: `third_party`, `thirdparty`, `third-party`,
  `3rdparty`, `vendored`, `deps`, `dependencies`) plus `vendor` /
  `node_modules` (never indexed) and the manifest's per-repository
  third-party trees (`[[health.exclude]]`, each with its evidence);
- copies of GoogleTest / GoogleMock (`googletest`, `googlemock`, `gtest`,
  `gmock` directories, `gtest-all.cc`, `gmock-all.cc`);
- amalgamations (`single_include/`, `singleheader/`, file names containing
  `amalgamat`, and per-repository copies listed in `[[health.exclude]]`);
- files misdetected as the language (`[[health.not_language]]`: a path glob
  or a content pattern per rule, e.g. Objective-C headers or MicroPython's
  `qstrdefs*.h` macro tables in a C repository).

Each repository is indexed twice (all files, then the gated subset); the
excluded files are counted per rule in the output.

Raw outputs: .cache/runs/health/rueca/<name>.json.
"""

from __future__ import annotations

import fnmatch
import re
import traceback
from pathlib import Path

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


# Rueca's ranking (rueca-segment priors.rs, VENDORED_DIRS) plus the index's
# own excludes (`vendor`, `node_modules`).
RANKING_VENDORED_DIRS = ("third_party", "thirdparty", "third-party", "3rdparty", "vendored", "deps", "dependencies")
INDEX_EXCLUDED_DIRS = ("vendor", "node_modules")
GTEST_DIRS = ("googletest", "googlemock", "gtest", "gmock")
GTEST_FILES = ("gtest-all.cc", "gmock-all.cc", "gtest_main.cc", "gmock_main.cc")
AMALGAMATION_DIRS = ("single_include", "singleheader")


def _rules() -> dict:
    return load_manifest().get("health", {})


def exclusion_reason(path: str, language: str, repo: str | None = None, root: Path | None = None) -> str | None:
    """Why a file is out of the gated parse-error rate, or None (see the
    module docstring). Content rules read the first 64 KiB from `root`."""
    parts = path.split("/")
    dirs, name = parts[:-1], parts[-1]
    rules = _rules()
    if any(d in RANKING_VENDORED_DIRS or d in INDEX_EXCLUDED_DIRS for d in dirs):
        return "vendored"
    for spec in rules.get("exclude", []):
        if spec.get("repo") in (None, repo) and any(path == p or path.startswith(p.rstrip("/") + "/") for p in spec["paths"]):
            return spec.get("reason", "vendored")
    if any(d in GTEST_DIRS for d in dirs) or name in GTEST_FILES:
        return "gtest copy"
    if any(d in AMALGAMATION_DIRS for d in dirs) or "amalgamat" in name.lower():
        return "amalgamation"
    for rule in rules.get("not_language", []):
        if language not in rule.get("languages", [language]):
            continue
        if rule.get("repo") not in (None, repo):
            continue
        if "glob" in rule and not fnmatch.fnmatch(path, rule["glob"]):
            continue
        if "content" in rule:
            if root is None:
                continue
            try:
                head = (root / path).read_bytes()[:65536].decode("utf-8", "replace")
            except OSError:
                continue
            if not re.search(rule["content"], head, re.MULTILINE):
                continue
        return rule["reason"]
    return None


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
                raw = arm.health(root, t["name"], files)
                excluded: dict[str, int] = {}
                kept: list[str] = []
                for f in files:
                    why = exclusion_reason(f, t["language"], t["repo"], root)
                    if why:
                        excluded[why] = excluded.get(why, 0) + 1
                    else:
                        kept.append(f)
                rec = arm.health(root, t["name"], kept) if excluded else dict(raw)
                rec["raw"] = {k: raw.get(k) for k in ("ok", "files_listed", "files_extracted", "files_partial", "duration_ms")}
                rec["excluded"] = excluded
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
                "raw_files_extracted": (rec.get("raw") or rec).get("files_extracted") or 0,
                "raw_files_partial": (rec.get("raw") or rec).get("files_partial") or 0,
                "excluded": rec.get("excluded") or {},
            }
        )
    return {"rows": rows}
