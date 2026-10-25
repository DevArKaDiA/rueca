"""Pinned external tool binaries (manifest `[tools.<key>.binaries.<platform>]`).

Each platform entry gives a release `url` and either `binary_sha256` (a raw
binary) or `archive_sha256` + `member` (a .tar.gz holding the binary; its
`binary_sha256` is pinned too when known). Binaries land in
bench/.cache/bin/<name> and are read from $RUECA_BENCH_SHARED_CACHE when the
sha256 matches.
"""

from __future__ import annotations

import shutil
import tarfile
import tempfile
import urllib.request
from pathlib import Path

from .arms.cbm import cbm_platform_key
from .common import BIN_DIR, CACHE, load_manifest, log, pinned_asset, sha256_file


def tool_spec(key: str) -> dict:
    return load_manifest()["tools"][key]


def _platform(spec: dict) -> dict | None:
    """The platform entry for this host, or `any` (JVM launchers)."""
    bins = spec.get("binaries", {})
    return bins.get(cbm_platform_key()) or bins.get("any")


def tool_binary(key: str) -> Path:
    spec = tool_spec(key)
    plat = _platform(spec) or {}
    return pinned_asset(BIN_DIR / spec["name"], plat.get("binary_sha256"))


def tool_available(key: str) -> tuple[bool, str]:
    spec = tool_spec(key)
    if _platform(spec) is None:
        return False, f"no pinned {spec['name']} binary for {cbm_platform_key()}"
    if spec.get("image"):
        from .phpstan import docker_available

        ok, why = docker_available()
        if not ok:
            return False, f"{spec['name']} runs in Docker: {why}"
    return True, ""


def fetch_tool(key: str) -> Path:
    spec = tool_spec(key)
    plat = _platform(spec)
    if plat is None:
        raise RuntimeError(f"no pinned {spec['name']} binary for platform {cbm_platform_key()}")
    dest = tool_binary(key)
    want = plat.get("binary_sha256")
    if dest.exists() and (want is None or sha256_file(dest) == want):
        return dest
    dest = BIN_DIR / spec["name"]
    dl = CACHE / "dl"
    dl.mkdir(parents=True, exist_ok=True)
    BIN_DIR.mkdir(parents=True, exist_ok=True)
    fname = Path(plat["url"]).name
    if "archive_sha256" in plat:
        archive = pinned_asset(dl / fname, plat["archive_sha256"])
        if not archive.exists() or sha256_file(archive) != plat["archive_sha256"]:
            archive = dl / fname
            log(f"download {plat['url']}")
            urllib.request.urlretrieve(plat["url"], archive)
        if sha256_file(archive) != plat["archive_sha256"]:
            raise RuntimeError(f"{spec['name']} archive sha256 mismatch")
        with tempfile.TemporaryDirectory(dir=dl) as tmp:
            with tarfile.open(archive) as tf:
                tf.extractall(tmp, filter="data")
            found = [p for p in Path(tmp).rglob(plat.get("member", spec["name"])) if p.is_file()]
            if not found:
                raise RuntimeError(f"{plat.get('member')} not found in {fname}")
            shutil.move(str(found[0]), dest)
    else:
        log(f"download {plat['url']}")
        tmp = dest.with_suffix(".part")
        urllib.request.urlretrieve(plat["url"], tmp)
        tmp.replace(dest)
    dest.chmod(0o755)
    if want and sha256_file(dest) != want:
        raise RuntimeError(f"{spec['name']} binary sha256 mismatch: {sha256_file(dest)}")
    log(f"{spec['name']} {spec.get('version')} ready at {dest}")
    return dest


def fetch_artifact(key: str) -> Path:
    """A pinned file (`[artifacts.<key>] url, sha256`), e.g. a compile-only
    jar a SCIP indexer needs on the classpath."""
    spec = load_manifest()["artifacts"][key]
    dest = pinned_asset(CACHE / "dl" / Path(spec["url"]).name, spec["sha256"])
    if dest.exists() and sha256_file(dest) == spec["sha256"]:
        return dest
    dest = CACHE / "dl" / Path(spec["url"]).name
    dest.parent.mkdir(parents=True, exist_ok=True)
    log(f"download {spec['url']}")
    tmp = dest.with_suffix(dest.suffix + ".part")
    urllib.request.urlretrieve(spec["url"], tmp)
    if sha256_file(tmp) != spec["sha256"]:
        tmp.unlink()
        raise RuntimeError(f"{key} sha256 mismatch")
    tmp.replace(dest)
    return dest
