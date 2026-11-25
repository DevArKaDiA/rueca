"""Dump a Clang/Swift index store (libIndexStore C API) as JSON.

Standalone, standard library only (ctypes): it runs inside the pinned
`swift` image, where the bench's `indexstore.py` mounts it read-only.

    python3 indexstore_dump.py <store_path> <source_root> <out.json> [libIndexStore.so]

The C API is clang/include/indexstore/indexstore.h of swiftlang/llvm-project
(the image ships it as /usr/local/include/indexstore/indexstore.h; checked
against INDEXSTORE_VERSION 0.16, store format 5). Every function used here is
declared below with its exact C signature.

Walk: every unit of the store (sorted) -> its RECORD dependencies (one record
per source file a compilation indexed) -> the record's occurrences. Units and
dependencies flagged as system, and records of files outside `source_root`
(SDK modules, other packages' checkouts), are skipped. A record is read once
however many units share it (record names are content hashes), and
occurrences are deduplicated per file on (line, col, usr, roles, relations).

Output (sorted, deterministic):

    {"format": 1, "indexstore_version": .., "indexstore_format_version": ..,
     "source_root": .., "stats": {..},
     "files": {"<path relative to source_root>": [
        {"usr", "name", "kind", "subkind", "props", "roles", "line", "col",
         "tok": [start, end], "rel": [{"roles", "usr"}, ...]}, ...]}}

`line` / `col` are what the store gives (1-based; `col` counts UTF-8 bytes).
`tok` is the 0-based, end-exclusive UTF-8 byte span of the token actually
written at that position (`token_span`): an identifier (`foo` for
`foo(a:b:)`, `init`, `subscript`, `Foo` where `Foo(x:)` calls an
initializer), the name inside backticks for an escaped identifier, the
operator characters for an operator symbol. An empty span means no
identifier or operator starts there (e.g. an implicit occurrence).
"""

from __future__ import annotations

import ctypes
import json
import os
import sys

# indexstore_symbol_subkind_t values of the Swift operator sub-kinds.
OPERATOR_SUBKINDS = frozenset((1008, 1009, 1010))
# indexstore_unit_dependency_kind_t
DEP_RECORD = 2
# ASCII characters an operator can be written with (`.` only leads a dot
# operator; Unicode operator characters are non-ASCII bytes, accepted below).
OPERATOR_BYTES = frozenset(b"/=-+!*%<>&|^~?.")
BACKTICK = 0x60
LIB_DEFAULT = "/usr/lib/libIndexStore.so"


class StringRef(ctypes.Structure):
    _fields_ = [("data", ctypes.c_char_p), ("length", ctypes.c_size_t)]


_P = ctypes.c_void_p
_U64 = ctypes.c_uint64
_UNIT_CB = ctypes.CFUNCTYPE(ctypes.c_bool, _P, StringRef)
_PTR_CB = ctypes.CFUNCTYPE(ctypes.c_bool, _P, _P)

# name -> (restype, argtypes), exactly as declared in indexstore.h.
_SIGNATURES = {
    "indexstore_format_version": (ctypes.c_uint, []),
    "indexstore_version": (ctypes.c_uint, []),
    "indexstore_error_get_description": (ctypes.c_char_p, [_P]),
    "indexstore_error_dispose": (None, [_P]),
    "indexstore_store_create": (_P, [ctypes.c_char_p, ctypes.POINTER(_P)]),
    "indexstore_store_dispose": (None, [_P]),
    "indexstore_store_units_apply_f": (ctypes.c_bool, [_P, ctypes.c_uint, _P, _UNIT_CB]),
    "indexstore_unit_reader_create": (_P, [_P, ctypes.c_char_p, ctypes.POINTER(_P)]),
    "indexstore_unit_reader_dispose": (None, [_P]),
    "indexstore_unit_reader_is_system_unit": (ctypes.c_bool, [_P]),
    "indexstore_unit_reader_dependencies_apply_f": (ctypes.c_bool, [_P, _P, _PTR_CB]),
    "indexstore_unit_dependency_get_kind": (ctypes.c_int, [_P]),
    "indexstore_unit_dependency_is_system": (ctypes.c_bool, [_P]),
    "indexstore_unit_dependency_get_filepath": (StringRef, [_P]),
    "indexstore_unit_dependency_get_name": (StringRef, [_P]),
    "indexstore_record_reader_create": (_P, [_P, ctypes.c_char_p, ctypes.POINTER(_P)]),
    "indexstore_record_reader_dispose": (None, [_P]),
    "indexstore_record_reader_occurrences_apply_f": (ctypes.c_bool, [_P, _P, _PTR_CB]),
    "indexstore_occurrence_get_symbol": (_P, [_P]),
    "indexstore_occurrence_get_roles": (_U64, [_P]),
    "indexstore_occurrence_get_line_col": (None, [_P, ctypes.POINTER(ctypes.c_uint), ctypes.POINTER(ctypes.c_uint)]),
    "indexstore_occurrence_relations_apply_f": (ctypes.c_bool, [_P, _P, _PTR_CB]),
    "indexstore_symbol_relation_get_roles": (_U64, [_P]),
    "indexstore_symbol_relation_get_symbol": (_P, [_P]),
    "indexstore_symbol_get_kind": (ctypes.c_int, [_P]),
    "indexstore_symbol_get_subkind": (ctypes.c_int, [_P]),
    "indexstore_symbol_get_properties": (_U64, [_P]),
    "indexstore_symbol_get_name": (StringRef, [_P]),
    "indexstore_symbol_get_usr": (StringRef, [_P]),
}


def load_library(path: str) -> ctypes.CDLL:
    lib = ctypes.CDLL(path)
    for name, (restype, argtypes) in _SIGNATURES.items():
        fn = getattr(lib, name)
        fn.restype = restype
        fn.argtypes = argtypes
    return lib


def _text(ref: StringRef) -> str:
    if not ref.data or not ref.length:
        return ""
    return ctypes.string_at(ref.data, ref.length).decode("utf-8", errors="replace")


def _check(lib: ctypes.CDLL, handle, err: ctypes.c_void_p, what: str):
    if handle:
        return handle
    msg = lib.indexstore_error_get_description(err).decode() if err.value else "unknown error"
    if err.value:
        lib.indexstore_error_dispose(err)
    raise RuntimeError(f"{what}: {msg}")


# --- store walk -------------------------------------------------------------


def unit_names(lib: ctypes.CDLL, store) -> list[str]:
    names: list[str] = []

    def applier(_ctx, ref):
        names.append(_text(ref))
        return True

    lib.indexstore_store_units_apply_f(store, 1, None, _UNIT_CB(applier))
    return names


def unit_records(lib: ctypes.CDLL, store, unit: str) -> list[tuple[str, str]]:
    """(record name, source file path) of a non-system unit's non-system
    record dependencies."""
    err = ctypes.c_void_p()
    reader = _check(lib, lib.indexstore_unit_reader_create(store, unit.encode(), ctypes.byref(err)), err, f"unit {unit}")
    out: list[tuple[str, str]] = []

    def applier(_ctx, dep):
        if lib.indexstore_unit_dependency_get_kind(dep) == DEP_RECORD and not lib.indexstore_unit_dependency_is_system(dep):
            out.append((_text(lib.indexstore_unit_dependency_get_name(dep)), _text(lib.indexstore_unit_dependency_get_filepath(dep))))
        return True

    try:
        if not lib.indexstore_unit_reader_is_system_unit(reader):
            lib.indexstore_unit_reader_dependencies_apply_f(reader, None, _PTR_CB(applier))
    finally:
        lib.indexstore_unit_reader_dispose(reader)
    return out


def _symbol(lib: ctypes.CDLL, sym) -> dict:
    return {
        "usr": _text(lib.indexstore_symbol_get_usr(sym)),
        "name": _text(lib.indexstore_symbol_get_name(sym)),
        "kind": lib.indexstore_symbol_get_kind(sym),
        "subkind": lib.indexstore_symbol_get_subkind(sym),
        "props": lib.indexstore_symbol_get_properties(sym),
    }


def _relations(lib: ctypes.CDLL, occ) -> list[dict]:
    rels: list[dict] = []

    def applier(_ctx, rel):
        sym = lib.indexstore_symbol_relation_get_symbol(rel)
        rels.append({"roles": lib.indexstore_symbol_relation_get_roles(rel), "usr": _text(lib.indexstore_symbol_get_usr(sym))})
        return True

    lib.indexstore_occurrence_relations_apply_f(occ, None, _PTR_CB(applier))
    return sorted(rels, key=lambda r: (r["usr"], r["roles"]))


def record_occurrences(lib: ctypes.CDLL, store, record: str) -> list[dict]:
    err = ctypes.c_void_p()
    reader = _check(lib, lib.indexstore_record_reader_create(store, record.encode(), ctypes.byref(err)), err, f"record {record}")
    out: list[dict] = []
    line, col = ctypes.c_uint(), ctypes.c_uint()

    def applier(_ctx, occ):
        lib.indexstore_occurrence_get_line_col(occ, ctypes.byref(line), ctypes.byref(col))
        entry = _symbol(lib, lib.indexstore_occurrence_get_symbol(occ))
        entry.update(roles=lib.indexstore_occurrence_get_roles(occ), line=line.value, col=col.value, rel=_relations(lib, occ))
        out.append(entry)
        return True

    try:
        lib.indexstore_record_reader_occurrences_apply_f(reader, None, _PTR_CB(applier))
    finally:
        lib.indexstore_record_reader_dispose(reader)
    return out


# --- token spans ------------------------------------------------------------


def _ident_byte(b: int) -> bool:
    return b >= 0x80 or b in (0x5F, 0x24) or 0x30 <= b <= 0x39 or 0x41 <= b <= 0x5A or 0x61 <= b <= 0x7A


def _operator_byte(b: int) -> bool:
    return b >= 0x80 or b in OPERATOR_BYTES


def _scan(line: bytes, start: int, accept) -> int:
    end = start
    while end < len(line) and accept(line[end]):
        end += 1
    return end


def token_span(line: bytes, col0: int, operator: bool) -> tuple[int, int]:
    """0-based end-exclusive byte span of the token written at `col0`."""
    if col0 >= len(line):
        return col0, col0
    if line[col0] == BACKTICK:
        close = line.find(b"`", col0 + 1)
        if close > col0 + 1:
            return col0 + 1, close
    return col0, _scan(line, col0, _operator_byte if operator else _ident_byte)


# --- assembly ---------------------------------------------------------------


def _relative(path: str, root: str) -> str | None:
    real = os.path.realpath(path)
    if real != root and not real.startswith(root + os.sep):
        return None
    return os.path.relpath(real, root)


def collect_records(lib: ctypes.CDLL, store, root: str, stats: dict) -> dict[str, str]:
    """record name -> source path relative to `root` (first unit wins)."""
    records: dict[str, str] = {}
    units = unit_names(lib, store)
    stats["units"] = len(units)
    for unit in units:
        for record, path in unit_records(lib, store, unit):
            rel = _relative(path, root) if path else None
            if rel is None:
                stats["records_outside_root"] += 1
                continue
            records.setdefault(record, rel)
    stats["records"] = len(records)
    return records


def _key(o: dict) -> tuple:
    return (o["line"], o["col"], o["usr"], o["roles"], tuple((r["usr"], r["roles"]) for r in o["rel"]))


def _with_token(o: dict, lines: list[bytes]) -> dict:
    line = lines[o["line"] - 1] if 0 < o["line"] <= len(lines) else b""
    start, end = token_span(line, o["col"] - 1, o["subkind"] in OPERATOR_SUBKINDS)
    o["tok"] = [start, end]
    return o


def _source_lines(root: str, rel: str) -> list[bytes]:
    try:
        with open(os.path.join(root, rel), "rb") as fh:
            return fh.read().split(b"\n")
    except OSError:
        return []


def dump(store_path: str, source_root: str, lib_path: str = LIB_DEFAULT) -> dict:
    lib = load_library(lib_path)
    units_dir = os.path.join(store_path, f"v{lib.indexstore_format_version()}", "units")
    if not os.path.isdir(units_dir):
        raise RuntimeError(f"no index store at {store_path} ({units_dir} missing)")
    root = os.path.realpath(source_root)
    err = ctypes.c_void_p()
    store = _check(lib, lib.indexstore_store_create(store_path.encode(), ctypes.byref(err)), err, f"store {store_path}")
    stats = {"records_outside_root": 0, "occurrences_raw": 0}
    per_file: dict[str, dict[tuple, dict]] = {}
    try:
        for record, rel in sorted(collect_records(lib, store, root, stats).items()):
            seen = per_file.setdefault(rel, {})
            for o in record_occurrences(lib, store, record):
                stats["occurrences_raw"] += 1
                seen.setdefault(_key(o), o)
    finally:
        lib.indexstore_store_dispose(store)
    files = {}
    for rel in sorted(per_file):
        lines = _source_lines(root, rel)
        files[rel] = [_with_token(per_file[rel][k], lines) for k in sorted(per_file[rel])]
    stats["files"] = len(files)
    stats["occurrences"] = sum(len(v) for v in files.values())
    return {
        "format": 1,
        "indexstore_version": lib.indexstore_version(),
        "indexstore_format_version": lib.indexstore_format_version(),
        "source_root": root,
        "stats": stats,
        "files": files,
    }


def main(argv: list[str]) -> int:
    if len(argv) not in (4, 5):
        sys.stderr.write("usage: indexstore_dump.py <store_path> <source_root> <out.json> [libIndexStore.so]\n")
        return 2
    data = dump(argv[1], argv[2], argv[4] if len(argv) == 5 else LIB_DEFAULT)
    with open(argv[3], "w", encoding="utf-8") as fh:
        json.dump(data, fh, separators=(",", ":"), ensure_ascii=False)
        fh.write("\n")
    sys.stderr.write(f"indexstore_dump: {json.dumps(data['stats'], sort_keys=True)}\n")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
