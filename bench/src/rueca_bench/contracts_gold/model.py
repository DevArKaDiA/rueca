"""Sites the labelers find, and the file scope of the S2 gold."""

from __future__ import annotations

import re
from dataclasses import dataclass, field
from pathlib import Path

# A labeler's finding: one route declaration, spec operation, rpc, server
# implementation method or client call site.
CALL, HANDLER, SPEC, IMPL = "call", "handler", "spec", "impl"


@dataclass(frozen=True)
class Site:
    kind: str  # "http" | "rpc"
    role: str  # CALL (consumes) | HANDLER | SPEC | IMPL (provide)
    key: str  # canonical key
    repo: str
    path: str
    symbol: str  # enclosing definition (CALL), handler / impl method, route or rpc name ("" = file level)
    line: int
    raw: str  # the evidence text, ≤ 200 characters
    framework: str
    # names a graph endpoint may carry to be this site (bare names, route
    # keys, `Service.Method`; "" = the file node)
    accept: tuple[str, ...] = field(default=())

    @property
    def provides(self) -> bool:
        return self.role != CALL


SKIP_DIRS = {".git", "node_modules", "vendor", ".venv", "venv", "__pycache__", "dist", "build", "out", "site-packages", "third_party", "testdata"}
_TEST_DIRS = {"test", "tests", "__tests__", "e2e", "integration", "testing", "fixtures", "mocks", "mock", "fake", "fakes"}
_TEST_FILE = re.compile(r"(?:^test_.*\.py$|.*_test\.py$|.*_test\.go$|.*\.(?:test|spec)\.[cm]?[jt]sx?$|^conftest\.py$)")
_GENERATED_FILE = re.compile(r"(?:_pb2(?:_grpc)?\.pyi?$|\.pb(?:\.gw)?\.go$|_grpc\.pb\.go$|\.connect\.go$|_pb\.[jt]s$|_grpc_pb\.[jt]s$|\.pb\.validate\.go$)")
_GO_GENERATED = re.compile(r"^// Code generated .* DO NOT EDIT\.$", re.M)


def is_test_path(rel: str) -> bool:
    parts = rel.split("/")
    return any(p in _TEST_DIRS for p in parts[:-1]) or bool(_TEST_FILE.match(parts[-1]))


def is_generated_path(rel: str) -> bool:
    return bool(_GENERATED_FILE.search(rel.rsplit("/", 1)[-1]))


def in_scope(rel: str | None) -> bool:
    """Gold and scored predictions cover non-test, non-generated files."""
    return bool(rel) and not is_test_path(rel) and not is_generated_path(rel)


def go_generated(text: str) -> bool:
    """Go's own marker (`// Code generated ... DO NOT EDIT.`) in the header."""
    return bool(_GO_GENERATED.search(text[:2000]))


def files(root: Path, exts: tuple[str, ...]) -> list[str]:
    """Repository-relative paths of in-scope files with one of `exts`."""
    out = []
    for p in root.rglob("*"):
        if not p.name.endswith(exts) or not p.is_file():
            continue
        rel = p.relative_to(root).as_posix()
        if any(part in SKIP_DIRS for part in rel.split("/")[:-1]) or not in_scope(rel):
            continue
        out.append(rel)
    return sorted(out)


def read(root: Path, rel: str) -> str:
    try:
        return (root / rel).read_text(errors="replace")
    except OSError:
        return ""


def clip(text: str, n: int = 200) -> str:
    t = " ".join(text.split())
    return t if len(t) <= n else t[: n - 1] + "…"


def symbol_tokens(qn: str | None) -> tuple[str, str]:
    """(bare name, `Parent.Name`) of a graph qualified name, whatever its
    separators (`.`, `::`, `#`, `/`, `:`)."""
    words = re.findall(r"[A-Za-z_]\w*", qn or "")
    if not words:
        return "", ""
    return words[-1], ".".join(words[-2:])
