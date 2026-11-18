"""Embeddings for the `vector` arm and the CoIR runner (RFC 0007 §11.1).

An `Embedder` turns texts into one row each (float32, any width). Two
implementations:

- `RuecaEmbedder` shells out to `rueca embed` (the binary of the Rueca arm,
  `$RUECA_BIN`), one process per call, through temporary files:

      rueca embed --preset P --max-tokens N [--batch B] [--cpu] [--threads T]
                  [--instruction I] --input in.jsonl --output out.f32 --format f32le

  input lines `{"id", "text", "query"}` (the binary prefixes queries with the
  preset's instruction, or with `--instruction`); output raw little-endian
  float32 rows plus `out.f32.ids` (one id per line, input order).
  `rueca embed --preset P --info` prints the model identity
  (`model_key`, `sha256`, `dims`, `quant`, `runtime`, ...).
- `FakeEmbedder`: deterministic feature hashing of identifier-aware tokens
  (texts sharing tokens are close), for tests.

`CachedEmbedder` puts a content-addressed cache in front of either: rows are
stored at the model's full width as float16 in an append-only store per
namespace (`<vector dir>/<model key>/t<max tokens>/doc` for documents,
`.../query-<instruction tag>` for queries), keyed by sha256 of the text.
Matryoshka dimensions are applied after the cache (`truncate_normalize`:
keep the first `dims` components, L2-normalize), so a sweep over
dimensions or query instructions never re-embeds a document.
"""

from __future__ import annotations

import fcntl
import hashlib
import json
import os
import tempfile
import threading
from dataclasses import dataclass, field
from pathlib import Path
from typing import Protocol, Sequence

import numpy as np

from .common import CACHE, log, run, safe_name
from .queries import tokenize

DEFAULT_PRESET = "qwen3-0.6b-q8"
DEFAULT_MAX_TOKENS = 512
DEFAULT_CHUNK = 2048
EMBED_TIMEOUT = 4 * 3600.0


class EmbedError(RuntimeError):
    """`rueca embed` failed or broke its output contract."""


class Embedder(Protocol):
    """Texts -> rows (float32, shape (n, width)), in input order."""

    def model_key(self) -> str: ...

    def namespace(self, query: bool) -> str:
        """Cache namespace (relative path) of one side: everything besides
        the text that changes a vector (model, token budget, instruction)."""
        ...

    def info(self) -> dict: ...

    def embed(self, texts: Sequence[str], query: bool) -> np.ndarray: ...


def text_key(text: str) -> str:
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def short_hash(text: str) -> str:
    return hashlib.sha256(text.encode("utf-8")).hexdigest()[:12]


def truncate_normalize(mat: np.ndarray, dims: int | None = None) -> np.ndarray:
    """Matryoshka truncation to the first `dims` components (all when None),
    then L2 normalization of every row (zero rows stay zero)."""
    out = np.asarray(mat, dtype=np.float32)
    if dims is not None and dims < out.shape[1]:
        out = out[:, :dims]
    norms = np.linalg.norm(out, axis=1, keepdims=True)
    norms[norms == 0] = 1.0
    return out / norms


# --- the fake embedder (tests) ----------------------------------------------------------


@dataclass
class FakeEmbedder:
    """Deterministic feature hashing: every identifier-aware token adds ±1 to
    one of `width` components (sha256 of the token picks both), so texts
    sharing tokens point the same way. `calls` counts embed calls and texts."""

    width: int = 64
    key: str = "fake"
    calls: list[int] = field(default_factory=list)

    def model_key(self) -> str:
        return f"{self.key}-{self.width}"

    def namespace(self, query: bool) -> str:
        return f"{self.model_key()}/{'query' if query else 'doc'}"

    def info(self) -> dict:
        return {"preset": "fake", "model_key": self.model_key(), "dims": self.width, "runtime": "python"}

    def _row(self, text: str) -> np.ndarray:
        row = np.zeros(self.width, dtype=np.float32)
        for tok in tokenize(text):
            h = hashlib.sha256(tok.encode()).digest()
            row[int.from_bytes(h[:4], "little") % self.width] += 1.0 if h[4] & 1 else -1.0
        return row

    def embed(self, texts: Sequence[str], query: bool) -> np.ndarray:
        self.calls.append(len(texts))
        if not texts:
            return np.zeros((0, self.width), dtype=np.float32)
        return np.stack([self._row(t) for t in texts])


# --- `rueca embed` --------------------------------------------------------------------


@dataclass
class RuecaEmbedder:
    """`rueca embed` through temporary files (module docstring). `instruction`
    overrides the preset's query instruction (`--instruction`); `None` keeps
    the preset's."""

    binary: str
    preset: str = DEFAULT_PRESET
    max_tokens: int = DEFAULT_MAX_TOKENS
    instruction: str | None = None
    batch: int | None = None
    cpu: bool = False
    threads: int | None = None
    timeout: float = EMBED_TIMEOUT
    # cache CPU vectors apart (`<model key>-cpu`): the backend overlap row
    # (RFC 0007 §9) compares rankings from vectors embedded on each backend
    separate_backend: bool = False
    _info: dict | None = field(default=None, init=False, repr=False)

    def _common_args(self) -> list[str]:
        args = [self.binary, "embed", "--preset", self.preset]
        if self.cpu:
            args.append("--cpu")
        if self.threads:
            args += ["--threads", str(self.threads)]
        return args

    def info(self) -> dict:
        if self._info is None:
            res = run([*self._common_args(), "--max-tokens", str(self.max_tokens), "--info"], timeout=300)
            if res.rc != 0:
                raise EmbedError(f"rueca embed --info failed ({res.rc}): {res.stderr.strip()[-1500:]}")
            try:
                self._info = json.loads(res.stdout)
            except json.JSONDecodeError as exc:
                raise EmbedError(f"rueca embed --info printed no JSON: {res.stdout[:300]!r}") from exc
            if not self._info.get("model_key"):
                raise EmbedError(f"rueca embed --info has no model_key: {self._info}")
        return self._info

    def model_key(self) -> str:
        return str(self.info()["model_key"])

    def supports_instruction(self) -> bool:
        res = run([self.binary, "embed", "--help"], timeout=60)
        return "--instruction" in (res.stdout + res.stderr)

    def instruction_tag(self) -> str:
        """`default` (the preset's own instruction, unless `--info` names it
        as `instruction` or `query_prefix`) or a hash of the instruction the
        queries carry."""
        info = self.info()
        instr = self.instruction if self.instruction is not None else (info.get("instruction") or info.get("query_prefix"))
        return short_hash(instr) if instr else "default"

    def namespace(self, query: bool) -> str:
        side = f"query-{self.instruction_tag()}" if query else "doc"
        backend = "-cpu" if self.cpu and self.separate_backend else ""
        return f"{safe_name(self.model_key())}{backend}/t{self.max_tokens}/{side}"

    def command(self, src: Path, out: Path) -> list[str]:
        args = [*self._common_args(), "--max-tokens", str(self.max_tokens)]
        if self.batch:
            args += ["--batch", str(self.batch)]
        if self.instruction is not None:
            args += ["--instruction", self.instruction]
        return [*args, "--input", str(src), "--output", str(out), "--format", "f32le"]

    def embed(self, texts: Sequence[str], query: bool) -> np.ndarray:
        if not texts:
            return np.zeros((0, 0), dtype=np.float32)
        with tempfile.TemporaryDirectory(prefix="rueca-embed-") as tmp:
            src, out = Path(tmp) / "in.jsonl", Path(tmp) / "out.f32"
            ids = [str(i) for i in range(len(texts))]
            with src.open("w", encoding="utf-8") as fh:
                for i, t in zip(ids, texts, strict=True):
                    fh.write(json.dumps({"id": i, "text": t, "query": query}, ensure_ascii=False) + "\n")
            res = run(self.command(src, out), timeout=self.timeout)
            if res.rc != 0:
                raise EmbedError(f"rueca embed failed ({res.rc}{', timed out' if res.timed_out else ''}): {res.stderr.strip()[-1500:]}")
            return read_f32le(out, ids)


def read_f32le(out: Path, ids: list[str]) -> np.ndarray:
    """Rows of an `--format f32le` output, checked against the input ids."""
    ids_path = Path(str(out) + ".ids")
    if not out.exists() or not ids_path.exists():
        raise EmbedError(f"rueca embed wrote no {out.name} / {ids_path.name}")
    got = ids_path.read_text(encoding="utf-8").splitlines()
    if got != ids:
        raise EmbedError(f"rueca embed ids out of order or missing ({len(got)} for {len(ids)} inputs)")
    data = np.fromfile(out, dtype="<f4")
    if data.size % len(ids):
        raise EmbedError(f"rueca embed output of {data.size} floats is not {len(ids)} rows")
    return data.reshape(len(ids), -1).astype(np.float32)


# --- the vector cache -------------------------------------------------------------------


def vector_dir() -> Path:
    """`$RUECA_BENCH_VECTOR_DIR`, else `<bench cache>/vectors` (one cache can
    serve several checkouts: vectors depend only on model and text)."""
    return Path(os.environ.get("RUECA_BENCH_VECTOR_DIR") or CACHE / "vectors").resolve()


class VectorStore:
    """Append-only store of one namespace: `keys.txt` (one sha256 per line,
    line i = row i) and `rows.f16` (float16 rows of a fixed width). Rows are
    written before their keys, under an exclusive `flock`, so a key another
    process can read always has its row."""

    def __init__(self, root: Path):
        self.root = root
        self._lock = threading.Lock()
        self._index: dict[str, int] = {}
        self._n_lines = 0
        self._offset = 0
        self._width: int | None = None

    @property
    def _keys(self) -> Path:
        return self.root / "keys.txt"

    @property
    def _rows(self) -> Path:
        return self.root / "rows.f16"

    def _flock(self, exclusive: bool):
        self.root.mkdir(parents=True, exist_ok=True)
        fh = (self.root / "lock").open("a")
        fcntl.flock(fh, fcntl.LOCK_EX if exclusive else fcntl.LOCK_SH)
        return fh

    def _refresh(self) -> None:
        meta = self.root / "meta.json"
        if self._width is None and meta.exists():
            self._width = int(json.loads(meta.read_text())["width"])
        if not self._keys.exists():
            return
        with self._keys.open("rb") as fh:
            fh.seek(self._offset)
            data = fh.read()
        complete = data[: data.rfind(b"\n") + 1]
        self._offset += len(complete)
        for line in complete.decode().splitlines():
            self._index.setdefault(line, self._n_lines)
            self._n_lines += 1

    def get_many(self, keys: Sequence[str]) -> dict[str, np.ndarray]:
        with self._lock:
            fh = self._flock(exclusive=False)
            try:
                self._refresh()
                rows = {k: self._index[k] for k in set(keys) if k in self._index}
                if not rows or self._width is None:
                    return {}
                mm = np.memmap(self._rows, dtype="<f2", mode="r", shape=(self._n_lines, self._width))
                return {k: np.asarray(mm[i], dtype=np.float32) for k, i in rows.items()}
            finally:
                fh.close()

    def put_many(self, keys: Sequence[str], mat: np.ndarray) -> None:
        if not len(keys):
            return
        with self._lock:
            fh = self._flock(exclusive=True)
            try:
                self._refresh()
                if self._width is None:
                    self._width = int(mat.shape[1])
                    (self.root / "meta.json").write_text(json.dumps({"width": self._width}) + "\n")
                if mat.shape[1] != self._width:
                    raise EmbedError(f"vector width {mat.shape[1]} != {self._width} of the cache at {self.root}")
                new = [i for i, k in enumerate(keys) if k not in self._index]
                if not new:
                    return
                with self._rows.open("ab") as out:
                    out.write(np.asarray(mat[new], dtype="<f2").tobytes())
                with self._keys.open("a") as out:
                    out.write("".join(keys[i] + "\n" for i in new))
                self._refresh()
            finally:
                fh.close()


_STORES: dict[Path, VectorStore] = {}
_STORES_LOCK = threading.Lock()


def open_store(root: Path) -> VectorStore:
    """One store object per namespace and process (S1 runs repositories in
    threads)."""
    with _STORES_LOCK:
        if root not in _STORES:
            _STORES[root] = VectorStore(root)
        return _STORES[root]


@dataclass
class CachedEmbedder:
    """Content-hash cache in front of an `Embedder` (module docstring).
    Misses are embedded `chunk` texts per call and stored after each call, so
    an interrupted long run keeps what it embedded. `hits` / `misses` count
    texts (duplicates within a call once)."""

    inner: Embedder
    root: Path | None = None
    chunk: int = DEFAULT_CHUNK
    progress: bool = False
    hits: int = 0
    misses: int = 0

    def __post_init__(self) -> None:
        if self.root is None:
            self.root = vector_dir()

    def model_key(self) -> str:
        return self.inner.model_key()

    def info(self) -> dict:
        return self.inner.info()

    def embed(self, texts: Sequence[str], query: bool) -> np.ndarray:
        """Full-width float32 rows (not normalized), input order."""
        store = open_store(self.root / self.inner.namespace(query))
        keys = [text_key(t) for t in texts]
        have = store.get_many(keys)
        todo: dict[str, str] = {}
        for k, t in zip(keys, texts, strict=True):
            if k not in have and k not in todo:
                todo[k] = t
        self.hits += len(set(keys)) - len(todo)
        self.misses += len(todo)
        items = list(todo.items())
        for start in range(0, len(items), self.chunk):
            part = items[start : start + self.chunk]
            mat = self.inner.embed([t for _, t in part], query)
            store.put_many([k for k, _ in part], mat)
            have.update({k: mat[i] for i, (k, _) in enumerate(part)})
            if self.progress:
                log(f"embedded {min(start + self.chunk, len(items))}/{len(items)} ({'queries' if query else 'documents'})")
        if not keys:
            return np.zeros((0, 0), dtype=np.float32)
        return np.stack([have[k] for k in keys]).astype(np.float32)
