"""Common arm interface.

Every arm answers the same four questions on an indexed working tree:
- localize(query): ranked files + ranked functions (S1)
- resolve(func): can the arm find this definition? returns an arm-specific ref
- neighbors(ref, direction): depth-1 callers or callees (S3)
- lookup(name): ranked definitions for a bare name (S3 symbol lookup)

Hits are normalized to dicts: {"path", "name", "label", "start_line", "end_line"}
with repository-relative paths and 1-based lines.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

CALLABLE_LABELS = {"Function", "Method", "Test", "Constructor"}


def is_callable_label(label: str | None) -> bool:
    if not label:
        return False
    if label in CALLABLE_LABELS:
        return True
    return label.startswith("ext:") and label.endswith(("Function", "Method"))


@dataclass
class Handle:
    arm: str
    key: str
    root: Path
    language: str
    ok: bool = True
    error: str | None = None
    index_seconds: float = 0.0
    info: dict[str, Any] = field(default_factory=dict)
    state: Any = None


@dataclass
class CallResult:
    ok: bool
    seconds: float
    hits: list[dict] = field(default_factory=list)
    error: str | None = None
    extra: dict[str, Any] = field(default_factory=dict)


class Arm:
    name = "base"

    def available(self) -> tuple[bool, str]:
        return True, ""

    def version(self) -> str:
        return "unknown"

    def index(self, root: Path, key: str, language: str) -> Handle:
        raise NotImplementedError

    def localize(self, h: Handle, query: str, limit: int = 100) -> CallResult:
        raise NotImplementedError

    def resolve(self, h: Handle, func: dict) -> CallResult:
        raise NotImplementedError

    def neighbors(self, h: Handle, ref: Any) -> CallResult:
        """Depth-1 callers and callees in one answer (one tool call where the
        tool allows it): extra = {"callers": hits, "callees": hits,
        "tokens": cl100k tokens of the raw answer or None}. Hits may carry
        `confidence` (Rueca: the best confidence of the edges linking the
        neighbour to the root)."""
        raise NotImplementedError

    def lookup(self, h: Handle, name: str, limit: int = 10) -> CallResult:
        raise NotImplementedError

    # -- data-format suite (data.py) ----------------------------------------

    def data_nodes(self, h: Handle, files: list[str]) -> CallResult:
        """Every node the arm has in the given data files: hits of
        {path, name, label, start_line}."""
        raise NotImplementedError

    def env_edges(self, h: Handle, names: list[str]) -> CallResult:
        """Env bridge edges: extra = {"reads": [{path, function, env, line}],
        "writes": [{path, env, line}]}; `names` are candidate variable names
        for arms that cannot enumerate env vars."""
        raise NotImplementedError

    def cleanup(self, h: Handle) -> None:
        pass


def line_match(hit_line: int | None, func: dict, tol: int = 2) -> bool:
    """Arm line matches a gold def if within +-tol of the name line, the
    declaration start, or the decorator start."""
    if hit_line is None:
        return True
    anchors = {func.get("def_line"), func.get("start"), func.get("outer_start")}
    return any(a is not None and abs(hit_line - a) <= tol for a in anchors)


def same_def(hit: dict, func: dict, tol: int = 2) -> bool:
    return hit.get("path") == func.get("path") and hit.get("name") == func.get("name") and line_match(hit.get("start_line"), func, tol)
