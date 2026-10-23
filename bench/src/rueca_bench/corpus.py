"""One partial clone per repository; detached worktrees per commit."""

from __future__ import annotations

import shutil
from contextlib import contextmanager
from pathlib import Path
from typing import Iterator

from .common import REPOS_DIR, WORK_DIR, log, named_lock, run, safe_name


# LFS pointers stay pointers (git-lfs need not be installed; binaries are
# irrelevant to code intelligence).
NO_LFS = ["-c", "filter.lfs.process=", "-c", "filter.lfs.smudge=cat", "-c", "filter.lfs.clean=cat", "-c", "filter.lfs.required=false"]


def repo_dir(repo: str) -> Path:
    return REPOS_DIR / repo.replace("/", "__")


def ensure_clone(repo: str) -> Path:
    path = repo_dir(repo)
    with named_lock(f"git:{repo}"):
        if (path / ".git").exists() or (path / "HEAD").exists():
            return path
        path.parent.mkdir(parents=True, exist_ok=True)
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
        run(["git", "fetch", "--quiet", "--filter=blob:none", "origin", commit], cwd=path, timeout=900, check=True)


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
