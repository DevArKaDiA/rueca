"""The `vector` arm (arms/vector.py): canonical text (golden strings), sweep
configuration, ranking on a tiny repository with the fake embedder, the
vector cache across instances, persisted raw scores, preflight."""

import numpy as np
import pytest

from conftest import git_repo
from rueca_bench.arms import run_label
from rueca_bench.arms.vector import VectorArm, VectorConfig, canonical_text, file_text
from rueca_bench.embed import FakeEmbedder, RuecaEmbedder
from rueca_bench.s1 import _filtered_scores
from rueca_bench.treesitter import extract_functions

PY = '''import os


class Loader:
    # Reads the settings file.
    @classmethod
    def load(cls, path: str,
             strict: bool = False) -> "Loader":
        """Load settings from `path`.

        Raises on a missing file when strict."""
        if not os.path.exists(path):
            raise FileNotFoundError(path)
        return cls()


def helper(x): return x + 1
'''

GO = """package conf

// Parse reads a configuration.
// It never panics.
func (c *Config) Parse(data []byte,
	strict bool) (int, error) {
	if len(data) == 0 {
		return 0, nil
	}
	return len(data), nil
}
"""

RS = """/// Adds one.
#[inline]
pub fn add_one(x: u32) -> u32 { x + 1 }
"""


def _texts(path: str, src: str, lang: str, max_tokens: int = 512) -> list[str]:
    lines = src.splitlines()
    return [canonical_text(lang, path, f, lines, max_tokens) for f in extract_functions(path, src.encode(), lang)]


def test_canonical_text_python_golden():
    load, helper = _texts("pkg/loader.py", PY, "python")
    assert load == (
        "python Method Loader.load\n"
        "path: pkg/loader.py\n"
        'def load(cls, path: str, strict: bool = False) -> "Loader":\n'
        '"""Load settings from `path`.\n'
        'Raises on a missing file when strict."""\n'
        "if not os.path.exists(path):\n"
        "    raise FileNotFoundError(path)\n"
        "return cls()"
    )
    assert helper == "python Function helper\npath: pkg/loader.py\ndef helper(x): return x + 1"


def test_canonical_text_brace_languages_golden():
    (parse,) = _texts("conf/parse.go", GO, "go")
    assert parse == (
        "go Method Config.Parse\n"
        "path: conf/parse.go\n"
        "func (c *Config) Parse(data []byte, strict bool) (int, error)\n"
        "// Parse reads a configuration.\n"
        "// It never panics.\n"
        "if len(data) == 0 {\n"
        "\treturn 0, nil\n"
        "}\n"
        "return len(data), nil"
    )
    (add,) = _texts("src/lib.rs", RS, "rust")
    assert add == "rust Function add_one\npath: src/lib.rs\npub fn add_one(x: u32) -> u32\n/// Adds one.\nx + 1"


def test_canonical_text_token_budget():
    (parse,) = _texts("conf/parse.go", GO, "go", max_tokens=20)
    assert len(parse) <= 80
    assert parse.startswith("go Method Config.Parse\npath: conf/parse.go\nfunc (c *Config)")
    assert file_text("go", "conf/parse.go", GO, 20) == "go File conf/parse.go\npath: conf/parse.go\npackage conf\n// Parse reads a configur"


def test_config_tag_covers_every_knob(monkeypatch):
    for k in ("PRESET", "DIMS", "MAX_TOKENS", "INSTRUCTION", "FILE_TEXT"):
        monkeypatch.delenv(f"RUECA_BENCH_VECTOR_{k}", raising=False)
    base = VectorConfig.from_env()
    assert base == VectorConfig()
    assert run_label("vector") == f"vector/{base.tag()}" == "vector/qwen3-0.6b-q8-dfull-t512-idefault-f0-x1"
    assert run_label("grep") == "grep"
    tags = {base.tag()}
    for k, v in (("PRESET", "coderank-137m"), ("DIMS", "256"), ("MAX_TOKENS", "1024"), ("INSTRUCTION", "Find it"), ("FILE_TEXT", "1")):
        monkeypatch.setenv(f"RUECA_BENCH_VECTOR_{k}", v)
        tags.add(VectorConfig.from_env().tag())
    assert len(tags) == 6


REPO = {
    "src/config.py": "def parse_config(path):\n    return read_settings(path)\n\n\ndef read_settings(path):\n    return open(path).read()\n",
    "src/net.py": "class Client:\n    def send_packet(self, payload):\n        return self.socket.write(payload)\n",
    "src/constants.py": "TIMEOUT = 30\nRETRIES = 3\n",
    "tests/test_config.py": "def test_parse_config():\n    assert parse_config('x')\n",
}


def test_vector_arm_ranks_caches_and_scores(tmp_path):
    root = git_repo(tmp_path / "repo", REPO)
    fake = FakeEmbedder(256)
    arm = VectorArm(VectorConfig(), embedder=fake, vector_root=tmp_path / "vectors")
    h = arm.index(root, "t", "python")
    assert h.ok and h.info["functions"] == 4 and h.info["embedded"] == 4 and h.info["cached"] == 0
    res = arm.localize(h, "send packet payload", 10)
    assert res.ok
    assert res.extra["functions"][0]["name"] == "send_packet"
    assert res.extra["files"][0] == "src/net.py"
    assert "src/constants.py" not in res.extra["files"]  # no functions, file texts off
    sc = res.extra["scores"]
    assert sc["files"][0][0] == "src/net.py" and sc["functions"][0][:3] == ["src/net.py", "send_packet", 2]
    assert sc["functions"][0][3] >= sc["functions"][-1][3]
    assert [x[0] for x in _filtered_scores(sc, "python")["files"]] == [p for p, _ in sc["files"] if not p.startswith("tests/")]
    # a second instance of the same repository: every function vector is a cache hit
    arm2 = VectorArm(VectorConfig(dims=64), embedder=fake, vector_root=tmp_path / "vectors")
    h2 = arm2.index(root, "t2", "python")
    assert h2.info["embedded"] == 0 and h2.info["cached"] == 4 and h2.info["dims"] == 64
    np.testing.assert_allclose(np.linalg.norm(h2.state.func_vecs, axis=1), 1.0, rtol=1e-5)


def test_vector_arm_file_text_ranks_files_without_functions(tmp_path):
    root = git_repo(tmp_path / "repo", REPO)
    arm = VectorArm(VectorConfig(file_text=True), embedder=FakeEmbedder(256), vector_root=tmp_path / "vectors")
    h = arm.index(root, "t", "python")
    res = arm.localize(h, "TIMEOUT RETRIES constants", 10)
    assert res.extra["files"][0] == "src/constants.py"


def test_preflight_fails_without_instruction_flag(fake_rueca, monkeypatch):
    monkeypatch.setenv("FAKE_RUECA_NO_INSTRUCTION", "1")
    cfg = VectorConfig(instruction="Find the code to change")
    arm = VectorArm(cfg, embedder=RuecaEmbedder(str(fake_rueca), instruction=cfg.instruction))
    with pytest.raises(RuntimeError, match="no --instruction flag"):
        arm.preflight()
    monkeypatch.delenv("FAKE_RUECA_NO_INSTRUCTION")
    arm.preflight()
    assert "model_key fake-qwen3-0.6b-q8" in arm.version()


def test_vector_arm_through_the_fake_binary(fake_rueca, tmp_path):
    root = git_repo(tmp_path / "repo", REPO)
    arm = VectorArm(VectorConfig(), embedder=RuecaEmbedder(str(fake_rueca)), vector_root=tmp_path / "vectors")
    h = arm.index(root, "t", "python")
    assert h.ok and h.info["embedded"] == 4
    res = arm.localize(h, "parse config", 5)
    assert res.ok and len(res.extra["functions"]) == 4 and len(res.extra["files"]) == 3
