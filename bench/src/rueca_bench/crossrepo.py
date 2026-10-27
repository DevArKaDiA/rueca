"""Cross-repository edges (RFC 0006 §8, `crossrepo.py`): precision and recall
of the organization link layer (packages) on public repositories that depend
on each other.

**Pairs** ([`GROUPS`]): a consumer pinned with the libraries it depends on,
each library at a release the consumer's manifest accepts — Flask 3.1.3 with
Click 8.1.8 and Jinja 3.1.6 (Python), Laravel (the S1 organization commit)
with Carbon, Monolog 3.9.0 and league/commonmark 2.8.3 (PHP), Apache Dubbo
and Apollo (S1 commits) with Gson 2.15.0 (Java). Each group is one
organization (`rueca index --org`); `rueca links --org` lists its
cross-repository edges.

**Gold**, labelled from the consumer's own source with an independent,
per-language labeler (Python `ast`; PHP and Java by regular expressions over
`use` / `import` statements, the library's Composer PSR-4 map or package
directories):

- *imports* — (consumer file, library file) for every import statement of a
  library module or class: Python `import m` / `from m import x` (the
  module's file, plus `m.x` when that is a submodule), PHP top-level `use`
  of a library class (its PSR-4 file), Java single-type and static imports
  (the class's file). Compared with the arm's `IMPORTS` edges between files.
- *uses* — (consumer file, library symbol) for every library class or
  function the file imports by name **and uses** outside its import lines,
  plus Python attribute chains through an imported library module
  (`click.echo` → `click.utils.echo`, following the library's re-exports),
  and PHP fully qualified names of library classes. An arm's non-`IMPORTS`
  edge from a node of that file is **correct** when its target is that
  symbol, a member of it, or a member of one of its library ancestors
  (base classes, PHP traits and interfaces, as the labeler reads them); a
  gold use is **found** when some edge of the file hits it that way.
  Library ancestors of the consumer's own classes that the file names
  (`Illuminate\\Support\\Carbon extends Carbon\\Carbon`, Flask's
  `AppGroup(click.Group)`) are accepted targets too (not gold uses).

Precision = correct / predicted, recall = found / gold, pooled over the
pairs, per kind and overall. Edges whose target the labeler cannot see —
a method of an object a library call returned (`ctx = get_context();
ctx.exit()`), a library class used without an import — count against
precision, which is therefore a lower bound.

Raw outputs: .cache/runs/org/crossrepo-rueca.json.
"""

from __future__ import annotations

import ast
import json
import re
import shutil
import traceback
from collections.abc import Callable, Iterator
from pathlib import Path
from typing import NamedTuple

from .arms import make_arm
from .common import CACHE, log, read_json, run, write_json
from .corpus import add_worktree, remove_worktree
from .org import run_path, write_org_file

# consumer, its commit, and the libraries (repo, commit, language) it imports
GROUPS = [
    {
        "consumer": "pallets/flask",
        "commit": "22d924701a6ae2e4cd01e9a15bbaf3946094af65",  # 3.1.3
        "language": "python",
        "libraries": [
            {"repo": "pallets/click", "commit": "934813e4d421071a1b3db3973c02fe2721359a6e", "packages": ["click"]},  # 8.1.8
            {"repo": "pallets/jinja", "commit": "15206881c006c79667fe5154fe80c01c65410679", "packages": ["jinja2"]},  # 3.1.6
        ],
    },
    {
        "consumer": "laravel/framework",
        "commit": "b8ca298a5475497d01b194ed5688441454d8ee34",  # S1 organization commit (11.x)
        "language": "php",
        "libraries": [
            {"repo": "briannesbitt/carbon", "commit": "d481d8d69a94dc00e5c1b147ec1f7e7492626b12"},
            {"repo": "Seldaek/monolog", "commit": "10d85740180ecba7896c87e06a166e0c95a0e3b6"},  # 3.9.0
            {"repo": "thephpleague/commonmark", "commit": "1902f60f984235023acbe03db6ad614a37b3c3e7"},  # 2.8.3
        ],
    },
    {
        "consumer": "apache/dubbo",
        "commit": "d8e6dfa42a22e9c21697ad37709abec7ae33895b",
        "language": "java",
        "libraries": [{"repo": "google/gson", "commit": "fcd416909db2bea96eb916bcfb2d0a83cb9b86c3"}],  # 2.15.0
    },
    {
        "consumer": "apolloconfig/apollo",
        "commit": "63c16c117a869426f2cbbfdaa099de449789fd18",
        "language": "java",
        "libraries": [{"repo": "google/gson", "commit": "fcd416909db2bea96eb916bcfb2d0a83cb9b86c3"}],  # 2.15.0
    },
]

SKIP_DIRS = {".git", "node_modules", "vendor", ".venv", "venv", "__pycache__", "build", "dist", "out", "site-packages", "bower_components", "coverage", ".tox"}


def _files(root: Path, ext: str) -> list[str]:
    out = []
    for p in root.rglob(f"*{ext}"):
        rel = p.relative_to(root)
        if any(part in SKIP_DIRS for part in rel.parts[:-1]) or not p.is_file():
            continue
        out.append(rel.as_posix())
    return sorted(out)


def _read(root: Path, rel: str) -> str:
    try:
        return (root / rel).read_text(errors="replace")
    except OSError:
        return ""


# --- Python ------------------------------------------------------------------------------


class PyLib:
    """A library's modules (dotted name → file) and definitions."""

    def __init__(self, root: Path, packages: list[str]):
        self.root = root
        self.packages = set(packages)
        self.modules: dict[str, str] = {}
        for rel in _files(root, ".py"):
            parts = rel.split("/")
            for base in (0, 1):  # `pkg/...` or `src/pkg/...`
                if len(parts) > base and parts[base] in self.packages and (base == 0 or parts[0] == "src"):
                    mod = parts[base:]
                    mod[-1] = mod[-1][:-3]
                    if mod[-1] == "__init__":
                        mod = mod[:-1]
                    self.modules[".".join(mod)] = rel
        self._trees: dict[str, ast.Module | None] = {}

    def tree(self, module: str) -> ast.Module | None:
        if module not in self._trees:
            rel = self.modules.get(module)
            try:
                self._trees[module] = ast.parse(_read(self.root, rel)) if rel else None
            except SyntaxError:
                self._trees[module] = None
        return self._trees[module]

    def resolve(self, module: str, name: str, depth: int = 0) -> str | None:
        """Qualified name of `name` as seen from `module`: a top-level
        definition there, else followed through its imports (re-exports)."""
        if depth > 8:
            return None
        tree = self.tree(module)
        if tree is None:
            return None
        for node in tree.body:
            if isinstance(node, (ast.ClassDef, ast.FunctionDef, ast.AsyncFunctionDef)) and node.name == name:
                return f"{module}.{name}"
            if isinstance(node, (ast.Assign, ast.AnnAssign)):
                targets = node.targets if isinstance(node, ast.Assign) else [node.target]
                if any(isinstance(t, ast.Name) and t.id == name for t in targets):
                    return f"{module}.{name}"
        for node in ast.walk(tree):
            if isinstance(node, ast.ImportFrom):
                src = self._abs(module, node)
                for a in node.names:
                    if (a.asname or a.name) == name and src is not None:
                        if f"{src}.{a.name}" in self.modules:
                            return None  # a submodule, not a definition
                        return self.resolve(src, a.name, depth + 1)
        return None

    def _abs(self, module: str, node: ast.ImportFrom) -> str | None:
        if node.level == 0:
            return node.module
        is_pkg = self.modules.get(module, "").endswith("__init__.py")
        parts = module.split(".")
        base = parts if is_pkg else parts[:-1]
        base = base[: len(base) - (node.level - 1)] if node.level > 1 else base
        return ".".join(base + ([node.module] if node.module else []))

    def bases(self, qn: str, depth: int = 0) -> set[str]:
        """Library base classes of class `qn` (transitively)."""
        out: set[str] = set()
        if depth > 8 or "." not in qn:
            return out
        module, name = qn.rsplit(".", 1)
        tree = self.tree(module)
        if tree is None:
            return out
        for node in tree.body:
            if isinstance(node, ast.ClassDef) and node.name == name:
                for b in node.bases:
                    dotted = _dotted(b)
                    if not dotted:
                        continue
                    head, _, rest = dotted.partition(".")
                    target = self.resolve(module, head) if not rest else None
                    if target is None and rest:
                        target = self.resolve(module, dotted.rsplit(".", 1)[1])
                    if target:
                        out.add(target)
                        out |= self.bases(target, depth + 1)
        return out


def _dotted(node: ast.AST) -> str | None:
    if isinstance(node, ast.Name):
        return node.id
    if isinstance(node, ast.Attribute):
        inner = _dotted(node.value)
        return f"{inner}.{node.attr}" if inner else None
    return None


def label_python(consumer: Path, libs: list[tuple[str, PyLib]]) -> tuple[set, set, dict, dict]:
    """(imports, uses, ancestors, inherited): imports {(file, lib, lib file)},
    uses {(file, lib, qualified name)}, ancestors {(lib, qn): {base qns}},
    inherited {(file, lib): {qns}} = library ancestors of consumer classes
    the file names (accepted targets for precision, not gold uses)."""
    imports: set = set()
    uses: set = set()
    lib_of = dict(libs)
    texts: dict[str, str] = {}
    class_lib: dict[str, set] = {}  # consumer class name -> {(lib, qn)} of library bases
    class_bases: dict[str, set] = {}  # consumer class name -> names of its other bases
    for rel in _files(consumer, ".py"):
        try:
            tree = ast.parse(_read(consumer, rel))
        except SyntaxError:
            continue
        modules: dict[str, tuple[str, str]] = {}  # local name -> (lib, module)
        symbols: dict[str, tuple[str, str]] = {}  # local name -> (lib, qn)
        import_lines: set[int] = set()
        for node in ast.walk(tree):
            if isinstance(node, ast.Import):
                for a in node.names:
                    for lname, lib in libs:
                        if a.name.split(".")[0] in lib.packages and a.name in lib.modules:
                            imports.add((rel, lname, lib.modules[a.name]))
                            import_lines.add(node.lineno)
                            if a.asname:
                                modules[a.asname] = (lname, a.name)
                            else:
                                top = a.name.split(".")[0]
                                if top in lib.modules:
                                    modules[top] = (lname, top)
            elif isinstance(node, ast.ImportFrom) and node.level == 0 and node.module:
                for lname, lib in libs:
                    if node.module.split(".")[0] not in lib.packages or node.module not in lib.modules:
                        continue
                    imports.add((rel, lname, lib.modules[node.module]))
                    import_lines.add(node.lineno)
                    for a in node.names:
                        sub = f"{node.module}.{a.name}"
                        if sub in lib.modules:
                            imports.add((rel, lname, lib.modules[sub]))
                            modules[a.asname or a.name] = (lname, sub)
                        elif a.name != "*":
                            qn = lib.resolve(node.module, a.name)
                            if qn:
                                symbols[a.asname or a.name] = (lname, qn)
        texts[rel] = _read(consumer, rel)
        for node in tree.body:
            if isinstance(node, ast.ClassDef):
                for b in node.bases:
                    dotted = _dotted(b) or ""
                    head, _, rest = dotted.partition(".")
                    if not rest and head in symbols:
                        lname, qn = symbols[head]
                        class_lib.setdefault(node.name, set()).add((lname, qn))
                    elif rest and head in modules:
                        lname, mod = modules[head]
                        qn = lib_of[lname].resolve(mod, rest.rsplit(".", 1)[-1]) if "." not in rest else None
                        if qn:
                            class_lib.setdefault(node.name, set()).add((lname, qn))
                    elif dotted:
                        class_bases.setdefault(node.name, set()).add(dotted.rsplit(".", 1)[-1])
        for node in ast.walk(tree):
            if getattr(node, "lineno", None) in import_lines:
                continue
            if isinstance(node, ast.Name) and node.id in symbols:
                lname, qn = symbols[node.id]
                uses.add((rel, lname, qn))
            elif isinstance(node, ast.Attribute):
                dotted = _dotted(node)
                if not dotted:
                    continue
                head, *rest = dotted.split(".")
                if head not in modules or not rest:
                    continue
                lname, mod = modules[head]
                lib = lib_of[lname]
                for part in rest:
                    if f"{mod}.{part}" in lib.modules:
                        mod = f"{mod}.{part}"
                        continue
                    qn = lib.resolve(mod, part)
                    if qn:
                        uses.add((rel, lname, qn))
                    break
    ancestors = {(lname, qn): lib_of[lname].bases(qn) for _, lname, qn in uses}
    # Library ancestors of consumer classes, through consumer base classes.
    for _ in range(8):
        for name, bases in class_bases.items():
            for b in bases:
                class_lib.setdefault(name, set()).update(class_lib.get(b, set()))
    inherited: dict[tuple, set] = {}
    for rel, text in texts.items():
        for name, targets in class_lib.items():
            if targets and re.search(rf"\b{re.escape(name)}\b", text):
                for lname, qn in targets:
                    inherited.setdefault((rel, lname), set()).update({qn} | lib_of[lname].bases(qn))
    return imports, uses, ancestors, inherited


# --- PHP -----------------------------------------------------------------------------------

_PHP_NS = re.compile(r"^\s*namespace\s+([\w\\]+)\s*[;{]", re.M)
_PHP_FQN = re.compile(r"\\?\b([A-Z]\w*(?:\\[A-Z]\w*)+)\b")
# Statements are found in two steps, a pattern anchored on the keyword and a
# check of what precedes it on its line, so that no pattern has nested or
# overlapping quantifiers (linear time). The matches are the same as those of
# the one-regex forms quoted in each docstring.
_PHP_USE = re.compile(r"^use(\s[^;]*);", re.M)
_PHP_USE_SKIP = re.compile(r"(?:function|const)\b")
_PHP_DECL = re.compile(r"\b(class|interface|trait|enum)\s+(\w+)")
_PHP_MODIFIERS = frozenset({"abstract", "final", "readonly"})
_PHP_TRAIT_USE = re.compile(r"\buse\s([\w\\,\s]+);")


class _PhpDecl(NamedTuple):
    """`kind name header {`: `start` is the keyword's offset, `end` follows the `{`."""

    kind: str
    name: str
    header: str
    start: int
    end: int


def _line_prefix(text: str, pos: int) -> str:
    return text[text.rfind("\n", 0, pos) + 1 : pos]


def _search_all(pattern: re.Pattern[str], text: str, accept: Callable[[re.Match[str]], bool]) -> Iterator[re.Match[str]]:
    """`pattern.finditer(text)` keeping the matches `accept` takes; a rejected
    match does not consume its text (the search resumes one character on)."""
    pos = 0
    while m := pattern.search(text, pos):
        if accept(m):
            yield m
            pos = m.end()
        else:
            pos = m.start() + 1


def _php_decls(text: str) -> Iterator[_PhpDecl]:
    r"""Class-like declarations, as `^\s*(?:(?:abstract|final|readonly)\s+)*
    (class|interface|trait|enum)\s+(\w+)([^{]*)\{` (multiline) found them:
    the keyword preceded on its line only by whitespace and modifiers."""
    pos = 0
    while m := _PHP_DECL.search(text, pos):
        brace = text.find("{", m.end())
        if brace < 0:
            return
        if set(_line_prefix(text, m.start()).split()) <= _PHP_MODIFIERS:
            yield _PhpDecl(m.group(1), m.group(2), text[m.end() : brace], m.start(), brace + 1)
            pos = brace + 1
        else:
            pos = m.start() + 1


def _php_class_use(m: re.Match[str]) -> bool:
    r"""A top-level `use` of classes, as `^use\s+(?!function\b|const\b)([^;]+);`
    accepted it: not `use function` / `use const` — unless two or more
    whitespace characters follow `use`, where that lookahead passed by
    backtracking onto the last one."""
    rest = m.group(1)
    tail = rest.lstrip()
    if len(rest) - len(tail) >= 2:
        return True
    return bool(tail) and not _PHP_USE_SKIP.match(tail)


def _php_trait_uses(body: str) -> list[str]:
    r"""Names in the `use A, B;` statements (traits) of a class body, as
    `^\s*use\s+([\w\\,\s]+);` (multiline) found them: `use` first on its line."""
    out: list[str] = []
    for m in _search_all(_PHP_TRAIT_USE, body, lambda m: not _line_prefix(body, m.start()).strip()):
        out.extend(x.strip() for x in m.group(1).split(",") if x.strip())
    return out


def _php_uses(text: str) -> tuple[dict[str, str], set[int]]:
    """alias → FQN of top-level class `use` statements, and their line numbers."""
    first = next(_php_decls(text), None)
    head = text[: first.start] if first else text
    out: dict[str, str] = {}
    lines: set[int] = set()
    for m in _search_all(_PHP_USE, head, _php_class_use):
        lines.add(head.count("\n", 0, m.start()) + 1)
        body = " ".join(m.group(1).split())
        if "{" in body:
            prefix, inner = body.split("{", 1)
            prefix = prefix.strip().rstrip("\\")
            items = [f"{prefix}\\{x.strip()}" for x in inner.rstrip("}").split(",") if x.strip()]
        else:
            items = [x.strip() for x in body.split(",")]
        for it in items:
            # `body` is whitespace-normalized: single spaces around `as`
            parts = re.split(r" as ", it.strip().lstrip("\\"), flags=re.I)
            fqn = parts[0].strip()
            alias = parts[1].strip() if len(parts) > 1 else fqn.rsplit("\\", 1)[-1]
            out[alias] = fqn
    return out, lines


def _php_resolve(name: str, ns: str, uses: dict[str, str]) -> str:
    if name.startswith("\\"):
        return name[1:]
    head, _, rest = name.partition("\\")
    if head in uses:
        return uses[head] + (f"\\{rest}" if rest else "")
    return f"{ns}\\{name}" if ns else name


class PhpLib:
    def __init__(self, root: Path):
        self.root = root
        comp = json.loads(_read(root, "composer.json") or "{}")
        self.psr4: dict[str, list[str]] = {}
        for prefix, dirs in ((comp.get("autoload") or {}).get("psr-4") or {}).items():
            self.psr4[prefix.rstrip("\\")] = [d.rstrip("/") for d in ([dirs] if isinstance(dirs, str) else dirs)]
        self.classes: dict[str, str] = {}  # FQN -> file
        self.parents: dict[str, set[str]] = {}
        for rel in _files(root, ".php"):
            text = _read(root, rel)
            m = _PHP_NS.search(text)
            ns = m.group(1) if m else ""
            uses, _ = _php_uses(text)
            for d in _php_decls(text):
                fqn = f"{ns}\\{d.name}" if ns else d.name
                if not self._autoloaded(fqn, rel):
                    continue
                self.classes[fqn] = rel
                parents = set()
                for kw in ("extends", "implements"):
                    mm = re.search(rf"\b{kw}\s+([\w\\,\s]+)", d.header)
                    if mm:
                        parents |= {_php_resolve(x.strip(), ns, uses) for x in mm.group(1).split(",") if x.strip()}
                parents |= {_php_resolve(x, ns, uses) for x in _php_trait_uses(text[d.end : d.end + 4000])}
                self.parents[fqn] = parents

    def _autoloaded(self, fqn: str, rel: str) -> bool:
        for prefix, dirs in self.psr4.items():
            if fqn == prefix or fqn.startswith(prefix + "\\"):
                tail = fqn[len(prefix) :].lstrip("\\").replace("\\", "/") + ".php"
                if any(rel == f"{d}/{tail}".lstrip("/") for d in dirs):
                    return True
        return False

    def ancestors(self, fqn: str) -> set[str]:
        out: set[str] = set()
        todo = [fqn]
        while todo:
            for p in self.parents.get(todo.pop(), ()):
                if p in self.classes and p not in out:
                    out.add(p)
                    todo.append(p)
        return out


def label_php(consumer: Path, libs: list[tuple[str, PhpLib]]) -> tuple[set, set, dict, dict]:
    imports: set = set()
    uses: set = set()
    texts: dict[str, str] = {}
    cparents: dict[str, set[str]] = {}  # consumer class FQN -> parents (FQN)
    for rel in _files(consumer, ".php"):
        text = _read(consumer, rel)
        texts[rel] = text
        m = _PHP_NS.search(text)
        ns = m.group(1) if m else ""
        file_uses, _ = _php_uses(text)
        for d in _php_decls(text):
            parents = set()
            for kw in ("extends", "implements"):
                mm = re.search(rf"\b{kw}\s+([\w\\,\s]+)", d.header)
                if mm:
                    parents |= {_php_resolve(x.strip(), ns, file_uses) for x in mm.group(1).split(",") if x.strip()}
            parents |= {_php_resolve(x, ns, file_uses) for x in _php_trait_uses(text[d.end : d.end + 4000])}
            cparents[f"{ns}\\{d.name}" if ns else d.name] = parents
        aliases, use_lines = _php_uses(text)
        body = "\n".join(line for i, line in enumerate(text.splitlines(), 1) if i not in use_lines)
        for lname, lib in libs:
            for alias, fqn in aliases.items():
                if fqn in lib.classes:
                    imports.add((rel, lname, lib.classes[fqn]))
                    if re.search(rf"(?<![\w\\$]){re.escape(alias)}\b", body):
                        uses.add((rel, lname, fqn))
            for m in _PHP_FQN.finditer(body):
                if m.group(1) in lib.classes and m.group(0).startswith("\\"):
                    uses.add((rel, lname, m.group(1)))
    lib_of = dict(libs)
    ancestors = {(lname, qn): lib_of[lname].ancestors(qn) for _, lname, qn in uses}
    # Library ancestors of consumer classes (Illuminate\Support\Carbon extends Carbon\Carbon).
    memo: dict[str, set] = {}

    def lib_anc(fqn: str, depth: int = 0) -> set:
        if fqn in memo or depth > 10:
            return memo.get(fqn, set())
        out: set = set()
        for p in cparents.get(fqn, ()):
            for lname, lib in libs:
                if p in lib.classes:
                    out |= {(lname, p)} | {(lname, a) for a in lib.ancestors(p)}
            if p in cparents:
                out |= lib_anc(p, depth + 1)
        memo[fqn] = out
        return out

    with_lib = {fqn: lib_anc(fqn) for fqn in cparents}
    with_lib = {fqn: v for fqn, v in with_lib.items() if v}
    inherited: dict[tuple, set] = {}
    for rel, text in texts.items():
        for fqn, targets in with_lib.items():
            if re.search(rf"\b{re.escape(fqn.rsplit(chr(92), 1)[-1])}\b", text):
                for lname, qn in targets:
                    inherited.setdefault((rel, lname), set()).add(qn)
    return imports, uses, ancestors, inherited


# --- Java ----------------------------------------------------------------------------------

_JAVA_IMPORT = re.compile(r"^import\s+(static\s+)?([\w.]+?)(\.\*)?\s*;", re.M)
_JAVA_PKG = re.compile(r"^package\s+([\w.]+)\s*;", re.M)


class JavaLib:
    def __init__(self, root: Path):
        self.root = root
        self.classes: dict[str, str] = {}
        self.parents: dict[str, set[str]] = {}
        for rel in _files(root, ".java"):
            if "/test/" in f"/{rel}" or "/src/test/" in rel:
                continue
            text = _read(root, rel)
            m = _JAVA_PKG.search(text)
            if not m:
                continue
            fqn = f"{m.group(1)}.{Path(rel).stem}"
            self.classes[fqn] = rel
            imports = {x.group(2).rsplit(".", 1)[-1]: x.group(2) for x in _JAVA_IMPORT.finditer(text) if not x.group(1) and not x.group(3)}
            d = re.search(rf"\b(?:class|interface|enum)\s+{Path(rel).stem}\b([^{{]*)\{{", text)
            parents = set()
            if d:
                for kw in ("extends", "implements"):
                    mm = re.search(rf"\b{kw}\s+([\w.,<>\s?]+?)(?:\bimplements\b|$)", d.group(1))
                    if mm:
                        for x in re.sub(r"<[^>]*>", "", mm.group(1)).split(","):
                            x = x.strip()
                            if x:
                                parents.add(imports.get(x, f"{m.group(1)}.{x}" if "." not in x else x))
            self.parents[fqn] = parents

    def ancestors(self, fqn: str) -> set[str]:
        out: set[str] = set()
        todo = [fqn]
        while todo:
            for p in self.parents.get(todo.pop(), ()):
                if p in self.classes and p not in out:
                    out.add(p)
                    todo.append(p)
        return out


def label_java(consumer: Path, libs: list[tuple[str, JavaLib]]) -> tuple[set, set, dict, dict]:
    imports: set = set()
    uses: set = set()
    for rel in _files(consumer, ".java"):
        text = _read(consumer, rel)
        found = list(_JAVA_IMPORT.finditer(text))
        lines = {text.count("\n", 0, m.start()) + 1 for m in found}
        body = "\n".join(line for i, line in enumerate(text.splitlines(), 1) if i not in lines)
        for m in found:
            if m.group(3):
                continue  # on-demand: a package directory, not a file
            target = m.group(2)
            if m.group(1):  # static: class.member
                target, member = target.rsplit(".", 1)
            else:
                member = None
            for lname, lib in libs:
                if target in lib.classes:
                    imports.add((rel, lname, lib.classes[target]))
                    simple = member or target.rsplit(".", 1)[-1]
                    if re.search(rf"\b{re.escape(simple)}\b", body):
                        uses.add((rel, lname, target))
    lib_of = dict(libs)
    ancestors = {(lname, qn): lib_of[lname].ancestors(qn) for _, lname, qn in uses}
    return imports, uses, ancestors, {}


# --- scoring -------------------------------------------------------------------------------


def _owner_match(dst: str, target: str, language: str) -> bool:
    """`dst` (an arm's target qualified name) is `target` or a member of it."""
    if dst == target:
        return True
    seps = {"python": ".", "php": "::", "java": "."}
    return dst.startswith(target + seps[language])


def score_group(edges: list[dict], consumer_id: str, lib_ids: dict[str, str], gold: tuple, language: str) -> dict:
    """Precision / recall of one group's link edges (those observed in the
    consumer) against its gold."""
    imports, uses, ancestors, inherited = gold
    by_id = {v: k for k, v in lib_ids.items()}
    pred_imp: set = set()
    pred_sym: list[tuple] = []
    for e in edges:
        if e.get("consumer", consumer_id) != consumer_id:
            continue
        if e.get("src_repo") == consumer_id and e.get("dst_repo") in by_id:
            lname = by_id[e["dst_repo"]]
            here, there, there_label = e.get("src_path"), e["dst"], e.get("dst_label")
        elif e.get("dst_repo") == consumer_id and e.get("src_repo") in by_id:
            # into the consumer (DECORATES: a library decorator on a consumer definition)
            lname = by_id[e["src_repo"]]
            here, there, there_label = e.get("dst_path"), e["src"], e.get("src_label")
        else:
            continue
        if e["kind"] == "IMPORTS":
            if e.get("src_label") == "File" and there_label == "File":
                pred_imp.add((e["src_path"], lname, e["dst_path"]))
            continue
        pred_sym.append((here, lname, there, e["kind"], e.get("strategy")))
    gold_by_file: dict[tuple, set] = {k: set(v) for k, v in inherited.items()}
    for f, lname, qn in uses:
        accept = {qn} | set(ancestors.get((lname, qn), set()))
        gold_by_file.setdefault((f, lname), set()).update(accept)
    sym_ok = [p for p in pred_sym if any(_owner_match(p[2], t, language) for t in gold_by_file.get((p[0], p[1]), ()))]
    found_uses = {
        (f, lname, qn)
        for f, lname, qn in uses
        if any(p[0] == f and p[1] == lname and any(_owner_match(p[2], t, language) for t in {qn} | set(ancestors.get((lname, qn), set()))) for p in pred_sym)
    }
    imp_ok = pred_imp & imports
    by_kind: dict[str, list[int]] = {}
    for p in pred_sym:
        k = by_kind.setdefault(p[3], [0, 0])
        k[0] += 1
        k[1] += int(p in sym_ok)
    wrong = [p for p in pred_sym if p not in sym_ok]
    return {
        "imports": {"predicted": len(pred_imp), "gold": len(imports), "correct": len(imp_ok), "found": len(imp_ok)},
        "uses": {"predicted": len(pred_sym), "gold": len(uses), "correct": len(sym_ok), "found": len(found_uses)},
        "by_kind": {k: {"predicted": v[0], "correct": v[1]} for k, v in sorted(by_kind.items())},
        "missed_imports": sorted(imports - pred_imp)[:20],
        "wrong_imports": sorted(pred_imp - imports)[:20],
        "missed_uses": sorted(uses - found_uses)[:20],
        "wrong_uses": sorted(wrong)[:30],
    }


def _pr(c: dict) -> dict:
    p = c["correct"] / c["predicted"] if c["predicted"] else None
    r = c["found"] / c["gold"] if c["gold"] else None
    return {**c, "precision": p, "recall": r}


def summarize(groups: list[dict]) -> dict:
    tot = {k: {"predicted": 0, "gold": 0, "correct": 0, "found": 0} for k in ("imports", "uses", "all")}
    for g in groups:
        if g.get("status") != "ok":
            continue
        for lib in g["libraries"]:
            s = lib["score"]
            for k in ("imports", "uses"):
                for f in ("predicted", "gold", "correct", "found"):
                    tot[k][f] += s[k][f]
                    tot["all"][f] += s[k][f]
    return {k: _pr(v) for k, v in tot.items()}


# --- running -------------------------------------------------------------------------------


def run_crossrepo(arm_names: list[str], force: bool = False) -> None:
    if "rueca" not in arm_names:
        return
    arm = make_arm("rueca")
    if not arm.available()[0]:
        return
    p = run_path("crossrepo", "rueca")
    if p.exists() and not force and read_json(p).get("status") == "ok":
        return
    groups = []
    for g in GROUPS:
        log(f"crossrepo rueca: {g['consumer']} with {', '.join(x['repo'] for x in g['libraries'])}")
        try:
            groups.append(_group(arm, g))
        except Exception as exc:
            groups.append({"consumer": g["consumer"], "status": "error", "error": f"{exc}\n{traceback.format_exc()[-1500:]}"})
    rec = {"status": "ok", "groups": groups, "summary": summarize(groups), "arm_version": arm.version()}
    write_json(p, rec)


def _group(arm, g: dict) -> dict:
    store = CACHE / "org" / "crossrepo-store"
    shutil.rmtree(store, ignore_errors=True)
    store.mkdir(parents=True, exist_ok=True)
    org_file = CACHE / "org" / "crossrepo-org.toml"
    roots: dict[str, Path] = {}
    try:
        roots[g["consumer"]] = add_worktree(g["consumer"], g["commit"], f"xr-{g['consumer']}")
        for lib in g["libraries"]:
            roots[lib["repo"]] = add_worktree(lib["repo"], lib["commit"], f"xr-{lib['repo']}")
        write_org_file(org_file, list(roots.values()), name="crossrepo")
        res = run([arm.bin, "index", "--org", str(org_file), "--store", str(store), "--json"], timeout=arm.index_timeout)
        if res.rc != 0:
            return {"consumer": g["consumer"], "status": "error", "error": res.stderr[-500:]}
        report = json.loads(res.stdout)
        ids = {Path(x["root"]).resolve(): x["segment"]["snapshot"]["repo"] for x in report["indexed"]}
        rid = {repo: ids.get(root.resolve()) for repo, root in roots.items()}
        out = run([arm.bin, "links", "--org", str(org_file), "--store", str(store), "--json"], timeout=600)
        if out.rc != 0:
            return {"consumer": g["consumer"], "status": "error", "error": out.stderr[-500:]}
        edges = json.loads(out.stdout)["edges"]
        lang = g["language"]
        consumer = roots[g["consumer"]]
        if lang == "python":
            libs = [(x["repo"], PyLib(roots[x["repo"]], x["packages"])) for x in g["libraries"]]
            gold = label_python(consumer, libs)
        elif lang == "php":
            libs = [(x["repo"], PhpLib(roots[x["repo"]])) for x in g["libraries"]]
            gold = label_php(consumer, libs)
        else:
            libs = [(x["repo"], JavaLib(roots[x["repo"]])) for x in g["libraries"]]
            gold = label_java(consumer, libs)
        per_lib = []
        for x in g["libraries"]:
            sub = (
                {t for t in gold[0] if t[1] == x["repo"]},
                {t for t in gold[1] if t[1] == x["repo"]},
                {k: v for k, v in gold[2].items() if k[0] == x["repo"]},
                {k: v for k, v in gold[3].items() if k[1] == x["repo"]},
            )
            per_lib.append({"repo": x["repo"], "commit": x["commit"], "score": score_group(edges, rid[g["consumer"]], {x["repo"]: rid[x["repo"]]}, sub, lang)})
        return {
            "consumer": g["consumer"],
            "commit": g["commit"],
            "language": lang,
            "status": "ok",
            "org_ms": report.get("org_ms"),
            "link_ms": (report.get("update") or {}).get("link_ms"),
            "edges": len(edges),
            "libraries": per_lib,
        }
    finally:
        for repo, root in roots.items():
            remove_worktree(repo, root)
        shutil.rmtree(store, ignore_errors=True)


def score_crossrepo() -> dict:
    p = run_path("crossrepo", "rueca")
    if not p.exists():
        return {}
    rec = read_json(p)
    if rec.get("status") != "ok":
        return {"status": rec.get("status"), "error": str(rec.get("error"))[:300]}
    return {"status": "ok", "summary": summarize(rec["groups"]), "groups": rec["groups"], "version": rec.get("arm_version")}
