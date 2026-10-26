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
//! | inside a namespace macro (`FMT_BEGIN_NAMESPACE` … `FMT_END_NAMESPACE`) | `fmt::v12::format_to` once the resolver expands the macro (see *Macros*) |
//! | anonymous namespace | contributes nothing (`describe`) |
//! | class template specialization | `formatter<std::error_code>`, `formatter<std::vector<T>>` (with its arguments; members `formatter<std::error_code>::parse`) |
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
//! links to its body. Callables with the same parameter types are told
//! apart by a comment at the end of the list, which lookups and arities
//! ignore: their cv / ref qualifiers when those differ (`get()` and
//! `get(/*const*/)`), then, for another body with the same suffix (SFINAE
//! overloads, one body per `#if` branch), its ordinal (`write(OutputIt,
//! T/*#2*/)`); before, the second merged into the first and was lost.
//! Names that are not overloaded stay bare. The resolver
//! picks an overload by argument count (read from the signature, so
//! default arguments are optional), then by the argument types the call
//! carries (*References*), else links all of them at × 0.8;
//! `qualified_name` lookups treat the suffix as optional. An overload set
//! split across files (one `vformat` per header) is one set in the
//! resolver: bare same-name callables join it (`rueca-resolve`, *RFC 0003
//! hooks*).
//!
//! **Templates** are named without arguments (`Vec`, `clamp_value`); a
//! class template **specialization** (explicit or partial) is its own
//! class, named with its arguments (`Vec<bool>`, `formatter<std::tm,
//! Char>`), so the members of distinct specializations do not merge; so is
//! an explicit specialization of a function template (`template <> int
//! num_bits<int128_opt>()` → `num_bits<int128_opt>`, name `num_bits`), so
//! calls bind the primary template. An
//! out-of-line member of the primary template (`template <typename Char>
//! void basic_string_view<Char>::f()`, arguments = the template's
//! parameters) belongs to the template (`basic_string_view::f`).
//!
//! # Definitions
//!
//! | Source | Label | Notes |
//! |---|---|---|
//! | function definition (also `static`, `inline`, templates, trailing return) | `Function` | `exported`, also when `static` or in an anonymous namespace (`#include` is textual: a header's `static inline` helper is visible to its includers) |
//! | prototype `void f(int);` | `Function`, `is_declaration` | also with a parenthesized name, `int (lua_gettop) (lua_State *L);` (Lua's API) |
//! | `friend` function defined in a class | `Function` of the enclosing namespace | `friend bool operator==(A, A) {…}` → `ns::operator==` |
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
//! | `#define NAME(args) body` | `Function` | signature = the whole definition, continuation lines joined |
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
//! - `exported`: namespace-level definitions (also `static` and
//!   anonymous-namespace ones: an include binds what the included file
//!   declares, textually); members when public; macros always; out-of-line
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
//!   fields of classes of the file, and the return type of a callable of
//!   the file: `Conn::open()->close()`, `make().m()`, `a.get().m()`),
//!   `this`; inside a method, a receiver that is a member chain the file
//!   cannot type (`member_.m()`, `this->a_.b_->m()` in an out-of-line body
//!   whose class is declared in a header, or a member of a base declared
//!   elsewhere) is `$member:member_` / `$member:a_.b_`, typed by the
//!   resolver from the members' declarations; else the receiver's text (≤
//!   200 chars). `arity` = argument count. Operators called by name are
//!   calls (`a.operator+(b)`, `operator==(o)` → `operator+`, `operator==`);
//!   casts (`static_cast<T>`) are not. **Macro invocations are uses, not
//!   calls**: a call of a function-like macro of the file or of an
//!   upper-case name (`GEO_SQ(r)`, `FMT_ASSERT(…)`) is a `Read` (a
//!   `REFERENCES` edge to the macro), since the code it expands to runs at
//!   the call site; its arguments are code as usual. The calls the
//!   macro's **body** makes are `Call`s enclosed by the macro's definition
//!   (`#define lua_pop(L,n) lua_settop(L, -(n)-1)` calls `lua_settop`),
//!   read lexically (`macros.rs`: a name or `a::b::name` before `(`, not a
//!   parameter, keyword, member call or pasted token); the resolver passes
//!   them on to every user of the macro (`rueca-resolve`, *Macro
//!   expansion*). A macro defined once per branch of an `#if` group
//!   reports the calls of one definition, the first that is not for
//!   release builds (`#ifdef NDEBUG`), as one configuration would
//!   (`FMT_ASSERT` calls `assert_fail`, not `ignore_unused`).
//! - **Argument types** (C++): a call or construction whose arguments
//!   include literals (`1` → `int`, `'a'` → `char`, `"s"` → `char*`,
//!   `1.0` → `double`, `true` → `bool`), casts (`static_cast<char>(v)`,
//!   `(int)x`), `std::string(…)` / constructions of the file's classes, or
//!   locals and parameters with a declared type (not a template type
//!   parameter; `const std::string& s` → `string`) carries their types in
//!   its name (`Write$args:char`, `rueca_extract::with_arg_types`; `?` for
//!   the others), for overload choice in the resolver.
//! - **Qualified calls** `Q::f()`: a qualifier rooted in `std`, `testing`,
//!   `Catch`, `doctest` or `__gnu_cxx` is kept as receiver (external). A
//!   qualifier that is a type — written with template arguments, written
//!   as a type anywhere in the file, or not a namespace of the file and
//!   capitalized — gives `$type:Q` (a static member); a relative qualified
//!   type inside a namespace is prefixed with it (`detail::buffer` inside
//!   `fmt::v11` → `$type:fmt::v11::detail::buffer`) for an exact match. A
//!   namespace qualifier gives `$ns:Q` (`ns::util()` → `$ns:ns`, `::f()` →
//!   `$ns:`, `::a::f()` → `$ns:::a`): the resolver looks `Q::name` up from
//!   the enclosing namespaces outwards, never as an implicit member
//!   (`rueca_extract::NAMESPACE_RECEIVER_PREFIX`).
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
//!   macro invocations (above), `Q::name` (as qualified calls), `this->x` /
//!   `obj.x` with a typed or member-chain receiver. Deduplicated per
//!   `(enclosing, receiver, name)`.
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
//! There is no C preprocessor (no headers, no compiler flags: extraction
//! sees one file). Instead the pack prepares the text the grammar parses
//! with **same-length rewrites** — bytes become spaces, a keyword or
//! punctuation, newlines are never touched — so every byte offset, line
//! and column of the parsed text is the original's (the source map is the
//! identity) and names, signatures and docs are read from the original
//! source. Three stages ([`preprocessed`] exposes the result):
//!
//! 1. **Masking** (`mask.rs`): macro tokens whose position allows no other
//!    reading are blanked (`FMT_API`, `FMT_BEGIN_NAMESPACE`, `Q_OBJECT`,
//!    `FMT_ENABLE_IF(…)` in a template head, `extern "C" {` split by
//!    `#ifdef __cplusplus`; `mask.rs` lists the rules), null directives
//!    (`#` alone) go, and `#if` conditions become `1`.
//! 2. **Conditional compilation** (`cond.rs`): each `#if` group is kept
//!    with all its branches when every branch is balanced, ends at a
//!    statement boundary and the group stands where the grammar takes one;
//!    branches dead whatever the target (`#if 0`, `#ifdef __cplusplus` in
//!    C) are blanked. Otherwise the group **collapses** to one branch — the
//!    first whose condition is not known to be false, evaluated in
//!    three-valued logic with `__cplusplus` per language, GCC / Clang
//!    defined and Windows undefined — as a preprocessor with one
//!    configuration would: a function header written once per branch, a
//!    group inside an expression, an initializer list or an `if` without
//!    braces. `#include` / `#define` lines inside initializer lists, enum
//!    bodies or after an `=` (X-macro tables) are blanked. C test heads
//!    before a block (`TEST(Group, Name) {`) become `if  (Group, Name)`,
//!    and operators called by name in C++ expressions (`a.operator+(b)`)
//!    become identifiers (`a.operator_(b)`), both read back from the
//!    original text.
//! 3. **Error-driven repair** (`repair.rs`): the file is parsed and, at
//!    each parse error, a targeted rewrite is tried — a type as a call
//!    argument (`va_arg(ap, char*)`), a block or loop macro (`each(a, i, x)
//!    {` gets its `;`), a statement macro without `;`, an `if`-like macro
//!    (`run_with_period(100) cron();`), unknown specifier macros
//!    (`static redisAtomic size_t n`, `int CJSON_CDECL main`), trailing
//!    attribute macros, calling conventions, `__attribute__`, `= {}`
//!    default arguments, `enum : T`, `typename` in expressions, keyword
//!    arguments of test macros (`TEST(s, enum)`), `decltype(a, b)`,
//!    explicit instantiations, `= delete` declarations, member pointers,
//!    Qt `signals:` and access macros (`JSON_PRIVATE_UNLESS_TESTED:`),
//!    declaration macros after `static` (`static MP_DEFINE_CONST_DICT(…);`),
//!    macros wrapping declarations or statements (`LUAI_DDEC(…;)`,
//!    `LUAI_TRY(L, c, f(););`), operators and empty statements as macro
//!    arguments (`intop(+, a, b)`, `EXPECT_NO_FATAL_FAILURE(;)`), empty
//!    arguments, attribute macros around a declaration, `asm` macros and
//!    labels, a subscripted macro call first in a block
//!    (`MP_STATE_PORT(h)[i] = c;`), `using ns::MACRO(name);`, unnamed
//!    pointer template parameters with a default (the table in
//!    `repair.rs`) — or the `#if` group next
//!    to the error collapses (keeping the branch with the error). A round
//!    is kept only when the error count drops (each site is retried alone
//!    when a whole round does not help), at most six rounds and 32 parses
//!    per file.
//!
//! Collapses make one configuration per file: a macro a collapse assumed
//! defined (or not) stays so for the groups after it. A goto label right
//! before a directive and a branch of prose (an `#error`-like text) go.
//!
//! **Namespace macros.** A masked bare-line macro at declaration level whose
//! name has a `BEGIN` segment (`FMT_BEGIN_NAMESPACE`,
//! `NLOHMANN_JSON_NAMESPACE_BEGIN`) opens a scope until one with an `END`
//! segment closes it; its definition is usually in another file, so the
//! definitions inside get the qualified-name segment `${FMT_BEGIN_NAMESPACE}`
//! (`${FMT_BEGIN_NAMESPACE}::detail::buffer`), which the resolver replaces
//! by the namespaces the macro's definition opens anywhere in the snapshot
//! (`fmt::v12::detail::buffer`), or drops (`rueca-resolve`,
//! `expand_namespace_macros`). A macro's signature is its whole definition
//! (continuation lines joined): `#define FMT_BEGIN_NAMESPACE namespace fmt {
//! inline namespace v12 {`.
//!
//! On the smoke repositories (files with at least one parse error, before →
//! after): hiredis 17.0 → 0.0%, jq 26.7 → 1.3% (the Bison parser), cJSON
//! (with its vendored Unity) 31.3 → 0.0%, redis `src/` 15.3 → 0.5%
//! (`redismodule.h`), fmt 37.1 → 5.7% (1.5% without the vendored
//! GoogleTest / gMock). On the benchmark corpus (RFC 0004 health, gated
//! repositories): C 12.0 → 3.1% (micropython 16.7 → 2.7%, Lua 14.7 →
//! 1.5%), C++ 11.3 → 7.0% (nlohmann/json 11.6 → 4.9%, simdjson 11.7 →
//! 5.0%; most of the rest is vendored GoogleTest and Objective-C).
//!
//! # Deviations and limitations
//!
//! - The remaining parse errors are grammar limits without a preprocessor:
//!   macros that expand to a function header (`YY_DECL`) or hold
//!   declarations (`YY_INITIAL_VALUE (static …)`), statements and
//!   `throw` expressions as macro arguments in some nestings, and
//!   GoogleTest / gMock internals.
//! - Conditions are not evaluated with the project's `-D` flags
//!   (`compile_commands.json`): extraction sees one file, and a group is
//!   collapsed only when its branches cannot be parsed together.
//! - Receiver typing is syntactic: return types are followed only for
//!   callables of the same file, and a member's type declared in another
//!   file is typed by the resolver (`$member:`), not here.
//! - Lower-case macros are not masked before parsing; the repairs blank
//!   them where the grammar fails (`ffc_inline`, `__unused`).
//! - Argument types are known only for literals, casts, constructions and
//!   declared locals (not fields, calls or template-dependent expressions);
//!   overloads that differ only by template constraints are all linked.
//!   ADL, calls through function pointers and virtual dispatch are not
//!   modelled (RFC 0003 §10.3 *Heuristic*).
//! - C++20 modules are out of scope.

mod builtins;
mod cond;
mod extract;
mod includes;
mod locals;
mod macros;
mod mask;
mod preprocess;
mod repair;
mod text;

use rueca_extract::{Budget, ExtractError, LanguagePack, ProjectFiles};
use rueca_model::{FileFacts, Import, Language};

pub use extract::MAX_REFS;

/// The text the grammar parses for `source` (crate docs, *Macros and
/// conditional compilation*): masked macros, collapsed or dead `#if`
/// branches and error-driven repairs, all as same-length rewrites, so
/// offsets and lines are the original's.
pub fn preprocessed(source: &[u8], language: Language) -> Result<Vec<u8>, ExtractError> {
    preprocess::prepare(source, language, &Budget::default()).map(|(text, _)| text)
}

/// Byte offsets where the grammar reports a parse error (ERROR or MISSING
/// node, outermost only) in `source` as the pack parses it, after the
/// preprocessing of the crate docs (*Macros*). A diagnostic: `parse_errors`
/// in the facts counts the same nodes (nested ones included).
pub fn parse_error_offsets(source: &[u8], language: Language) -> Result<Vec<usize>, ExtractError> {
    extract::error_offsets(source, language, &Budget::default())
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
        3
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
