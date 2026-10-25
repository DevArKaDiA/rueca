"""`rueca-bench setup`: pinned tools, datasets, clones, gold labels.

Everything lands in bench/.cache (override with RUECA_BENCH_CACHE). Every
step is idempotent; re-running only does what is missing.
"""

from __future__ import annotations

import shutil
import tarfile
import tempfile
import urllib.request
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

from .arms.cbm import cbm_binary, cbm_platform_key
from .common import BIN_DIR, CACHE, lang_family, load_manifest, log, sha256_file
from .corpus import ensure_commit
from .datasets import fetch_dataset, select_instances, write_instances
from .phpstan import docker_available, fetch_phpstan
from .s1 import instances, s1_gold
from .s3 import build_micro_gold, build_repo_gold, suites


def fetch_cbm() -> Path:
    man = load_manifest()["tools"]["cbm"]
    key = cbm_platform_key()
    spec = man["binaries"].get(key)
    if spec is None:
        raise RuntimeError(f"no pinned codebase-memory-mcp binary for platform {key}")
    dest = cbm_binary()
    if dest.exists() and sha256_file(dest) == spec["binary_sha256"]:
        return dest
    dl = CACHE / "dl"
    dl.mkdir(parents=True, exist_ok=True)
    archive = dl / Path(spec["url"]).name
    if not archive.exists() or sha256_file(archive) != spec["archive_sha256"]:
        log(f"download {spec['url']}")
        urllib.request.urlretrieve(spec["url"], archive)
    got = sha256_file(archive)
    if got != spec["archive_sha256"]:
        raise RuntimeError(f"cbm archive sha256 mismatch: {got}")
    with tempfile.TemporaryDirectory(dir=dl) as tmp:
        with tarfile.open(archive) as tf:
            tf.extractall(tmp, filter="data")
        found = [p for p in Path(tmp).rglob("codebase-memory-mcp") if p.is_file()]
        if not found:
            raise RuntimeError("codebase-memory-mcp binary not found in archive")
        BIN_DIR.mkdir(parents=True, exist_ok=True)
        shutil.move(str(found[0]), dest)
    dest.chmod(0o755)
    got = sha256_file(dest)
    if got != spec["binary_sha256"]:
        raise RuntimeError(f"cbm binary sha256 mismatch: {got}")
    log(f"cbm {man['version']} ready at {dest}")
    return dest


def _datasets_for(languages: set[str] | None) -> list[str]:
    man = load_manifest()
    if not languages:
        return list(man["datasets"])
    keys = {src for r in man["s1"]["repos"] if lang_family(r["language"]) in languages for src in r["sources"]}
    keys |= set(man["s1"].get("gold_assets", [])) | {"locagent_lite", "contextbench"}
    return [k for k in man["datasets"] if k in keys]


def setup(select: bool = False, workers: int = 4, skip_s3: bool = False, languages: set[str] | None = None) -> None:
    """`languages`: restrict to these language families (manifest
    `[languages]` keys); selection (`--select`) always covers every repo so the
    generated instance list stays deterministic."""
    fetch_cbm()
    for key in (load_manifest()["datasets"] if select else _datasets_for(languages)):
        fetch_dataset(key)
    if select or not instances():
        write_instances(select_instances())
    insts = [i for i in instances() if not languages or lang_family(i["language"]) in languages]
    by_repo: dict[str, list[dict]] = {}
    for i in insts:
        by_repo.setdefault(i["repo"], []).append(i)

    def _gold(items: list[dict]) -> None:
        for inst in items:
            ensure_commit(inst["repo"], inst["base_commit"])
            s1_gold(inst)

    with ThreadPoolExecutor(max_workers=workers) as ex:
        list(ex.map(_gold, by_repo.values()))
    log(f"s1 gold ready for {len(insts)} instances")
    if skip_s3:
        return
    sets = suites(languages)
    if any(not micro and spec.get("gold") == "phpstan" for _, spec, micro in sets):
        ok, why = docker_available()
        if ok:
            fetch_phpstan()
        else:
            log(f"phpstan gold needs docker ({why}); PHP repository gold will be missing")
    for name, spec, micro in sets:
        try:
            build_micro_gold(spec) if micro else build_repo_gold(spec)
        except Exception as exc:  # recorded; the report shows the set as missing
            log(f"s3 gold FAILED for {name}: {exc}")
