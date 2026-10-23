//! Python language pack for Telar (`tree-sitter-python` 0.25).
//!
//! See `telar_extract::LanguagePack` for the contract. This crate turns one
//! `.py` / `.pyi` file into [`FileFacts`] and implements Python module
//! semantics (module names, import resolution, builtins).
//!
//! # Definitions
//!
//! | Source | Label | Notes |
//! |---|---|---|
//! | `def` / `async def` at module level | `Function` | |
//! | `def` directly in a class body (also inside `if`/`try` there) | `Method` | |
//! | `def` nested in a function | `Function` | `parent` = the outer function |
//! | `class` | `Class` | `bases` = positional base expressions (keywords such as `metaclass=` and `*`/`**` splats skipped) |
//! | module-level `X = …` / `X: T = …` / `X: T` | `Constant` if `UPPER_CASE`, else `Variable` | tuple targets give one per name; first assignment wins |
//! | class-body assignment | `Field` | |
//! | `self.x = …` in `__init__` | `Field` of the class | deduplicated by name, first occurrence (class body first) wins |
//! | `type X = …` (PEP 695) | `TypeAlias` | |
//!
//! - `qualified_name` is module-relative: `Class.method`, `outer.inner`,
//!   `Class.Nested.m`.
//! - `span` covers decorators; `name_span` is the identifier.
//! - `signature`: the def/class header up to (excluding) the final `:`,
//!   comments removed and whitespace collapsed to one line, e.g.
//!   `def request(self, method, url, params=None) -> Response`; truncated to
//!   200 chars. Variables/constants/fields/type aliases use the first line of
//!   their statement (`TIMEOUT = 30`).
//! - `doc`: the docstring (first statement string literal; f-strings and
//!   bytes excluded), quotes stripped, `inspect.cleandoc`-dedented, truncated
//!   to 1000 chars.
//! - `decorators`: decorator expressions without `@`, one line.
//! - `exported`: module-level and not `_`-prefixed; if a literal `__all__`
//!   list/tuple of strings exists (`=` plus `+=` extensions), exported iff
//!   listed. Non-module-level definitions are never exported.
//! - `is_test`: functions/methods named `test` / `test_*`, classes named
//!   `Test*`, and methods named `test*` inside `Test*` classes.
//!
//! # References
//!
//! `enclosing` is the innermost enclosing definition (`None` at module level).
//! Decorator, base-class, parameter annotation and default-value references
//! are attributed to the decorated/declared definition itself (they are inside
//! its span); a variable/field annotation is attributed to the variable.
//!
//! - `Call`: `f(…)` → `f`; `a.b.c(…)` → name `c`, receiver `a.b`. Non-chain
//!   receivers (`get().x()`) use their one-line source text (≤ 200 chars) and
//!   are also visited. Calls are emitted for every callee (builtins included;
//!   the resolver filters with [`LanguagePack::is_builtin`]).
//! - `Decorator`: last identifier of the decorator callee, receiver = prefix
//!   (`@app.route("/x")` → `route` / `app`). Arguments are visited normally.
//! - `Throw`: `raise X` / `raise X(…)` → `X` (local variables such as
//!   `raise err` are skipped). The constructor call is not also a `Call`.
//! - `Type`: every non-builtin identifier or dotted name in parameter, return,
//!   variable and type-alias annotations (`Optional[Foo]` → `Optional`, `Foo`),
//!   simple string forward references (`"pkg.Foo"`), and each class base.
//! - `Read`: identifiers and attribute chains in value position whose root
//!   is neither a builtin nor a local binding (see *Local bindings* below).
//!   `self.x` / `cls.x` (also as the prefix of a longer chain or call
//!   receiver, `self.repo.save()`) yields a read of field `x` with receiver
//!   `self`. The target of `x += …` is also a read. Deduplicated per
//!   `(enclosing, receiver, name)`.
//! - `Write`: assignments (including augmented) to `self.x` / `cls.x`,
//!   deduplicated like reads.
//! - No references for keyword-argument names, string contents (f-string
//!   interpolations are code and are visited), comments or `match` patterns.
//! - At most 20,000 references per file.
//!
//! # Local bindings (vendored `queries/locals.scm`)
//!
//! Which identifiers are locals is decided by a tree-sitter locals query,
//! not by ad-hoc tree walking (RFC 0002 §3.4). `queries/locals.scm` is
//! adapted from nvim-treesitter's `python/locals.scm` (Apache-2.0, commit
//! `29212d82`) with lambda / generator scopes, lambda parameters and `global`
//! from bloop's `python/scopes.scm` (Apache-2.0, commit `431e9e82`); the
//! file header and `queries/README.md` record the exact URLs and every
//! adaptation. The query is compiled once per process and run per file with a
//! `QueryCursor` (match limit 1024, deadline = `Budget::max_parse_time`) to
//! build a scope table: `@local.scope` nodes, `@local.definition.<kind>`
//! bindings, `@local.global` / `@local.nonlocal` declarations and
//! `@local.reference` uses. The Python scope rules a query cannot express are
//! applied in Rust (`locals.rs`):
//!
//! - a name bound anywhere in a function / lambda / comprehension is local to
//!   all of it, and visible to nested functions (closures);
//! - `def` / `class` names bind in the enclosing scope; `:=` binds outside
//!   comprehensions; class-body names are visible only in the class body
//!   (PEP 695 type parameters also in its methods);
//! - defaults, annotations and bases are resolved in the enclosing scope
//!   (`def f(limit=limit)` reads the outer `limit`);
//! - `global x` resolves at module level, `nonlocal x` in an outer function;
//! - imports bind names but are never "local", so reads of imported names are
//!   reported for the resolver;
//! - at module level only loop / comprehension / `with` / `except` / walrus /
//!   `match` targets are local; module variables, functions and classes are
//!   reported when read.
//!
//! Known gaps: the first iterable of a comprehension is resolved inside the
//! comprehension, `del` does not unbind, and if the cursor hits its match
//! limit or deadline the table is partial (unknown identifiers count as
//! non-local, i.e. more `Read`s, never fewer facts). PEP 695 type parameters
//! never produce `Type` references.
//!
//! Containment and qualified names, imports, inheritance and calls with
//! receivers are written in Rust (RFC 0002 §3.4: no permissive query source
//! has them at graph quality); the upstream `tags.scm` is not vendored
//! because it adds nothing beyond that code.
//!
//! # Imports
//!
//! - `import a.b.c` → module `a.b.c`, no names; `import a.b as x` →
//!   `module_alias = x`. One [`Import`] per imported module.
//! - `from a.b import c, d as e` → names `c`, `d` (alias `e`);
//!   `from . import x` → module `""`, level 1; `from ..p.m import *` → name
//!   `*`, level 2, module `p.m`; `from __future__ import …` → module `__future__`.
//! - Imports anywhere are recorded (functions, `try`); `type_only` is true
//!   inside `if TYPE_CHECKING:` / `if typing.TYPE_CHECKING:`. `reexport` is
//!   always false.
//!
//! # Module names and source roots
//!
//! A file in a package (its directory has `__init__.py[i]`) is named relative
//! to the parent of its topmost package directory, unless that parent lies
//! below a known root (a `src/` ancestor or a configured root), in which case
//! the known root is used (namespace packages under `src/`). A file outside a
//! package is named relative to the deepest known root containing it, else the
//! repository root. Configured roots come from `pyproject.toml`
//! (`[tool.setuptools] package-dir = {"" = "lib"}`, `[tool.setuptools.packages.find]
//! where`, poetry `from = "…"`, hatch wheel `packages`) and `setup.cfg`
//! (`[options] package_dir = =lib`, `[options.packages.find] where`).
//!
//! Absolute imports are tried against the repository root, `src/`, configured
//! roots and the importing file's own source root, in that order, as
//! `a/b/c.py`, `a/b/c.pyi`, `a/b/c/__init__.py`, `a/b/c/__init__.pyi`.
//! Relative imports start at the importing file's directory, go up
//! `level - 1` directories and append the module path; `from . import x`
//! resolves to the package `__init__.py`.

mod builtins;
mod extract;
mod locals;
mod modules;
mod text;

use telar_extract::{Budget, ExtractError, LanguagePack, ProjectFiles};
use telar_model::{FileFacts, Import, Language};

pub use extract::MAX_REFS;

/// Python pack. Stateless; construct with `PythonPack::default()`.
#[derive(Debug, Default)]
pub struct PythonPack;

/// `dir(builtins)` of CPython 3.13 plus the implicit module globals
/// `__file__`, `__builtins__`, `__cached__`, `__path__`.
pub(crate) fn is_builtin_name(name: &str) -> bool {
    builtins::BUILTINS.binary_search(&name).is_ok()
}

impl LanguagePack for PythonPack {
    fn languages(&self) -> &'static [Language] {
        &[Language::Python]
    }
    fn extractor_version(&self) -> u32 {
        1
    }
    fn extract(&self, _path: &str, language: Language, source: &[u8], budget: &Budget) -> Result<FileFacts, ExtractError> {
        if language != Language::Python {
            return Err(ExtractError::Unsupported(language));
        }
        extract::extract(source, budget)
    }
    fn module_name(&self, path: &str, project: &ProjectFiles) -> String {
        modules::module_name(path, project)
    }
    fn resolve_import(&self, from_path: &str, import: &Import, project: &ProjectFiles) -> Option<String> {
        modules::resolve_import(from_path, import, project)
    }
    fn is_builtin(&self, name: &str) -> bool {
        is_builtin_name(name)
    }
    fn self_receivers(&self) -> &'static [&'static str] {
        &["self", "cls"]
    }
}

#[cfg(test)]
mod tests;
