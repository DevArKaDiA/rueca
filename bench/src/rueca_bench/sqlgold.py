"""S3 gold for SQL: a code-backed dependency labeler built on sqlglot.

No compiler-grade cross-reference tool exists for SQL repositories (a
database resolves names at run time, against a search path the repository
does not state), so the bench derives the gold itself, independently of the
SQL pack (`rueca-lang-sql`, sqlparser-rs, and `rueca-resolve` `sql`): it
parses with sqlglot (MIT, pinned in pyproject.toml) and its own statement
chunking, and reads dbt Jinja with regular expressions. The dialect of a
file comes from the manifest (`dialects`: path prefix → sqlglot dialect)
or, in the micro-benchmark, from the snippet's dialect directory — never
from the pack's detection.

- **Callables** (the definitions questions are asked about): `CREATE
  TABLE` / `VIEW` / `MATERIALIZED VIEW` / `FUNCTION` / `PROCEDURE` /
  `TRIGGER` statements (sqlglot's `Create`, else a `CREATE … <kind> name`
  header when sqlglot cannot parse the statement), `-- name: X` annotated
  queries (sqlc, yesql), and in dbt projects every model (a Jinja `.sql`
  file under `models/`, named by its file stem) and `{% macro %}` /
  `{% test %}` block. Names follow the documented normalization: unquoted
  identifiers lower case, quoted ones kept in PostgreSQL / Snowflake /
  ANSI (folded in MySQL, SQLite, T-SQL, BigQuery; an all-upper-case
  quoted Snowflake name folded), a leading default schema dropped
  (`public`, `dbo`, `main`).
- **Edges** (callable → callable, any dependency): the tables sqlglot finds
  in a parsed statement (`exp.Table`, minus the statement's own name and
  its CTE names — FROM / JOIN / DML targets / foreign keys / trigger
  tables), user functions it calls (`exp.Anonymous`) and procedures it
  executes (`exp.Execute`); for routine bodies kept as text (PostgreSQL
  `$$`) and statements sqlglot cannot parse, the names after `FROM`,
  `JOIN`, `INTO`, `UPDATE`, `REFERENCES`, `ON` (trigger / index targets),
  `EXEC` / `CALL` / `PERFORM`, and `name(` calls of project routines. dbt:
  `ref('m')` → model `m`, a call of a project macro → that macro.
- **Targets**: a written name matches the callables with that normalized
  qualified name (else, when exactly one qualified name ends with the
  written segments or the other way round, that one); among several: the
  last one earlier in the same file, else one later in the same file, else
  the ones in the deepest directory shared with the referencing file when
  they are in one file; else no edge. File-level statements (inserts, data
  loads) produce no edge (no callable encloses them).
"""

from __future__ import annotations

import logging
import re
from collections import defaultdict
from contextlib import contextmanager
from contextvars import ContextVar
from pathlib import Path

import sqlglot
from sqlglot import exp
from sqlglot.errors import ErrorLevel
from sqlglot.tokens import TokenType

from .common import is_test_path
from .scip import add_class_stats, fid, in_scope
from .treesitter import Func

# sqlglot logs every statement it reads as a raw command.
logging.getLogger("sqlglot").setLevel(logging.ERROR)

LANGUAGE = "sql"
EXTENSIONS = (".sql", ".psql", ".pgsql", ".plpgsql", ".mysql", ".tsql", ".bq", ".pks", ".pkb", ".pck", ".pls", ".plsql", ".tps", ".tpb")

# Bench dialect names → sqlglot dialects.
SQLGLOT = {
    "postgres": "postgres",
    "mysql": "mysql",
    "sqlite": "sqlite",
    "tsql": "tsql",
    "bigquery": "bigquery",
    "snowflake": "snowflake",
    "dbt": "snowflake",
    "oracle": "oracle",
    # sqlglot has no DB2 dialect: read as ANSI, like the pack's parser.
    "db2": "",
    "ansi": "",
}
# Dialects whose unquoted identifiers fold to upper case: a quoted
# upper-case name is the unquoted one.
UPPER_FOLDING = {"snowflake", "dbt", "oracle", "db2"}
CASE_INSENSITIVE = {"mysql", "sqlite", "tsql", "bigquery"}
DEFAULT_SCHEMA = {"postgres": "public", "tsql": "dbo", "sqlite": "main"}

_CREATE = re.compile(
    r"^\s*CREATE\s+(?:OR\s+(?:REPLACE|ALTER)\s+)?(?:DEFINER\s*=\s*\S+\s+)?(?:SQL\s+SECURITY\s+\w+\s+)?"
    r"(?:(?:GLOBAL|LOCAL)\s+)?(?:(?:TEMP|TEMPORARY|UNLOGGED|MATERIALIZED|VIRTUAL|TRANSIENT|SECURE)\s+)*"
    r"(TABLE|VIEW|FUNCTION|PROCEDURE|PROC|TRIGGER)\s+(?:IF\s+NOT\s+EXISTS\s+)?"
    r"((?:[\w$#@]+|\"[^\"]+\"|`[^`]+`|\[[^\]]+\])(?:\s*\.\s*(?:[\w$#@]+|\"[^\"]+\"|`[^`]+`|\[[^\]]+\]))*)",
    re.IGNORECASE,
)
# `-- name: GetAuthor :one`, `/* name: X :many */`, yesql `-- name: get-x`,
# HugSQL `-- :name x :? :1`; not pg_dump's `-- Name: x; Type: TABLE`.
_NAMED = re.compile(r"(?:--|/\*)\s*(?:name:\s*|:name\s+)([\w-]+)[ \t]*(?=$|:|\*/)", re.IGNORECASE | re.MULTILINE)
_PART = r"(?:[\w$]+|\"[^\"]+\"|`[^`]+`|\[[^\]]+\])"
_REL = re.compile(r"\b(FROM|JOIN|INTO|UPDATE|REFERENCES|ON|EXEC|EXECUTE|CALL|PERFORM|TABLE)\s+(" + _PART + r"(?:\s*\.\s*" + _PART + r")*)", re.IGNORECASE)
_CALL = re.compile(r"(?<![\w.$])(" + _PART + r"(?:\.\w+)*)\s*\(")
_DOLLAR = re.compile(r"\$(\w*)\$(.*?)\$\1\$", re.DOTALL)
_JINJA = re.compile(r"\{\{.*?\}\}|\{%.*?%\}", re.DOTALL)
_MACRO = re.compile(r"\{%-?\s*(macro|test|materialization)\s+(\w+)")
_ENDMACRO = re.compile(r"\{%-?\s*end(?:macro|test|materialization)\s*-?%\}")
_REF = re.compile(r"\bref\(\s*['\"]([\w.-]+)['\"]\s*(?:,\s*['\"]([\w.-]+)['\"]\s*)?(?:,[^)]*)?\)")
_JCALL = re.compile(r"(?<![\w.])(?:(\w+)\.)?(\w+)\s*\(")
_KEYWORDS = {"select", "from", "where", "values", "on", "using", "set", "as", "join", "into", "table", "if", "not", "exists", "only", "lateral"}


# --- names ----------------------------------------------------------------------


def _unquote(part: str) -> tuple[str, bool]:
    p = part.strip()
    if len(p) >= 2 and (p[0], p[-1]) in (('"', '"'), ("`", "`"), ("[", "]")):
        return p[1:-1], True
    return p, False


def norm_part(text: str, quoted: bool, dialect: str) -> str:
    if not quoted or dialect in CASE_INSENSITIVE:
        return text.lower()
    if dialect in UPPER_FOLDING and text == text.upper():
        return text.lower()
    return text


def norm_name(parts: list[tuple[str, bool]], dialect: str) -> str:
    segs = [norm_part(t, q, dialect) for t, q in parts]
    schema = DEFAULT_SCHEMA.get(dialect)
    if len(segs) >= 2 and schema and segs[-2] == schema:
        del segs[-2]
    return ".".join(segs)


_CLOSE = {'"': '"', "`": "`", "[": "]"}


def _split_dotted(text: str) -> list[str]:
    """The parts of a dotted name, dots inside quotes kept."""
    parts, cur, close = [], [], None
    for ch in text:
        if close is None and ch == ".":
            parts.append("".join(cur).strip())
            cur = []
            continue
        if close is None and ch in _CLOSE:
            close = _CLOSE[ch]
        elif ch == close:
            close = None
        cur.append(ch)
    parts.append("".join(cur).strip())
    return parts


def written_name(text: str, dialect: str) -> str:
    """Normalized name of a dotted name as written (`[dbo].[T]`, `a.b`)."""
    return norm_name([_unquote(p) for p in _split_dotted(text)], dialect)


def _is_go(line: str) -> bool:
    """A T-SQL batch separator: `GO` alone on its line (optionally a count)."""
    words = line.split()
    return bool(words) and words[0].upper() == "GO" and (len(words) == 1 or (len(words) == 2 and words[1].isdigit()))


def _delimiter(line: str) -> str | None:
    """The new delimiter of a MySQL `DELIMITER x` line."""
    words = line.split()
    return words[1] if len(words) == 2 and words[0].upper() == "DELIMITER" else None


def table_name(t: exp.Table, dialect: str) -> str:
    parts = [p for p in (t.args.get("catalog"), t.args.get("db"), t.this) if isinstance(p, exp.Identifier)]
    return norm_name([(p.this, bool(p.quoted)) for p in parts], dialect)


# --- statements -----------------------------------------------------------------


class Stmt:
    def __init__(self, text: str, start: int, end: int, line: int, end_line: int, leading: str):
        self.text, self.start, self.end, self.line, self.end_line, self.leading = text, start, end, line, end_line, leading


_DB2_TERMINATOR = re.compile(r"^\s*--#\s*SET\s+TERMINATOR\s+(\S+)\s*$", re.IGNORECASE)
# An Oracle buffer that is one PL/SQL unit (it runs to its `/` line).
_PLSQL_UNIT = re.compile(
    r"\s*(?:DECLARE\b|BEGIN\b(?!\s*(?:TRANSACTION|WORK|;))|CREATE\s+(?:OR\s+REPLACE\s+)?(?:(?:NON)?EDITIONABLE\s+)?"
    r"(?:PROCEDURE|FUNCTION|PACKAGE|TRIGGER|TYPE\s+BODY)\b)",
    re.IGNORECASE,
)


def _batches(source: str, dialect: str = "") -> list[tuple[int, int, bool]]:
    """(start, end, split on `;`) regions between T-SQL `GO` lines, Oracle
    `/` lines, and MySQL `DELIMITER` / DB2 `--#SET TERMINATOR` regions (a
    custom delimiter ends each statement)."""
    out: list[tuple[int, int, bool]] = []
    pos, start, delim = 0, 0, None
    for line in source.splitlines(keepends=True):
        new = _delimiter(line)
        if not new and dialect == "db2" and (m := _DB2_TERMINATOR.match(line)):
            new = m.group(1)
        if new or _is_go(line) or (dialect == "oracle" and line.strip() == "/"):
            out.append((start, pos, delim is None))
            start = pos + len(line)
            if new:
                delim = None if new == ";" else new
        elif delim:
            for d in [i for i in range(len(line)) if line.startswith(delim, i)]:
                out.append((start, pos + d, False))
                start = pos + d + len(delim)
        pos += len(line)
    out.append((start, len(source), delim is None))
    return [b for b in out if source[b[0] : b[1]].strip()]


def _depth_step(tok, nxt, depth: int) -> int:
    t = tok.token_type
    word = tok.text.upper()
    nxt_word = nxt.text.upper() if nxt is not None else ""
    if word == "BEGIN" and nxt_word not in ("", ";", "TRANSACTION", "TRAN", "WORK", "IMMEDIATE", "DEFERRED", "EXCLUSIVE", "ISOLATION"):
        return depth + 1
    if t == TokenType.CASE:
        return depth + 1
    if word == "END" and nxt_word not in ("IF", "LOOP", "WHILE", "REPEAT", "FOR"):
        return max(0, depth - 1)
    return depth


def _token_chunks(source: str, start: int, end: int, dialect: str) -> list[tuple[int, int]]:
    text = source[start:end]
    try:
        toks = sqlglot.Dialect.get_or_raise(SQLGLOT.get(dialect, "") or None).tokenize(text)
    except Exception:
        return [(start, end)]
    out, cur, depth = [], None, 0
    for i, tok in enumerate(toks):
        if tok.token_type == TokenType.SEMICOLON and depth == 0:
            if cur:
                out.append(cur)
            cur = None
            continue
        depth = _depth_step(tok, toks[i + 1] if i + 1 < len(toks) else None, depth)
        a, b = start + tok.start, start + tok.end + 1
        cur = (cur[0], b) if cur else (a, b)
    if cur:
        out.append(cur)
    return out


def statements(source: str, dialect: str) -> list[Stmt]:
    out: list[Stmt] = []
    prev_end = 0
    for b_start, b_end, split in _batches(source, dialect):
        whole = not split or (dialect == "tsql" and re.match(r"\s*(CREATE|ALTER)\s+(OR\s+ALTER\s+)?(PROC|PROCEDURE|FUNCTION|TRIGGER|VIEW)\b", source[b_start:b_end], re.I))
        whole = whole or (dialect == "oracle" and bool(_PLSQL_UNIT.match(source[b_start:b_end])))
        chunks = [(b_start, b_end)] if whole else _token_chunks(source, b_start, b_end, dialect)
        if dialect == "oracle" and not whole:
            # A PL/SQL unit after other statements of its buffer runs to
            # the end of the buffer (its declarations end in `;` too).
            unit = next((i for i, (s, e) in enumerate(chunks) if _PLSQL_UNIT.match(source[s:e])), None)
            if unit is not None:
                chunks = chunks[:unit] + [(chunks[unit][0], b_end)]
        for s, e in chunks:
            text = source[s:e].strip()
            if not text:
                continue
            s += len(source[s:e]) - len(source[s:e].lstrip())
            line = source.count("\n", 0, s) + 1
            out.append(Stmt(text, s, s + len(text), line, line + text.count("\n"), source[prev_end:s]))
            prev_end = s + len(text)
    return out


# --- definitions ----------------------------------------------------------------


def _parse(text: str, dialect: str):
    try:
        e = sqlglot.parse_one(text, read=SQLGLOT.get(dialect, "") or None, error_level=ErrorLevel.RAISE)
    except Exception:
        return None
    return None if isinstance(e, (exp.Command, exp.Block)) else e


def _created(e, dialect: str) -> tuple[str, int] | None:
    """(normalized name, offset of the name in the statement) of a parsed
    `CREATE` of a callable kind."""
    if not isinstance(e, exp.Create) or str(e.args.get("kind", "")).upper() not in ("TABLE", "VIEW", "FUNCTION", "PROCEDURE", "TRIGGER"):
        return None
    target = e.this.this if isinstance(e.this, (exp.UserDefinedFunction, exp.StoredProcedure, exp.Schema)) else e.this
    if isinstance(target, exp.Table):
        ident = target.this
        return table_name(target, dialect), ident.meta.get("start", 0) if isinstance(ident, exp.Identifier) else 0
    if isinstance(target, exp.Identifier):
        return norm_part(target.this, bool(target.quoted), dialect), target.meta.get("start", 0)
    return None


def _header(text: str, dialect: str) -> tuple[str, int] | None:
    m = _CREATE.match(text)
    if not m:
        return None
    return written_name(m.group(2), dialect), m.start(2) + len(m.group(2)) - len(m.group(2).split(".")[-1].strip())


_TRIGGER = re.compile(r"\s*CREATE\s+(?:OR\s+\w+\s+)?(?:\w+\s+)*?TRIGGER\b", re.IGNORECASE)
_ROUTINE = re.compile(r"\s*CREATE\s+(?:OR\s+REPLACE\s+)?(?:(?:NON)?EDITIONABLE\s+)?(?:FUNCTION|PROCEDURE|TRIGGER)\b", re.IGNORECASE)
_ON = re.compile(r"\bON\s+(" + _PART + r"(?:\s*\.\s*" + _PART + r")*)", re.IGNORECASE)


def _trigger_qualified(text: str, name: str, dialect: str) -> str:
    """Triggers are named per table (PostgreSQL): `table.trigger`."""
    if not _TRIGGER.match(text):
        return name
    m = _ON.search(text)
    return f"{written_name(m.group(1), dialect)}.{name.rsplit('.', 1)[-1]}" if m else name


def _named(leading: str) -> str | None:
    m = None
    for m in _NAMED.finditer(leading):
        pass
    if m is None or leading[m.end() :].strip("\n *-/\t:").count("\n") > 1:
        return None
    return m.group(1).replace("-", "_")


def _callable(path: str, name: str, st: Stmt, name_offset: int, source: str) -> dict:
    short = name.rsplit(".", 1)[-1]
    def_line = source.count("\n", 0, st.start + name_offset) + 1
    f = Func(name=short, qualname=name, label="Function", start=st.line, end=st.end_line, def_line=def_line, outer_start=st.line, indent=0)
    return dict(f.as_dict(), path=path)


class SqlFile:
    """Definitions and raw references of one SQL file."""

    def __init__(self, path: str, source: str, dialect: str):
        self.path, self.source, self.dialect = path, source, dialect
        self.funcs: list[dict] = []
        self.refs: list[tuple[int, str, str]] = []  # (callable index, kind, normalized name)
        if "{{" in source or "{%" in source:
            self._dbt()
        else:
            for st in statements(source, dialect):
                self._statement(st)

    def _statement(self, st: Stmt) -> None:
        named = _named(st.leading)
        e = _parse(st.text, self.dialect)
        created = _created(e, self.dialect) if e is not None else _header(st.text, self.dialect)
        if named:
            self.funcs.append(_callable(self.path, named, st, 0, self.source))
            self.funcs[-1]["def_line"] = st.line - 1
        elif created:
            self.funcs.append(_callable(self.path, _trigger_qualified(st.text, created[0], self.dialect), st, created[1], self.source))
        else:
            return
        own = None if named else created[0]
        idx = len(self.funcs) - 1
        names = _parsed_refs(e, own, self.dialect) if e is not None else _text_refs(st.text, own, self.dialect)
        names += _body_refs(st.text, self.dialect)
        if self.dialect in ("oracle", "db2") and _ROUTINE.match(st.text):
            # PL/SQL and SQL PL bodies: sqlglot keeps them as text.
            names += [n for n in _text_refs(st.text, own, self.dialect) if n not in names]
            # `BEFORE UPDATE ON t`: the table after the event list.
            if _TRIGGER.match(st.text) and (m := _ON.search(st.text)):
                names.append(("relation", written_name(m.group(1), self.dialect)))
        self.refs += [(idx, k, n) for k, n in names]

    def _dbt(self) -> None:
        parts = self.path.split("/")
        for m in _MACRO.finditer(self.source):
            end = _ENDMACRO.search(self.source, m.end())
            stop = end.end() if end else len(self.source)
            self._dbt_callable(m.group(2), m.start(2), m.start(), stop)
        if "models" in parts[:-1] and "macros" not in parts[:-1] and not _MACRO.search(self.source):
            stem = parts[-1].rsplit(".", 1)[0].lower()
            self._dbt_callable(stem, 0, 0, len(self.source))

    def _dbt_callable(self, name: str, at: int, start: int, stop: int) -> None:
        line = self.source.count("\n", 0, start) + 1
        end_line = self.source.count("\n", 0, max(start, stop - 1)) + 1
        f = Func(name=name, qualname=name, label="Function", start=line, end=end_line, def_line=self.source.count("\n", 0, at) + 1, outer_start=line, indent=0)
        self.funcs.append(dict(f.as_dict(), path=self.path))
        idx = len(self.funcs) - 1
        body = self.source[start:stop]
        if at == 0 and start == 0:
            body = _ENDMACRO.sub("", _strip_macros(body))
        for tag in _JINJA.findall(body):
            for m in _REF.finditer(tag):
                self.refs.append((idx, "relation", (m.group(2) or m.group(1)).lower()))
            for m in _JCALL.finditer(tag):
                if m.group(2) not in ("ref", "source", "var", "env_var", "config", "macro", "test"):
                    self.refs.append((idx, "macro", m.group(2)))


def _strip_macros(text: str) -> str:
    out, pos = [], 0
    for m in _MACRO.finditer(text):
        out.append(text[pos : m.start()])
        end = _ENDMACRO.search(text, m.end())
        pos = end.end() if end else len(text)
    out.append(text[pos:])
    return "".join(out)


def _parsed_refs(e, own: str | None, dialect: str) -> list[tuple[str, str]]:
    ctes = {c.alias_or_name.lower() for c in e.find_all(exp.CTE)}
    out: list[tuple[str, str]] = []
    skipped_own = False
    for t in e.find_all(exp.Table):
        name = table_name(t, dialect)
        if not name:
            continue
        if name == own and not skipped_own and isinstance(e, exp.Create):
            skipped_own = True
            continue
        kind = "routine" if isinstance(t.parent, exp.Execute) else "relation"
        if kind == "relation" and "." not in name and name.lower() in ctes:
            continue
        out.append((kind, name))
    out += [("routine", f.name.lower()) for f in e.find_all(exp.Anonymous) if f.name]
    return out


def _text_refs(text: str, own: str | None, dialect: str) -> list[tuple[str, str]]:
    out: list[tuple[str, str]] = []
    header = _CREATE.match(text)
    body = text[header.end() :] if header else text
    for m in _REL.finditer(body):
        name = written_name(m.group(2), dialect)
        if name.split(".")[-1] in _KEYWORDS or name == own:
            continue
        kind = "routine" if m.group(1).upper() in ("EXEC", "EXECUTE", "CALL", "PERFORM") else "relation"
        out.append((kind, name))
    out += [("routine", written_name(m.group(1), dialect)) for m in _CALL.finditer(body)]
    return out


def _body_refs(text: str, dialect: str) -> list[tuple[str, str]]:
    """References in `$$ … $$` routine bodies (sqlglot keeps them as text)."""
    out: list[tuple[str, str]] = []
    for m in _DOLLAR.finditer(text):
        out += _text_refs(m.group(2), None, dialect)
    return out


# --- the graph ------------------------------------------------------------------


def dialect_for(path: str, dialects: dict[str, str]) -> str:
    """The dialect the manifest assigns to the longest matching prefix."""
    best = ""
    for prefix in dialects:
        if path.startswith(prefix) and len(prefix) > len(best):
            best = prefix
    return dialects.get(best, "ansi")


def is_sql_file(path: str) -> bool:
    return path.endswith(EXTENSIONS)


def _shared_dirs(a: str, b: str) -> int:
    da, db = a.split("/")[:-1], b.split("/")[:-1]
    n = 0
    for x, y in zip(da, db):
        if x != y:
            break
        n += 1
    return n


class _Index:
    def __init__(self, funcs: list[dict]):
        self.by_name: dict[str, list[dict]] = defaultdict(list)
        self.by_last: dict[str, list[dict]] = defaultdict(list)
        for f in funcs:
            self.by_name[f["qualname"]].append(f)
            self.by_last[f["qualname"].rsplit(".", 1)[-1]].append(f)

    def candidates(self, name: str) -> list[dict]:
        if name in self.by_name:
            return self.by_name[name]
        written = name.split(".")
        loose = [f for f in self.by_last.get(written[-1], []) if _suffix_compatible(written, f["qualname"].split("."))]
        return loose if len({f["qualname"] for f in loose}) == 1 else []

    def pick(self, src: dict, name: str) -> dict | None:
        cands = self.candidates(name)
        here = [c for c in cands if c["path"] == src["path"]]
        before = [c for c in here if c["start"] <= src["start"]]
        if before:
            return max(before, key=lambda c: c["start"])
        if here:
            return here[0]
        if not cands:
            return None
        best = max(_shared_dirs(src["path"], c["path"]) for c in cands)
        near = [c for c in cands if _shared_dirs(src["path"], c["path"]) == best]
        return near[0] if len({c["path"] for c in near}) == 1 else None


def _suffix_compatible(a: list[str], b: list[str]) -> bool:
    n = min(len(a), len(b))
    return a[len(a) - n :] == b[len(b) - n :]


def sql_call_graph(root: Path, include: list[str], exclude: list[str] | None = None, files: list[str] | None = None, dialects: dict[str, str] | None = None) -> dict:
    """The dependency graph of the SQL files under `root` (module docs);
    callables are those of in-scope, non-test files, every file is a
    target."""
    exclude = exclude or []
    dialects = dialects or {}
    paths = sorted(files if files is not None else [str(p.relative_to(root)) for p in root.rglob("*") if p.is_file()])
    parsed = [SqlFile(p, (root / p).read_text(errors="replace"), dialect_for(p, dialects)) for p in paths if is_sql_file(p) and (root / p).stat().st_size <= 2 * 1024 * 1024]
    index = _Index([f for sf in parsed for f in sf.funcs])
    callables, callees = {}, defaultdict(set)
    for sf in parsed:
        keep = in_scope(sf.path, include, exclude) and not is_test_path(sf.path)
        for idx, _kind, name in sf.refs:
            src = sf.funcs[idx]
            dst = index.pick(src, name)
            if keep and dst is not None and dst is not src:
                callees[fid(src["path"], src)].add(fid(dst["path"], dst))
        if keep:
            callables.update({fid(f["path"], f): f for f in sf.funcs})
    callees = {k: v for k, v in callees.items() if k in callables}
    callers: dict[str, set[str]] = defaultdict(set)
    for s, ds in callees.items():
        for d in ds:
            callers[d].add(s)
    graph = {
        "callables": callables,
        "callers": {k: sorted(v) for k, v in sorted(callers.items()) if k in callables},
        "callees": {k: sorted(v) for k, v in sorted(callees.items())},
        "stats": {"files": len(parsed), "edges": sum(len(v) for v in callees.values())},
    }
    add_class_stats(graph)
    return graph


# The manifest `dialects` (path prefix -> dialect) of the S1 repository at
# hand: `sql_functions` has no repository argument (it is called through
# `treesitter.extract_functions`, like every other labeler), so the S1 gold
# and the BM25 arm set it around their work on one instance.
_FILE_DIALECTS: ContextVar[dict[str, str] | None] = ContextVar("sql_file_dialects", default=None)


@contextmanager
def file_dialects(dialects: dict[str, str] | None):
    token = _FILE_DIALECTS.set(dict(dialects or {}))
    try:
        yield
    finally:
        _FILE_DIALECTS.reset(token)


def sql_functions(path: str, source: bytes, dialect: str | None = None) -> list[Func]:
    """Callables of one file (the micro-benchmark labels; S1 function gold
    with the dialect `file_dialects` gives the path, `ansi` without one)."""
    dialect = dialect or dialect_for(path, _FILE_DIALECTS.get() or {})
    sf = SqlFile(path, source.decode("utf-8", errors="replace"), dialect)
    keys = ("name", "qualname", "label", "start", "end", "def_line", "outer_start", "indent")
    return [Func(**{k: f[k] for k in keys}) for f in sf.funcs]


def version_string() -> str:
    return f"rueca-bench SQL labeler (sqlglot {sqlglot.__version__})"
