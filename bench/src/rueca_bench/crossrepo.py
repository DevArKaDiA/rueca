"""Cross-repository edges (RFC 0006 §8, `crossrepo.py`): precision and recall
of the organization link layer (packages) on public repositories that depend
on each other.

**Pairs** ([`GROUPS`]): a consumer pinned with the libraries it depends on,
each library at a release the consumer's manifest accepts — Flask 3.1.3 with
Click 8.1.8, Jinja 3.1.6 and Werkzeug 3.1.5 (Python), Laravel (the S1
organization commit) with Carbon, Monolog 3.9.0 and league/commonmark 2.8.3
(PHP), Apache Dubbo and Apollo (S1 commits) with Gson 2.15.0 (Java), Axum
(its organization pin) with Tokio (its organization pin), Hyper 0.14.27 and
Tower 0.4.13, and Tokio with Mio 1.0.1 (Rust); the pin rule is above
`GROUPS`. Each group is one organization (`rueca index --org`); `rueca
links --org` lists its cross-repository edges.

**Gold**, labelled from the consumer's own source with an independent,
per-language labeler (Python `ast`; PHP and Java by regular expressions over
`use` / `import` statements, the library's Composer PSR-4 map or package
directories; Rust by a token walk over the library's Cargo packages and
module tree, see below):

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

**Rust** (`RustLib`, `label_rust`). A library is every Cargo package of the
library repository with a library target (`[lib] path`, default
`src/lib.rs`; crate `[lib] name`, else the package name with `-` → `_`;
workspace members alike, `version.workspace` read from the root). Its
module tree follows `mod x;` (`x.rs` / `x/mod.rs`, `#[path = "…"]`) and
inline `mod x { … }`, also inside macro invocation bodies (`cfg_net! { pub
mod net; }`: a macro body is transparent, a function, impl or `macro_rules!`
body is not). A path resolves segment by segment through child modules,
items, `use` bindings (re-exports, `as`, groups, `self`, `crate` / `super`,
other crates of the repository) and glob re-exports; a crate root also sees
its `#[macro_export]` macros. Qualified names are Rueca's: the defining
file's module path (crate, then directories and file stem below the crate
root's directory) and the local name (`tokio::net::tcp::listener::TcpListener`,
`kit::time.sleep` in an inline module). Consumer side: a code name is a
library crate when a manifest of the consumer depends on its package
(renames, `workspace = true`) with a requirement whose compatibility range
holds the pinned version. *imports* = (consumer file, the library file
defining what a `use` path names — the item's file after re-exports, the
module's file for a module; Rueca's convention, not the path's module
prefix); glob imports count as imports of the module. *uses* = items bound
by `use` and named outside the `use` statements (a trait also when one of
its methods is called, `use …::AsyncReadExt as _`), plus paths in code
through a library crate or an imported library module (`tokio::spawn`,
`#[tokio::main]` → `tokio_macros::main`, `mpsc::channel`); bindings of the
consumer's modules a file glob-imports (`use super::*`) count as its own.
Library ancestors of a used type are the library traits it implements
(`impl Trait for Type`, by last segment). Blind spots: items a macro
generates (`doc! { macro_rules! select … }`, derives), glob imports of a
library module (no use is attributed through them), `#[cfg]` alternatives
(the first binding wins), shadowing by local names, `#[path]` inside inline
modules; methods of values a library call returned count against precision.

Raw outputs: .cache/runs/org/crossrepo-rueca.json.
"""

from __future__ import annotations

import ast
import json
import os
import re
import shutil
import tomllib
import traceback
from collections.abc import Callable, Iterator
from datetime import datetime
from pathlib import Path
from typing import NamedTuple

from .arms import make_arm
from .common import CACHE, log, org_run_path, read_json, run, write_json, write_org_file
from .corpus import add_worktree, remove_worktree

# consumer, its commit, and the libraries (repo, commit, language) it imports.
#
# Pins. A repository of the gated S1 set is pinned where the organization
# scenario pins it: the base commit of its first gated `[[s1.instances]]`
# entry in manifest order (the "org pin"). A library outside the benchmark
# is pinned by one deterministic rule (`pick_release`): the release tag the
# consumer's lockfile names when the consumer commits one (`uv.lock`,
# `Cargo.lock`), else the highest release tag that meets the consumer's
# requirement at its pin (Cargo.toml / pyproject.toml) and was published
# before the consumer pin's commit date. The Rueca linker matches Cargo
# dependencies by package name only (`cargo:<name>` requires, `rust:<crate>`
# import keys), never by version; the labeler skips a dependency whose
# requirement is outside the pinned version's compatibility range (axum's
# `hyper1 = { package = "hyper", version = "=1.0.0-rc.4" }` is not hyper 0.14).
GROUPS = [
    {
        "consumer": "pallets/flask",
        "commit": "22d924701a6ae2e4cd01e9a15bbaf3946094af65",  # 3.1.3
        "language": "python",
        "libraries": [
            {"repo": "pallets/click", "commit": "934813e4d421071a1b3db3973c02fe2721359a6e", "packages": ["click"]},  # 8.1.8
            {"repo": "pallets/jinja", "commit": "15206881c006c79667fe5154fe80c01c65410679", "packages": ["jinja2"]},  # 3.1.6
            # 3.1.5: flask 3.1.3's uv.lock (requirement werkzeug>=3.1.0; 3.1.6 was tagged the day after the pin)
            {"repo": "pallets/werkzeug", "commit": "e3d06f4b1f7ff40a63eba78f81d9cda18f805d6d", "packages": ["werkzeug"]},
        ],
    },
    {
        # org pin: base of tokio-rs__axum-2096 (axum 0.6, 2023-07-16; no Cargo.lock)
        "consumer": "tokio-rs/axum",
        "commit": "6f7ff8556587d51c6919a864926e4b8090f92c3f",
        "language": "rust",
        "libraries": [
            # org pin: base of tokio-rs__tokio-6724 (tokio 1.39.2; axum requires tokio 1.25.0)
            {"repo": "tokio-rs/tokio", "commit": "0cbf1a5adae81e8ff86ca6d040aeee67cf888262"},
            # v0.14.27 (2023-06-26): highest release meeting hyper 0.14.24 before the axum pin
            {"repo": "hyperium/hyper", "commit": "d77c2599bc023b258b90a17f5b633c8b7b0cbd4b"},
            # tower-0.4.13 (2022-06-17): highest release meeting tower 0.4.13 before the axum pin
            # (tower-layer 0.3.1 / tower-service 0.3.2 in that tree; axum asks 0.3.2 / 0.3)
            {"repo": "tower-rs/tower", "commit": "04527aeb439761875a3e4f96d2090622731bc719"},
        ],
    },
    {
        # org pin: base of tokio-rs__tokio-6724 (tokio 1.39.2, 2024-07-29; no Cargo.lock)
        "consumer": "tokio-rs/tokio",
        "commit": "0cbf1a5adae81e8ff86ca6d040aeee67cf888262",
        "language": "rust",
        # v1.0.1 (2024-07-23): highest release meeting mio 1.0.1 before the tokio pin
        "libraries": [{"repo": "tokio-rs/mio", "commit": "4a5114e518b982f49ce093be6d0d2a2ab86472d1"}],
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


def label_python(consumer: Path, libs: list[tuple[str, PyLib]], only: set[str] | None = None) -> tuple[set, set, dict, dict]:
    """(imports, uses, ancestors, inherited): imports {(file, lib, lib file)},
    uses {(file, lib, qualified name)}, ancestors {(lib, qn): {base qns}},
    inherited {(file, lib): {qns}} = library ancestors of consumer classes
    the file names (accepted targets for precision, not gold uses). `only`
    restricts the consumer files labelled (every labeler)."""
    imports: set = set()
    uses: set = set()
    lib_of = dict(libs)
    texts: dict[str, str] = {}
    class_lib: dict[str, set] = {}  # consumer class name -> {(lib, qn)} of library bases
    class_bases: dict[str, set] = {}  # consumer class name -> names of its other bases
    for rel in _files(consumer, ".py"):
        if only is not None and rel not in only:
            continue
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
                        *subs, last = rest.split(".")
                        for part in subs:  # `werkzeug.test.EnvironBuilder` through submodules
                            mod = f"{mod}.{part}" if f"{mod}.{part}" in lib_of[lname].modules else ""
                        qn = lib_of[lname].resolve(mod, last) if mod else None
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


def label_php(consumer: Path, libs: list[tuple[str, PhpLib]], only: set[str] | None = None) -> tuple[set, set, dict, dict]:
    imports: set = set()
    uses: set = set()
    texts: dict[str, str] = {}
    cparents: dict[str, set[str]] = {}  # consumer class FQN -> parents (FQN)
    for rel in _files(consumer, ".php"):
        if only is not None and rel not in only:
            continue
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


def label_java(consumer: Path, libs: list[tuple[str, JavaLib]], only: set[str] | None = None) -> tuple[set, set, dict, dict]:
    imports: set = set()
    uses: set = set()
    for rel in _files(consumer, ".java"):
        if only is not None and rel not in only:
            continue
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


# --- Rust ----------------------------------------------------------------------------------

_RUST_TOKEN = re.compile(r"(?:r#)?[A-Za-z_][A-Za-z0-9_]*|::|\S")
_RUST_PATH_ATTR = re.compile(r"#\[\s*path\s*=\s*\"([^\"]+)\"\s*\]")
_RUST_PATH = re.compile(r"(?<![\w:])(?:::\s*)?[A-Za-z_]\w*(?:\s*::\s*[A-Za-z_]\w*)+")
_RUST_IDENT = re.compile(r"[A-Za-z_]\w*")
_RUST_RAW = re.compile(r'(?:br|cr|r)(#*)"')
_RUST_MODIFIERS = frozenset({"pub", "unsafe", "async", "default", "auto", "extern", "const", '"'})
_RUST_ITEMS = frozenset({"fn", "struct", "enum", "union", "trait", "type", "const", "static"})
# Module-level contexts: a macro invocation's body (`cfg_rt! { … }`) is
# transparent, an inline module adds its name to the local name prefix.
_RUST_TRANSPARENT = ("macro", "mod")


def _rust_strip(text: str) -> str:
    """`text` with comments, string and character literals blanked (newlines
    kept, a string becomes `""`), lifetimes left alone."""
    out: list[str] = []
    i, n = 0, len(text)

    def blank(s: str) -> str:
        return "".join("\n" if c == "\n" else " " for c in s)

    while i < n:
        c = text[i]
        if c == "/" and text.startswith("//", i):
            j = text.find("\n", i)
            j = n if j < 0 else j
            out.append(" " * (j - i))
            i = j
        elif c == "/" and text.startswith("/*", i):
            depth, j = 1, i + 2
            while j < n and depth:
                if text.startswith("/*", j):
                    depth, j = depth + 1, j + 2
                elif text.startswith("*/", j):
                    depth, j = depth - 1, j + 2
                else:
                    j += 1
            out.append(blank(text[i:j]))
            i = j
        elif c in "rbc" and text[i + 1 : i + 2] in ('"', "#", "r") and (i == 0 or not (text[i - 1].isalnum() or text[i - 1] == "_")) and (m := _RUST_RAW.match(text, i)):
            close = '"' + m.group(1)
            j = text.find(close, m.end())
            j = n if j < 0 else j + len(close)
            out.append('""' + blank(text[i + 2 : j]))
            i = j
        elif c == '"':
            j = i + 1
            while j < n and text[j] != '"':
                j += 2 if text[j] == "\\" else 1
            j = min(j + 1, n)
            out.append('""' + blank(text[i + 2 : j]))
            i = j
        elif c == "'":
            if text.startswith("\\", i + 1):
                j = text.find("'", i + 3)
                j = n if j < 0 else j + 1
            elif i + 2 < n and text[i + 2] == "'":
                j = i + 3
            else:  # a lifetime or a label
                out.append(c)
                i += 1
                continue
            out.append(blank(text[i:j]))
            i = j
        else:
            out.append(c)
            i += 1
    return "".join(out)


def _rust_use_tree(toks: list[str]) -> list[tuple[tuple[str, ...], str | None]]:
    """Entries of a `use` tree (the tokens between `use` and `;`): (path,
    bound name), the bound name `None` for a glob import (`a::*`). A group's
    `self` binds its prefix; `as _` binds `_`."""
    out: list[tuple[tuple[str, ...], str | None]] = []
    pos = 0

    def tree(prefix: tuple[str, ...]) -> None:
        nonlocal pos
        path = list(prefix)
        while pos < len(toks):
            t = toks[pos]
            if t == "::":
                pos += 1
            elif t == "{":
                pos += 1
                while pos < len(toks) and toks[pos] != "}":
                    if toks[pos] == ",":
                        pos += 1
                        continue
                    tree(tuple(path))
                pos += 1
                return
            elif t == "*":
                pos += 1
                out.append((tuple(path), None))
                return
            elif _RUST_IDENT.fullmatch(t.removeprefix("r#")):
                pos += 1
                name = t.removeprefix("r#")
                if pos < len(toks) and toks[pos] == "::":
                    path.append(name)
                    continue
                alias = None
                if pos + 1 < len(toks) and toks[pos] == "as":
                    alias = toks[pos + 1].removeprefix("r#")
                    pos += 2
                if name == "self":
                    if path:
                        out.append((tuple(path), alias or path[-1]))
                else:
                    out.append((tuple(path + [name]), alias or name))
                return
            else:
                pos += 1
                return

    while pos < len(toks):
        if toks[pos] == ",":
            pos += 1
            continue
        tree(())
    return out


class _RustFile(NamedTuple):
    """What one Rust file declares. Keys carry the inline-module prefix:
    `(("tests",), "helper")` is `helper` in `mod tests { … }`."""

    mods: dict  # (inline, name) -> {"inline": bool, "path": str | None}
    items: dict  # (inline, name) -> kind (fn, struct, trait, macro_rules, …)
    uses: dict  # inline -> [(path, bound name or None for a glob)] (module level)
    all_uses: list  # [(start offset, end offset, entries, inline)], every `use` of the file
    impls: list  # [(trait last segment or None, self type last segment)]
    trait_methods: dict  # (inline, trait) -> {method names}
    macro_exports: set  # names of `#[macro_export] macro_rules!`
    code: str  # the stripped text


def _rust_strip_attrs(stmt: list[str]) -> list[str]:
    """`stmt` without its leading attributes (`#[…]`, `#![…]`)."""
    i = 0
    while i < len(stmt) and stmt[i] == "#":
        j = i + 1
        if j < len(stmt) and stmt[j] == "!":
            j += 1
        if j >= len(stmt) or stmt[j] != "[":
            break
        depth = 0
        while j < len(stmt):
            depth += {"[": 1, "]": -1}.get(stmt[j], 0)
            j += 1
            if depth == 0:
                break
        i = j
    return stmt[i:]


def _rust_head(stmt: list[str]) -> list[str]:
    """`stmt` from its item keyword on: attributes, visibility (`pub`,
    `pub(crate)`) and modifiers (`unsafe`, `async`, `extern "C"`, `const fn`)
    left out."""
    s = _rust_strip_attrs(stmt)
    i = 0
    while i < len(s):
        if s[i] == "pub" and i + 1 < len(s) and s[i + 1] == "(":
            j = s.index(")", i) if ")" in s[i:] else len(s) - 1
            i = j + 1
        elif s[i] == "const" and i + 1 < len(s) and s[i + 1] in ("fn", "unsafe", "async", "extern"):
            i += 1
        elif s[i] in _RUST_MODIFIERS and s[i] != "const":
            i += 1
        else:
            break
    return s[i:]


def _rust_scan(text: str) -> _RustFile:
    """Declarations of a Rust file by a token walk with a context stack:
    module-level items are those outside every block but macro invocation
    bodies (`cfg_net! { pub mod net; }`) and inline modules."""
    code = _rust_strip(text)
    toks = [(m.group(), m.start()) for m in _RUST_TOKEN.finditer(code)]
    path_attrs = {}
    for m in _RUST_PATH_ATTR.finditer(text):
        mm = re.match(r"\s*(?:#\[[^\]]*\]\s*)*(?:pub(?:\([^)]*\))?\s+)?mod\s+(\w+)", text[m.end() :])
        if mm:
            path_attrs[mm.group(1)] = m.group(1)
    exports = set(re.findall(r"#\[\s*macro_export[^\]]*\]\s*(?:#\[[^\]]*\]\s*)*macro_rules!\s*(\w+)", code))
    f = _RustFile({}, {}, {}, [], [], {}, exports, code)
    stack: list[tuple[str, str]] = []
    stmt: list[str] = []

    def level() -> tuple[str, ...] | None:
        """The inline-module prefix when at module level, else None."""
        if all(k in _RUST_TRANSPARENT for k, _ in stack):
            return tuple(n for k, n in stack if k == "mod")
        return None

    def in_trait() -> tuple[tuple[str, ...], str] | None:
        if stack and stack[-1][0] == "trait" and all(k in _RUST_TRANSPARENT for k, _ in stack[:-1]):
            return tuple(n for k, n in stack[:-1] if k == "mod"), stack[-1][1]
        return None

    def item(head: list[str], inline: tuple[str, ...]) -> None:
        if len(head) >= 2 and head[0] in _RUST_ITEMS and head[1] != "_" and _RUST_IDENT.fullmatch(head[1].removeprefix("r#")):
            f.items.setdefault((inline, head[1].removeprefix("r#")), head[0])
        elif len(head) >= 3 and head[0] == "macro_rules" and head[1] == "!":
            f.items.setdefault((inline, head[2]), "macro_rules")

    i = 0
    while i < len(toks):
        t, off = toks[i]
        head = _rust_head(stmt) if stmt and t in ("use", "{", ";") else []
        if t == "use" and not head:
            j = i + 1
            while j < len(toks) and toks[j][0] != ";":
                j += 1
            entries = _rust_use_tree([x for x, _ in toks[i + 1 : j]]) if j > i + 1 and toks[i + 1][0] != "<" else []
            end = toks[j][1] + 1 if j < len(toks) else len(code)
            f.all_uses.append((off, end, entries, tuple(n for k, n in stack if k == "mod")))
            inline = level()
            if inline is not None:
                f.uses.setdefault(inline, []).extend(entries)
            stmt = []
            i = j + 1
            continue
        if t == "{":
            inline = level()
            trait = in_trait()
            if "macro_rules" in head:
                kind = ("opaque", "")
                if inline is not None:
                    item(head, inline)
            elif head and len(head) >= 2 and head[-1] == "!" and _RUST_IDENT.fullmatch(head[-2]) and all(x == "::" or _RUST_IDENT.fullmatch(x) for x in head[:-1]):
                kind = ("macro", "")
            elif head[:1] == ["mod"] and len(head) >= 2:
                kind = ("mod", head[1].removeprefix("r#"))
                if inline is not None:
                    f.mods.setdefault((inline, kind[1]), {"inline": True, "path": None})
            elif head[:1] == ["trait"] and len(head) >= 2:
                kind = ("trait", head[1])
                if inline is not None:
                    item(head, inline)
            elif head[:1] == ["impl"]:
                kind = ("impl", "")
                if inline is not None:
                    depth, names, trait_name, prev = 0, [], None, ""
                    for x in head[1:]:
                        if x == "<" or (x == ">" and prev != "-"):
                            depth += 1 if x == "<" else -1
                        prev = x
                        if x == "where" and depth == 0:
                            break
                        if depth == 0 and x == "for" and names:
                            trait_name = names[-1]
                            names = []
                        elif depth == 0 and _RUST_IDENT.fullmatch(x) and x not in ("dyn", "mut", "const", "unsafe", "for"):
                            names.append(x)
                    if names:
                        f.impls.append((trait_name, names[-1]))
            else:
                kind = ("opaque", "")
                if inline is not None:
                    item(head, inline)
                elif trait is not None and head[:1] == ["fn"] and len(head) >= 2:
                    f.trait_methods.setdefault(trait, set()).add(head[1])
            stack.append(kind)
            stmt = []
        elif t == "}":
            if stack:
                stack.pop()
            stmt = []
        elif t == ";":
            inline = level()
            trait = in_trait()
            if inline is not None and head[:1] == ["mod"] and len(head) >= 2:
                name = head[1].removeprefix("r#")
                f.mods.setdefault((inline, name), {"inline": False, "path": path_attrs.get(name)})
            elif inline is not None:
                item(head, inline)
            elif trait is not None and head[:1] == ["fn"] and len(head) >= 2:
                f.trait_methods.setdefault(trait, set()).add(head[1])
            stmt = []
        else:
            stmt.append(t)
        i += 1
    return f


def _toml(root: Path, rel: str) -> dict:
    try:
        return tomllib.loads(_read(root, rel))
    except (tomllib.TOMLDecodeError, ValueError):
        return {}


def _manifests(root: Path) -> list[str]:
    return [m for m in _files(root, "Cargo.toml") if m.rsplit("/", 1)[-1] == "Cargo.toml" and "/target/" not in f"/{m}"]


class RustLib:
    """A library repository's Cargo packages: every `[package]` with a library
    target (`[lib] path`, default `src/lib.rs`; crate name `[lib] name`, else
    the package name with `-` → `_`), its module tree from `mod x;`
    declarations (`x.rs` / `x/mod.rs`, `#[path]`, inline `mod x { … }`,
    declarations inside macro bodies), its items and its `use` statements
    (re-exports, followed when resolving a path)."""

    def __init__(self, root: Path):
        self.root = root
        self.crates: dict[str, dict] = {}  # crate name -> {package, version, root, dir}
        self.files: dict[str, _RustFile] = {}
        self.children: dict[tuple, tuple] = {}  # (module, name) -> child module
        self.parent: dict[tuple, tuple] = {}
        self.crate_of: dict[str, str] = {}  # file -> crate name
        self._memo: dict[tuple, tuple | None] = {}
        self._paths: dict[str, str] | None = None
        workspace_version = None
        for m in sorted(_manifests(root), key=lambda p: (p.count("/"), p)):
            doc = _toml(root, m)
            ws = (doc.get("workspace") or {}).get("package") or {}
            if workspace_version is None and isinstance(ws.get("version"), str):
                workspace_version = ws["version"]
            pkg = doc.get("package")
            if not isinstance(pkg, dict) or not isinstance(pkg.get("name"), str):
                continue
            d = m.rsplit("/", 1)[0] if "/" in m else ""
            lib = doc.get("lib") if isinstance(doc.get("lib"), dict) else {}
            lib_root = "/".join(x for x in (d, lib.get("path") or "src/lib.rs") if x)
            lib_root = os.path.normpath(lib_root).replace(os.sep, "/")
            if not (root / lib_root).is_file():
                continue
            name = (lib.get("name") or pkg["name"]).replace("-", "_")
            version = pkg.get("version") if isinstance(pkg.get("version"), str) else workspace_version
            if name not in self.crates:
                self.crates[name] = {"package": pkg["name"], "version": version, "root": lib_root, "dir": lib_root.rsplit("/", 1)[0] if "/" in lib_root else ""}
                self._load_tree(name, lib_root)
        trait_qns: dict[str, set[str]] = {}
        for file, info in self.files.items():
            for (inline, name), kind in info.items.items():
                if kind == "trait":
                    trait_qns.setdefault(name, set()).add(self.qn((file, inline), name))
        self.traits_of: dict[str, set[str]] = {}  # self type name -> trait qns it implements
        for info in self.files.values():
            for trait, ty in info.impls:
                if trait and trait in trait_qns:
                    self.traits_of.setdefault(ty, set()).update(trait_qns[trait])

    def _load_tree(self, crate: str, root_file: str) -> None:
        todo = [root_file]
        while todo:
            file = todo.pop()
            if file in self.files or not (self.root / file).is_file():
                continue
            self.crate_of[file] = crate
            info = self.files[file] = _rust_scan(_read(self.root, file))
            base = file.rsplit("/", 1)[0] if "/" in file else ""
            stem = file.rsplit("/", 1)[-1][:-3]
            own_dir = base if file == root_file or stem == "mod" else f"{base}/{stem}".lstrip("/")
            for (inline, name), decl in sorted(info.mods.items()):
                mod = (file, inline)
                if decl["inline"]:
                    child = (file, inline + (name,))
                elif decl["path"]:
                    target = os.path.normpath(f"{'/'.join([base, *inline]) if inline else base}/{decl['path']}").replace(os.sep, "/").lstrip("/")
                    child = (target, ())
                    todo.append(target)
                else:
                    d = "/".join(x for x in (own_dir, *inline) if x)
                    cands = [f"{d}/{name}.rs".lstrip("/"), f"{d}/{name}/mod.rs".lstrip("/")]
                    target = next((c for c in cands if (self.root / c).is_file()), None)
                    if target is None:
                        continue
                    child = (target, ())
                    todo.append(target)
                self.children.setdefault((mod, name), child)
                self.parent.setdefault(child, mod)
            for inline_key in {k[0] for k in info.items} | set(info.uses):
                for n in range(1, len(inline_key) + 1):
                    self.parent.setdefault((file, inline_key[:n]), (file, inline_key[: n - 1]))

    def module_path(self, file: str) -> str:
        """Rueca's module path of a file: crate name, then the directories and
        file stem below the crate root's directory (`mod.rs` names its
        directory, the root is the crate)."""
        crate = self.crate_of.get(file, "")
        info = self.crates.get(crate)
        if info is None or file == info["root"]:
            return crate
        rel = file[len(info["dir"]) + 1 :] if info["dir"] and file.startswith(info["dir"] + "/") else file
        parts = rel[:-3].split("/")
        if parts[-1] == "mod":
            parts = parts[:-1]
        return "::".join([crate, *parts])

    def qn(self, mod: tuple, name: str) -> str:
        file, inline = mod
        return f"{self.module_path(file)}::{'.'.join((*inline, name))}"

    def root_module(self, crate: str) -> tuple | None:
        info = self.crates.get(crate)
        return (info["root"], ()) if info and info["root"] in self.files else None

    def lookup(self, mod: tuple, name: str, want_module: bool, depth: int = 0) -> tuple | None:
        """`name` seen from module `mod`: ("mod", module, None) or ("item", module,
        name), through child modules, items, `use` bindings (re-exports) and
        glob imports; a crate root also sees its `#[macro_export]` macros."""
        key = (mod, name, want_module)
        if key in self._memo:
            return self._memo[key]
        if depth > 16:
            return None
        self._memo[key] = None  # in progress: cycles resolve to nothing
        res = self._lookup(mod, name, want_module, depth)
        self._memo[key] = res
        return res

    def _lookup(self, mod: tuple, name: str, want_module: bool, depth: int) -> tuple | None:
        file, inline = mod
        info = self.files.get(file)
        if info is None:
            return None
        child = self.children.get((mod, name))
        if want_module and child:
            return ("mod", child, None)
        if (inline, name) in info.items:
            return ("item", mod, name)
        for path, bound in info.uses.get(inline, ()):
            if bound == name and path:
                r = self.resolve(mod, path, want_module, depth + 1)[0]
                if r:
                    return r
        if child:
            return ("mod", child, None)
        for path, bound in info.uses.get(inline, ()):
            if bound is None and path:
                target = self.resolve(mod, path, True, depth + 1)[0]
                if target and target[0] == "mod":
                    r = self.lookup(target[1], name, want_module, depth + 1)
                    if r:
                        return r
        crate = self.crate_of.get(file)
        if crate and mod == self.root_module(crate):
            for f2, info2 in self.files.items():
                if self.crate_of.get(f2) == crate and name in info2.macro_exports:
                    for (inl, n), kind in info2.items.items():
                        if n == name and kind == "macro_rules":
                            return ("item", (f2, inl), name)
        return None

    def resolve(self, mod: tuple, path: tuple[str, ...], want_module: bool = False, depth: int = 0) -> tuple[tuple | None, tuple | None]:
        """A path seen from module `mod` → (resolution, deepest module reached).
        Stops at the first item: further segments are its members."""
        if not path or depth > 16:
            return None, None
        head, rest = path[0], list(path[1:])
        if head == "crate":
            cur = self.root_module(self.crate_of.get(mod[0], ""))
        elif head == "self":
            cur = mod
        elif head == "super":
            cur = self.parent.get(mod)
            while rest and rest[0] == "super" and cur:
                cur = self.parent.get(cur)
                rest = rest[1:]
        else:
            r = self.lookup(mod, head, bool(rest) or want_module, depth)
            if r is None and head in self.crates:
                r = ("mod", self.root_module(head), None)
            if r is None:
                return None, None
            if r[0] == "item":
                return r, mod
            cur = r[1]
        return self._walk(cur, rest, want_module, depth)

    def _walk(self, cur: tuple | None, rest: list[str], want_module: bool, depth: int) -> tuple[tuple | None, tuple | None]:
        if cur is None:
            return None, None
        for k, seg in enumerate(rest):
            last = k == len(rest) - 1
            r = self.lookup(cur, seg, not last or want_module, depth)
            if r is None:
                return None, cur
            if r[0] == "item":
                return r, cur
            cur = r[1]
        return ("mod", cur, None), cur

    def resolve_extern(self, crate: str, path: tuple[str, ...]) -> tuple[tuple | None, tuple | None]:
        """`crate::path` from outside the library (a consumer's `use` or path)."""
        root = self.root_module(crate)
        return self._walk(root, list(path), False, 0) if root else (None, None)

    def path_of(self, qn: str) -> str | None:
        """The file defining item `qn`."""
        if self._paths is None:
            self._paths = {self.qn((file, inline), name): file for file, info in self.files.items() for (inline, name) in info.items}
        return self._paths.get(qn)

    def file_of(self, res: tuple | None) -> str | None:
        return res[1][0] if res else None

    def kind(self, res: tuple) -> str | None:
        file, inline = res[1]
        return self.files[file].items.get((inline, res[2]))

    def methods(self, res: tuple) -> set[str]:
        file, inline = res[1]
        return self.files[file].trait_methods.get((inline, res[2]), set())

    def ancestors(self, qn: str) -> set[str]:
        """Library traits the type `qn` implements (`impl Trait for Type`,
        matched by last segment): their default methods are members too."""
        return set(self.traits_of.get(re.split(r"::|\.", qn)[-1], set())) - {qn}


def _cargo_dependencies(root: Path) -> dict[str, set[tuple[str, str | None]]]:
    """Code name → {(package name, version requirement)} of every dependency
    table of every manifest of a repository (`[dependencies]`, dev, build and
    `[target.…]` tables; `package =` renames; `workspace = true` entries read
    from the root's `[workspace.dependencies]`); `path`-only entries are the
    repository's own crates and left out."""
    out: dict[str, set[tuple[str, str | None]]] = {}
    workspace: dict = {}
    docs = []
    for m in sorted(_manifests(root), key=lambda p: (p.count("/"), p)):
        doc = _toml(root, m)
        ws = (doc.get("workspace") or {}).get("dependencies")
        if isinstance(ws, dict) and not workspace:
            workspace = ws
        docs.append(doc)

    def tables(doc: dict) -> Iterator[dict]:
        for key in ("dependencies", "dev-dependencies", "build-dependencies"):
            if isinstance(doc.get(key), dict):
                yield doc[key]
        for t in (doc.get("target") or {}).values():
            if isinstance(t, dict):
                for key in ("dependencies", "dev-dependencies", "build-dependencies"):
                    if isinstance(t.get(key), dict):
                        yield t[key]

    for doc in docs:
        for table in tables(doc):
            for key, spec in table.items():
                if isinstance(spec, dict) and spec.get("workspace") is True:
                    base = workspace.get(key)
                    spec = {**(base if isinstance(base, dict) else {"version": base}), **{k: v for k, v in spec.items() if k != "workspace"}}
                if isinstance(spec, str):
                    spec = {"version": spec}
                if not isinstance(spec, dict):
                    continue
                if "path" in spec and "version" not in spec:
                    continue
                version = spec.get("version") if isinstance(spec.get("version"), str) else None
                out.setdefault(key.replace("-", "_"), set()).add((spec.get("package") or key, version))
    return out


def _rust_crate_map(consumer: Path, libs: list[tuple[str, RustLib]]) -> dict[str, tuple[str, RustLib, str]]:
    """Code name → (library, its RustLib, crate) of the consumer's
    dependencies on library crates (see `label_rust`)."""
    deps = _cargo_dependencies(consumer)
    own = {(_toml(consumer, m).get("package") or {}).get("name", "").replace("-", "_") for m in _manifests(consumer)}
    crate_of: dict[str, tuple[str, RustLib, str]] = {}
    for code, wanted in sorted(deps.items()):
        if code in own:
            continue
        for lname, lib in libs:
            for crate, info in lib.crates.items():
                if any(pkg == info["package"] and (req is None or info["version"] is None or compatible(info["version"], req)) for pkg, req in wanted):
                    crate_of.setdefault(code, (lname, lib, crate))
    return crate_of


class _RustConsumer:
    """A consumer's files seen through its library dependencies: per file,
    the `use` bindings of library items and modules (its own `use`
    statements, plus those of the consumer modules it glob-imports:
    `use super::*` in a test module), and the library files they import."""

    def __init__(self, root: Path, libs: list[tuple[str, RustLib]]):
        self.root = root
        self.crate_of = _rust_crate_map(root, libs)
        self.own = RustLib(root)  # the consumer's own library crates: module tree for its glob imports
        self._scans: dict[str, _RustFile] = {}
        self._direct: dict[str, tuple[dict, set]] = {}

    def scan(self, rel: str) -> _RustFile:
        if rel not in self._scans:
            self._scans[rel] = self.own.files.get(rel) or _rust_scan(_read(self.root, rel))
        return self._scans[rel]

    def direct(self, rel: str) -> tuple[dict, set]:
        """(bound name → (library, RustLib, resolution), {(library, file)})
        of a file's own `use` statements naming a library crate."""
        if rel in self._direct:
            return self._direct[rel]
        bindings: dict[str, tuple[str, RustLib, tuple]] = {}
        files: set = set()
        for _, _, entries, _ in self.scan(rel).all_uses:
            for path, bound in entries:
                if not path or path[0] not in self.crate_of:
                    continue
                lname, lib, crate = self.crate_of[path[0]]
                res = lib.resolve_extern(crate, path[1:])[0]
                if res is None:
                    continue
                files.add((lname, lib.file_of(res)))
                if bound is not None:
                    bindings[bound] = (lname, lib, res)
        self._direct[rel] = (bindings, files)
        return self._direct[rel]

    def bindings(self, rel: str, depth: int = 0) -> dict:
        out = dict(self.direct(rel)[0])
        if depth > 3 or rel not in self.own.files:
            return out
        for _, _, entries, inline in self.scan(rel).all_uses:
            for path, bound in entries:
                if bound is not None or not path or path[0] in self.crate_of:
                    continue
                target = self.own.resolve((rel, inline), path, True)[0]
                if target and target[0] == "mod" and target[1][0] != rel:
                    for k, v in self.bindings(target[1][0], depth + 1).items():
                        out.setdefault(k, v)
        return out

    def label(self, rel: str) -> tuple[set, set]:
        """({(library, library file)} imported, {(library, qualified name)} used)."""
        f = self.scan(rel)
        bindings = self.bindings(rel)
        body = list(f.code)
        for start, end, _, _ in f.all_uses:
            body[start:end] = [" "] * (end - start)
        text = "".join(body)
        idents = set(_RUST_IDENT.findall(text))
        called = called_names(text)
        used: set = set()
        for bound, (lname, lib, res) in bindings.items():
            if res[0] != "item":
                continue
            if (bound != "_" and bound in idents) or (lib.kind(res) == "trait" and lib.methods(res) & called):
                used.add((lname, lib.qn(res[1], res[2])))
        for m in _RUST_PATH.finditer(text):
            segs = [x.strip() for x in m.group().split("::") if x.strip()]
            if segs[0] in self.crate_of:
                lname, lib, crate = self.crate_of[segs[0]]
                res = lib.resolve_extern(crate, tuple(segs[1:]))[0]
            elif segs[0] in bindings and bindings[segs[0]][2][0] == "mod":
                lname, lib, mod = bindings[segs[0]]
                res = lib._walk(mod[1], segs[1:], False, 0)[0]
            else:
                continue
            if res is not None and res[0] == "item":
                used.add((lname, lib.qn(res[1], res[2])))
        return self.direct(rel)[1], used


_IDENT_RE = re.compile(r"[A-Za-z_]\w*")
_WS = " \t\r\n"


def called_names(text: str) -> set[str]:
    """Names of the method calls in `text`: `.name(` and `.name::<` (a
    turbofish call), whitespace allowed around the name. One linear scan over
    identifiers (no regex with optional whitespace runs)."""
    out: set[str] = set()
    for m in _IDENT_RE.finditer(text):
        i = m.start() - 1
        while i >= 0 and text[i] in _WS:
            i -= 1
        if i < 0 or text[i] != ".":
            continue
        j = m.end()
        while j < len(text) and text[j] in _WS:
            j += 1
        if text.startswith("(", j) or (text.startswith("::", j) and text[j + 2 :].lstrip(_WS).startswith("<")):
            out.add(m.group())
    return out


def label_rust(consumer: Path, libs: list[tuple[str, RustLib]], only: set[str] | None = None) -> tuple[set, set, dict, dict]:
    """(imports, uses, ancestors, {}) of a Rust consumer, `only` restricting
    the consumer files labelled. A code name maps to a library crate when one
    of the consumer's manifests depends on its package with a requirement
    whose compatibility range holds the pinned version (`hyper1 = { package
    = "hyper", version = "=1.0.0-rc.4" }` is not hyper 0.14); the consumer's
    own crates shadow a library's."""
    imports: set = set()
    uses: set = set()
    c = _RustConsumer(consumer, libs)
    for rel in _files(consumer, ".rs"):
        if (only is not None and rel not in only) or "target" in rel.split("/")[:-1]:
            continue
        files, used = c.label(rel)
        imports |= {(rel, lname, file) for lname, file in files}
        uses |= {(rel, lname, qn) for lname, qn in used}
    lib_of = dict(libs)
    ancestors = {(lname, qn): lib_of[lname].ancestors(qn) for _, lname, qn in uses}
    return imports, uses, ancestors, {}


# --- release pins --------------------------------------------------------------------------

_VERSION = re.compile(r"v?(\d+)(?:\.(\d+))?(?:\.(\d+))?(?:[-.]?((?:rc|alpha|beta|a|b|dev|pre)[.\w-]*))?")


def parse_version(text: str) -> tuple[int, int, int, str] | None:
    """`1.2.3` / `v0.14.27` / `tower-0.4.13` / `1.0.0-rc.4` → (major, minor,
    patch, pre-release or ""); missing parts are 0."""
    m = _VERSION.search(text)
    if not m:
        return None
    return int(m.group(1)), int(m.group(2) or 0), int(m.group(3) or 0), m.group(4) or ""


def _vkey(v: tuple[int, int, int, str]) -> tuple:
    # a pre-release sorts before its release
    return (v[0], v[1], v[2], 0 if v[3] else 1, v[3])


def _comparators(req: str, eco: str) -> list[tuple[str, tuple[int, int, int, str], int]]:
    """(operator, version, number of version parts written) of a requirement."""
    out = []
    for part in req.split(","):
        part = part.strip()
        if not part or part == "*":
            continue
        m = re.match(r"(\^|~=|~|==|=|>=|<=|>|<|!=)?\s*(.+)", part)
        op = m.group(1) or ("^" if eco == "cargo" else "==")
        body = m.group(2).strip()
        v = parse_version(body.replace("*", "0"))
        if v is None:
            continue
        written = len(re.match(r"[\d.]*\d", body.lstrip("v")).group().split(".")) if re.match(r"v?\d", body) else 3
        if body.endswith(".*"):
            op = {"==": "~", "=": "~"}.get(op, op)
        out.append((op, v, written))
    return out


def _caret_lower(v: tuple[int, int, int, str], written: int) -> tuple[int, int, int, str]:
    """The first release of `v`'s compatibility series (`0.14.24` → `0.14.0`)."""
    if v[0] > 0 or written == 1:
        return (v[0], 0, 0, "")
    if v[1] > 0 or written == 2:
        return (0, v[1], 0, "")
    return (0, 0, v[2], "")


def _caret_upper(v: tuple[int, int, int, str], written: int) -> tuple[int, int, int, str]:
    if v[0] > 0 or written == 1:
        return (v[0] + 1, 0, 0, "")
    if v[1] > 0 or written == 2:
        return (0, v[1] + 1, 0, "")
    return (0, 0, v[2] + 1, "")


def satisfies(version: str, req: str, eco: str = "cargo") -> bool:
    """Whether `version` meets requirement `req`: Cargo (`1.25`, `^0.14.24`,
    `~1.2`, `=1.0.0-rc.4`, `>=1, <2`) or PEP 440 (`>=3.1.0`, `~=3.1`,
    `==3.*`, `!=3.0.1`)."""
    v = parse_version(version)
    if v is None:
        return False
    k = _vkey(v)
    for op, w, written in _comparators(req, eco):
        lo = _vkey(w)
        if op == "^":
            ok = lo <= k < _vkey(_caret_upper(w, written))
        elif op in ("~", "~="):
            if op == "~=" and written >= 2:
                up = (w[0] + 1, 0, 0, "") if written == 2 else (w[0], w[1] + 1, 0, "")
            else:
                up = (w[0] + 1, 0, 0, "") if written == 1 else (w[0], w[1] + 1, 0, "")
            ok = lo <= k < _vkey(up)
        elif op in ("=", "=="):
            ok = v[:written] == w[:written] and (written < 3 or v[3] == w[3])
        elif op == "!=":
            ok = v != w
        else:
            ok = {">=": k >= lo, "<=": k <= lo, ">": k > lo, "<": k < lo}[op]
        if not ok:
            return False
    return True


def compatible(version: str, req: str) -> bool:
    """Whether `version` is in the compatibility range of Cargo requirement
    `req` (the upper bound of its caret / tilde / exact comparators; the lower
    bound is not checked: a pinned library one patch or minor release below
    a consumer's minimum has the same API for the labeler)."""
    v = parse_version(version)
    if v is None:
        return False
    for op, w, written in _comparators(req, "cargo"):
        if op in ("^", "=", "=="):
            if not _vkey(_caret_lower(w, written)) <= _vkey(v[:3] + ("",)) < _vkey(_caret_upper(w, written)):
                return False
        elif op == "~" and v[:2] != w[:2]:
            return False
        elif op == "<" and not _vkey(v) < _vkey(w):
            return False
    return True


def pick_release(candidates: list[tuple[str, str, str]], req: str, before: str, eco: str = "cargo") -> tuple[str, str, str] | None:
    """The pin rule for a library outside the benchmark: among (tag, sha,
    ISO date) release tags, the highest version that meets the consumer's
    requirement and was published before the consumer pin's commit date
    (ISO 8601, compared as instants); pre-releases only when `req` names
    one."""
    best = None
    cut = _instant(before)
    for tag, sha, date in candidates:
        v = parse_version(tag)
        if v is None or (v[3] and "-" not in req and "rc" not in req) or _instant(date) >= cut or not satisfies(tag, req, eco):
            continue
        if best is None or _vkey(v) > _vkey(parse_version(best[0])):
            best = (tag, sha, date)
    return best


def _instant(iso: str) -> datetime:
    return datetime.fromisoformat(iso.replace("Z", "+00:00"))


# --- per language ---------------------------------------------------------------------------

LABELERS: dict[str, Callable] = {"python": label_python, "php": label_php, "java": label_java, "rust": label_rust}


def make_libs(language: str, roots: dict[str, Path], libraries: list[dict]) -> list[tuple[str, PyLib | PhpLib | JavaLib | RustLib]]:
    """(repo, library index) of a group's libraries checked out at `roots`."""
    if language == "python":
        return [(x["repo"], PyLib(roots[x["repo"]], x["packages"])) for x in libraries]
    cls = {"php": PhpLib, "java": JavaLib, "rust": RustLib}[language]
    return [(x["repo"], cls(roots[x["repo"]])) for x in libraries]


def symbol_name(qn: str, language: str) -> str:
    """The last segment of a library symbol's qualified name (`Carbon\\Carbon`
    → `Carbon`, `tokio::sync::broadcast::Sender` → `Sender`)."""
    return re.split(r"\\|::|\.", qn)[-1] if language in ("php", "rust") else qn.rsplit(".", 1)[-1]


def symbol_path(lib: PyLib | PhpLib | JavaLib | RustLib, qn: str, language: str) -> str | None:
    """The library file that defines symbol `qn` (a `uses` target)."""
    if language == "python":
        return lib.modules.get(qn.rsplit(".", 1)[0])
    if language == "rust":
        return lib.path_of(qn)
    return lib.classes.get(qn)


# --- scoring -------------------------------------------------------------------------------


def _owner_match(dst: str, target: str, language: str) -> bool:
    """`dst` (an arm's target qualified name) is `target` or a member of it."""
    if dst == target:
        return True
    seps = {"python": ".", "php": "::", "java": ".", "rust": "."}
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
    p = org_run_path("crossrepo", "rueca")
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
        gold = LABELERS[lang](roots[g["consumer"]], make_libs(lang, roots, g["libraries"]))
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
    p = org_run_path("crossrepo", "rueca")
    if not p.exists():
        return {}
    rec = read_json(p)
    if rec.get("status") != "ok":
        return {"status": rec.get("status"), "error": str(rec.get("error"))[:300]}
    return {"status": "ok", "summary": summarize(rec["groups"]), "groups": rec["groups"], "version": rec.get("arm_version")}
