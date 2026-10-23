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
    """bench/ directory: $TELAR_BENCH_DIR, the source checkout (editable
    install), the project of the active venv (`uv run --project bench`), or
    ./bench relative to the current directory."""
    cands = []
    if os.environ.get("TELAR_BENCH_DIR"):
        cands.append(Path(os.environ["TELAR_BENCH_DIR"]))
    cands.append(Path(__file__).resolve().parents[2])
    if os.environ.get("VIRTUAL_ENV"):
        cands.append(Path(os.environ["VIRTUAL_ENV"]).resolve().parent)
    cwd = Path.cwd().resolve()
    for d in (cwd, *cwd.parents):
        cands += [d, d / "bench"]
    for c in cands:
        if (c / "manifest.toml").is_file() and (c / "pyproject.toml").is_file():
            return c.resolve()
    raise SystemExit("telar-bench: cannot locate bench/manifest.toml (set TELAR_BENCH_DIR)")


BENCH_DIR = _find_bench_dir()
REPO_ROOT = BENCH_DIR.parent
MANIFEST_PATH = BENCH_DIR / "manifest.toml"
CACHE = Path(os.environ.get("TELAR_BENCH_CACHE", BENCH_DIR / ".cache")).resolve()
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

PY_EXTS = (".py", ".pyi")
ECMA_EXTS = (".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs", ".cjs")


def load_manifest() -> dict[str, Any]:
    with MANIFEST_PATH.open("rb") as fh:
        return tomllib.load(fh)


def lang_family(language: str) -> str:
    return "python" if language == "python" else "ecma"


def code_exts(language: str) -> tuple[str, ...]:
    return PY_EXTS if lang_family(language) == "python" else ECMA_EXTS


def is_code_file(path: str, language: str) -> bool:
    if path.endswith(".d.ts"):
        return False
    return path.endswith(code_exts(language))


_TEST_SEGMENTS = {"test", "tests", "__tests__", "__mocks__", "__fixtures__", "e2e", "spec", "specs"}
_TOP_LEVEL_TEST_SEGMENTS = {"testing"}
_TEST_FILE = re.compile(
    r"(^test_.*\.py$|.*_test\.py$|^conftest\.py$|.*\.(test|spec)\.[cm]?[jt]sx?$|.*-test\.[cm]?[jt]s$)"
)


def is_test_path(path: str) -> bool:
    """Same heuristic for gold and every arm (documented in bench/README.md)."""
    parts = path.split("/")
    if parts and parts[0] in _TOP_LEVEL_TEST_SEGMENTS:
        return True
    if any(p in _TEST_SEGMENTS for p in parts[:-1]):
        return True
    return bool(_TEST_FILE.match(parts[-1]))


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
