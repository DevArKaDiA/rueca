"""Swift gold: the compiler's index store, read as SCIP-like documents.

Pipeline (`run_swift_index`), all inside the pinned official `swift` image:

1. networked container: the checkout (mounted read-only) is copied, without
   `.git`, to `<scratch>/src` and `swift package resolve` fetches the
   dependencies (git clones only) into `<scratch>/build`. The copy exists
   because SwiftPM writes `Package.resolved` next to `Package.swift` when the
   package does not commit one, which a read-only mount refuses (I/O error
   30); the checkout itself gains nothing.
2. `--network=none` container: `swift build --build-tests` (every target,
   tests included) with the Swift Build engine and index-while-building,
   plus `-Xswiftc -index-include-locals` (without it nested local functions
   have no occurrence at all: neither their definition nor their calls),
   then `indexstore_dump.py` (mounted read-only) walks the store through
   libIndexStore and writes JSON. The image has the Python runtime that
   lldb links against (libpython3.12 + standard library, ctypes included)
   but no `python3` executable; the script compiles the two-line
   `Py_BytesMain` launcher with the image's clang when `python3` is missing.
3. the dump is moved to `out` and the scratch directory (copy, checkouts,
   build products, index store) is removed.

Conversion (`read_indexstore`): one `scip.Doc` per source file (path
relative to the package root, `language = "swift"`, `position_encoding = 1`:
0-based UTF-8 byte columns), one `scip.Occ` per kept occurrence, symbol =
the USR as is. Decisions:

- Range: line - 1, and the byte span of the token written at the
  occurrence (`indexstore_dump.token_span`), so the source slice is the bare
  name: `foo` for `foo(a:b:)`, `init` / `subscript` / `deinit` / `didSet`
  at their declarations, the name inside backticks for `` `default` ``, the
  operator characters for an operator symbol. At `Foo(x: 1)` the slice is
  `Foo`: the store puts the type reference and the initializer call at the
  same column; `Foo.init(x: 1)` and `.init(x: 1)` give `init`.
- Roles: `scip.ROLE_DEFINITION` (1) when the store says DEFINITION or
  DECLARATION (a protocol requirement is a definition with DYNAMIC; a
  declaration-only occurrence is defined the same way); WRITE / READ map to
  SCIP's WriteAccess (4) / ReadAccess (8). Every other occurrence is a
  reference (0); the call-site test stays tree-sitter's
  (`scip.derive_call_graph`), as for every other language.
- IMPLICIT occurrences are dropped, definitions included. None is a call
  written by name in the source: they are property getters / setters
  (`x.count`, `box.value = 2`: reading or writing a property is not a call
  in the gold), string-interpolation and literal initializers, implicit
  memberwise / default initializers and accessors, and the conformance
  witnesses the compiler synthesizes on a type's name. A call to an
  implicit memberwise initializer (`Point(x: 1, y: 2)`) is itself recorded
  as an explicit reference with the CALL role, but its callee has no
  explicit definition, so no callable binds to it.
- Occurrences with no identifier or operator at their column are dropped:
  `callAsFunction` called as `c(4)` and a subscript used as `self[3]`
  (both at the `(` / `[`), and the getter of a computed property written
  without `get` (at its `{`).
- Kinds are not filtered: functions, methods, initializers, subscripts,
  accessors with a keyword (`get`, `set`, `didSet`, ...), types, cases,
  properties, parameters and (with `-index-include-locals`) local variables
  and local functions all stay; which ones are callables is decided by the
  tree-sitter binding.
- Occurrences in files outside the package root (SDK, other packages'
  checkouts) are never in the dump; system units and records are skipped.
"""

from __future__ import annotations

import os
import shlex
import shutil
from pathlib import Path

from .common import SCIP_DIR, CmdResult, read_json
from . import indexstore_dump
from .common import run as sh
from .scip import ROLE_DEFINITION, Doc, Occ

# indexstore_symbol_role_t
IS_DECLARATION = 1 << 0
IS_DEFINITION = 1 << 1
IS_READ = 1 << 3
IS_WRITE = 1 << 4
IS_IMPLICIT = 1 << 8
# SCIP SymbolRole
SCIP_WRITE = 4
SCIP_READ = 8

DEFAULT_BUILD_ARGS = ["-Xswiftc", "-index-include-locals"]

# Shell lines (run under `set -euo pipefail`) that set PY to a Python 3
# interpreter: the image's `python3`, else a `Py_BytesMain` launcher linked
# against the image's libpython.
_PY_LAUNCHER = [
    'PY=$(command -v python3 || true)',
    'if [ -z "$PY" ]; then',
    "  printf 'int Py_BytesMain(int, char **);\\nint main(int c, char **v) { return Py_BytesMain(c, v); }\\n' > /tmp/py.c",
    '  LIBPY=$(ls /usr/lib/*/libpython3.*.so | sort | tail -n 1)',
    '  clang -o /tmp/python3 /tmp/py.c "$LIBPY"',
    '  PY=/tmp/python3',
    'fi',
]


def _docker(image: str, work: Path, mounts: list[str], network: bool, script: str, timeout: float) -> CmdResult:
    user = [] if os.name != "posix" else ["--user", f"{os.getuid()}:{os.getgid()}"]
    net = [] if network else ["--network=none"]
    cmd = ["docker", "run", "--rm", *net, *user, "-e", "HOME=/work/home", "-v", f"{work}:/work", *mounts, image, "bash", "-c", script]
    return sh(cmd, timeout=timeout)


def _resolve_script() -> str:
    return "\n".join([
        "set -euo pipefail",
        "mkdir -p /work/home /work/src",
        "tar -C /src --exclude=.git -cf - . | tar -C /work/src -xf -",
        "swift package resolve --package-path /work/src --scratch-path /work/build",
    ])


def _build_script(build_args: list[str]) -> str:
    args = " ".join(shlex.quote(a) for a in build_args)
    return "\n".join([
        "set -euo pipefail",
        f"swift build --package-path /work/src --scratch-path /work/build --build-system swiftbuild --enable-index-store --build-tests {args}",
        "STORE=$(find /work/build -type d -path '*/index/store/v*/units' | sort | sed -n 1p)",
        'test -n "$STORE"',
        'STORE=$(dirname "$(dirname "$STORE")")',
        *_PY_LAUNCHER,
        '"$PY" -I /tools/indexstore_dump.py "$STORE" /work/src /work/index.json',
    ])


def run_swift_index(root: Path, out: Path, image: str, name: str, build_args: list[str] | None = None) -> CmdResult:
    """Build the SwiftPM package at `root` (all targets, tests included)
    with index-while-building in `image` and write the index-store dump to
    `out` (JSON, see indexstore_dump.py). `build_args` are appended to
    `swift build` (default `DEFAULT_BUILD_ARGS`). Returns the combined
    result of the two containers (resolve, then build + dump)."""
    work = SCIP_DIR / f"{name}-swift"
    shutil.rmtree(work, ignore_errors=True)
    work.mkdir(parents=True)
    src = ["-v", f"{root}:/src:ro"]
    try:
        res = _docker(image, work, src, True, _resolve_script(), 1800)
        if res.rc != 0:
            return res
        dumper = ["-v", f"{Path(indexstore_dump.__file__).resolve()}:/tools/indexstore_dump.py:ro"]
        args = DEFAULT_BUILD_ARGS if build_args is None else build_args
        build = _docker(image, work, dumper, False, _build_script(args), 3600)
        if (work / "index.json").exists() and build.rc == 0:
            out.parent.mkdir(parents=True, exist_ok=True)
            shutil.move(str(work / "index.json"), out)
        return CmdResult(build.rc, res.stdout + build.stdout, res.stderr + build.stderr, res.seconds + build.seconds, build.timed_out)
    finally:
        shutil.rmtree(work, ignore_errors=True)


def _scip_roles(roles: int) -> int:
    out = ROLE_DEFINITION if roles & (IS_DEFINITION | IS_DECLARATION) else 0
    if roles & IS_WRITE:
        out |= SCIP_WRITE
    if roles & IS_READ:
        out |= SCIP_READ
    return out


def _occurrence(o: dict) -> Occ | None:
    if o["roles"] & IS_IMPLICIT:
        return None
    start, end = o["tok"]
    if end <= start:
        return None
    line0 = o["line"] - 1
    return Occ(o["usr"], _scip_roles(o["roles"]), (line0, start, line0, end), None)


def _document(rel: str, occurrences: list[dict]) -> Doc:
    doc = Doc(path=rel, language="swift", position_encoding=1)
    seen: set[tuple] = set()
    for o in occurrences:
        occ = _occurrence(o)
        if occ is None:
            continue
        key = (occ.symbol, occ.roles, occ.range)
        if key not in seen:
            seen.add(key)
            doc.occurrences.append(occ)
    return doc


def read_indexstore(path: Path) -> list[Doc]:
    """The dump at `path` as `scip.Doc`s, one per source file, sorted."""
    data = read_json(path)
    return [_document(rel, occs) for rel, occs in sorted(data["files"].items())]
