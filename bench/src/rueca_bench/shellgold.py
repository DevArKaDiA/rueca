"""S3 gold for shell scripts: a code-backed call-graph labeler.

No compiler-grade indexer exists for shell (no SCIP indexer, no type
checker), so the bench derives the gold itself from tree-sitter-bash (and
tree-sitter-zsh for zsh files, `shelldialect.py`), with
rules written from bash's own semantics and independently of the shell
pack (`rueca-lang-shell`, `rueca-resolve` `shell`): the pack evaluates
`source` paths (the script's directory through `$(dirname "$0")`,
directory variables, unknown prefixes) and resolves names through import
edges; this labeler matches sourced paths by their literal tail only and
resolves names over files. Both read the same grammar.

- **Files**: every shell file of the repository (manifest `[languages.shell]`
  extensions; extension-less scripts are not in the bench's file set).
  Callables are the functions of in-scope, non-test files (the `shell`
  labeler of `treesitter.py`: every `function_definition`, named by its
  name).
- **Sources**: `source P` / `. P` (also `\\. P`) where the end of `P` is a
  literal path (`"$ROOT/lib/x.sh"` → `lib/x.sh`, `./a.sh` → `a.sh`; `.` and
  `..` dropped; the tail must start at a `/` or be the whole word, so
  `"$f"` and `"${name}.sh"` source nothing; `~/…` and absolute paths are
  outside the repository); the file whose path ends with
  that tail, when exactly one does. Transitive (breadth-first, depth ≤ 10).
- **Calls**: every command whose word is a literal name (`\\` stripped), in
  any position: pipelines, lists, subshells, `$(…)`, `<(…)`, conditions.
  Not: arguments of other commands (`command f`, `exec f`, `xargs f`,
  `trap f EXIT` — a callback — and `eval "f"`), dynamic words (`$cmd`).
  The caller is the innermost callable whose lines contain the command;
  file-level code produces no edge.
- **Targets** (bash looks functions up by name at run time): the
  definitions of the name in the caller's own file; else in the files it
  sources, when they are all in one file (definitions in several sourced
  files: no edge — which one runs depends on the order); else the
  definitions in the only non-test file of the repository that defines the
  name (a library calling a function its entry script provides); else none.
  Several definitions of one name in one file (conditional definitions) are
  one equivalence class.
"""

from __future__ import annotations

import re
from collections import defaultdict, deque
from pathlib import Path

from .common import is_test_path
from .match import UnionFind
from .scip import add_class_stats, fid, in_scope
from .treesitter import extract_functions, grammar_for, parse

MAX_DEPTH = 10
LANGUAGE = "shell"

# The literal end of a sourced path: a run of path characters after a `/`
# (or the whole word).
_TAIL = re.compile(r"(?:^|/)([A-Za-z0-9_.+@%,-]+(?:/[A-Za-z0-9_.+@%,-]+)*)$")
_NAME = re.compile(r"^[A-Za-z_][A-Za-z0-9_:.@+-]*$")


def is_shell_file(path: str) -> bool:
    kind = grammar_for(path)
    return kind is not None and kind == "bash"


def source_tail(raw: str) -> str | None:
    """The literal end of a sourced path as written (`None` when dynamic)."""
    text = raw.strip().strip("\"'")
    if not text or text.endswith(("}", ")", "`")) or text.startswith(("~", "/")):
        return None
    if "$" not in text and "`" not in text:
        tail = text
    else:
        m = _TAIL.search(text)
        if m is None or m.start(1) == 0:
            return None
        tail = m.group(1)
    segs = [s for s in tail.split("/") if s not in ("", ".", "..")]
    if not segs or "$" in tail or "*" in tail:
        return None
    return "/".join(segs)


def resolve_tail(tail: str, files: list[str]) -> str | None:
    """The only file whose path ends with `tail` (a whole path suffix)."""
    hits = [f for f in files if f == tail or f.endswith("/" + tail)]
    return hits[0] if len(hits) == 1 else None


def _text(node, source: bytes) -> str:
    return source[node.start_byte : node.end_byte].decode("utf-8", errors="replace")


def _command_word(cmd, source: bytes) -> tuple[str, object] | None:
    name = cmd.child_by_field_name("name")
    if name is None or name.named_child_count != 1 or name.named_children[0].type != "word":
        return None
    return _text(name, source).lstrip("\\"), name


def _commands(root):
    stack = [root]
    while stack:
        n = stack.pop()
        if n.type == "command":
            yield n
        stack.extend(reversed(n.children))


class _File:
    def __init__(self, path: str, source: bytes):
        self.path = path
        self.source = source
        self.tree = parse(path, source)
        self.funcs = [dict(f.as_dict(), path=path) for f in extract_functions(path, source)]
        self.defs: dict[str, list[dict]] = defaultdict(list)
        for f in self.funcs:
            self.defs[f["name"]].append(f)
        self.sources: list[str] = []
        self.calls: list[tuple[str, int]] = []  # (name, 1-based line)
        for cmd in _commands(self.tree.root_node):
            self._command(cmd)

    def _command(self, cmd) -> None:
        word = _command_word(cmd, self.source)
        if word is None:
            return
        name, node = word
        if name in (".", "source"):
            args = cmd.children_by_field_name("argument")
            tail = source_tail(_text(args[0], self.source)) if args else None
            if tail:
                self.sources.append(tail)
            return
        if _NAME.match(name) and "/" not in name:
            self.calls.append((name, node.start_point[0] + 1))

    def caller(self, line: int) -> dict | None:
        best = None
        for f in self.funcs:
            if f["start"] <= line <= f["end"] and (best is None or f["end"] - f["start"] < best["end"] - best["start"]):
                best = f
        return best


def _closure(start: str, sourced: dict[str, list[str]]) -> list[str]:
    seen = {start}
    out: list[str] = []
    level = deque([start])
    for _ in range(MAX_DEPTH):
        nxt: list[str] = []
        for f in level:
            for g in sourced.get(f, []):
                if g not in seen:
                    seen.add(g)
                    nxt.append(g)
        if not nxt:
            break
        out.extend(nxt)
        level = deque(nxt)
    return out


def shell_call_graph(root: Path, include: list[str], exclude: list[str] | None = None, paths: list[str] | None = None) -> dict:
    """The gold call graph of the shell files under `root` (module docs),
    in the shape of `scip.derive_call_graph`. `paths`: the repository's
    files (default: every file under `root`)."""
    exclude = exclude or []
    if paths is None:
        paths = sorted(str(p.relative_to(root)) for p in root.rglob("*") if p.is_file() and ".git" not in p.parts)
    shell_paths = [p for p in paths if is_shell_file(p) and (root / p).is_file()]
    files = {p: _File(p, (root / p).read_bytes()) for p in shell_paths}
    stats: dict[str, int] = defaultdict(int)
    stats["documents_total"] = len(files)
    sourced = {p: [t for t in (resolve_tail(tail, shell_paths) for tail in f.sources) if t and t != p] for p, f in files.items()}
    stats["sources"] = sum(len(f.sources) for f in files.values())
    stats["sources_resolved"] = sum(len(v) for v in sourced.values())
    repo_defs: dict[str, set[str]] = defaultdict(set)
    for p, f in files.items():
        if not is_test_path(p):
            for name in f.defs:
                repo_defs[name].add(p)
    scoped = {p for p in files if in_scope(p, include, exclude)}
    stats["documents_in_scope"] = len(scoped)
    callables = {fid(f["path"], f): f for p in sorted(scoped) for f in files[p].funcs}
    classes = UnionFind()
    for p in scoped:
        for defs in files[p].defs.values():
            for d in defs[1:]:
                classes.union(fid(p, defs[0]), fid(p, d))

    def targets(p: str, name: str) -> list[dict]:
        own = files[p].defs.get(name)
        if own:
            stats["targets_same_file"] += 1
            return own
        holders = [g for g in _closure(p, sourced) if name in files[g].defs]
        if len(holders) == 1:
            stats["targets_sourced"] += 1
            return files[holders[0]].defs[name]
        if len(holders) > 1:
            stats["targets_ambiguous"] += 1
            return []
        defs = repo_defs.get(name, set())
        if len(defs) == 1:
            stats["targets_unique_name"] += 1
            return files[next(iter(defs))].defs[name]
        return []

    callers: dict[str, set[str]] = defaultdict(set)
    callees: dict[str, set[str]] = defaultdict(set)
    for p in sorted(scoped):
        f = files[p]
        for name, line in f.calls:
            src = f.caller(line)
            if src is None:
                stats["call_sites_module_level"] += 1
                continue
            stats["call_sites"] += 1
            caller = fid(p, src)
            for t in targets(p, name):
                callee = fid(t["path"], t)
                if callee in callables:
                    callees[caller].add(callee)
                    callers[callee].add(caller)
    stats["edges"] = sum(len(v) for v in callees.values())
    graph = {
        "callables": callables,
        "callers": {k: sorted(v) for k, v in callers.items()},
        "callees": {k: sorted(v) for k, v in callees.items()},
        "classes": classes.groups(),
        "stats": dict(stats),
    }
    add_class_stats(graph)
    return graph


def version_string() -> str:
    import importlib.metadata as md

    return f"rueca-bench shell labeler (tree-sitter-bash {md.version('tree-sitter-bash')}, tree-sitter-zsh {md.version('tree-sitter-zsh')})"
