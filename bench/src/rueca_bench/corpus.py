"""One partial clone per repository; detached worktrees per commit.

Two clone modes (manifest `[corpus] shallow`):
- full partial clone (`--filter=blob:none`): all commits and trees, blobs on
  demand — the alpha default;
- shallow (repositories listed in `[corpus] shallow`, or every new clone
  when it is `true`): an empty repository with `origin` as a promisor remote;
  each needed commit is fetched with `--depth=1 --filter=blob:none` (the
  Zoekt fetch pattern of RFC 0002 §7.1), so large histories (hugo,
  terraform, redis, ...) cost one tree per pinned commit instead of their
  whole history. Blobs are fetched on checkout / `git show`.
"""

from __future__ import annotations

import shutil
from contextlib import contextmanager
from pathlib import Path
from typing import Iterator

from .common import REPOS_DIR, WORK_DIR, load_manifest, log, named_lock, run, safe_name


# LFS pointers stay pointers (git-lfs need not be installed; binaries are
# irrelevant to code intelligence).
NO_LFS = ["-c", "filter.lfs.process=", "-c", "filter.lfs.smudge=cat", "-c", "filter.lfs.clean=cat", "-c", "filter.lfs.required=false"]


def repo_dir(repo: str) -> Path:
    return REPOS_DIR / repo.replace("/", "__")


def _shallow_wanted(repo: str) -> bool:
    cfg = load_manifest().get("corpus", {}).get("shallow", False)
    return cfg is True or (isinstance(cfg, list) and repo in cfg)


def is_shallow(path: Path) -> bool:
    return (path / ".git" / "shallow").exists() or (path / ".git" / "rueca-bench-shallow").exists()


def ensure_clone(repo: str) -> Path:
    path = repo_dir(repo)
    with named_lock(f"git:{repo}"):
        if (path / ".git").exists() or (path / "HEAD").exists():
            return path
        path.parent.mkdir(parents=True, exist_ok=True)
        if _shallow_wanted(repo):
            log(f"init {repo} (shallow, blob:none)")
            path.mkdir(parents=True, exist_ok=True)
            run(["git", "init", "--quiet", str(path)], timeout=60, check=True)
            for cmd in (
                ["git", "remote", "add", "origin", f"https://github.com/{repo}.git"],
                ["git", "config", "remote.origin.promisor", "true"],
                ["git", "config", "remote.origin.partialclonefilter", "blob:none"],
                ["git", "config", "extensions.partialClone", "origin"],
            ):
                run(cmd, cwd=path, timeout=60, check=True)
            (path / ".git" / "rueca-bench-shallow").write_text("shallow, one commit per fetch\n")
            return path
        log(f"clone {repo} (blob:none)")
        run(
            [
                "git",
                "clone",
                "--quiet",
                "--filter=blob:none",
                "--no-checkout",
                f"https://github.com/{repo}.git",
                str(path),
            ],
            timeout=1800,
            check=True,
        )
    return path


def ensure_commit(repo: str, commit: str) -> None:
    path = ensure_clone(repo)
    with named_lock(f"git:{repo}"):
        if run(["git", "cat-file", "-e", f"{commit}^{{commit}}"], cwd=path).rc == 0:
            return
        log(f"fetch {repo}@{commit[:10]}")
        depth = ["--depth=1"] if is_shallow(path) else []
        run(["git", "fetch", "--quiet", *depth, "--filter=blob:none", "origin", commit], cwd=path, timeout=900, check=True)


def git_show(repo: str, commit: str, rel_path: str) -> str | None:
    res = run(["git", "show", f"{commit}:{rel_path}"], cwd=repo_dir(repo), timeout=300)
    return res.stdout if res.rc == 0 else None


def file_exists_at(repo: str, commit: str, rel_path: str) -> bool:
    return run(["git", "cat-file", "-e", f"{commit}:{rel_path}"], cwd=repo_dir(repo), timeout=120).rc == 0


def add_worktree(repo: str, commit: str, name: str) -> Path:
    ensure_commit(repo, commit)
    dest = WORK_DIR / safe_name(name)
    clone = repo_dir(repo)
    with named_lock(f"git:{repo}"):
        if dest.exists():
            head = run(["git", "rev-parse", "HEAD"], cwd=dest).stdout.strip()
            if head == commit:
                return dest
            remove_worktree_locked(clone, dest)
        dest.parent.mkdir(parents=True, exist_ok=True)
        run(["git", "worktree", "prune"], cwd=clone)
        run(
            ["git", *NO_LFS, "-c", "advice.detachedHead=false", "worktree", "add", "--quiet", "--detach", str(dest), commit],
            cwd=clone,
            timeout=1800,
            check=True,
        )
    return dest


def remove_worktree_locked(clone: Path, dest: Path) -> None:
    run(["git", "worktree", "remove", "--force", str(dest)], cwd=clone, timeout=300)
    if dest.exists():
        shutil.rmtree(dest, ignore_errors=True)
    run(["git", "worktree", "prune"], cwd=clone)


def remove_worktree(repo: str, dest: Path) -> None:
    with named_lock(f"git:{repo}"):
        remove_worktree_locked(repo_dir(repo), dest)


@contextmanager
def worktree(repo: str, commit: str, name: str, keep: bool = False) -> Iterator[Path]:
    path = add_worktree(repo, commit, name)
    try:
        yield path
    finally:
        if not keep:
            remove_worktree(repo, path)


def list_files(worktree_path: Path) -> list[str]:
    res = run(["git", "ls-files", "-z"], cwd=worktree_path, timeout=300, check=True)
    return sorted(p for p in res.stdout.split("\0") if p)


def prune_clone(repo: str) -> None:
    """Delete a repository's clone (and the blobs its checkouts fetched) once
    a run no longer needs it; the next use re-creates it from the pinned
    commits. Keeps the cache within a CI runner's disk."""
    with named_lock(f"git:{repo}"):
        shutil.rmtree(repo_dir(repo), ignore_errors=True)
