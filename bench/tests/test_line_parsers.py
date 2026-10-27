"""Line parsers rewritten without overlapping quantifiers: the expectations
are what the former regular expressions returned on the same lines."""

import pytest

from rueca_bench.data import _toml_header
from rueca_bench.gold import parse_patch


@pytest.mark.parametrize(
    ("line", "header"),
    [
        ("[tool.uv]", ("[", "tool.uv")),
        ("  [[bin]]  # comment", ("[[", "bin")),
        ("[ a . b ]", ("[", "a . b")),
        ("[[a]", ("[[", "a")),
        ("[[]]", ("[", "[")),
        ("[  ]", ("[", " ")),
        ("[]", None),
        ("[a]]]", None),
        ("[a] x", None),
        ("x = [1]", None),
    ],
)
def test_toml_header(line, header):
    assert _toml_header(line) == header


def test_diff_header_splits_on_the_last_b_slash():
    patch = "diff --git a/x b/y.py b/x b/y.py\nindex 1..2\ndiff --git a/ b/z\ndiff --git a/no-separator\n"
    assert [(f.old_path, f.new_path) for f in parse_patch(patch)] == [("x b/y.py b/x", "y.py"), ("", "z"), (None, None)]
