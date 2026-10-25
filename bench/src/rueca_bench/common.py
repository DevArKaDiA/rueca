"""Paths, manifest loading, subprocess helpers, caching, path filters."""

from __future__ import annotations

import hashlib
import json
import os
import re
import subprocess
import threading
import time
import tomllib
from dataclasses import dataclass
from pathlib import Path
from typing import Any

def _find_bench_dir() -> Path:
    """bench/ directory: $RUECA_BENCH_DIR, the source checkout (editable
    install), the project of the active venv (`uv run --project bench`), or
    ./bench relative to the current directory."""
    cands = []
    if os.environ.get("RUECA_BENCH_DIR"):
        cands.append(Path(os.environ["RUECA_BENCH_DIR"]))
    cands.append(Path(__file__).resolve().parents[2])
    if os.environ.get("VIRTUAL_ENV"):
        cands.append(Path(os.environ["VIRTUAL_ENV"]).resolve().parent)
    cwd = Path.cwd().resolve()
    for d in (cwd, *cwd.parents):
        cands += [d, d / "bench"]
    for c in cands:
        if (c / "manifest.toml").is_file() and (c / "pyproject.toml").is_file():
            return c.resolve()
    raise SystemExit("rueca-bench: cannot locate bench/manifest.toml (set RUECA_BENCH_DIR)")


BENCH_DIR = _find_bench_dir()
REPO_ROOT = BENCH_DIR.parent
MANIFEST_PATH = BENCH_DIR / "manifest.toml"
CACHE = Path(os.environ.get("RUECA_BENCH_CACHE", BENCH_DIR / ".cache")).resolve()
RESULTS_DIR = BENCH_DIR / "results"
LABELS_DIR = BENCH_DIR / "labels"
DOCS_DIR = REPO_ROOT / "docs" / "bench"

REPOS_DIR = CACHE / "repos"
WORK_DIR = CACHE / "work"
DATASETS_DIR = CACHE / "datasets"
RUNS_DIR = CACHE / "runs"
SCIP_DIR = CACHE / "scip"
BIN_DIR = CACHE / "bin"
GOLD_DIR = CACHE / "gold"

# Pinned, sha256-verified assets (tool binaries, datasets, archives) may be
# read from another bench cache instead of being downloaded again, e.g. when
# several checkouts of the bench share one machine:
#   RUECA_BENCH_SHARED_CACHE=/path/to/other/bench/.cache
# Only files whose sha256 matches the manifest pin are used from there; nothing
# is ever written to it.
SHARED_CACHE = Path(os.environ["RUECA_BENCH_SHARED_CACHE"]).resolve() if os.environ.get("RUECA_BENCH_SHARED_CACHE") else None


def pinned_asset(local: Path, sha256: str | None) -> Path:
    """`local` if present, else the same relative path in the shared cache when
    its sha256 matches the pin, else `local` (to be fetched)."""
    if local.exists() or SHARED_CACHE is None or not sha256:
        return local
    try:
        shared = SHARED_CACHE / local.relative_to(CACHE)
    except ValueError:
        return local
    if shared.is_file() and sha256_file(shared) == sha256:
        return shared
    return local


def load_manifest() -> dict[str, Any]:
    with MANIFEST_PATH.open("rb") as fh:
        return tomllib.load(fh)


# --- languages (manifest `[languages.<family>]`) -----------------------------------
#
# A language family groups the instance languages that share one Rueca
# language pack and one report column (python; typescript + javascript;
# php; ...). Everything the harness needs per family is data in the manifest:
# member languages, report label, source extensions, excluded suffixes,
# test-file patterns. See bench/README.md, "Adding a language".


@dataclass(frozen=True)
class LanguageFamily:
    key: str
    label: str
    members: tuple[str, ...]
    extensions: tuple[str, ...]
    exclude_suffixes: tuple[str, ...]
    test_files: tuple[str, ...]


_FAMILIES: dict[str, LanguageFamily] | None = None


def families() -> dict[str, LanguageFamily]:
    """Families in manifest order."""
    global _FAMILIES
    if _FAMILIES is None:
        out: dict[str, LanguageFamily] = {}
        for key, spec in load_manifest().get("languages", {}).items():
            out[key] = LanguageFamily(
                key=key,
                label=spec.get("label", key),
                members=tuple(spec.get("members", [key])),
                extensions=tuple(spec["extensions"]),
                exclude_suffixes=tuple(spec.get("exclude_suffixes", [])),
                test_files=tuple(spec.get("test_files", [])),
            )
        _FAMILIES = out
    return _FAMILIES


def family_of(language: str) -> LanguageFamily:
    for fam in families().values():
        if language in fam.members:
            return fam
    raise KeyError(f"language {language!r} is not declared in manifest [languages]")


def lang_family(language: str) -> str:
    return family_of(language).key


def lang_group(language: str) -> str:
    """Report column label of an instance language (python, ts/js, php, ...)."""
    return family_of(language).label


def code_exts(language: str) -> tuple[str, ...]:
    return family_of(language).extensions


def is_code_file(path: str, language: str) -> bool:
    fam = family_of(language)
    if fam.exclude_suffixes and path.endswith(fam.exclude_suffixes):
        return False
    return path.endswith(fam.extensions)


_TEST_SEGMENTS = {"test", "tests", "__tests__", "__mocks__", "__fixtures__", "e2e", "spec", "specs"}
_TOP_LEVEL_TEST_SEGMENTS = {"testing"}
_TEST_FILE: re.Pattern | None = None


def _test_file_re() -> re.Pattern:
    global _TEST_FILE
    if _TEST_FILE is None:
        pats = [p for fam in families().values() for p in fam.test_files]
        _TEST_FILE = re.compile("(" + "|".join(pats) + ")") if pats else re.compile(r"(?!)")
    return _TEST_FILE


def is_test_path(path: str) -> bool:
    """Same heuristic for gold and every arm (documented in bench/README.md):
    a test directory segment, or a file name matching any language's
    `test_files` pattern."""
    parts = path.split("/")
    if parts and parts[0] in _TOP_LEVEL_TEST_SEGMENTS:
        return True
    if any(p in _TEST_SEGMENTS for p in parts[:-1]):
        return True
    return bool(_test_file_re().match(parts[-1]))


def candidate_path(path: str, language: str) -> bool:
    """Post-filter applied identically to every arm's ranked output."""
    return is_code_file(path, language) and not is_test_path(path)


@dataclass
class CmdResult:
    rc: int
    stdout: str
    stderr: str
    seconds: float
    timed_out: bool = False


def run(
    cmd: list[str],
    *,
    cwd: Path | None = None,
    env: dict[str, str] | None = None,
    timeout: float | None = None,
    check: bool = False,
) -> CmdResult:
    full_env = dict(os.environ)
    if env:
        full_env.update(env)
    t0 = time.perf_counter()
    try:
        proc = subprocess.run(
            cmd,
            cwd=cwd,
            env=full_env,
            capture_output=True,
            text=True,
            timeout=timeout,
            errors="replace",
        )
    except subprocess.TimeoutExpired as exc:
        out = exc.stdout.decode(errors="replace") if isinstance(exc.stdout, bytes) else (exc.stdout or "")
        err = exc.stderr.decode(errors="replace") if isinstance(exc.stderr, bytes) else (exc.stderr or "")
        return CmdResult(-1, out, err, time.perf_counter() - t0, timed_out=True)
    res = CmdResult(proc.returncode, proc.stdout, proc.stderr, time.perf_counter() - t0)
    if check and res.rc != 0:
        raise RuntimeError(f"command failed ({res.rc}): {' '.join(cmd)}\n{res.stderr[-2000:]}")
    return res


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def stable_key(s: str) -> str:
    return hashlib.sha256(s.encode()).hexdigest()


def read_json(path: Path) -> Any:
    with path.open() as fh:
        return json.load(fh)


def write_json(path: Path, data: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + ".tmp")
    with tmp.open("w") as fh:
        json.dump(data, fh, indent=1, sort_keys=False)
        fh.write("\n")
    tmp.replace(path)


def safe_name(s: str) -> str:
    return re.sub(r"[^A-Za-z0-9_.-]+", "_", s)


_locks: dict[str, threading.Lock] = {}
_locks_guard = threading.Lock()


def named_lock(name: str) -> threading.Lock:
    with _locks_guard:
        if name not in _locks:
            _locks[name] = threading.Lock()
        return _locks[name]


def log(msg: str) -> None:
    print(f"[{time.strftime('%H:%M:%S')}] {msg}", flush=True)
