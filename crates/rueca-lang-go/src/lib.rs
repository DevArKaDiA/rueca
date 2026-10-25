//! Go language pack for Rueca (`tree-sitter-go` 0.25).
//!
//! See `rueca_extract::LanguagePack` for the contract. This crate turns one
//! `.go` file into [`FileFacts`] and implements Go's module model (import
//! paths from `go.mod` / `go.work`, packages as directories), import
//! resolution and builtins. Design: RFC 0003 §10.2.
//!
//! What is assembled and what is Rueca code (RFC 0002 §3.4, §4.1):
//!
//! - **Grammar**: `tree-sitter-go` 0.25 (MIT), linked against the
//!   tree-sitter 0.27 runtime (it depends only on `tree-sitter-language`).
//! - **Local scopes** come from a vendored, adapted `queries/locals.scm`
//!   (nvim-treesitter and bloop, Apache-2.0; provenance in
//!   `queries/README.md`, semantics in `locals.rs`). They decide which
//!   identifiers are locals and drive receiver typing.
//! - Definitions, qualified names, imports, embedding, calls with receivers,
//!   the module model and the import-name heuristics are Rueca code
//!   (`extract.rs`, `modules.rs`). Upstream `tags.scm` (MIT) is not
//!   vendored: it has no containment, receivers or qualified names.
//!
//! # Module names and qualified names
//!
//! A Go package is a directory, and code refers to it by **import path**.
//! The module name of a file is its package's import path: the `module` of
//! the nearest ancestor `go.mod` plus the relative directory
//! (`github.com/gin-gonic/gin/binding`); without a `go.mod`, the directory
//! path. Files of an external test package (`package gin_test`) are the
//! module `<import path>_test` (read from the facts by
//! [`LanguagePack::module_name_for`]). The pack is package-scoped
//! ([`ModuleScope::Package`]): every file with the same module name is one
//! namespace, so names declared in another file of the package resolve
//! without imports.
//!
//! Global qualified name = import path + `.` + local name, as Go tooling
//! prints them: `github.com/gin-gonic/gin.Engine.ServeHTTP`.
//!
//! | Definition | Label | Local qualified name |
//! |---|---|---|
//! | `func F()` | `Function` | `F` |
//! | `func (e *Engine) Run()` (pointer or value receiver, also generic `List[T]`) | `Method` | `Engine.Run`, top level (`parent` = none): the resolver attaches it to `Engine` through package scope, wherever the type is declared |
//! | `type T struct {…}` | `Struct` | `T`; fields → `Field` `T.f`; embedded fields → `Field` named after the type (`T.Base`) and a base |
//! | `type I interface {…}` | `Interface` | `I`; methods → `Method` `I.M`; embedded interfaces → bases; union / `~T` terms (constraints) are references only |
//! | any other `type T …` and `type A = B` | `TypeAlias` | `T`; an alias (`=`) lists its target as a base, so the target's members are found through it |
//! | package-level `const` / `var` (each name; `_` skipped) | `Constant` / `Variable` | `X` |
//! | `package x_test` | `Package` (test) | `x_test` — only in external test packages, to mark the module |
//!
//! - Local types, variables and function literals are not definitions:
//!   their code is attributed to the enclosing function.
//! - Type parameters (`func Map[T any]`, `type List[T any]`, `func (l
//!   *List[T])`) are never definitions or references.
//! - `bases`: embedded struct fields and embedded interfaces as written,
//!   without `*` (`Base`, `pkg.Other`, `List[int]`), and the target of a
//!   type alias → `INHERITS` (method promotion is inherited member lookup).
//! - `span` covers the declaration (the whole `type` / `var` / `const`
//!   declaration when it has one spec, else the spec); `name_span` is the
//!   identifier.
//! - `signature`: the declaration up to its body, one line, ≤ 200 chars:
//!   `func (engine *Engine) ServeHTTP(w http.ResponseWriter, req *http.Request)`,
//!   `type Engine struct`, `var ErrX = errors.New("x")`, `Name string` for
//!   fields (tags dropped).
//! - `doc`: the `//` or `/* */` comment block directly above (no blank line;
//!   for a spec in a group, its own comment, else the group's when the
//!   group has one spec), delimiters stripped, directive lines
//!   (`//go:generate`, `//nolint:…`) and trailing comments of the previous
//!   line dropped, ≤ 1000 chars.
//! - `exported`: the name starts with an upper-case letter.
//! - `is_test`: in `_test.go` files, functions named `Test…`, `Benchmark…`,
//!   `Fuzz…` or `Example…` (followed by nothing or a non-lower-case
//!   character, as `go test` requires). Because this depends on the file
//!   name, [`LanguagePack::facts_depend_on_path`] is true.
//!
//! # References
//!
//! `enclosing` is the innermost definition: functions and methods for their
//! signatures and bodies (function literals included), fields and interface
//! methods for their types, a package-level `var` / `const` for its
//! initializer (`None` for blank names).
//!
//! - `Call`: `f(x)` → `f`; `x.M(…)` → `M` with a receiver (below);
//!   `F[int](x)` → `F`; conversions `T(x)` are calls of `T`. `arity` = the
//!   argument count. Calls of local function values are not reported;
//!   builtins (`len`, `append`, `panic`) are, and the resolver filters
//!   them with [`LanguagePack::is_builtin`].
//! - `New`: composite literals of a named type, `T{…}`, `&pkg.T{…}`,
//!   `List[int]{…}` (struct literal keys are field names, not references).
//! - `Type`: named types in parameter, result, field, `var`, conversion,
//!   assertion, type-switch, constraint, `new(T)` / `make(T)` and
//!   instantiation positions; `pkg.T` → `T` with receiver `pkg`.
//!   Predeclared types and type parameters are skipped.
//! - `Read`: non-local identifiers in value position (package-level names,
//!   function values such as `r.GET("/", handler)`), `pkg.Name` not called
//!   (`Name` with receiver `pkg`), and `recv.field` (receiver `$self`).
//! - `Write`: assignments, `++` / `--` to package-level variables and to
//!   `recv.field` (receiver `$self`).
//! - `EnvVar`: `os.Getenv("X")` / `os.LookupEnv("X")` with `os` imported in
//!   the file and a literal name. `ConfigKey`: `viper.Get…("a.b")`,
//!   `IsSet`, `UnmarshalKey`, `Sub` with `github.com/spf13/viper` imported;
//!   namespace `viper`, or `viper:<name>` when the file calls
//!   `viper.SetConfigName("<name>")` (RFC 0003 §7.2).
//! - `Read` / `Write` / `Type` are deduplicated per
//!   `(enclosing, receiver, name)`; at most 20,000 references per file.
//!
//! # Receivers
//!
//! The receiver text of `x.M()` says what the pack knows about `x` from
//! this file alone (RFC 0003 §9.2 reserved texts):
//!
//! | `x` is | Receiver |
//! |---|---|
//! | the method's receiver (`func (s *Server)`) | `$self` (listed in [`LanguagePack::self_receivers`]) |
//! | a local with a known type: typed parameter (`c *gin.Context`, also in function literals), `var x T`, `x := T{}` / `&T{}` / `new(T)`, `x := f()` for a function `f` of this file with a named result type, `x := NewT()` / `pkg.NewT()` (constructor convention, a heuristic), `x := y.(*T)`, a type-switch alias inside a single-type `case`, a copy of a typed local | `$type:T` (`$type:gin.Context`) |
//! | a package-level `var` of this file with a known type | `$type:T` |
//! | an untyped local named `err` | `$type:error` (Go convention; `error` is external) |
//! | `r.a.b` chains where every base has a struct type declared in this file | `$type:<type of b>` |
//! | `T{}.M()`, `NewT().M()`, `x.(*T).M()` | `$type:T` |
//! | a chain that reaches a package-qualified type: `c.Request.Header` with `Request *http.Request`, `w.Header()` with `w := httptest.NewRecorder()` | that type, then the rest of the chain without calls: `http.Request.Header`, `httptest.Recorder.Header` |
//! | a chain rooted at an import: `template.New("t")` | the chain without calls: `template.New` |
//! | an untyped local initialized by `q.F(…)` (`r := require.New(t)`) | `q.F` (`require.New`) |
//! | an import binding (`pkg.F()`) or anything else | the source text (`pkg`, `get()`); an untyped local named like an import or package-level name is parenthesized (`(url)`) so it never binds to them |
//!
//! The chain forms exist for one reason: a receiver whose head binds a
//! third-party import resolves as *external* (an expected miss) instead of
//! a name-only guess; through a project package they fall back to the
//! unique-method rung exactly as the raw text would. Return types of
//! methods and of functions in other files are not followed (`a.B().C()`
//! on a project type is a name-only guess in the resolver).
//!
//! # Imports
//!
//! | Source | `module` | `names` | `module_alias` |
//! |---|---|---|---|
//! | `import "net/http"` | `net/http` | | `http` (conventional name, below) |
//! | `import h "net/http"` | `net/http` | | `h` |
//! | `import . "strings"` | `strings` | `*` | |
//! | `import _ "embed"` | `embed` | | |
//!
//! Binding name of an unaliased import (RFC 0003 §10.2): Go binds the
//! target's `package` clause, which a pack cannot read for another file.
//! The pack uses the conventional name — the last path element without a
//! `/vN` element, a `.vN` suffix, a `go-` prefix or a `-go` suffix
//! (`github.com/mattn/go-isatty` → `isatty`) — and, because Go rejects
//! unused imports, re-binds an import whose conventional name is never used
//! as a qualifier in the file: to the one unexplained qualifier the import
//! path contains (`jsoniter` for `github.com/json-iterator/go`), else to the
//! only unexplained qualifier when exactly one import is unmatched.
//!
//! [`LanguagePack::resolve_import`] returns the package **directory** when
//! the import path is under a project module (longest module-path prefix
//! over every `go.mod` and local `replace` of `go.mod` / `go.work`) and the
//! directory holds `.go` files; standard library and third-party imports
//! return `None`. The resolver then adds `IMPORTS` File → Folder and binds
//! the package in that directory (preferring the module named like the
//! import path, so an external test package is never bound by mistake).
//! `vendor/` is excluded by the indexer, so vendored code is third-party.
//! `internal/` packages resolve like any other (visibility not enforced).
//!
//! # Interfaces
//!
//! Go interfaces are satisfied implicitly. `rueca-resolve` adds heuristic
//! `IMPLEMENTS` edges (and `OVERRIDES` from each method to the interface
//! method, `heuristic` 0.5) in a Go-only post-pass when a type's method
//! names — own and promoted through embedding — cover an interface's
//! (≥ 1 method; ≥ 2 known methods when the interface embeds one outside
//! the project, such as `fmt.Stringer`), at most 64 implementers per
//! interface (see the `rueca_resolve` crate docs, *Go*).
//!
//! # Limitations
//!
//! - Types are file-local: a field declared in another file of the package,
//!   method results and generic instantiations are not used for receivers.
//! - Method values, function-typed fields and interface dispatch resolve
//!   by name only (heuristic); calls through embedded fields of external
//!   types are external.
//! - Build tags and `cgo` are ignored: every `.go` file is indexed, and
//!   files of one directory with different `package` clauses (other than
//!   `_test`) share one module.
//! - `testdata/` directories are indexed like any other directory.

mod builtins;
mod extract;
mod locals;
mod modules;

use rueca_extract::{Budget, ExtractError, LanguagePack, ModuleScope, ProjectFiles, SELF_RECEIVER};
use rueca_model::{FileFacts, Import, Label, Language};

pub use extract::MAX_REFS;

/// Go pack. Construct with `GoPack::default()`. Holds only a cache of the
/// module tables (`go.mod` / `go.work`) of the most recent snapshots.
#[derive(Default)]
pub struct GoPack {
    modules: modules::ModulesCache,
}

impl std::fmt::Debug for GoPack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GoPack").finish_non_exhaustive()
    }
}

fn parent_dir(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

/// Whether facts declare an external test package (`package x_test`).
fn is_external_test(facts: &FileFacts) -> bool {
    facts.definitions.iter().any(|d| d.label == Label::Package && d.parent.is_none() && d.name.ends_with("_test"))
}

impl LanguagePack for GoPack {
    fn languages(&self) -> &'static [Language] {
        &[Language::Go]
    }
    fn extractor_version(&self) -> u32 {
        1
    }
    fn extract(
        &self,
        path: &str,
        language: Language,
        source: &[u8],
        budget: &Budget,
    ) -> Result<FileFacts, ExtractError> {
        if language != Language::Go {
            return Err(ExtractError::Unsupported(language));
        }
        extract::extract(path, source, budget)
    }
    /// The import path of the file's package (crate docs, *Module names*).
    fn module_name(&self, path: &str, project: &ProjectFiles) -> String {
        self.modules.get(project).import_path(parent_dir(path))
    }
    /// [`LanguagePack::module_name`], plus `_test` for files of an external
    /// test package.
    fn module_name_for(&self, path: &str, facts: Option<&FileFacts>, project: &ProjectFiles) -> String {
        let base = self.module_name(path, project);
        if facts.is_some_and(is_external_test) { format!("{base}_test") } else { base }
    }
    fn resolve_import(&self, _from_path: &str, import: &Import, project: &ProjectFiles) -> Option<String> {
        let dir = self.modules.get(project).package_dir(&import.module)?;
        modules::has_go_files(project, &dir).then_some(dir)
    }
    fn is_builtin(&self, name: &str) -> bool {
        builtins::is_predeclared(name)
    }
    fn self_receivers(&self) -> &'static [&'static str] {
        &[SELF_RECEIVER]
    }
    fn module_scope(&self, _language: Language) -> ModuleScope {
        ModuleScope::Package
    }
    /// `is_test` depends on the `_test.go` suffix.
    fn facts_depend_on_path(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests;
