"""PHP call-edge gold from PHPStan (S3; RFC 0002 §4.2: scip-php is not viable).

PHPStan (pinned release PHAR) runs inside the official PHP CLI image (pinned
by digest) with the bench's collector extension (bench/phpstan/). The
collector records, for every call expression, the target PHPStan resolves
through its type inference (declared and phpdoc types, `$this`, `self` /
`static` / `parent`, `new`, inherited and trait methods). The bench then
derives call edges the same way as for SCIP:

1. callables = tree-sitter functions/methods of in-scope files;
2. the callee is the callable in the target's file whose name equals the
   resolved method/function name and whose declaration starts within ±2 lines
   of the reflected start line (methods), or the unique function of that name
   in the file (functions);
3. the caller is the innermost callable containing the call line (calls in
   closures belong to the enclosing named function; top-level code produces
   no edge); both endpoints must be in scope.

No `composer install` is run: vendor classes are unknown to PHPStan, which
only loses edges into vendor code (out of scope anyway) and receiver types
that come from vendor return types.
"""

from __future__ import annotations

import json
import shutil
import urllib.request
from collections import defaultdict
from pathlib import Path

from .common import BENCH_DIR, BIN_DIR, CACHE, load_manifest, log, pinned_asset, run, sha256_file
from .scip import fid, in_scope
from .treesitter import extract_functions

EXT_DIR = BENCH_DIR / "phpstan"
CONTAINER_SRC = "/src"

_CONFIG = """includes:
    - /ext/extension.neon
parameters:
    tmpDir: /tmp/phpstan
"""


def phar_path() -> Path:
    spec = load_manifest()["tools"]["phpstan"]
    return pinned_asset(BIN_DIR / "phpstan.phar", spec["sha256"])


def fetch_phpstan() -> Path:
    spec = load_manifest()["tools"]["phpstan"]
    dest = phar_path()
    if dest.exists() and sha256_file(dest) == spec["sha256"]:
        return dest
    dest = BIN_DIR / "phpstan.phar"
    BIN_DIR.mkdir(parents=True, exist_ok=True)
    log(f"download {spec['url']}")
    tmp = dest.with_suffix(".part")
    urllib.request.urlretrieve(spec["url"], tmp)
    got = sha256_file(tmp)
    if got != spec["sha256"]:
        tmp.unlink()
        raise RuntimeError(f"phpstan.phar sha256 mismatch: {got}")
    tmp.replace(dest)
    return dest


def docker_available() -> tuple[bool, str]:
    if shutil.which("docker") is None:
        return False, "docker not on PATH"
    res = run(["docker", "info", "--format", "{{.ServerVersion}}"], timeout=60)
    if res.rc != 0:
        return False, f"docker daemon unavailable: {res.stderr.strip()[-200:]}"
    return True, res.stdout.strip()


def version_string() -> str:
    t = load_manifest()["tools"]
    return f"phpstan@{t['phpstan']['version']} + rueca-bench collector"


def run_phpstan(root: Path, targets: list[str], work: Path, timeout: float = 3600) -> dict[str, dict]:
    """Analyse each target (a path relative to `root`) separately; returns
    {target: {container_file: [call records]}}. One container for all targets."""
    ok, why = docker_available()
    if not ok:
        raise RuntimeError(why)
    phar = phar_path()
    if not phar.exists():
        phar = fetch_phpstan()
    image = load_manifest()["tools"]["php_image"]["image"]
    if work.exists():
        shutil.rmtree(work)
    (work / "out").mkdir(parents=True)
    (work / "phpstan.neon").write_text(_CONFIG)
    # run from a copy inside the container: reading the PHAR through the bind
    # mount intermittently fails ("Cannot open phar archive") on macOS VMs
    lines = ["set -u", "cp /phpstan.phar /tmp/phpstan.phar"]
    for i, t in enumerate(targets):
        lines.append(
            f"RUECA_OUT=/out/{i}.json php -d memory_limit=-1 /tmp/phpstan.phar analyse --no-progress --no-interaction "
            f"--error-format=raw -c /cfg/phpstan.neon -a /ext/RuecaCallCollector.php '{CONTAINER_SRC}/{t}' > /out/{i}.log 2>&1 || true"
        )
    (work / "run.sh").write_text("\n".join(lines) + "\n")
    cmd = [
        "docker", "run", "--rm", "--network=none",
        "-v", f"{root}:{CONTAINER_SRC}:ro",
        "-v", f"{EXT_DIR}:/ext:ro",
        "-v", f"{phar}:/phpstan.phar:ro",
        "-v", f"{work}:/cfg:ro",
        "-v", f"{work / 'out'}:/out",
        image, "sh", "/cfg/run.sh",
    ]
    res = run(cmd, timeout=timeout)
    if res.timed_out or res.rc != 0:
        raise RuntimeError(f"phpstan container failed rc={res.rc} timed_out={res.timed_out}: {res.stderr[-1500:]}")
    out: dict[str, dict] = {}
    for i, t in enumerate(targets):
        p = work / "out" / f"{i}.json"
        if not p.exists():
            log_tail = (work / "out" / f"{i}.log").read_text(errors="replace")[-1500:] if (work / "out" / f"{i}.log").exists() else ""
            raise RuntimeError(f"phpstan produced no output for {t}: {log_tail}")
        out[t] = json.loads(p.read_text())
    return out


def _rel(container_path: str | None, prefix: str) -> str | None:
    if not container_path or not container_path.startswith(prefix):
        return None
    return container_path[len(prefix) :]


def edges_from_calls(calls: dict, prefix: str, callables_by_file: dict[str, list[dict]], scope) -> tuple[dict, dict, dict]:
    """Map PHPStan call records onto tree-sitter callables (module doc, steps
    2-3). `prefix` is the container path of the analysed root + "/"; `scope`
    decides which repository-relative paths are in scope."""
    stats: dict[str, int] = defaultdict(int)
    callers: dict[str, set[str]] = defaultdict(set)
    callees: dict[str, set[str]] = defaultdict(set)

    def callee_of(t: dict) -> str | None:
        rel = _rel(t.get("file"), prefix)
        if rel is None or not scope(rel):
            stats["targets_out_of_scope"] += 1
            return None
        name = t["name"].rsplit("::", 1)[-1] if t["kind"] == "method" else t["name"].rsplit("\\", 1)[-1]
        cands = [f for f in callables_by_file.get(rel, []) if f["name"].lower() == name.lower()]
        if t["kind"] == "method" and t.get("line"):
            near = [f for f in cands if any(abs(t["line"] - a) <= 2 for a in (f["start"], f["def_line"], f["outer_start"]))]
            cands = near or cands
        if not cands:
            stats["targets_unmapped"] += 1
            return None
        f = min(cands, key=lambda f: f["start"])
        return fid(rel, f)

    for _file, records in sorted(calls.items()):
        for rec in records:
            stats["call_sites"] += 1
            if not rec.get("targets"):
                stats["call_sites_no_target"] += 1  # builtin, vendor, untyped receiver or dynamic
                continue
            rel = _rel(rec.get("file"), prefix)
            if rel is None or not scope(rel):
                stats["call_sites_out_of_scope"] += 1
                continue
            line = rec["line"]
            best = None
            for f in callables_by_file.get(rel, []):
                if f["start"] <= line <= f["end"] and (best is None or f["end"] - f["start"] < best["end"] - best["start"]):
                    best = f
            if best is None:
                stats["call_sites_module_level"] += 1
                continue
            caller = fid(rel, best)
            for t in rec["targets"]:
                c = callee_of(t)
                if c is None:
                    continue
                stats["resolved_targets"] += 1
                callees[caller].add(c)
                callers[c].add(caller)
    return callers, callees, dict(stats)


def phpstan_call_graph(root: Path, include: list[str], name: str) -> dict:
    callables: dict[str, dict] = {}
    by_file: dict[str, list[dict]] = {}
    files = sorted(str(p.relative_to(root)) for p in root.rglob("*.php") if p.is_file())
    for rel in files:
        if not in_scope(rel, include):
            continue
        lst = []
        for f in extract_functions(rel, (root / rel).read_bytes()):
            d = f.as_dict()
            d["path"] = rel
            callables[fid(rel, d)] = d
            lst.append(d)
        by_file[rel] = lst
    targets = [p.rstrip("/") for p in include]
    raw = run_phpstan(root, targets, CACHE / "phpstan" / name)
    calls: dict = {}
    for t in targets:
        calls.update(raw[t])
    callers, callees, stats = edges_from_calls(calls, CONTAINER_SRC + "/", by_file, lambda rel: in_scope(rel, include))
    stats["edges"] = sum(len(v) for v in callees.values())
    stats["files_in_scope"] = len(by_file)
    return {
        "callables": callables,
        "callers": {k: sorted(v) for k, v in callers.items()},
        "callees": {k: sorted(v) for k, v in callees.items()},
        "stats": stats,
    }
