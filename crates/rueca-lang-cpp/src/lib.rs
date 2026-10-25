//! C and C++ language pack for Rueca (`tree-sitter-c` 0.24 and
//! `tree-sitter-cpp` 0.23, one pack for both, language family `C`).
//!
//! See `rueca_extract::LanguagePack` for the contract. This crate turns one
//! C or C++ file into [`FileFacts`], resolves `#include` paths (with
//! `compile_commands.json` / `compile_flags.txt` include directories) and
//! lists builtins. `.c` is C; `.cc`, `.cpp`, `.cxx`, `.hpp`, … are C++; a
//! `.h` header is C++ when the snapshot has C++ sources, else C (decided
//! per snapshot by the indexer, `Language::header_language`).
//!
//! What is assembled and what is Rueca code (RFC 0002 §3.4, §4.1):
//!
//! - **Grammars**: `tree-sitter-c` and `tree-sitter-cpp` (MIT), linked with
//!   the tree-sitter 0.27 runtime (their `tree-sitter` requirement is a
//!   dev-dependency).
//! - **Local scopes** come from vendored, adapted `queries/c-locals.scm`
//!   and `queries/cpp-locals.scm` (nvim-treesitter, Apache-2.0; provenance
//!   in `queries/README.md`, semantics in `locals.rs`). They decide which
//!   names are locals (never reported) and give the declared types used
//!   for typed receivers.
//! - Definitions, containment, qualified names, overload suffixes, calls
//!   with receivers, includes, macro masking and include-directory
//!   resolution are Rueca code (`extract.rs`, `mask.rs`, `includes.rs`).
//!   Upstream `tags.scm` (MIT) is not vendored: it has no containment,
//!   qualified names or receivers.
//!
//! # Qualified names
//!
//! C and C++ code refers to names by namespace and class path, never to
//! files, so [`LanguagePack::module_name`] is empty and a definition's
//! local qualified name is global: the namespace and class path joined by
//! `::` (RFC 0003 §10.3).
//!
//! | Definition | Qualified name |
//! |---|---|
//! | function in namespaces | `fmt::v11::format_to` (inline namespaces included) |
//! | class, struct, union, enum | `geo::Circle`, nested `geo::Circle::Inner` |
//! | method, constructor, destructor, operator | `geo::Circle::area`, `geo::Circle::Circle`, `geo::Circle::~Circle`, `geo::Point::operator+`, `V::operator bool` |
//! | out-of-line body `void Foo::bar() {}` | `ns::Foo::bar` (enclosing namespaces + written qualifier; a leading `::` drops the enclosing namespaces) |
//! | field, static data member, enumerator | `geo::Circle::r_`, `Color::Red` (also for unscoped enums; anonymous enums: `ns::kA`) |
//! | typedef / `using` alias | `geo::Vec` |
//! | variable, constant | `ns::counter` |
//! | macro | `GEO_PI` (macros ignore namespaces) |
//! | anonymous namespace | contributes nothing (`describe`); its names are not exported |
//! | GoogleTest `TEST(Suite, Name)` | `ns::Suite::Name` (`name` is `Name`) |
//! | Catch2 / doctest `TEST_CASE("title")` | `title`; `TEST_CASE_METHOD(Fixture, "title")` → `ns::Fixture::title` |
//!
//! **Overloads** (RFC 0003 §10.1, open question 4): when one file has
//! callables with the same qualified name and different parameter lists,
//! **all** of them get the normalized parameter types as a suffix
//! (`geo::scale(double)`, `geo::scale(double, double)`, `P::put(const
//! std::string&, int)`; names, default values and attributes dropped,
//! `(void)` written `()`, a C variadic as `...`); a declaration and its body
//! with the same parameters share the suffix, so the declaration still
//! links to its body. Names that are not overloaded stay bare. The resolver
//! picks an overload by argument count, else links all of them at × 0.8;
//! `qualified_name` lookups treat the suffix as optional. Limits: overloads
//! split across files (two `.cpp` files each defining one) are not seen as
//! overloads there, and default arguments are not part of the arity
//! (`f(int, int = 0)` called with one argument links every overload).
//!
//! **Templates** are named without arguments: a class template and its
//! explicit specializations share one qualified name (`Vec`), and the
//! resolver merges same-named definitions of one file into one node.
//!
//! # Definitions
//!
//! | Source | Label | Notes |
//! |---|---|---|
//! | function definition (also `static`, `inline`, templates, trailing return) | `Function` | `exported` unless `static` or in an anonymous namespace |
//! | prototype `void f(int);` | `Function`, `is_declaration` | |
//! | `class` | `Class` | `bases` from the base clause; default access private |
//! | `struct`, `union` | `Struct` | default access public; anonymous struct / union members belong to the enclosing class |
//! | `enum`, `enum class` | `Enum` | enumerators → `Constant` children |
//! | method declared in a class | `Method`, `is_declaration` | also constructors / destructors / operators / conversion operators; pure virtual methods |
//! | method defined in a class (also `= default` / `= delete`) | `Method` | |
//! | out-of-line body `R Foo::bar(…) {…}` | `Method`, top level (no parent), not `exported` | `Function` when the qualifier is a namespace of the file (`void ns::f() {}`) |
//! | field, bit field, function-pointer field | `Field` | `static const` / `constexpr` data members → `Constant` |
//! | `int Foo::count = 0;` | `Field`, top level | |
//! | namespace-level variable | `Variable`; `Constant` when `constexpr`, or `const` and not a pointer | `extern` without initializer → `is_declaration` |
//! | `typedef`, `using X = T;` | `TypeAlias` | `bases` = the aliased type, so members are found through the alias (`node_t` → `node`) |
//! | `typedef struct tag {…} name_t;` | `Struct` `tag` + `TypeAlias` `name_t` | an anonymous struct takes the alias name; `typedef struct x x;` adds nothing |
//! | `#define NAME value` | `Constant` | top level, global name; valueless include guards skipped |
//! | `#define NAME(args) body` | `Function` | signature = the define line |
//! | `TEST`, `TEST_F`, `TEST_P`, `TYPED_TEST`, `TYPED_TEST_P` | `Test`, `is_test` | references in the body belong to the test |
//! | `TEST_CASE`, `TEST_CASE_METHOD`, `SCENARIO`, `TEMPLATE_TEST_CASE`, `TEST_CASE_FIXTURE` | `Test`, `is_test` | the grammar's missing `;` after the head is not counted as a parse error |
//!
//! Namespaces are not definitions (they only qualify names), forward
//! declarations (`class Foo;`) and local classes / lambdas inside function
//! bodies are not definitions, and nesting is walked to 64 namespace /
//! class levels.
//!
//! - `span` covers a wrapping `template <…>` and the whole declaration;
//!   `name_span` is the declared name (for out-of-line bodies the last
//!   identifier, for tests the second macro argument or the title).
//! - `signature`: the source from the declaration's start to its body,
//!   initializer list or `;`, comments removed, whitespace collapsed, ≤ 200
//!   chars, read from the original text (masked macros included): `class
//!   FMT_API format_error : public std::runtime_error`, `double
//!   geo::Circle::radius() const`, `#define GEO_SQ(x) ((x) * (x))`.
//! - `doc`: the comments right before the declaration (or its `template`),
//!   with no blank line in between and not trailing code on their line:
//!   `///`, `//!`, `//` blocks, `/** */`, `/*! */`, `/* */`; markers and
//!   leading `*` stripped, ≤ 1000 chars. Doxygen `///<` member comments are
//!   not docs of the next member.
//! - `exported`: namespace-level definitions unless `static` or in an
//!   anonymous namespace; members when public; macros always; out-of-line
//!   bodies and tests never (the in-class declaration carries the
//!   visibility, and an include must not bind a method body by its bare
//!   name).
//!
//! # References
//!
//! `enclosing` is the innermost definition (a field or variable for its
//! initializer, the class for its base clause, `None` at file level).
//! Locals (parameters, local variables, `for` / range-`for` / `catch` /
//! lambda bindings, structured bindings) and template type parameters are
//! never references.
//!
//! - `Call`: `f()` → `f`; `obj.m()` / `p->m()` → `m` with receiver
//!   `$type:T` when the declared type of `obj` is known in the file (a
//!   parameter, local, field of the enclosing class, or namespace-level
//!   variable declared `T`, `T*`, `const T&`, `T x(…)`, `auto x = new T` /
//!   `std::make_unique<T>()`; also `this->field.m()` and `a.b.m()` through
//!   fields of classes of the file), `this`, else the receiver's text (≤ 200
//!   chars); `arity` = argument count. Calls of function-like macros are
//!   calls (`GEO_SQ(r)`), casts (`static_cast<T>`) are not.
//! - **Qualified calls** `Q::f()`: a qualifier rooted in `std`, `testing`,
//!   `Catch`, `doctest` or `__gnu_cxx` is kept as receiver (external). A
//!   qualifier that is a type — written with template arguments, written
//!   as a type anywhere in the file, or not a namespace of the file and
//!   capitalized — gives `$type:Q` (a static member); a relative qualified
//!   type inside a namespace is prefixed with it (`detail::buffer` inside
//!   `fmt::v11` → `$type:fmt::v11::detail::buffer`) for an exact match. A
//!   namespace qualifier gives no receiver (namespaces are not nodes): the
//!   name resolves through includes, the same file and unique names; inside
//!   a method with a member of that name, the qualifier is kept so the
//!   implicit-self rung does not pick the member.
//! - `New`: `new T(…)` → `T` with its arity; a nested class `new
//!   Outer::Inner` → `Inner` with receiver `$type:Outer`.
//! - Constructions: `T x(…)`, `T x{…}` and `T{…}` are `Call`s of `T` (the
//!   resolver adds the constructor, a member named like the class).
//! - `Throw`: `throw E(…)` / `throw E{…}` → `E` (also a call).
//! - `Type`: types of parameters, returns, fields, variables, casts,
//!   `sizeof`, template arguments and base clauses; a qualified type gives
//!   its last name (`ns::Foo` → `Foo`) unless rooted in a builtin
//!   namespace. Fundamental types and `auto` are skipped.
//! - `Read`: non-local identifiers in expressions (globals, enumerators,
//!   object-like macros, implicit members, functions passed as values),
//!   `Q::name` (as qualified calls), `this->x` / `obj.x` with a typed
//!   receiver. Deduplicated per `(enclosing, receiver, name)`.
//! - `Write`: assignment and increment targets (`g = 1`, `this->n = v`,
//!   `++n`).
//! - `EnvVar`: `getenv("NAME")`, `secure_getenv("NAME")`,
//!   `std::getenv("NAME")` with a literal name (RFC 0003 §7.2).
//! - At most [`MAX_REFS`] references per file.
//!
//! # Includes
//!
//! Every `#include "x.h"` / `#include <x.h>` outside comments is an import
//! of `x.h` / `<x.h>` with the single name `*` (a star import), read
//! lexically so includes survive parse errors around them; `#include MACRO`
//! is skipped. The resolver treats C/C++ star imports transitively (depth ≤
//! 10), so a file sees the exported top-level names of everything it
//! includes. [`LanguagePack::resolve_import`] maps the path to a project
//! file: relative to the including file, then the `-iquote` and `-I`
//! directories of every `compile_commands.json` and `compile_flags.txt`
//! (`-isystem` ignored; absolute paths mapped into the repository through
//! the entries' `file` paths), then `include/`, `src/` and the repository
//! root, then the only project file whose path ends with the written path.
//! `<x.h>` skips the including directory, `-iquote` and `src/` / root, and
//! uses the suffix match only for paths with a `/`. See `includes.rs`.
//!
//! # Macros and conditional compilation
//!
//! There is no preprocessor. Both branches of `#if` are parsed, code
//! produced by macro expansion is invisible, and macros that expand to
//! declaration syntax (`FMT_API`, `FMT_BEGIN_NAMESPACE`, `Q_OBJECT`,
//! `FMT_ENABLE_IF(…)` in a template head, `extern "C" {` split by `#ifdef
//! __cplusplus`, function headers written once per `#if` branch) are
//! **masked** before parsing: replaced by spaces when their position makes
//! any other reading invalid C/C++ (`mask.rs` lists the nine rules). Byte
//! offsets and lines are preserved and all text is read from the original
//! source. A masked namespace macro (`FMT_BEGIN_NAMESPACE`) contributes
//! nothing to qualified names. [`mask_macros`] exposes the masked text.
//!
//! # Deviations and limitations
//!
//! - Parse errors remain common on real code (the grammars cannot parse
//!   `va_arg(ap, char*)`, functional casts of keyword types
//!   `unsigned(x)`, statement macros whose arguments are statements,
//!   splices of statements across `#if` branches, C++26 annotations): the
//!   smoke repositories have parse errors in 17–40% of their files, mostly
//!   one to a few local errors per file (see RFC 0003 §10.3).
//! - Receiver typing is syntactic and file-local: return types are not
//!   followed (`make().m()`), and fields declared in a header are not known
//!   while extracting an out-of-line body in a `.cpp` (`member_.m()` there
//!   falls back to the unique-method rung; `m()` and `this->m()` resolve
//!   through implicit self in the resolver).
//! - Lower-case macros (`ffc_inline`, `__unused`) and macros in other
//!   positions are not masked.
//! - `OVERRIDES` edges join the in-class declarations (where the override
//!   relation is written), not the out-of-line bodies.
//! - ADL, overload choice by argument types, calls through function
//!   pointers and virtual dispatch are not modelled (RFC 0003 §10.3
//!   *Heuristic*).
//! - C++20 modules are out of scope.

mod builtins;
mod extract;
mod includes;
mod locals;
mod mask;
mod text;

use rueca_extract::{Budget, ExtractError, LanguagePack, ProjectFiles};
use rueca_model::{FileFacts, Import, Language};

pub use extract::MAX_REFS;

/// `source` with the macro tokens the pack masks before parsing replaced by
/// spaces (crate docs, *Macros*); offsets and lines are unchanged.
pub fn mask_macros(source: &[u8]) -> Vec<u8> {
    mask::mask(source).unwrap_or_else(|| source.to_vec())
}

/// C and C++ pack (one pack for both languages, family `C`). Construct with
/// `CppPack::default()`; it holds only a cache of the include directories
/// of the most recent snapshots.
#[derive(Default)]
pub struct CppPack {
    includes: includes::IncludeCache,
}

impl std::fmt::Debug for CppPack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CppPack").finish_non_exhaustive()
    }
}

/// Whether `name` is a C / C++ standard-library or test-framework name
/// (see `builtins.rs`).
pub(crate) fn is_builtin_name(name: &str) -> bool {
    builtins::BUILTINS.binary_search(&name).is_ok()
}

impl LanguagePack for CppPack {
    fn languages(&self) -> &'static [Language] {
        &[Language::Cpp, Language::C]
    }
    fn extractor_version(&self) -> u32 {
        1
    }
    fn extract(
        &self,
        _path: &str,
        language: Language,
        source: &[u8],
        budget: &Budget,
    ) -> Result<FileFacts, ExtractError> {
        extract::extract(source, language, budget)
    }
    /// Always empty: C and C++ have no modules; definitions carry their
    /// namespace and class path (crate docs, *Qualified names*).
    fn module_name(&self, _path: &str, _project: &ProjectFiles) -> String {
        String::new()
    }
    fn resolve_import(&self, from_path: &str, import: &Import, project: &ProjectFiles) -> Option<String> {
        self.includes.get(project).resolve(from_path, &import.module, project)
    }
    fn is_builtin(&self, name: &str) -> bool {
        is_builtin_name(name)
    }
    fn self_receivers(&self) -> &'static [&'static str] {
        &["this"]
    }
    fn implicit_self(&self) -> bool {
        true
    }
    fn overloads(&self) -> bool {
        true
    }
    fn overrides(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests;
