"""Dense-retrieval arm (`vector`, RFC 0007 §11.1): the `grep` arm's units
embedded through `rueca embed`, ranked by cosine similarity.

Units are exactly the `grep` arm's: the same source files (`code_files`)
and the same tree-sitter functions (`extract_functions`), so the two arms
differ only in the scorer. Each function is embedded once as a **canonical
text** (RFC 0007 §5.2, text builder version `TEXT_VERSION` = 1):

    <language> <label> <qualified name>
    path: <repository-relative path>
    <signature>
    <doc comment>
    <body head>

- `language`: the instance language (`python`, `typescript`, `cpp`, ...);
  `label`: the labeler's `Function` or `Method`; `qualified name`: the
  labeler's local qualified name (`Class.method`, `outer.inner`, `ns::f`).
- `signature`: the declaration from its first line (after decorators and
  attributes) to the end of the header, lines stripped and joined by one
  space: up to and including the `:` that ends a Python header, or up to
  (excluding) the first `{` outside parentheses / brackets in other
  languages; at most 12 lines (else only the first line).
- `doc comment`: in Python the docstring (the body's first statement, a
  string literal, lines stripped), else the comment lines directly above
  the declaration (above its decorators, Rust attributes skipped; no blank
  line between), stripped, markers kept (`//`, `///`, `/*`, `*`, `*/`; `#`
  in Python only). Omitted when empty.
- `body head`: the rest of the function after the header (and after the
  docstring), without the closing `}` of brace languages, dedented,
  trailing spaces removed, blank lines dropped.
- The whole text is cut to `max_tokens` × 4 characters (default 512 tokens
  → 2,048 characters; the binary cuts again at `--max-tokens` real
  tokens), then right-stripped. Lines are joined with `\\n`; empty parts
  are left out (no blank lines).

Optional file texts (`file_text`, off by default):
`<language> File <path>\\npath: <path>\\n<file head>` (non-blank lines,
right-stripped, same budget). A file's score is the best cosine of its
functions (and of its file text when on); files without functions are
unranked unless the file text is on (RFC 0007 §5.1: no file vectors).

Queries (`kw` / `raw`, the same strings every arm gets) are embedded with
the query flag, so the binary adds the preset's instruction (or
`instruction`). Scores are exact cosines (numpy matmul over L2-normalized
rows), ties broken by unit order. Sweep knobs (environment, every one part
of the run directory `runs/s1/vector/<tag>/`): `RUECA_BENCH_VECTOR_PRESET`,
`_DIMS`, `_MAX_TOKENS`, `_INSTRUCTION`, `_FILE_TEXT` (`1` / `0`), and
`RUECA_BENCH_VECTOR_DIR` (the vector cache, `embed.vector_dir`).
"""

from __future__ import annotations

import os
import re
import textwrap
import time
from dataclasses import dataclass
from pathlib import Path

import numpy as np

from ..embed import DEFAULT_MAX_TOKENS, DEFAULT_PRESET, CachedEmbedder, Embedder, RuecaEmbedder, short_hash, truncate_normalize
from ..treesitter import Func, extract_functions
from .base import Arm, CallResult, Handle
from .grep import SCORES_KEEP, code_files, func_record, scored
from .rueca import rueca_binary

TEXT_VERSION = 1
CHARS_PER_TOKEN = 4
MAX_SIGNATURE_LINES = 12
_OPEN = "(["
_CLOSE = ")]"
_COMMENT = ("//", "/*", "*", "*/")
_DOCSTRING = re.compile(r"""^[rRbBuUfF]{0,2}("{3}|'{3}|"|')""")


# --- canonical text ------------------------------------------------------------------------


def _scan_header(line: str, depth: int, python: bool) -> tuple[int, int | None]:
    """Paren depth after `line`, and the column where the header ends on it
    (Python: just after a final `:` at depth 0; else the first `{` at depth 0)."""
    for col, ch in enumerate(line):
        if ch in _OPEN:
            depth += 1
        elif ch in _CLOSE:
            depth = max(0, depth - 1)
        elif ch == "{" and depth == 0 and not python:
            return depth, col
    if python and depth == 0 and line.rstrip().endswith(":"):
        return depth, len(line.rstrip())
    return depth, None


def signature_of(lines: list[str], start: int, python: bool) -> tuple[str, int, str]:
    """(signature, index of the header's last line, the code after the header
    on that line) for a declaration starting at 0-based line `start`."""
    depth = 0
    parts: list[str] = []
    for i in range(start, min(len(lines), start + MAX_SIGNATURE_LINES)):
        depth, end = _scan_header(lines[i], depth, python)
        if end is None:
            parts.append(lines[i].strip())
            continue
        parts.append(lines[i][:end].strip())
        return " ".join(p for p in parts if p), i, lines[i][end:].lstrip("{").strip()
    first = lines[start].strip() if start < len(lines) else ""
    return first, start, ""


def _is_comment(line: str, python: bool) -> bool:
    s = line.strip()
    if python:
        return s.startswith("#")
    return s.startswith(_COMMENT)


def _is_attribute(line: str, python: bool) -> bool:
    return not python and line.strip().startswith(("#[", "#!["))


def doc_above(lines: list[str], outer_start: int, python: bool) -> str:
    """Comment lines directly above 0-based line `outer_start` (Rust
    attributes between them and the declaration are skipped, not kept)."""
    out: list[str] = []
    i = outer_start - 1
    while i >= 0 and lines[i].strip():
        if _is_comment(lines[i], python):
            out.append(lines[i].strip())
        elif not _is_attribute(lines[i], python):
            break
        i -= 1
    return "\n".join(reversed(out))


def python_docstring(body: list[str]) -> tuple[str, int]:
    """(docstring lines stripped, number of body lines it spans) when the
    body opens with a string literal."""
    if not body:
        return "", 0
    m = _DOCSTRING.match(body[0].strip())
    if not m:
        return "", 0
    quote = m.group(1)
    first = body[0].strip()
    rest = first[m.end() :]
    if quote in rest:
        return first, 1
    for j in range(1, len(body)):
        if quote in body[j]:
            return "\n".join(x.strip() for x in body[: j + 1] if x.strip()), j + 1
    return "\n".join(x.strip() for x in body if x.strip()), len(body)


_CLOSING = re.compile(r"\s*\}\s*;?\s*$")


def _body_text(body: list[str], python: bool) -> str:
    """Dedented, blank lines dropped; in brace languages the closing `}` of
    the function is left out."""
    text = "\n".join(x.rstrip() for x in body)
    if not python:
        text = _CLOSING.sub("", text)
    text = textwrap.dedent(text)
    return "\n".join(x for x in text.splitlines() if x.strip())


def cut(text: str, max_tokens: int) -> str:
    return text[: max_tokens * CHARS_PER_TOKEN].rstrip()


def canonical_text(language: str, path: str, func: Func, lines: list[str], max_tokens: int = DEFAULT_MAX_TOKENS) -> str:
    """The function's canonical text (module docstring)."""
    python = language == "python"
    signature, last, rest = signature_of(lines, func.start - 1, python)
    body = ([rest] if rest else []) + lines[last + 1 : func.end]
    doc = doc_above(lines, func.outer_start - 1, python)
    if python and not rest:
        docstring, span = python_docstring(body)
        body = body[span:]
        doc = docstring or doc
    head = f"{language} {func.label} {func.qualname}\npath: {path}"
    parts = [head, signature, doc, _body_text(body, python)]
    return cut("\n".join(p for p in parts if p), max_tokens)


def file_text(language: str, path: str, text: str, max_tokens: int = DEFAULT_MAX_TOKENS) -> str:
    head = "\n".join(x.rstrip() for x in text.splitlines()[: max_tokens * 2] if x.strip())
    return cut(f"{language} File {path}\npath: {path}\n{head}", max_tokens)


# --- configuration ------------------------------------------------------------------------


def _env_int(name: str) -> int | None:
    v = os.environ.get(name, "").strip()
    return int(v) if v else None


@dataclass(frozen=True)
class VectorConfig:
    preset: str = DEFAULT_PRESET
    dims: int | None = None
    max_tokens: int = DEFAULT_MAX_TOKENS
    instruction: str | None = None
    file_text: bool = False

    @classmethod
    def from_env(cls) -> "VectorConfig":
        instr = os.environ.get("RUECA_BENCH_VECTOR_INSTRUCTION")
        return cls(
            preset=os.environ.get("RUECA_BENCH_VECTOR_PRESET") or DEFAULT_PRESET,
            dims=_env_int("RUECA_BENCH_VECTOR_DIMS"),
            max_tokens=_env_int("RUECA_BENCH_VECTOR_MAX_TOKENS") or DEFAULT_MAX_TOKENS,
            instruction=instr if instr else None,
            file_text=os.environ.get("RUECA_BENCH_VECTOR_FILE_TEXT", "0").strip().lower() in ("1", "true", "on", "yes"),
        )

    def tag(self) -> str:
        """Every knob (and the text builder version): two sweep settings never
        share a run directory."""
        instr = short_hash(self.instruction) if self.instruction else "default"
        return f"{self.preset}-d{self.dims or 'full'}-t{self.max_tokens}-i{instr}-f{int(self.file_text)}-x{TEXT_VERSION}"

    def describe(self) -> str:
        instr = f"instruction {self.instruction!r}" if self.instruction else "preset instruction"
        return f"{self.preset}, dims {self.dims or 'full'}, {self.max_tokens} tokens, {instr}, file text {'on' if self.file_text else 'off'}, text v{TEXT_VERSION}"


# --- the arm --------------------------------------------------------------------------------


@dataclass
class _Index:
    funcs: list[dict]
    func_file: np.ndarray  # file index of each function
    func_vecs: np.ndarray  # (n_funcs, d), L2-normalized
    files: list[str]
    file_vecs: np.ndarray | None  # (n_files, d) when file texts are on


class VectorArm(Arm):
    name = "vector"

    def __init__(self, config: VectorConfig | None = None, embedder: Embedder | None = None, vector_root: Path | None = None):
        self.config = config or VectorConfig.from_env()
        self.bin = rueca_binary()
        if embedder is None and self.bin:
            embedder = RuecaEmbedder(self.bin, self.config.preset, self.config.max_tokens, self.config.instruction)
        self.embedder = embedder
        self.cache = CachedEmbedder(embedder, vector_root) if embedder is not None else None

    def available(self) -> tuple[bool, str]:
        if self.embedder is None:
            return False, "RUECA_BIN not set"
        if isinstance(self.embedder, RuecaEmbedder) and not Path(self.embedder.binary).exists():
            return False, f"RUECA_BIN={self.embedder.binary} does not exist"
        return True, ""

    def preflight(self) -> None:
        """Fail loudly when a knob cannot be honoured (an instruction override
        on a binary without `--instruction`), before any instance runs."""
        if isinstance(self.embedder, RuecaEmbedder):
            self.embedder.info()
            if self.config.instruction is not None and not self.embedder.supports_instruction():
                raise RuntimeError("RUECA_BENCH_VECTOR_INSTRUCTION is set but `rueca embed` has no --instruction flag")

    def run_label(self) -> str:
        return f"vector/{self.config.tag()}"

    def version(self) -> str:
        if self.embedder is None:
            return "pending (RUECA_BIN not set)"
        try:
            info = self.embedder.info()
        except RuntimeError as exc:
            return f"rueca embed {self.config.describe()} (no model identity: {str(exc)[:200]})"
        ident = ", ".join(f"{k} {info[k]}" for k in ("model_key", "sha256", "quant", "runtime", "runtime_version", "backend", "cpu_level") if info.get(k))
        return f"rueca embed {self.config.describe()} ({ident})"

    def index(self, root: Path, key: str, language: str) -> Handle:
        t0 = time.perf_counter()
        h = Handle(self.name, key, root, language)
        hits0, miss0 = self.cache.hits, self.cache.misses
        funcs, func_file, texts, files, ftexts = [], [], [], [], []
        mt = self.config.max_tokens
        for rel, data in code_files(root, language):
            text = data.decode("utf-8", errors="replace")
            lines = text.splitlines()
            files.append(rel)
            if self.config.file_text:
                ftexts.append(file_text(language, rel, text, mt))
            for f in extract_functions(rel, data, language):
                funcs.append(func_record(rel, f))
                func_file.append(len(files) - 1)
                texts.append(canonical_text(language, rel, f, lines, mt))
        try:
            func_vecs = self._vectors(texts)
            file_vecs = self._vectors(ftexts) if self.config.file_text else None
        except RuntimeError as exc:
            h.ok, h.error = False, str(exc)
            return h
        h.state = _Index(funcs, np.asarray(func_file, dtype=np.int64), func_vecs, files, file_vecs)
        h.index_seconds = time.perf_counter() - t0
        h.info = {"files": len(files), "functions": len(funcs), "embedded": self.cache.misses - miss0, "cached": self.cache.hits - hits0, "dims": int(func_vecs.shape[1]) if func_vecs.size else None}
        return h

    def _vectors(self, texts: list[str], query: bool = False) -> np.ndarray:
        if not texts:
            return np.zeros((0, self.config.dims or 1), dtype=np.float32)
        return truncate_normalize(self.cache.embed(texts, query), self.config.dims)

    def file_scores(self, ix: _Index, fn_scores: np.ndarray, q: np.ndarray) -> np.ndarray:
        """Best function cosine per file (−inf when the file has none), and
        the file text's cosine when file texts are on."""
        out = np.full(len(ix.files), -np.inf, dtype=np.float32)
        if len(fn_scores):
            np.maximum.at(out, ix.func_file, fn_scores)
        if ix.file_vecs is not None and len(ix.file_vecs):
            out = np.maximum(out, ix.file_vecs @ q)
        return out

    def localize(self, h: Handle, query: str, limit: int = 100) -> CallResult:
        t0 = time.perf_counter()
        ix: _Index = h.state
        try:
            q = self._vectors([query], query=True)[0]
        except RuntimeError as exc:
            return CallResult(False, time.perf_counter() - t0, error=str(exc))
        fn_scores = ix.func_vecs @ q if len(ix.funcs) else np.zeros(0, dtype=np.float32)
        f_scores = self.file_scores(ix, fn_scores, q)
        keep = max(limit, SCORES_KEEP)
        fn_order = np.argsort(-fn_scores, kind="stable")[:keep]
        file_order = [i for i in np.argsort(-f_scores, kind="stable")[:keep] if np.isfinite(f_scores[i])]
        files = [(ix.files[i], float(f_scores[i])) for i in file_order]
        funcs = [(ix.funcs[i], float(fn_scores[i])) for i in fn_order]
        return CallResult(
            True,
            time.perf_counter() - t0,
            extra={
                "files": [p for p, _ in files[:limit]],
                "functions": [dict(f) for f, _ in funcs[:limit]],
                "scores": scored(files, funcs),
            },
        )
