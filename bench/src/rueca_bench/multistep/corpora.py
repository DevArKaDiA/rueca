"""Corpora of the multi-step suite: pinned checkouts and their Rueca stores.

Every corpus is a pinned input the bench already uses:

- `flask`, `requests`, `hono` — the S3 repositories at their `[[s3.repos]]`
  commits (gold: the S3 call graph);
- `xr-flask` — the cross-repository group of `crossrepo.GROUPS` led by
  Flask (Flask 3.1.3 with Click, Jinja and Werkzeug), one organization;
- `s2-ollama` — the S2 group `ollama` of `contracts.GROUPS` (ollama with its
  Python and JavaScript clients), one organization;
- `mr-requests` — the multi-ref scenario (`[multiref] instance`): the
  repository at the instance's base commit plus the fix commit (base + the
  gold patch, committed with a fixed identity and date) as a ref snapshot.

`prepare(key)` checks the worktrees out under `.cache/work/ms-*` and indexes
them into `.cache/multistep/<key>/store` (kept between runs: a stamp of the
spec and the binary's sha256 skips indexing when nothing changed).
"""

from __future__ import annotations

import json
import shutil
from dataclasses import dataclass, field
from pathlib import Path

from ..common import (
    CACHE,
    WORK_DIR,
    load_manifest,
    run,
    safe_name,
    sha256_file,
    write_json,
    write_org_file,
)
from ..contracts import GROUPS as CONTRACT_GROUPS
from ..corpus import add_worktree, remove_worktree
from ..crossrepo import GROUPS as CROSSREPO_GROUPS
from ..datasets import instance_row
from ..org import _GIT_ENV
from ..s1 import instances as s1_instances

MS_DIR = CACHE / "multistep"
INDEX_TIMEOUT = 1800.0

SINGLE = ("flask", "requests", "hono")
ORGS = ("xr-flask", "s2-ollama")
MULTIREF = "mr-requests"
ALL = (*SINGLE, *ORGS, MULTIREF)


@dataclass
class Corpus:
    """What a tool call needs: `--root` or `--org`, and `--store`."""

    key: str
    store: Path
    root: Path | None = None
    org: Path | None = None
    roots: dict[str, Path] = field(default_factory=dict)
    refs: dict[str, str] = field(default_factory=dict)

    def scope_args(self) -> list[str]:
        if self.org is not None:
            return ["--org", str(self.org), "--store", str(self.store)]
        return ["--root", str(self.root), "--store", str(self.store)]


def s3_spec(name: str) -> dict:
    return next(s for s in load_manifest()["s3"]["repos"] if s["repo"].split("/")[1] == name)


def crossrepo_group() -> dict:
    return next(g for g in CROSSREPO_GROUPS if g["consumer"] == "pallets/flask")


def contract_group() -> dict:
    return next(g for g in CONTRACT_GROUPS if g["name"] == "ollama")


def multiref_inst() -> dict:
    iid = load_manifest()["multiref"]["instance"]
    return next(i for i in s1_instances() if i["id"] == iid)


def members(key: str) -> list[tuple[str, str]]:
    """(repository, commit) of every checkout a corpus indexes."""
    if key in SINGLE:
        s = s3_spec(key)
        return [(s["repo"], s["commit"])]
    if key == "xr-flask":
        g = crossrepo_group()
        return [(g["consumer"], g["commit"])] + [(x["repo"], x["commit"]) for x in g["libraries"]]
    if key == "s2-ollama":
        return [(r["repo"], r["commit"]) for r in contract_group()["repos"]]
    if key == MULTIREF:
        inst = multiref_inst()
        return [(inst["repo"], inst["base_commit"])]
    raise KeyError(key)


def _stamp(key: str, binary: str) -> str:
    return json.dumps({"key": key, "members": members(key), "binary": sha256_file(Path(binary))}, sort_keys=True)


def _fresh(store: Path, stamp: str) -> bool:
    p = store / "ms-stamp.json"
    return p.exists() and p.read_text() == stamp


def _index(binary: str, cmd: list[str]) -> dict:
    res = run([binary, "index", *cmd, "--json"], timeout=INDEX_TIMEOUT)
    if res.rc != 0:
        raise RuntimeError(f"rueca index {' '.join(cmd)} failed: {res.stderr[-500:]}")
    return json.loads(res.stdout)


def _worktrees(key: str) -> dict[str, Path]:
    return {repo: add_worktree(repo, commit, f"ms-{key}-{repo}") for repo, commit in members(key)}


def _fix_commit(inst: dict) -> str:
    """The fix commit of the multi-ref scenario (org.py `_multiref`): base +
    the gold patch, committed with a fixed identity and date, so its id is
    deterministic. Its objects live in the shared clone."""
    fix_root = add_worktree(inst["repo"], inst["base_commit"], f"ms-mr-fix-{inst['id']}")
    try:
        pf = fix_root.parent / f"ms-{safe_name(inst['id'])}.patch"
        pf.write_text(instance_row(inst)["patch"])
        res = run(["git", "apply", "--whitespace=nowarn", str(pf)], cwd=fix_root, timeout=120)
        pf.unlink(missing_ok=True)
        if res.rc != 0:
            raise RuntimeError(f"git apply failed: {res.stderr[-300:]}")
        run(["git", "add", "-A"], cwd=fix_root, check=True)
        run(["git", "-c", "commit.gpgsign=false", "commit", "-q", "-m", f"rueca-bench fix of {inst['id']}"], cwd=fix_root, env=_GIT_ENV, check=True)
        return run(["git", "rev-parse", "HEAD"], cwd=fix_root).stdout.strip()
    finally:
        remove_worktree(inst["repo"], fix_root)


def prepare(key: str, binary: str) -> Corpus:
    """Check out and index one corpus (skipped when the store is current)."""
    base = MS_DIR / key
    store = base / "store"
    roots = _worktrees(key)
    stamp = _stamp(key, binary)
    meta_path = base / "meta.json"
    if not (_fresh(store, stamp) and meta_path.exists()):
        shutil.rmtree(store, ignore_errors=True)
        store.mkdir(parents=True, exist_ok=True)
        meta = _build(key, binary, roots, store, base)
        write_json(meta_path, meta)
        (store / "ms-stamp.json").write_text(stamp)
    meta = json.loads(meta_path.read_text())
    org = base / "org.toml" if key in ORGS else None
    root = None if org is not None else next(iter(roots.values()))
    return Corpus(key, store, root=root, org=org, roots=roots, refs=meta.get("refs", {}))


def _build(key: str, binary: str, roots: dict[str, Path], store: Path, base: Path) -> dict:
    if key in ORGS:
        org = base / "org.toml"
        write_org_file(org, list(roots.values()), name=f"ms-{key}")
        _index(binary, ["--org", str(org), "--store", str(store)])
        return {"refs": {}}
    root = next(iter(roots.values()))
    _index(binary, [str(root), "--store", str(store)])
    if key != MULTIREF:
        return {"refs": {}}
    inst = multiref_inst()
    fix = _fix_commit(inst)
    _index(binary, [str(root), "--ref", fix, "--store", str(store)])
    return {"refs": {"base": inst["base_commit"], "fix": fix}}


def release(key: str) -> None:
    """Remove a corpus's worktrees (the store is kept)."""
    for repo, _ in members(key):
        p = WORK_DIR / safe_name(f"ms-{key}-{repo}")
        if p.exists():
            remove_worktree(repo, p)
