"""Shared fixtures: a fake `rueca` binary (tests/fake_rueca.py) and a tiny git
repository for the localization arms."""

import subprocess
import sys
from pathlib import Path

import pytest

FAKE = Path(__file__).with_name("fake_rueca.py")


@pytest.fixture
def fake_rueca(tmp_path: Path) -> Path:
    exe = tmp_path / "bin" / "rueca"
    exe.parent.mkdir()
    exe.write_text(f"#!{sys.executable}\nimport sys\nsys.argv[0] = {str(FAKE)!r}\nexec(compile(open({str(FAKE)!r}).read(), {str(FAKE)!r}, 'exec'))\n")
    exe.chmod(0o755)
    return exe


def git_repo(root: Path, files: dict[str, str]) -> Path:
    for rel, text in files.items():
        p = root / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(text)
    subprocess.run(["git", "init", "-q"], cwd=root, check=True)
    subprocess.run(["git", "add", "-A"], cwd=root, check=True)
    return root
