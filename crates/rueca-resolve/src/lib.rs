//! Turns the facts of every file in a snapshot into a [`ResolvedGraph`].
//!
//! Contract (RFC 0001 §7 step 3): deterministic — same inputs in the same
//! order produce an identical graph (sorted iteration, stable tie-breaks).
//! Inputs are sorted by path first, so the input order does not matter either.
//!
//! # Graph shape
//!
//! - One `Repository` node (qualified name = repo id), one `Folder` node per
//!   directory that (transitively) contains files, one `File` node per input
//!   file (even for unsupported languages), and one node per [`Definition`].
//! - Definition nodes get a global qualified name:
//!   `pack.module_name_for(path, facts)` + separator + local qualified name.
//!   The separator is the family's (`LanguageFamily::module_separator`): `.`
//!   for Python, Go and Java, `:` for ECMAScript, Markdown and data (paths
//!   may contain dots). An empty module name keeps the local name: PHP and
//!   C/C++ definitions carry their namespace already
//!   (`App\Models\User::save`, `ns::Foo::bar`). The module name of every
//!   file is returned in `ResolvedGraph::modules`. Identical `(label,
//!   qualified_name, file)` triples are merged, keeping the first definition
//!   (Python redefinitions); later local ids map to the kept node. When the
//!   kept definition is a declaration (`Definition::is_declaration`, C/C++)
//!   and a body follows in the same file (a prototype, or an in-class
//!   declaration with its out-of-line body), the node takes the body's span
//!   and signature (and its doc, if it has one): the node is the
//!   definition, still a member of its class.
//! - Structural edges (`Syntactic`, 1.0): `CONTAINS` Repository→Folder/File and
//!   Folder→Folder/File; `DEFINES` File→top-level definition and
//!   parent→child definition.
//! - `IMPORTS` File→File for every import that resolves to a project file
//!   (`ImportResolved`, 1.0, evidence = import span). Python `from m import x`
//!   where `m.x` is a submodule also imports that submodule. An import that
//!   resolves to a directory gives `IMPORTS` File→Folder (see *RFC 0003
//!   hooks*).
//!
//! # Binding environments
//!
//! Each file gets a module-scope environment built from its imports
//! (top-level definitions are looked up separately, see strategy 4):
//!
//! - Python `import a.b.c` binds `a` (to `a` if it is a project module, else to
//!   a namespace prefix walked lazily); `import a.b as x` binds `x` to `a.b`;
//!   `from m import x` binds `x` to the symbol `x` of `m` or, failing that, to
//!   the submodule `m.x`; `from m import *` binds every exported top-level
//!   definition of `m`. Explicit imports override star imports.
//! - ECMAScript named imports bind the exported symbol of that name, `default`
//!   binds the target's default export (a definition named `default`, else an
//!   explicit re-export of `default` — packs record `export default x` and
//!   `export { a as b }` as re-exports of the file's own names — else the
//!   single exported class/function named after the file, else the only
//!   exported top-level definition), namespace imports bind the module.
//! - Imports of non-project modules bind the name as *external*: references
//!   through such names are reported unresolved and are never fuzzy-matched.
//! - PHP files have no environment: the pack applies `namespace` and `use`
//!   at extraction and reports names fully qualified (see *PHP* below). PHP
//!   `use` imports still produce `IMPORTS` edges through
//!   `pack.resolve_import` (Composer autoload).
//!
//! Looking a name up in a module checks its own top-level definitions, then
//! its re-exports (`export { x } from`, `export * from`, `export * as ns
//! from`), then the names its own imports bind (package `__init__.py`
//! re-exports; ECMAScript `import { X } from "./x"; export { X }`), then —
//! Python only — submodules. Lookups are
//! transitive, cycle-safe and bounded to depth 10.
//!
//! # Reference strategies
//!
//! Strategies are tried in order; the first success wins.
//!
//! | # | Strategy | When | Edge strategy | Confidence |
//! |---|---|---|---|---|
//! | 1 | Local scope | no receiver; name defined by an enclosing function (walking `parent`; ECMAScript `Method`s — object-literal and prototype members — are properties, never in scope) | `SameModule` | 0.95 |
//! | 2 | Self receiver | receiver ∈ `pack.self_receivers()`: member of the enclosing class | `TypeResolved` | 0.90 |
//! | 2 | Self receiver, inherited | …found in a resolved base class (depth-first, left-to-right, depth ≤ 10) | `TypeResolved` | 0.85 |
//! | 2 | `super` receiver | `super()` / `super`: member of a base class | `TypeResolved` | 0.85 |
//! | 3 | Imported binding | no receiver; name bound by an import | `ImportResolved` | 0.95 |
//! | 3 | Imported module as a value | ECMAScript `Call` / `New` / `Type` of a name bound to a whole module (`const log = require("./log"); log()`): the module's default export | `ImportResolved` | 0.90 |
//! | 3 | Imported module | receiver is (a dotted path through) an imported project module | `ImportResolved` | 0.90 |
//! | 3 | Imported class | receiver is an imported class (static member), or an ECMAScript definition owning methods (`import app from "./app"; app.start()`) | `ImportResolved` | 0.85 |
//! | 4 | Same module | no receiver; top-level definition of the same file | `SameModule` | 0.90 |
//! | 4 | Same-module class | receiver is a top-level class (or ECMAScript definition owning methods) of the same file | `SameModule` | 0.85 |
//! | 5 | Typed receiver | receiver `$type:T`: `T` resolves as an exact global qualified type name, else like a `Type` reference through the ladder; the member through its bases (see *RFC 0003 hooks*) | `TypeResolved` | 0.85 |
//! | 6 | Unique name | no receiver; exactly one compatible non-test definition with that name in the same language family, not a builtin | `UniqueName` | 0.60 |
//! | 6 | Unique method | call with an unknown receiver; exactly one compatible method with that name, name ≥ 4 chars, not a builtin and not a common builtin-type method (`update`, `keys`, `then`, `forEach`, …) | `Heuristic` | 0.50 |
//! | — | Constructor | a `Call`/`New` that hits a class also calls its `__init__` / `constructor` (own or inherited) | as the class edge | × 0.95 |
//!
//! Strategy 5 needs no assignment facts: packs type receivers in the file
//! (declarations, constructions, annotated returns; every pack's crate docs
//! say which) and report them as `$type:T`. Self receivers that miss, and
//! names bound to external modules or builtin objects, stop resolution
//! without falling back to strategy 6.
//!
//! Label compatibility for strategy 6: without a receiver, `Call`/`Decorator`
//! → Function, Class (a bare call never targets a method); `New` → Class,
//! Function; `Type` → Class, Interface, TypeAlias, Enum, Struct, Trait;
//! `Throw` → Class; `Read` → any symbol; `Write` never. With an unknown
//! receiver only calls are matched: `Call`/`Decorator` → Method, `New` →
//! Class. Candidates are definitions of the same language family that are
//! neither tests nor local to a function or test body.
//!
//! Class members are looked up in the class, then in its resolved bases
//! depth-first, left-to-right (cycle-safe, depth ≤ 10). This approximates the
//! Python MRO (C3 linearization differs only for diamonds) and is exact for
//! ECMAScript single inheritance.
//!
//! **Members by kind.** A type keeps one member per name *and kind*:
//! callables, nested types and values (fields, properties, constants), so a
//! field and a method with one name (Java `code` / `code()`) do not hide
//! each other. The reference kind decides the order: `Call` / `New` /
//! `Decorator` look for a callable, then a type, then a value; `Read` /
//! `Write` for a value, then a callable that is not a constructor, then a
//! type; `Type` references and receiver paths for a type, then a value,
//! then a callable that is not a constructor. Each slot is searched through
//! the whole hierarchy before the next. A constructor is a callable named
//! like the type that declares it (Java, C/C++), so it is reached by calls
//! only: a type name equal to an enclosing class or its superclass is the
//! type. An ECMAScript `Function`, `Variable` or `Constant` that owns
//! methods (prototype assignments, object-literal methods: `const api = {
//! get() {} }`, `return { next() {} }`, `export default { start() {} }`) is
//! a type for `this`, for `$type:` receivers and for receivers naming it.
//!
//! # PHP
//!
//! PHP references arrive with their names already qualified by the pack
//! (receiver encoding in `rueca-lang-php`, module `names`). The resolver
//! keeps an index of PHP classes / interfaces / traits / enums and functions
//! (case-insensitive) and namespace constants by fully qualified name, and
//! resolves each reference by its receiver:
//!
//! | Receiver | Meaning | Edge strategy | Confidence |
//! |---|---|---|---|
//! | `\A\B\` + `C` | symbol `A\B\C` by name | `ImportResolved` | 0.95 |
//! | `A\B\` + `f` | unqualified function / constant in namespace `A\B`: `A\B\f`, else the global `f` | `ImportResolved` | 0.95 / 0.90 |
//! | `\A\B\` + `C`, class not indexed | the class named `C` in the file Composer autoloading maps `A\B\C` to | `ImportResolved` | 0.80 |
//! | `\A\C` + `m` | static member of class `A\C` (`X::m()`, `X::CONST`, `X::$p`) | `ImportResolved` | 0.85 |
//! | `\A\C->` + `m` | member of an instance whose class is declared (typed parameter / property, `new`, docblock) | `TypeResolved` | 0.85 |
//! | `$this`, `self`, `static` | member of the enclosing class (strategy 2) | `TypeResolved` | 0.90 / 0.85 |
//! | `parent` | member of a base of the enclosing class | `TypeResolved` | 0.85 |
//! | anything else (`$repo`, `foo()->bar`) | unique method name (strategy 6) | `Heuristic` | 0.50 |
//!
//! Members are found through the parent class, used traits (listed in
//! `Definition::bases`, so they also get `INHERITS` edges) and implemented
//! interfaces (for constants). A name that is not in the index is a builtin
//! (`strlen`, `\Exception`), a miss when its first namespace segment is one
//! the project declares, and external otherwise (vendor code is not
//! indexed). A missing member of an *open* type — one that extends, uses or
//! implements something outside the project, a trait, or an enum — is
//! external too (inherited from vendor code or builtin); a missing member of
//! a closed project class (magic `__call` / `__callStatic`) falls back to
//! the unique method name. `$this` outside any class (Pest closures) is
//! external.
//!
//! # RFC 0003 hooks
//!
//! Opt-in behaviour for the next languages (`rueca-extract` crate docs list
//! the hooks); of it, only `$type:` receivers apply to Python, ECMAScript
//! and PHP (their packs type receivers in the file too), nothing to
//! Markdown.
//!
//! - **Package scope** (`ModuleScope::Package`: files of one family with the
//!   same module name; `ModuleScope::Directory`: files of one language in a
//!   directory). Rung 4 and module lookups search the file, then the other
//!   files of its package (`SameModule` 0.90); a top-level `T.m` is a member
//!   of type `T` of the package (Go methods); imports stay file-scoped and
//!   are not re-exported.
//! - **Directory imports**: `resolve_import` may return a directory. The
//!   import gets `IMPORTS` File→Folder and, for a package-scoped importer,
//!   binds the package in that directory (the one whose module name is the
//!   import specifier, else the largest). A star import of a package binds
//!   its exported top-level names; so does a C/C++ include, transitively
//!   (depth ≤ 10). An imported name `C.m` binds member `m` of `C` (Java
//!   `import static`).
//! - **Implicit self** (`implicit_self`): rung 1b, after local scope and
//!   before imports — a receiver-less name is a member of the enclosing
//!   types, innermost first, through bases (`TypeResolved` 0.90 / 0.85); a
//!   method body outside its type (`void Foo::bar() {}`) uses the type
//!   whose global name is its own minus the last segment.
//! - **`$self`** in `self_receivers()`: the receiver type is the enclosing
//!   type, else the type named by the method's qualified-name prefix (Go).
//! - **`$type:T`** receivers (strategy 5 for typed languages): `T` resolves as
//!   an exact global qualified type name, else as a `Type` reference through
//!   the ladder; the member is looked up through bases (`TypeResolved`
//!   0.85). A member missing from a type with a base outside the project is
//!   external; an unknown `T` falls back to the unique-method rung.
//! - **Declarations** (`Definition::is_declaration`): a declaration whose
//!   global qualified name has exactly one body in the same family is
//!   replaced by that body as a target, and leaves the strategy-6
//!   candidates. A declaration and its body in the same file are one node
//!   with the body's span (see *Graph shape*).
//! - **Overloads** (`overloads`): definitions whose qualified name ends in a
//!   parameter suffix (`parse(String)`) are grouped by (family, name without
//!   suffix); a reference that hits one is linked to the overloads whose
//!   arity fits `Reference::arity`, narrowed by the static argument types a
//!   pack writes into the name (`value$args:long,?`,
//!   `rueca_extract::with_arg_types`): overloads that cannot accept a known
//!   argument type are dropped (a primitive that does not widen, a
//!   non-`String` class for `String`, a C/C++ class for a pointer; class to
//!   class is always possible),
//!   then the one whose parameter types equal the known argument types, else
//!   the one whose parameters pass to every other's (the narrowest
//!   primitives), wins. One candidate keeps the confidence; several are all
//!   linked × 0.8; none fitting links the whole set × 0.8. Such a fan-out
//!   leaves out the calling overload itself when another candidate remains:
//!   an overload calling its own name with an argument of unknown type
//!   (`fromJson(String, Class)` → `fromJson(json, TypeToken.get(c))`)
//!   delegates to a sibling far more often than it recurses. A recursion
//!   chosen by arity or argument types keeps its self-loop. Constructors of
//!   Java and C/C++ are the members named like the class. A trailing
//!   comment in a suffix (C/C++ `get(/*const*/)`, `write(T/*#2*/)`: same
//!   parameters, other qualifiers or constraints) is not a parameter. For
//!   C/C++, a reference parameter binds an argument of its type, a string
//!   literal (`char*`) is matched exactly by a `string` / `string_view`
//!   parameter, never by a primitive one, and parameters past the
//!   arguments are defaults.
//! - **`OVERRIDES`** (`overrides`): a method of a type → the method with the
//!   same name in each resolved base (`TypeResolved` 0.90), paired by
//!   signature when the base has overloads: the parameter count must match
//!   (overload suffix, else the parameter list of the definition's
//!   signature), and among several, the overload with the same normalized
//!   parameter types wins. Constructors override nothing. For these families
//!   `IMPLEMENTS` targets also contribute members.
//!   C++ callables without a suffix join the group of their name (a pack
//!   suffixes per file, so a set split across files has bare members); bare
//!   members alone form a set only when their arities differ. C/C++ arity
//!   comes from the signature (default arguments optional, `...` and
//!   parameter packs variadic), else from the suffix, and an out-of-line
//!   body's range covers its declarations' (defaults are written there).
//!   A C/C++ declaration stands for its body at either end of `OVERRIDES`.
//! - **C++ namespaces** (C family): rung 1c, after implicit self — a
//!   receiver-less name inside `a::b::f` is the definition named exactly
//!   `a::b::name`, else `a::name` (overload suffixes optional; `SameModule`
//!   0.90 in the same file, else `ImportResolved` 0.95). A `$ns:a::b`
//!   receiver (`a::b::name` written in code; `$ns:` for `::name`, `$ns:::a`
//!   for `::a::name`) looks `a::b::name` up from the innermost enclosing
//!   namespace outwards, then globally, else resolves the bare name through
//!   imports, the same file and unique names — never as an implicit member.
//! - **Namespace macros** (C family, before nodes are built): a segment
//!   `${M}` that a pack wrote for a masked namespace macro
//!   (`${FMT_BEGIN_NAMESPACE}::format_to`) becomes the namespaces the
//!   object-like macro `M` opens in its definition anywhere in the snapshot
//!   (`#define FMT_BEGIN_NAMESPACE namespace fmt { inline namespace v12 {` →
//!   `fmt::v12::format_to`), in qualified names and receivers; it is
//!   dropped when `M` is not defined or opens no namespace. Exact
//!   namespace lookups see through the inline namespaces it opens
//!   (`fmt::format_to` finds `fmt::v12::format_to`).
//! - **`$member:a.b`** receivers (C++ members whose types are declared in
//!   another file): member `a` of the enclosing type (through bases), its
//!   declared type (the member's first `Type` reference, resolved in its
//!   file; `std::unique_ptr<T>` / `std::shared_ptr<T>` stand for `T`),
//!   member `b` of that type, then the name (`TypeResolved` 0.85). A first
//!   segment that is no member falls back to the unique-method rung.
//! - **`$type:Q` that names no type** (C family): `Q::name` is looked up
//!   as a namespace-qualified name (`Exp::Break()` with `namespace Exp`
//!   declared in another file) before the unique-method rung.
//! - **Macro expansion** (C family, after references): a pack reports the
//!   calls in a macro's body as calls of the macro's definition (a node
//!   whose signature starts with `#`). A definition that uses the macro —
//!   a `CALLS` edge (a lower-case macro called like a function) or a
//!   `REFERENCES` edge (a macro invocation) — also `CALLS` the definitions
//!   the body calls, through nested macros (depth ≤ 8, cycle-safe), with
//!   the use's strategy and call site and the product of the confidences ×
//!   0.95: `lua_pop(L, 1)` calls `lua_settop`, `FMT_ASSERT(…)` calls
//!   `assert_fail` (`hooks::expand_macro_calls`).
//!
//! # Go
//!
//! Go interfaces are implicit: a Go type whose method names (own and
//! promoted through embedding) cover an in-project interface's gets
//! `IMPLEMENTS` to it and `OVERRIDES` from each covering method
//! (`Heuristic` 0.5; an interface embedding one outside the project needs
//! two known methods; at most 64 implementers per interface; see the `go`
//! module). Nothing else is Go-specific: package scope, `$self` and
//! `$type:` are the hooks above.
//!
//! # Bridge and data references (RFC 0003 §6–§7)
//!
//! Handled outside the ladder (exact matches only, never name matching; see
//! the `bridge` module):
//!
//! | Reference | Target | Edge | Strategy | Confidence |
//! |---|---|---|---|---|
//! | `EnvVar` | canonical `$env:NAME` (file-less `EnvVar` node, created when anything reads or sets `NAME`) | `READS` | `Contract` | 0.95 |
//! | `EnvVar` definition (provider) | canonical `$env:NAME` | `WRITES` | `Syntactic` | 1.0 |
//! | `ConfigKey` with a known namespace | every `Key` with that key path in the namespace's config files (`spring`, `laravel`, `dotnet`, `node-config`, `viper[:name]`) | `READS` | `Contract` | 0.90 |
//! | `ConfigKey`, no scope match | the only `Key` with that key path | `READS` | `UniqueName` | 0.60 |
//! | `DataRef`, no receiver | key path in the same file / its package | `REFERENCES` | `Syntactic` / `SameModule` | 1.0 / 0.95 |
//! | `DataRef`, receiver = an import specifier | key path in the imported file | `REFERENCES` | `ImportResolved` | 0.95 |
//! | `DataRef`, `$qn` / `$any` | exact global qualified name / unique key path in any data file | `REFERENCES` | `ImportResolved` / `UniqueName` | 0.95 / 0.60 |
//! | base of a data definition (`key.path`, `<specifier>#key.path`) | as `DataRef` | `INHERITS` | as `DataRef` | as `DataRef` |
//!
//! Several candidates outside a scope resolve to nothing. `Key` and
//! `Document` definitions are never strategy-6 candidates.
//!
//! # Edge kinds
//!
//! `Call`/`New` → `CALLS` (a call that targets a class also `CALLS` its
//! `__init__`/`constructor` — PHP: `__construct` — own or inherited,
//! confidence × 0.95);
//! `Decorator` → `DECORATES` from the decorator to the decorated definition;
//! `Type`/`Read` → `REFERENCES`, except references that come from a class's own
//! base list, which become `INHERITS` / `IMPLEMENTS`; `Throw` → `THROWS`;
//! `Write` → `WRITES`; `EnvVar` / `ConfigKey` → `READS`, `DataRef` →
//! `REFERENCES` (see above). `Definition::bases` / `implements` are also resolved
//! directly (as `Type` references in the class's outer scope) into `INHERITS` /
//! `IMPLEMENTS`. A test definition that `CALLS` a non-test definition also
//! gets a `TESTS` edge with the same provenance.
//!
//! Edges are deduplicated by `(src, dst, kind)`: the highest-precedence
//! strategy and the maximum confidence are kept, together with the first
//! evidence span in `(file, byte offset)` order. Structural self-loops are
//! never emitted; recursive `CALLS` self-loops are. Every reference edge's
//! evidence is `(file, reference span)`.
//!
//! # Statistics
//!
//! [`ResolveStats`] counts every reference (plus decorators synthesized from
//! `Definition::decorators` when a pack reports no decorator references), the
//! resolved ones per strategy, and two classes of expected misses: builtins
//! (PHP: also names behind a namespace qualifier, `App\strlen`) and names
//! reached through non-project modules (PHP: third-party namespaces and
//! members of open types). Unresolved references are logged
//! at `trace` level (`tracing` target `rueca_resolve`).
//!
//! # Prior art
//!
//! The design follows RFC 0002 §4.1. Scope handling (enclosing-function
//! chains, hoisted module-level definitions, import bindings per file) follows
//! the per-file scope graph model of bloop's `scope_resolution` (Apache-2.0)
//! and Sourcegraph scip-syntax locals (MIT). The Python LEGB lookup with member
//! resolution through base classes, and the layering "local scope → imports →
//! module → project-wide name match with explicit confidence", are adapted from
//! codanna's per-language resolvers (Apache-2.0, <https://github.com/bartolli/codanna>).
//! No code is copied; the algorithms are re-implemented over Rueca's facts.

use std::cell::{OnceCell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ops::ControlFlow;

use rueca_extract::{
    DATA_REF_ANY, DATA_REF_QN, LanguagePack, MEMBER_RECEIVER_PREFIX, ModuleScope, NAMESPACE_RECEIVER_PREFIX,
    ProjectFiles, Registry, SELF_RECEIVER, TYPE_RECEIVER_PREFIX, split_arg_types,
};
use rueca_model::{
    Definition, Edge, EdgeKind, EdgeMeta, FileEntry, FileFacts, FileId, Import, ImportedName, Label, Language,
    LanguageFamily, LocalDefId, Node, NodeId, RefKind, Reference, ResolvedGraph, SnapshotInfo, Span, Strategy,
    env_var_qualified_name,
};

mod bridge;
mod go;
mod hooks;

/// One file of the snapshot as seen by the resolver.
#[derive(Debug, Clone)]
pub struct FileInput {
    /// Repository-relative path, `/` separators.
    pub path: String,
    pub blob_sha: String,
    pub language: Option<Language>,
    pub size: u64,
    /// `None` when the file was not extracted (unsupported language or skipped).
    pub facts: Option<FileFacts>,
    /// `ok`, `partial`, or `skipped:<reason>`; copied into `FileEntry::status`.
    pub status: String,
}

#[derive(Debug, Clone, Default)]
pub struct ResolveStats {
    pub references_total: u64,
    pub references_resolved: u64,
    /// Count per strategy name (`import_resolved`, `same_module`, ...).
    pub resolved_by_strategy: std::collections::BTreeMap<String, u64>,
    /// Unresolved references whose name is a language builtin (`print`, `console`).
    pub references_builtin: u64,
    /// Unresolved references that go through a name bound to a non-project
    /// module (`os.path.join`, `React.useState`).
    pub references_external: u64,
}

pub struct ResolveOutput {
    pub graph: ResolvedGraph,
    pub stats: ResolveStats,
}

/// Maximum depth for transitive lookups (re-exports, base classes).
const MAX_DEPTH: usize = 10;

/// The members of a type that share one name, one slot per kind: a field
/// and a method may share a name (Java `code` / `code()`), and a Java or
/// C++ constructor is a method named like its class, which must not hide a
/// nested type or stand for the class in a type lookup. First wins within a
/// slot (overloads are found through their overload set).
#[derive(Debug, Clone, Copy, Default)]
struct MemberSlots {
    /// Functions and methods (constructors included).
    callable: Option<NodeId>,
    /// Nested types.
    ty: Option<NodeId>,
    /// Fields, properties, constants and anything else.
    value: Option<NodeId>,
}

/// One slot of [`MemberSlots`] as searched by [`Resolver::find_member`].
#[derive(Debug, Clone, Copy)]
enum Slot {
    Callable,
    /// A callable that is not a constructor of the type that declares it.
    Method,
    Type,
    Value,
}

/// What a member lookup is for, which decides the order of the slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Want {
    /// A call or `new`: callable, then a nested type (Python `self.Inner()`,
    /// Java `new Inner()`), then a value (a field holding a function).
    Call,
    /// A read or write: value, then a callable that is not a constructor
    /// (a Python property, a method passed as a value), then a type.
    Value,
    /// A type, a receiver path segment or anything else: type, value, then a
    /// callable that is not a constructor. Constructors are reached only by
    /// calls.
    Type,
}

impl Want {
    pub(crate) fn of(kind: RefKind) -> Want {
        match kind {
            RefKind::Call | RefKind::New | RefKind::Decorator => Want::Call,
            RefKind::Read | RefKind::Write => Want::Value,
            _ => Want::Type,
        }
    }
}

/// Resolves a snapshot. `files` may be in any order; the resolver sorts by path.
pub fn resolve(
    snapshot: SnapshotInfo,
    mut files: Vec<FileInput>,
    project: &ProjectFiles,
    registry: &Registry,
) -> ResolveOutput {
    files.sort_by(|a, b| a.path.cmp(&b.path).then_with(|| a.blob_sha.cmp(&b.blob_sha)));
    files.dedup_by(|later, first| later.path == first.path);
    let inline_aliases = hooks::expand_namespace_macros(&mut files);

    let mut r = Resolver::new(&files, project, registry, &snapshot);
    r.inline_aliases = inline_aliases;
    r.structural_edges();
    r.import_edges();
    r.build_envs();
    r.resolve_bases();
    r.override_edges();
    r.go_implicit_interfaces();
    r.resolve_references();
    r.expand_macro_calls();
    r.env_provider_edges();
    r.tests_edges();

    let modules: Vec<String> =
        r.packs.iter().zip(&r.module_names).map(|(p, m)| if p.is_some() { m.clone() } else { String::new() }).collect();
    let Resolver { nodes, edges, stats, line_counts, file_node, .. } = r;
    let file_entries = files
        .iter()
        .enumerate()
        .map(|(i, f)| FileEntry {
            path: f.path.clone(),
            blob_sha: f.blob_sha.clone(),
            language: f.language,
            size: f.size,
            line_count: line_counts[i],
            node: file_node[i],
            status: f.status.clone(),
        })
        .collect();
    let edges = edges.into_iter().map(|((src, kind, dst), meta)| Edge { src, dst, kind, meta }).collect();
    ResolveOutput { graph: ResolvedGraph { snapshot, nodes, edges, files: file_entries, modules }, stats }
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

/// What a name is bound to.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    Symbol(NodeId),
    /// A project module (file).
    Module(FileId),
    /// Python `import a.b` where `a` is not itself a project file (namespace
    /// package); walked segment by segment.
    Prefix(String),
    /// A non-project module or a name imported from one.
    External,
}

/// Environment entry: `None` = imported from a project module but the name
/// could not be found there (later strategies may still apply).
type Env = BTreeMap<String, Option<Target>>;

enum Outcome {
    Hit(NodeId, Strategy, f32),
    External,
    Miss,
}

/// A reference to resolve, plus where its edge goes.
struct RefItem<'a> {
    kind: RefKind,
    name: &'a str,
    receiver: Option<&'a str>,
    enclosing: Option<LocalDefId>,
    span: Span,
    /// For decorators: the decorated definition, when already known.
    decorated: Option<LocalDefId>,
    /// Call arity, when the pack reports it (overload choice).
    arity: Option<u32>,
    /// Static argument types, when the pack reports them (`?` = unknown;
    /// `rueca_extract::with_arg_types`), for overload choice.
    arg_types: Option<Vec<&'a str>>,
}

/// Where the edges of one resolved reference go (see `Resolver::resolve_one`).
struct EdgePlan {
    outcome: Outcome,
    /// Source of an `INHERITS` / `IMPLEMENTS` edge from a base list.
    src: Option<NodeId>,
    /// Destination of a `DECORATES` edge: the decorated definition.
    dst: Option<NodeId>,
    kind: EdgeKind,
}

/// A definition whose base and implements lists are resolved.
struct BaseSite {
    file: FileId,
    class: NodeId,
    /// Scope the bases are resolved in: the definition's parent.
    scope: Option<LocalDefId>,
    span: Span,
    php: bool,
    /// Whether implemented interfaces contribute members.
    implements_members: bool,
}

struct Resolver<'a> {
    files: &'a [FileInput],
    project: &'a ProjectFiles,
    packs: Vec<Option<&'a dyn LanguagePack>>,
    families: Vec<Option<LanguageFamily>>,
    file_by_path: HashMap<&'a str, FileId>,
    module_names: Vec<String>,
    line_counts: Vec<u32>,

    nodes: Vec<Node>,
    repo_node: NodeId,
    folder_nodes: BTreeMap<String, NodeId>,
    file_node: Vec<NodeId>,
    /// Per file: local definition id → node id (after dedup).
    def_node: Vec<Vec<NodeId>>,
    /// Per file: sanitized parent of each definition.
    parents: Vec<Vec<Option<LocalDefId>>>,
    /// Per file: children of each definition, in definition order.
    children: Vec<Vec<Vec<LocalDefId>>>,
    /// Per file: top-level definitions, in definition order.
    top_level: Vec<Vec<LocalDefId>>,
    /// Per file: definitions with a non-empty base or implements list.
    with_bases: Vec<Vec<LocalDefId>>,
    /// Per file: first child of a definition by name.
    child_by_name: Vec<HashMap<(LocalDefId, &'a str), LocalDefId>>,
    /// Per file: top-level definition by name (first exported one, else first).
    top_by_name: Vec<HashMap<&'a str, NodeId>>,
    /// Members of each (canonical) definition node, by name, one slot per
    /// kind of member (see [`MemberSlots`]); first wins within a slot.
    members: HashMap<NodeId, BTreeMap<String, MemberSlots>>,
    /// Non-test definitions by simple name, for strategy 6.
    by_name: HashMap<String, Vec<NodeId>>,
    /// Per file, per import: resolved target file.
    import_targets: Vec<Vec<Option<FileId>>>,
    envs: Vec<Env>,
    /// Resolved base classes (INHERITS targets) per class node, in order.
    bases: HashMap<NodeId, Vec<NodeId>>,
    /// PHP symbols by fully qualified name (see [`PhpIndex`]).
    php: PhpIndex,

    // RFC 0003 hooks (see the `hooks` and `bridge` modules).
    /// Per file: index into `packages` for package-scoped files.
    package_of: Vec<Option<u32>>,
    /// Files of each package, in path order.
    packages: Vec<Vec<FileId>>,
    /// Packages with a file directly in a directory.
    packages_by_dir: HashMap<String, Vec<u32>>,
    /// Names bound by a star import of a file (see `star_exports`).
    star_memo: RefCell<HashMap<FileId, hooks::StarNames>>,
    /// Per file, per import: `Folder` node of a directory import.
    import_folders: Vec<Vec<Option<NodeId>>>,
    /// Non-PHP types with a base that is not a project symbol: a missing
    /// member is external, not a miss (typed receivers, implicit self).
    open_types: HashSet<NodeId>,
    /// Declaration node (C/C++ `is_declaration`) → the unique body with the
    /// same global qualified name.
    body_of: HashMap<NodeId, NodeId>,
    /// Overload sets (index into `overload_sets`) of overloaded definitions.
    overload_of: HashMap<NodeId, usize>,
    overload_sets: Vec<Vec<NodeId>>,
    /// Canonical env var nodes by name (RFC 0003 §7.1).
    env_nodes: BTreeMap<String, NodeId>,
    /// `Key` definitions by local key path, for config key reads.
    keys_by_path: HashMap<&'a str, Vec<NodeId>>,
    /// Per data file: definition by local qualified name (key path).
    data_by_local: Vec<HashMap<&'a str, NodeId>>,
    /// Every node by global qualified name, built on first use.
    by_qn: OnceCell<HashMap<String, Vec<NodeId>>>,
    /// C/C++ nodes by global qualified name without overload suffix.
    by_base_qn: OnceCell<HashMap<String, Vec<NodeId>>>,
    /// C++ namespace paths opened by namespace macros and the same path
    /// without its inline namespaces (`fmt::v12`, `fmt`).
    inline_aliases: Vec<(String, String)>,
    /// C/C++ body → files of the declarations paired with it.
    declared_in: HashMap<NodeId, Vec<FileId>>,
    /// C/C++ body → its declarations (`body_of` reversed), sorted.
    decls_of: HashMap<NodeId, Vec<NodeId>>,
    /// C/C++ file → files it sees through includes (memo).
    visible_memo: RefCell<HashMap<FileId, std::rc::Rc<HashSet<FileId>>>>,

    /// Key: (module, name, reached through `export *`).
    lookup_memo: RefCell<HashMap<(FileId, String, bool), Option<Target>>>,
    module_memo: RefCell<HashMap<(FileId, u32, String), Option<FileId>>>,

    edges: BTreeMap<(NodeId, EdgeKind, NodeId), EdgeMeta>,
    stats: ResolveStats,
}

fn is_ident(s: &str) -> bool {
    !s.is_empty()
        && s.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '$')
        && !s.starts_with(|c: char| c.is_ascii_digit())
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn parent_dir(path: &str) -> Option<&str> {
    path.rsplit_once('/').map(|(d, _)| d)
}

/// Splits a raw base-class expression into `(receiver, name)`:
/// `abc.ABC` → (`abc`, `ABC`), `Generic[T]` → (None, `Generic`). Keyword
/// arguments (`metaclass=M`) and complex expressions yield `None`.
fn split_base(text: &str) -> Option<(Option<&str>, &str)> {
    let t = text.trim();
    if t.contains('=') {
        return None;
    }
    let t = match t.find(['[', '<', '(']) {
        Some(i) => t[..i].trim(),
        None => t,
    };
    if t.is_empty() || !t.split('.').all(is_ident) {
        return None;
    }
    Some(match t.rsplit_once('.') {
        Some((r, n)) => (Some(r), n),
        None => (None, t),
    })
}

/// Splits a raw decorator expression (`app.route("/x")`, `@dataclass`).
fn split_decorator(text: &str) -> Option<(Option<&str>, &str)> {
    let t = text.trim().trim_start_matches('@');
    let t = match t.find('(') {
        Some(i) => &t[..i],
        None => t,
    };
    split_base(t)
}

/// Splits a PHP fully qualified name from `Definition::bases` /
/// `implements` (`\App\Models\Model`) into the namespace-qualifier
/// encoding of PHP references: (`\App\Models\`, `Model`).
fn split_php_name(text: &str) -> Option<(Option<&str>, &str)> {
    let t = text.trim();
    if !t.starts_with('\\') {
        return None;
    }
    let idx = t.rfind('\\')?;
    let name = &t[idx + 1..];
    if name.is_empty() || !is_ident(name) {
        return None;
    }
    Some((Some(&t[..=idx]), name))
}

/// PHP symbols by fully qualified name. Class and function names are
/// case-insensitive in PHP (keys lowercased); constants are not.
#[derive(Default)]
struct PhpIndex {
    /// Classes, interfaces, traits, enums.
    types: HashMap<String, Vec<NodeId>>,
    functions: HashMap<String, Vec<NodeId>>,
    /// Namespace constants and `define()` constants (not class constants).
    constants: HashMap<String, Vec<NodeId>>,
    /// Lowercase first namespace segments of project definitions (`app`,
    /// `illuminate`): names under them that are not found are misses, names
    /// elsewhere are third-party (external).
    roots: HashSet<String>,
    /// Types whose members may come from outside the project: classes with
    /// a parent, trait or interface that is not a project symbol, traits
    /// (their `$this` is the using class) and enums (builtin `from`,
    /// `cases`, `$value`). Missing members of these are external.
    open: HashSet<NodeId>,
}

impl PhpIndex {
    /// Indexes a definition by fully qualified name: types and functions
    /// (lowercased), and constants that are not class constants.
    fn insert(&mut self, label: Label, in_type: bool, qn: &str, node: NodeId) {
        let (map, key) = match label {
            l if l.is_type() => (&mut self.types, qn.to_lowercase()),
            Label::Function => (&mut self.functions, qn.to_lowercase()),
            Label::Constant if !in_type => (&mut self.constants, qn.to_string()),
            _ => return,
        };
        if let Some((root, _)) = key.split_once('\\') {
            self.roots.insert(root.to_lowercase());
        }
        let v = map.entry(key).or_default();
        if !v.contains(&node) {
            v.push(node);
        }
    }
}

/// A star re-export or the module alias of a re-export: the module when
/// its alias is `name`; unaliased, it is collected in `stars`.
fn reexported_alias(
    alias: Option<&str>,
    name: &str,
    target: Option<FileId>,
    stars: &mut Vec<Option<FileId>>,
) -> Option<Target> {
    match alias {
        Some(a) if a == name => Some(target.map_or(Target::External, Target::Module)),
        Some(_) => None,
        None => {
            stars.push(target);
            None
        }
    }
}

/// The local name an imported entry binds: its alias, else the name (the
/// last segment of a member import `C.m` outside Python).
fn bound_name(n: &ImportedName, python: bool) -> &str {
    match (&n.alias, python) {
        (Some(a), _) => a.as_str(),
        (None, false) => n.name.rsplit('.').next().unwrap_or(&n.name),
        (None, true) => n.name.as_str(),
    }
}

/// A node without file, span or source: repository, folder or file.
fn blank_node(label: Label, name: &str, qn: &str) -> Node {
    Node {
        label,
        name: name.to_string(),
        qualified_name: qn.to_string(),
        file: None,
        span: None,
        language: None,
        signature: None,
        doc: None,
        exported: false,
        is_test: false,
    }
}

/// The parent of each definition, without out-of-range or self parents
/// and with malformed parent cycles broken.
fn sanitized_parents(defs: &[Definition]) -> Vec<Option<LocalDefId>> {
    let n = defs.len();
    let mut parents: Vec<Option<LocalDefId>> =
        defs.iter().enumerate().map(|(j, d)| d.parent.filter(|&p| (p as usize) < n && p as usize != j)).collect();
    for j in 0..n {
        if in_parent_cycle(&parents, j) {
            parents[j] = None;
        }
    }
    parents
}

/// Whether the parent chain of `j` comes back to `j` (or is longer than
/// the definitions, which only a cycle makes it).
fn in_parent_cycle(parents: &[Option<LocalDefId>], j: usize) -> bool {
    let mut cur = parents[j];
    let mut steps = 0;
    while let Some(p) = cur {
        steps += 1;
        if p as usize == j || steps > parents.len() {
            return true;
        }
        cur = parents[p as usize];
    }
    false
}

/// Children of each definition and the top-level definitions, in
/// definition order.
fn children_and_top(parents: &[Option<LocalDefId>]) -> (Vec<Vec<LocalDefId>>, Vec<LocalDefId>) {
    let mut children = vec![Vec::new(); parents.len()];
    let mut top = Vec::new();
    for (j, p) in parents.iter().enumerate() {
        match p {
            Some(p) => children[*p as usize].push(j as LocalDefId),
            None => top.push(j as LocalDefId),
        }
    }
    (children, top)
}

/// First child of each definition by name.
fn children_by_name<'d>(
    defs: &'d [Definition],
    children: &[Vec<LocalDefId>],
) -> HashMap<(LocalDefId, &'d str), LocalDefId> {
    let mut by_name = HashMap::new();
    for (p, kids) in children.iter().enumerate() {
        for &c in kids {
            by_name.entry((p as LocalDefId, defs[c as usize].name.as_str())).or_insert(c);
        }
    }
    by_name
}

/// Whether definition `j` is local to a function or test body.
fn is_body_local(defs: &[Definition], parents: &[Option<LocalDefId>], j: usize) -> bool {
    let mut cur = parents[j];
    while let Some(p) = cur {
        let pd = &defs[p as usize];
        if pd.label.is_callable() || pd.is_test {
            return true;
        }
        cur = parents[p as usize];
    }
    false
}

fn normalize_stem(s: &str) -> String {
    s.chars().filter(|c| *c != '-' && *c != '_' && *c != '.').flat_map(|c| c.to_lowercase()).collect()
}

fn is_structural(kind: EdgeKind) -> bool {
    matches!(
        kind,
        EdgeKind::Contains | EdgeKind::Defines | EdgeKind::Imports | EdgeKind::Inherits | EdgeKind::Implements
    )
}

fn module_import(module: String, relative_level: u32) -> Import {
    Import {
        module,
        relative_level,
        names: Vec::new(),
        module_alias: None,
        type_only: false,
        reexport: false,
        span: Span::default(),
    }
}

/// Methods of the builtin container, string, iterator and promise types of
/// Python and ECMAScript. A call with an unknown receiver and one of these
/// names is far more likely to hit the builtin type than a project method, so
/// the unknown-receiver heuristic skips them (names under 4 chars are skipped
/// anyway).
const BUILTIN_METHODS: &[&str] = &[
    // Python dict / list / set / str / bytes / file
    "append",
    "clear",
    "copy",
    "count",
    "decode",
    "discard",
    "encode",
    "endswith",
    "extend",
    "format",
    "index",
    "insert",
    "items",
    "join",
    "keys",
    "lower",
    "lstrip",
    "partition",
    "popitem",
    "read",
    "readline",
    "readlines",
    "remove",
    "replace",
    "reverse",
    "rsplit",
    "rstrip",
    "setdefault",
    "sort",
    "split",
    "splitlines",
    "startswith",
    "strip",
    "title",
    "union",
    "update",
    "upper",
    "values",
    "write",
    "close",
    "flush",
    "seek",
    "tell",
    // ECMAScript Array / String / Map / Set / Promise / Object / Function
    "apply",
    "bind",
    "call",
    "catch",
    "concat",
    "entries",
    "every",
    "fill",
    "filter",
    "finally",
    "find",
    "findIndex",
    "flat",
    "flatMap",
    "forEach",
    "has",
    "includes",
    "indexOf",
    "lastIndexOf",
    "padEnd",
    "padStart",
    "push",
    "reduce",
    "shift",
    "slice",
    "some",
    "splice",
    "test",
    "then",
    "toFixed",
    "toLowerCase",
    "toString",
    "toUpperCase",
    "trim",
    "trimEnd",
    "trimStart",
    "unshift",
    "valueOf",
    "delete",
    "match",
    "matchAll",
    "charAt",
    "charCodeAt",
    "substring",
    "substr",
    "localeCompare",
];

/// Label compatibility for strategy 6.
fn compatible(kind: RefKind, label: Label, with_receiver: bool) -> bool {
    use Label::*;
    match kind {
        // A bare call never targets a method (no implicit receiver in Python
        // or ECMAScript).
        RefKind::Call | RefKind::Decorator => {
            if with_receiver {
                label == Method
            } else {
                matches!(label, Function | Class)
            }
        }
        RefKind::New => matches!(label, Class | Function) && (!with_receiver || label == Class),
        // Unknown-receiver matching is for method calls only.
        _ if with_receiver => false,
        RefKind::Type => matches!(label, Class | Interface | TypeAlias | Enum | Struct | Trait),
        RefKind::Throw => label == Class,
        RefKind::Read => !matches!(label, Repository | Folder | File | Module | Package),
        RefKind::Write => false,
        // Bridge and data references never go through name matching.
        RefKind::EnvVar | RefKind::ConfigKey | RefKind::DataRef => false,
    }
}

impl<'a> Resolver<'a> {
    fn new(files: &'a [FileInput], project: &'a ProjectFiles, registry: &'a Registry, snapshot: &SnapshotInfo) -> Self {
        let packs: Vec<Option<&dyn LanguagePack>> =
            files.iter().map(|f| f.language.and_then(|l| registry.for_language(l))).collect();
        let families = files.iter().map(|f| f.language.map(|l| l.family())).collect();
        let file_by_path = files.iter().enumerate().map(|(i, f)| (f.path.as_str(), i as FileId)).collect();
        let module_names = files
            .iter()
            .zip(&packs)
            .map(|(f, p)| match p {
                Some(p) => p.module_name_for(&f.path, f.facts.as_ref(), project),
                None => f.path.rsplit_once('.').map(|(s, _)| s.to_string()).unwrap_or_else(|| f.path.clone()),
            })
            .collect();
        let line_counts = files.iter().map(|f| f.facts.as_ref().map_or(0, |x| x.line_count)).collect();

        let mut r = Resolver {
            files,
            project,
            packs,
            families,
            file_by_path,
            module_names,
            line_counts,
            nodes: Vec::new(),
            repo_node: 0,
            folder_nodes: BTreeMap::new(),
            file_node: Vec::new(),
            def_node: Vec::new(),
            parents: Vec::new(),
            children: Vec::new(),
            top_level: Vec::new(),
            with_bases: Vec::new(),
            top_by_name: Vec::new(),
            child_by_name: Vec::new(),
            members: HashMap::new(),
            by_name: HashMap::new(),
            import_targets: Vec::new(),
            envs: Vec::new(),
            bases: HashMap::new(),
            php: PhpIndex::default(),
            package_of: Vec::new(),
            packages: Vec::new(),
            packages_by_dir: HashMap::new(),
            star_memo: RefCell::new(HashMap::new()),
            import_folders: Vec::new(),
            open_types: HashSet::new(),
            body_of: HashMap::new(),
            overload_of: HashMap::new(),
            overload_sets: Vec::new(),
            env_nodes: BTreeMap::new(),
            keys_by_path: HashMap::new(),
            data_by_local: Vec::new(),
            by_qn: OnceCell::new(),
            by_base_qn: OnceCell::new(),
            inline_aliases: Vec::new(),
            declared_in: HashMap::new(),
            decls_of: HashMap::new(),
            visible_memo: RefCell::new(HashMap::new()),
            lookup_memo: RefCell::new(HashMap::new()),
            module_memo: RefCell::new(HashMap::new()),
            edges: BTreeMap::new(),
            stats: ResolveStats::default(),
        };
        r.build_packages();
        r.build_nodes(snapshot);
        r.build_tables();
        r.build_php_index();
        r.build_env_nodes();
        r.build_hook_tables();
        r
    }

    fn defs(&self, f: FileId) -> &'a [Definition] {
        self.files[f as usize].facts.as_ref().map_or(&[], |x| x.definitions.as_slice())
    }

    fn imports(&self, f: FileId) -> &'a [Import] {
        self.files[f as usize].facts.as_ref().map_or(&[], |x| x.imports.as_slice())
    }

    fn is_python(&self, f: FileId) -> bool {
        self.families[f as usize] == Some(LanguageFamily::Python)
    }

    fn is_php(&self, f: FileId) -> bool {
        self.families[f as usize] == Some(LanguageFamily::Php)
    }

    fn node_of(&self, f: FileId, local: LocalDefId) -> NodeId {
        self.def_node[f as usize][local as usize]
    }

    // -- nodes ---------------------------------------------------------------

    fn build_nodes(&mut self, snapshot: &SnapshotInfo) {
        let repo_name = snapshot.repo.rsplit('/').next().unwrap_or(&snapshot.repo);
        self.nodes.push(blank_node(Label::Repository, repo_name, &snapshot.repo));
        self.repo_node = 0;
        self.build_folder_nodes();
        self.build_file_nodes();
        // Nodes kept from a declaration (C/C++ `is_declaration`): a body
        // merged into one gives it its span and signature.
        let mut seen: HashMap<(Label, String, FileId), NodeId> = HashMap::new();
        let mut declaration_only: HashSet<NodeId> = HashSet::new();
        for i in 0..self.files.len() {
            let map = self.build_definition_nodes(i as FileId, &mut seen, &mut declaration_only);
            self.def_node.push(map);
        }
    }

    /// One `Folder` node per directory holding a file, in path order.
    fn build_folder_nodes(&mut self) {
        let mut folders = BTreeSet::new();
        for f in self.files {
            let mut p = parent_dir(&f.path);
            while let Some(d) = p {
                if d.is_empty() || !folders.insert(d.to_string()) {
                    break;
                }
                p = parent_dir(d);
            }
        }
        for d in folders {
            let id = self.nodes.len() as NodeId;
            self.nodes.push(blank_node(Label::Folder, basename(&d), &d));
            self.folder_nodes.insert(d, id);
        }
    }

    fn build_file_nodes(&mut self) {
        for (i, f) in self.files.iter().enumerate() {
            let id = self.nodes.len() as NodeId;
            let mut n = blank_node(Label::File, basename(&f.path), &f.path);
            n.file = Some(i as FileId);
            n.language = f.language;
            self.nodes.push(n);
            self.file_node.push(id);
        }
    }

    /// The nodes of the definitions of `fid`, one per definition: a
    /// definition with the label and global qualified name of an earlier
    /// one of the same file shares its node.
    fn build_definition_nodes(
        &mut self,
        fid: FileId,
        seen: &mut HashMap<(Label, String, FileId), NodeId>,
        declaration_only: &mut HashSet<NodeId>,
    ) -> Vec<NodeId> {
        let i = fid as usize;
        let defs = self.defs(fid);
        let sep = self.families[i].map_or(":", LanguageFamily::module_separator);
        let module = self.module_names[i].clone();
        let mut map = Vec::with_capacity(defs.len());
        for d in defs {
            let qn =
                if module.is_empty() { d.qualified_name.clone() } else { format!("{module}{sep}{}", d.qualified_name) };
            let key = (d.label, qn, fid);
            if let Some(&existing) = seen.get(&key) {
                if !d.is_declaration && declaration_only.remove(&existing) {
                    self.merge_body(existing, d);
                }
                map.push(existing);
                continue;
            }
            let id = self.nodes.len() as NodeId;
            if d.is_declaration {
                declaration_only.insert(id);
            }
            self.nodes.push(Node {
                label: d.label,
                name: d.name.clone(),
                qualified_name: key.1.clone(),
                file: Some(fid),
                span: Some(d.span),
                language: self.files[i].language,
                signature: d.signature.clone(),
                doc: d.doc.clone(),
                exported: d.exported,
                is_test: d.is_test,
            });
            seen.insert(key, id);
            map.push(id);
        }
        map
    }

    /// A body merged into a declaration's node gives it its span,
    /// signature and (when it has one) doc.
    fn merge_body(&mut self, node: NodeId, body: &Definition) {
        let n = &mut self.nodes[node as usize];
        n.span = Some(body.span);
        n.signature = body.signature.clone();
        if body.doc.is_some() {
            n.doc = body.doc.clone();
        }
    }

    fn build_tables(&mut self) {
        for i in 0..self.files.len() {
            let defs = self.defs(i as FileId);
            let parents = sanitized_parents(defs);
            let (children, top) = children_and_top(&parents);
            self.add_parent_members(i, defs, &parents);
            self.index_names(i, defs, &parents);
            self.with_bases.push(
                (0..defs.len() as LocalDefId)
                    .filter(|&j| !defs[j as usize].bases.is_empty() || !defs[j as usize].implements.is_empty())
                    .collect(),
            );
            let top_by_name = self.top_names(i, defs, &top);
            self.top_by_name.push(top_by_name);
            self.child_by_name.push(children_by_name(defs, &children));
            self.parents.push(parents);
            self.children.push(children);
            self.top_level.push(top);
        }
        self.build_import_targets();
    }

    /// Records every definition of file `i` as a member of its parent.
    fn add_parent_members(&mut self, i: usize, defs: &[Definition], parents: &[Option<LocalDefId>]) {
        for (j, p) in parents.iter().enumerate() {
            let Some(p) = p else { continue };
            let owner = self.def_node[i][*p as usize];
            let member = self.def_node[i][j];
            if owner != member {
                self.add_member(owner, &defs[j].name, member, defs[j].label);
            }
        }
    }

    /// Adds the definitions of file `i` that name matching may find to
    /// `by_name`: neither tests, nor data details (keys are reached by
    /// exact key paths only: config reads, data pointers), nor definitions
    /// local to a function or test body (only visible through the local
    /// scope chain, strategy 1).
    fn index_names(&mut self, i: usize, defs: &[Definition], parents: &[Option<LocalDefId>]) {
        for (j, d) in defs.iter().enumerate() {
            if d.is_test || d.label == Label::Test || d.label.is_data_detail() || is_body_local(defs, parents, j) {
                continue;
            }
            let node = self.def_node[i][j];
            let v = self.by_name.entry(d.name.clone()).or_default();
            if !v.contains(&node) {
                v.push(node);
            }
        }
    }

    /// Top-level definitions of file `i` by name: the first exported one,
    /// else the first. Package scope: `T.m` declared outside `T` (Go
    /// methods) is a member of `T`, not a module-level name.
    fn top_names(&self, i: usize, defs: &'a [Definition], top: &[LocalDefId]) -> HashMap<&'a str, NodeId> {
        let mut by_name: HashMap<&'a str, (bool, NodeId)> = HashMap::new();
        let package_scoped = self.package_of[i].is_some();
        for &j in top {
            let d = &defs[j as usize];
            if package_scoped && d.qualified_name != d.name {
                continue;
            }
            let node = self.def_node[i][j as usize];
            match by_name.get(d.name.as_str()) {
                Some((true, _)) => {}
                Some((false, _)) if !d.exported => {}
                _ => {
                    by_name.insert(d.name.as_str(), (d.exported, node));
                }
            }
        }
        by_name.into_iter().map(|(k, (_, n))| (k, n)).collect()
    }

    /// Import targets: a project file, or a directory (`Folder` node,
    /// bound to its package when the importer is package-scoped).
    fn build_import_targets(&mut self) {
        let mut targets = Vec::with_capacity(self.files.len());
        let mut folders = Vec::with_capacity(self.files.len());
        for i in 0..self.files.len() {
            let (t, d) = self.import_targets_of(i as FileId);
            targets.push(t);
            folders.push(d);
        }
        self.import_targets = targets;
        self.import_folders = folders;
    }

    /// Per import of `fid`: its target file and its `Folder` node.
    fn import_targets_of(&self, fid: FileId) -> (Vec<Option<FileId>>, Vec<Option<NodeId>>) {
        let imports = self.imports(fid);
        let (mut t, mut d) = (vec![None; imports.len()], vec![None; imports.len()]);
        let Some(pack) = self.packs[fid as usize] else { return (t, d) };
        let path = &self.files[fid as usize].path;
        for (k, imp) in imports.iter().enumerate() {
            let Some(p) = pack.resolve_import(path, imp, self.project) else { continue };
            if let Some(file) = self.file_id(&p) {
                t[k] = Some(file);
            } else if let Some(folder) = self.folder_node(&p) {
                d[k] = Some(folder);
                t[k] = self.package_in_dir(fid, &p, &imp.module);
            }
        }
        (t, d)
    }

    fn file_id(&self, path: &str) -> Option<FileId> {
        self.file_by_path.get(path).copied()
    }

    // -- edges ---------------------------------------------------------------

    fn add_edge(&mut self, src: NodeId, dst: NodeId, kind: EdgeKind, meta: EdgeMeta) {
        if src == dst && is_structural(kind) {
            return;
        }
        match self.edges.entry((src, kind, dst)) {
            std::collections::btree_map::Entry::Vacant(v) => {
                v.insert(meta);
            }
            std::collections::btree_map::Entry::Occupied(mut o) => {
                let cur = o.get_mut();
                cur.strategy = cur.strategy.min(meta.strategy);
                cur.confidence = cur.confidence.max(meta.confidence);
                let key = |e: &Option<(FileId, Span)>| e.map(|(f, s)| (f, s.start_byte, s.end_byte));
                cur.evidence = match (cur.evidence, meta.evidence) {
                    (Some(a), Some(b)) => Some(if key(&Some(b)) < key(&Some(a)) { b } else { a }),
                    (a, b) => a.or(b),
                };
            }
        }
    }

    fn syntactic() -> EdgeMeta {
        EdgeMeta { confidence: 1.0, strategy: Strategy::Syntactic, evidence: None }
    }

    fn container_of(&self, path: &str) -> NodeId {
        match parent_dir(path) {
            Some(d) if !d.is_empty() => self.folder_nodes.get(d).copied().unwrap_or(self.repo_node),
            _ => self.repo_node,
        }
    }

    fn structural_edges(&mut self) {
        let folders: Vec<(String, NodeId)> = self.folder_nodes.iter().map(|(k, v)| (k.clone(), *v)).collect();
        for (path, id) in folders {
            let parent = self.container_of(&path);
            self.add_edge(parent, id, EdgeKind::Contains, Self::syntactic());
        }
        for i in 0..self.files.len() {
            let parent = self.container_of(&self.files[i].path);
            let fnode = self.file_node[i];
            self.add_edge(parent, fnode, EdgeKind::Contains, Self::syntactic());
            let fid = i as FileId;
            for j in 0..self.def_node[i].len() {
                let child = self.def_node[i][j];
                let src = match self.parents[i][j] {
                    Some(p) => self.node_of(fid, p),
                    None => fnode,
                };
                self.add_edge(src, child, EdgeKind::Defines, Self::syntactic());
            }
        }
    }

    fn import_edges(&mut self) {
        for i in 0..self.files.len() {
            let fid = i as FileId;
            let fnode = self.file_node[i];
            for (k, imp) in self.imports(fid).iter().enumerate() {
                let meta =
                    EdgeMeta { confidence: 1.0, strategy: Strategy::ImportResolved, evidence: Some((fid, imp.span)) };
                let target =
                    self.import_folders[i][k].or_else(|| self.import_targets[i][k].map(|t| self.file_node[t as usize]));
                if let Some(target) = target {
                    self.add_edge(fnode, target, EdgeKind::Imports, meta);
                }
                if self.is_python(fid) {
                    self.python_submodule_imports(fid, imp, meta);
                }
            }
        }
    }

    /// Python `from m import name`: `IMPORTS` of the submodule `m.name`
    /// when it is a project file.
    fn python_submodule_imports(&mut self, fid: FileId, imp: &Import, meta: EdgeMeta) {
        let fnode = self.file_node[fid as usize];
        for n in imp.names.iter().filter(|n| n.name != "*") {
            let sub = Self::join_module(&imp.module, &n.name);
            if let Some(t) = self.resolve_module(fid, imp.relative_level, &sub) {
                self.add_edge(fnode, self.file_node[t as usize], EdgeKind::Imports, meta);
            }
        }
    }

    // -- module lookups ------------------------------------------------------

    fn join_module(base: &str, name: &str) -> String {
        if base.is_empty() { name.to_string() } else { format!("{base}.{name}") }
    }

    /// Resolves a module specifier written in file `f` to a project file.
    fn resolve_module(&self, f: FileId, level: u32, module: &str) -> Option<FileId> {
        let key = (f, level, module.to_string());
        if let Some(v) = self.module_memo.borrow().get(&key) {
            return *v;
        }
        let pack = self.packs[f as usize]?;
        let imp = module_import(module.to_string(), level);
        let v = pack.resolve_import(&self.files[f as usize].path, &imp, self.project).and_then(|p| self.file_id(&p));
        self.module_memo.borrow_mut().insert(key, v);
        v
    }

    /// First top-level definition of `f` named `name`, preferring exported ones.
    fn top_level_def(&self, f: FileId, name: &str) -> Option<NodeId> {
        self.top_by_name[f as usize].get(name).copied()
    }

    /// ECMAScript default export of `f` (best effort, see crate docs).
    fn default_export(&self, f: FileId) -> Option<NodeId> {
        if let Some(n) = self.top_level_def(f, "default") {
            return Some(n);
        }
        let path = &self.files[f as usize].path;
        let file = basename(path);
        let mut stem = file.split('.').next().unwrap_or(file);
        if stem == "index"
            && let Some(d) = parent_dir(path)
        {
            stem = basename(d);
        }
        let stem = normalize_stem(stem);
        let defs = self.defs(f);
        let exported: Vec<LocalDefId> =
            self.top_level[f as usize].iter().copied().filter(|&j| defs[j as usize].exported).collect();
        let is_cf = |j: LocalDefId| matches!(defs[j as usize].label, Label::Class | Label::Function);
        let named: Vec<LocalDefId> =
            exported.iter().copied().filter(|&j| is_cf(j) && normalize_stem(&defs[j as usize].name) == stem).collect();
        if named.len() == 1 {
            return Some(self.node_of(f, named[0]));
        }
        if exported.len() == 1 && is_cf(exported[0]) {
            return Some(self.node_of(f, exported[0]));
        }
        None
    }

    /// Looks `name` up in module `f`: own definitions, re-exports, (Python)
    /// import bindings and submodules. Transitive, cycle-safe, bounded.
    fn lookup_in_module(&self, f: FileId, name: &str, visited: &mut Vec<(FileId, String)>) -> Option<Target> {
        self.lookup_in_module_as(f, name, false, visited)
    }

    /// `via_star`: reached through an ECMAScript `export *`, where only
    /// names the module itself exports count (not its plain imports).
    fn lookup_in_module_as(
        &self,
        f: FileId,
        name: &str,
        via_star: bool,
        visited: &mut Vec<(FileId, String)>,
    ) -> Option<Target> {
        let via_star = via_star && !self.is_python(f);
        let memo_key = (f, name.to_string(), via_star);
        if let Some(v) = self.lookup_memo.borrow().get(&memo_key) {
            return v.clone();
        }
        let key = (f, memo_key.1.clone());
        if visited.len() > MAX_DEPTH || visited.contains(&key) {
            return None;
        }
        visited.push(key);
        let result = self.lookup_in_module_uncached(f, name, via_star, visited);
        visited.pop();
        if visited.is_empty() {
            self.lookup_memo.borrow_mut().insert(memo_key, result.clone());
        }
        result
    }

    fn lookup_in_module_uncached(
        &self,
        f: FileId,
        name: &str,
        via_star: bool,
        visited: &mut Vec<(FileId, String)>,
    ) -> Option<Target> {
        if let Some(n) = self.module_def(f, name) {
            return Some(Target::Symbol(n));
        }
        // Re-exports, in order. Explicit names first (including a module's
        // re-export of its own names, `export default x` / `export { a as b }`,
        // which beats the default-export heuristic), then star re-exports.
        let stars = match self.explicit_reexport(f, name, visited) {
            ControlFlow::Break(found) => return found,
            ControlFlow::Continue(stars) => stars,
        };
        let python = self.is_python(f);
        if !python
            && name == "default"
            && let Some(n) = self.default_export(f)
        {
            return Some(Target::Symbol(n));
        }
        if name != "default"
            && let Some(found) = self.star_reexport(stars, name, visited)
        {
            return Some(found);
        }
        if let Some(b) = self.own_import_binding(f, name, via_star, visited) {
            return b;
        }
        if python {
            // Submodule `<module>.<name>`.
            let sub = Self::join_module(&self.module_names[f as usize], name);
            if let Some(t) = self.resolve_module(f, 0, &sub) {
                return Some(Target::Module(t));
            }
        }
        None
    }

    /// The re-exports of `f` that name `name` explicitly: `Break` with the
    /// binding, else `Continue` with the targets of its star re-exports.
    fn explicit_reexport(
        &self,
        f: FileId,
        name: &str,
        visited: &mut Vec<(FileId, String)>,
    ) -> ControlFlow<Option<Target>, Vec<Option<FileId>>> {
        let mut stars = Vec::new();
        for (k, imp) in self.imports(f).iter().enumerate() {
            if imp.reexport {
                let target = self.import_targets[f as usize][k];
                self.reexport_in_import(imp, target, name, &mut stars, visited)?;
            }
        }
        ControlFlow::Continue(stars)
    }

    /// One re-export (`imp`, resolved to `target`) looked up for `name`;
    /// unaliased star re-exports are collected in `stars`.
    fn reexport_in_import(
        &self,
        imp: &Import,
        target: Option<FileId>,
        name: &str,
        stars: &mut Vec<Option<FileId>>,
        visited: &mut Vec<(FileId, String)>,
    ) -> ControlFlow<Option<Target>> {
        if imp.names.is_empty() {
            return match reexported_alias(imp.module_alias.as_deref(), name, target, stars) {
                Some(t) => ControlFlow::Break(Some(t)),
                None => ControlFlow::Continue(()),
            };
        }
        for n in &imp.names {
            if n.name == "*" {
                if let Some(t) = reexported_alias(n.alias.as_deref(), name, target, stars) {
                    return ControlFlow::Break(Some(t));
                }
            } else if n.alias.as_deref().unwrap_or(&n.name) == name {
                let found = match target {
                    Some(t) => self.lookup_in_module(t, &n.name, visited),
                    None => Some(Target::External),
                };
                return ControlFlow::Break(found);
            }
        }
        ControlFlow::Continue(())
    }

    /// `name` through the star re-exports of a module (`export * from`): the
    /// first project binding.
    fn star_reexport(
        &self,
        stars: Vec<Option<FileId>>,
        name: &str,
        visited: &mut Vec<(FileId, String)>,
    ) -> Option<Target> {
        stars
            .into_iter()
            .flatten()
            .find_map(|t| self.lookup_in_module_as(t, name, true, visited).filter(|found| *found != Target::External))
    }

    /// Names bound by `f`'s own imports (last binding wins): Python package
    /// `__init__.py` re-exports, and ECMAScript `import { X } from "./x";
    /// export { X }` (packs record the local export only as a flag, so an
    /// imported name that another module imports from here is taken to be
    /// exported — valid code guarantees it). Package imports are
    /// file-scoped (Go, Java): not re-exported.
    fn own_import_binding(
        &self,
        f: FileId,
        name: &str,
        via_star: bool,
        visited: &mut Vec<(FileId, String)>,
    ) -> Option<Option<Target>> {
        if via_star || self.package_of[f as usize].is_some() {
            return None;
        }
        let mut found = None;
        for (k, imp) in self.imports(f).iter().enumerate() {
            if imp.reexport {
                continue;
            }
            if let Some(b) = self.import_binding(f, k, Some(name), visited).pop() {
                found = Some(b.1);
            }
        }
        found
    }

    /// Names bound in `f` by its `k`-th import (only `want` if given).
    fn import_binding(
        &self,
        f: FileId,
        k: usize,
        want: Option<&str>,
        visited: &mut Vec<(FileId, String)>,
    ) -> Vec<(String, Option<Target>)> {
        let imp = &self.imports(f)[k];
        let target = self.import_targets[f as usize][k];
        if imp.names.is_empty() {
            return self.module_binding(f, imp, target, want).into_iter().collect();
        }
        let python = self.is_python(f);
        let mut out = Vec::new();
        for n in &imp.names {
            if n.name == "*" {
                out.extend(self.star_binding(f, n, target, want));
                continue;
            }
            let local = bound_name(n, python);
            if want.is_none_or(|w| w == local) {
                out.push((local.to_string(), self.named_binding(f, imp, n, target, visited)));
            }
        }
        out
    }

    /// The name a module import without names binds: its alias, or (Python
    /// `import a.b`) its first segment.
    fn module_binding(
        &self,
        f: FileId,
        imp: &Import,
        target: Option<FileId>,
        want: Option<&str>,
    ) -> Option<(String, Option<Target>)> {
        let wants = |n: &str| want.is_none_or(|w| w == n);
        if let Some(a) = &imp.module_alias {
            return wants(a).then(|| (a.clone(), Some(target.map_or(Target::External, Target::Module))));
        }
        if !self.is_python(f) || imp.relative_level != 0 {
            return None;
        }
        let root = imp.module.split('.').next().unwrap_or("");
        if root.is_empty() || !wants(root) {
            return None;
        }
        let t = if let Some(m) = self.resolve_module(f, 0, root) {
            Target::Module(m)
        } else if target.is_some() {
            Target::Prefix(root.to_string())
        } else {
            Target::External
        };
        Some((root.to_string(), Some(t)))
    }

    /// The names a `*` entry of an import binds: its alias (`import * as
    /// ns`), the names of a package or (C/C++, transitively) an include, or
    /// the exported top-level definitions of a Python module.
    fn star_binding(
        &self,
        f: FileId,
        n: &ImportedName,
        target: Option<FileId>,
        want: Option<&str>,
    ) -> Vec<(String, Option<Target>)> {
        let wants = |n: &str| want.is_none_or(|w| w == n);
        if let Some(a) = &n.alias {
            let bound = (a.clone(), Some(target.map_or(Target::External, Target::Module)));
            return if wants(a) { vec![bound] } else { Vec::new() };
        }
        let Some(t) = target else { return Vec::new() };
        if !self.is_python(f) {
            return if self.star_binds(f, t) { self.star_exports(t, want) } else { Vec::new() };
        }
        let defs = self.defs(t);
        self.top_level[t as usize]
            .iter()
            .filter(|&&j| {
                let d = &defs[j as usize];
                d.exported && !d.name.starts_with('_') && wants(&d.name)
            })
            .map(|&j| (defs[j as usize].name.clone(), Some(Target::Symbol(self.node_of(t, j)))))
            .collect()
    }

    /// What a named import entry `n` of `imp` binds; `C.m` (a member import)
    /// binds `m`.
    fn named_binding(
        &self,
        f: FileId,
        imp: &Import,
        n: &ImportedName,
        target: Option<FileId>,
        visited: &mut Vec<(FileId, String)>,
    ) -> Option<Target> {
        let python = self.is_python(f);
        let bound = match target {
            Some(t) => self.imported_member(f, t, &n.name, python, visited),
            None => None,
        };
        if bound.is_some() {
            return bound;
        }
        if python && let Some(m) = self.resolve_module(f, imp.relative_level, &Self::join_module(&imp.module, &n.name))
        {
            return Some(Target::Module(m));
        }
        target.is_none().then_some(Target::External)
    }

    /// `name` in module `t`; `C.m` outside Python is a member of an
    /// imported type (Java `import static`).
    fn imported_member(
        &self,
        f: FileId,
        t: FileId,
        name: &str,
        python: bool,
        visited: &mut Vec<(FileId, String)>,
    ) -> Option<Target> {
        match name.split_once('.') {
            Some((head, rest)) if !python => match self.lookup_in_module(t, head, visited) {
                Some(Target::Symbol(c)) => self.walk(f, Target::Symbol(c), &rest.split('.').collect::<Vec<_>>()),
                other => other,
            },
            _ => self.lookup_in_module(t, name, visited),
        }
    }

    fn build_envs(&mut self) {
        let envs = (0..self.files.len()).map(|i| self.env_of(i as FileId)).collect();
        self.envs = envs;
    }

    /// The names bound by the imports of `fid`. PHP names arrive fully
    /// qualified (the pack applies `use`), so PHP files have no binding
    /// environment.
    fn env_of(&self, fid: FileId) -> Env {
        let mut env = Env::new();
        if self.is_php(fid) {
            return env;
        }
        let python = self.is_python(fid);
        for (k, imp) in self.imports(fid).iter().enumerate() {
            if imp.reexport && !python {
                continue;
            }
            let star = imp.names.iter().any(|n| n.name == "*" && n.alias.is_none());
            for (name, t) in self.import_binding(fid, k, None, &mut Vec::new()) {
                if star {
                    env.entry(name).or_insert(t);
                } else {
                    env.insert(name, t);
                }
            }
        }
        env
    }

    // -- PHP -----------------------------------------------------------------

    fn build_php_index(&mut self) {
        let mut index = PhpIndex::default();
        for i in 0..self.files.len() {
            if self.is_php(i as FileId) {
                self.index_php_file(i, &mut index);
            }
        }
        self.php = index;
    }

    fn index_php_file(&self, i: usize, index: &mut PhpIndex) {
        let defs = self.defs(i as FileId);
        for (j, d) in defs.iter().enumerate() {
            let node = self.def_node[i][j];
            if matches!(d.label, Label::Trait | Label::Enum) {
                index.open.insert(node);
            }
            let in_type = self.parents[i][j].is_some_and(|p| defs[p as usize].label.is_type());
            index.insert(d.label, in_type, &self.nodes[node as usize].qualified_name, node);
        }
    }

    /// First non-test node with this fully qualified name, else the first.
    fn php_pick(map: &HashMap<String, Vec<NodeId>>, key: &str, nodes: &[Node]) -> Option<NodeId> {
        let v = map.get(key)?;
        v.iter().copied().find(|&n| !nodes[n as usize].is_test).or_else(|| v.first().copied())
    }

    /// A PHP symbol by fully qualified name, for a reference of `kind`.
    fn php_symbol(&self, kind: RefKind, fqn: &str) -> Option<NodeId> {
        let lower = fqn.to_lowercase();
        match kind {
            RefKind::Call => Self::php_pick(&self.php.functions, &lower, &self.nodes),
            // Constants, then `X::class` reads of a class.
            RefKind::Read => Self::php_pick(&self.php.constants, fqn, &self.nodes)
                .or_else(|| Self::php_pick(&self.php.types, &lower, &self.nodes)),
            RefKind::Write => None,
            _ => Self::php_pick(&self.php.types, &lower, &self.nodes),
        }
    }

    /// A PHP class by fully qualified name; failing that, the class of that
    /// short name declared in the file Composer autoloading maps it to.
    fn php_class(&self, f: FileId, fqn: &str) -> Option<(NodeId, bool)> {
        if let Some(n) = Self::php_pick(&self.php.types, &fqn.to_lowercase(), &self.nodes) {
            return Some((n, true));
        }
        let short = fqn.rsplit('\\').next().unwrap_or(fqn);
        if !fqn.contains('\\') && self.packs[f as usize].is_some_and(|p| p.is_builtin(short)) {
            return None;
        }
        let file = self.resolve_module(f, 0, fqn)?;
        self.top_level_def(file, short).filter(|&n| self.nodes[n as usize].label.is_type()).map(|n| (n, false))
    }

    /// Whether a fully qualified name lives under a namespace the project
    /// declares (so a miss is a miss, not a third-party name).
    fn php_in_project(&self, fqn: &str) -> bool {
        fqn.split_once('\\').is_some_and(|(root, _)| self.php.roots.contains(&root.to_lowercase()))
    }

    /// Whether a type (or one of its project bases) is open: its members may
    /// be declared outside the project (see [`PhpIndex::open`]).
    fn php_is_open(&self, class: NodeId) -> bool {
        let mut stack = vec![(class, 0usize)];
        let mut seen = Vec::new();
        while let Some((c, depth)) = stack.pop() {
            if self.php.open.contains(&c) {
                return true;
            }
            if depth >= MAX_DEPTH || seen.contains(&c) {
                continue;
            }
            seen.push(c);
            for &b in self.bases.get(&c).map_or(&[][..], |v| v.as_slice()) {
                stack.push((b, depth + 1));
            }
        }
        false
    }

    /// A PHP member lookup that missed: external when the class is open (or
    /// unknown, as for `$this` in a Pest closure), else `fallback`.
    fn php_member_miss(&self, class: Option<NodeId>, fallback: impl FnOnce() -> Outcome) -> Outcome {
        match class {
            Some(c) if !self.php_is_open(c) => fallback(),
            _ => Outcome::External,
        }
    }

    /// Member `name` of the class enclosing `scope` (`$this`, `self`, `static`).
    fn self_member(&self, f: FileId, scope: Option<LocalDefId>, name: &str, want: Want) -> Outcome {
        match self.enclosing_type(f, scope).and_then(|c| self.find_member(c, name, want)) {
            Some((n, false)) => Outcome::Hit(n, Strategy::TypeResolved, 0.9),
            Some((n, true)) => Outcome::Hit(n, Strategy::TypeResolved, 0.85),
            None => Outcome::Miss,
        }
    }

    /// Member `name` of a base of the class enclosing `scope` (`super`, `parent`).
    fn super_member(&self, f: FileId, scope: Option<LocalDefId>, name: &str, want: Want) -> Outcome {
        let found = self
            .enclosing_type(f, scope)
            .and_then(|c| self.bases.get(&c)?.iter().find_map(|&b| self.find_member(b, name, want)));
        match found {
            Some((n, _)) => Outcome::Hit(n, Strategy::TypeResolved, 0.85),
            None => Outcome::Miss,
        }
    }

    /// PHP references (receiver encoding documented in `rueca-lang-php`):
    ///
    /// - `\A\B\` + `C`: the symbol `A\B\C` (`ImportResolved` 0.95);
    ///   classes not in the index are looked up in the file Composer maps
    ///   them to (0.80).
    /// - `A\B\` + `f` (unqualified function or constant in a namespace):
    ///   `A\B\f`, then the global `f` (0.90).
    /// - `\A\C` + `m`: static member of class `A\C` (`ImportResolved`
    ///   0.85); `\A\C->` + `m`: member of a typed instance (`TypeResolved`
    ///   0.85). Members are searched through parents, traits and interfaces.
    /// - `$this` / `self` / `static`: member of the enclosing class;
    ///   `parent`: member of its bases.
    /// - Anything else: unknown receiver (unique method name, 0.50).
    ///
    /// Names that are not found are builtins (`strlen`), misses (under a
    /// project namespace) or external (third-party namespaces, vendor
    /// classes). A missing member of an *open* class (one extending or using
    /// something outside the project, a trait, an enum) is external: it is
    /// most likely inherited from vendor code. A missing member of a closed
    /// project class (magic `__call` / `__callStatic`) falls back to the
    /// unique-method heuristic.
    fn resolve_php(&self, f: FileId, item: &RefItem<'_>, scope: Option<LocalDefId>) -> Outcome {
        let Some(receiver) = item.receiver.map(str::trim) else {
            return self.php_unqualified(f, item, scope);
        };
        let selfs: &[&str] = self.packs[f as usize].map_or(&[], |p| p.self_receivers());
        if selfs.contains(&receiver) || receiver == "parent" {
            return self.php_self_member(f, item, scope, receiver == "parent");
        }
        // Namespace qualifier: a class, function or constant by name.
        if let Some(qualifier) = receiver.strip_suffix('\\') {
            return self.php_qualified(f, item, qualifier);
        }
        // Class member: `\A\C` (static) or `\A\C->` (typed instance).
        if let Some(class) = receiver.strip_prefix('\\') {
            return self.php_class_member(f, item, class);
        }
        // Unknown receiver (`$repo`, `foo()->bar`, `class@anonymous`).
        self.unique_name(f, item.kind, item.name, true)
    }

    /// A PHP name without receiver: local scope, the file, unique name.
    fn php_unqualified(&self, f: FileId, item: &RefItem<'_>, scope: Option<LocalDefId>) -> Outcome {
        if let Some(n) = self.local_scope(f, scope, item.name) {
            return Outcome::Hit(n, Strategy::SameModule, 0.95);
        }
        if let Some(n) = self.top_level_def(f, item.name) {
            return Outcome::Hit(n, Strategy::SameModule, 0.9);
        }
        self.unique_name(f, item.kind, item.name, false)
    }

    /// `$this` / `self` / `static` (`parent`: a base) member.
    fn php_self_member(&self, f: FileId, item: &RefItem<'_>, scope: Option<LocalDefId>, parent: bool) -> Outcome {
        let want = Want::of(item.kind);
        let outcome = if parent {
            self.super_member(f, scope, item.name, want)
        } else {
            self.self_member(f, scope, item.name, want)
        };
        match outcome {
            Outcome::Miss => self.php_member_miss(self.enclosing_type(f, scope), || Outcome::Miss),
            hit => hit,
        }
    }

    /// A name behind a namespace qualifier (`\A\B\` exact, `A\B\` with the
    /// global fallback).
    fn php_qualified(&self, f: FileId, item: &RefItem<'_>, qualifier: &str) -> Outcome {
        let name = item.name;
        let (exact, ns) = match qualifier.strip_prefix('\\') {
            Some(ns) => (true, ns),
            None => (false, qualifier),
        };
        let fqn = if ns.is_empty() { name.to_string() } else { format!("{ns}\\{name}") };
        if let Some(n) = self.php_symbol(item.kind, &fqn) {
            return Outcome::Hit(n, Strategy::ImportResolved, 0.95);
        }
        if !exact
            && !ns.is_empty()
            && let Some(n) = self.php_symbol(item.kind, name)
        {
            return Outcome::Hit(n, Strategy::ImportResolved, 0.9);
        }
        if exact
            && !matches!(item.kind, RefKind::Call | RefKind::Write)
            && let Some((n, false)) = self.php_class(f, &fqn)
        {
            return Outcome::Hit(n, Strategy::ImportResolved, 0.8);
        }
        if self.packs[f as usize].is_some_and(|p| p.is_builtin(name)) || self.php_in_project(&fqn) {
            return Outcome::Miss;
        }
        Outcome::External
    }

    /// A member of class `class` (`A\C`, static; `A\C->`, a typed instance).
    fn php_class_member(&self, f: FileId, item: &RefItem<'_>, class: &str) -> Outcome {
        let (class, instance) = match class.strip_suffix("->") {
            Some(c) => (c, true),
            None => (class, false),
        };
        let Some((c, _)) = self.php_class(f, class) else {
            if self.php_in_project(class) {
                return self.unique_name(f, item.kind, item.name, true);
            }
            return Outcome::External;
        };
        match self.find_member(c, item.name, Want::of(item.kind)) {
            Some((n, _)) if instance => Outcome::Hit(n, Strategy::TypeResolved, 0.85),
            Some((n, _)) => Outcome::Hit(n, Strategy::ImportResolved, 0.85),
            None => self.php_member_miss(Some(c), || self.unique_name(f, item.kind, item.name, true)),
        }
    }

    // -- members and bases ---------------------------------------------------

    /// Records `member` (a definition with `label`) under `name` in the
    /// slot of its kind (see [`MemberSlots`]); the first one wins.
    pub(crate) fn add_member(&mut self, owner: NodeId, name: &str, member: NodeId, label: Label) {
        let slots = self.members.entry(owner).or_default().entry(name.to_string()).or_default();
        let slot = if label.is_callable() {
            &mut slots.callable
        } else if label.is_type() {
            &mut slots.ty
        } else {
            &mut slots.value
        };
        slot.get_or_insert(member);
    }

    /// Member `name` of class `class` for a lookup of kind `want`, searching
    /// resolved bases depth-first, left-to-right. Each slot is searched
    /// through the whole hierarchy before the next one (see [`Want`]), so a
    /// field never hides a method for a call, nor a method a field for a
    /// read, and a constructor (a method named like its class) is reached
    /// by calls only. Returns the member and whether it was inherited.
    fn find_member(&self, class: NodeId, name: &str, want: Want) -> Option<(NodeId, bool)> {
        let order: &[Slot] = match want {
            Want::Call => &[Slot::Callable, Slot::Type, Slot::Value],
            Want::Value => &[Slot::Value, Slot::Method, Slot::Type],
            Want::Type => &[Slot::Type, Slot::Value, Slot::Method],
        };
        order.iter().find_map(|&slot| {
            let mut visited = Vec::new();
            self.find_member_rec(class, name, slot, 0, &mut visited).map(|(n, d)| (n, d > 0))
        })
    }

    fn find_member_rec(
        &self,
        class: NodeId,
        name: &str,
        slot: Slot,
        depth: usize,
        visited: &mut Vec<NodeId>,
    ) -> Option<(NodeId, usize)> {
        if depth > MAX_DEPTH || visited.contains(&class) {
            return None;
        }
        visited.push(class);
        if let Some(m) = self.members.get(&class).and_then(|m| m.get(name)) {
            let found = match slot {
                Slot::Callable => m.callable,
                // A constructor is a callable named like its class.
                Slot::Method => m.callable.filter(|_| name != self.nodes[class as usize].name),
                Slot::Type => m.ty,
                Slot::Value => m.value,
            };
            if let Some(n) = found {
                return Some((n, depth));
            }
        }
        for &b in self.bases.get(&class).map_or(&[][..], |v| v.as_slice()) {
            if let Some(found) = self.find_member_rec(b, name, slot, depth + 1, visited) {
                return Some(found);
            }
        }
        None
    }

    /// The constructor of `class` in families whose constructors are the
    /// callables named like the class (Java, C/C++); own, not inherited.
    fn named_constructor(&self, class: NodeId) -> Option<NodeId> {
        let name = &self.nodes[class as usize].name;
        self.members.get(&class).and_then(|m| m.get(name)).and_then(|s| s.callable)
    }

    fn resolve_bases(&mut self) {
        for i in 0..self.files.len() {
            let fid = i as FileId;
            for (j, d) in self.defs(fid).iter().enumerate() {
                if d.bases.is_empty() && d.implements.is_empty() {
                    continue;
                }
                if self.families[i] == Some(LanguageFamily::Data) {
                    self.data_bases(fid, j as LocalDefId, d);
                } else {
                    self.code_bases(fid, j as LocalDefId, d);
                }
            }
        }
    }

    /// `INHERITS` of a data definition (a key path it extends).
    fn data_bases(&mut self, fid: FileId, j: LocalDefId, d: &Definition) {
        let class = self.node_of(fid, j);
        for text in &d.bases {
            if let Outcome::Hit(target, strategy, confidence) = self.data_base(fid, text)
                && target != class
            {
                let meta = EdgeMeta { confidence, strategy, evidence: Some((fid, d.name_span)) };
                self.add_edge(class, target, EdgeKind::Inherits, meta);
            }
        }
    }

    /// The base and implements lists of a code definition, resolved as
    /// `Type` references in the definition's outer scope.
    fn code_bases(&mut self, fid: FileId, j: LocalDefId, d: &Definition) {
        let php = self.is_php(fid);
        // Implemented interfaces contribute members (default methods,
        // constants) in PHP and the RFC 0003 languages.
        let family = self.families[fid as usize];
        let implements_members =
            php || !matches!(family, Some(LanguageFamily::Python | LanguageFamily::EcmaScript) | None);
        let site = BaseSite {
            file: fid,
            class: self.node_of(fid, j),
            scope: self.parents[fid as usize][j as usize],
            span: d.name_span,
            php,
            implements_members,
        };
        for (kind, list) in [(EdgeKind::Inherits, &d.bases), (EdgeKind::Implements, &d.implements)] {
            for text in list {
                self.resolve_base(&site, kind, text);
            }
        }
    }

    /// One entry of a base (`kind` `INHERITS`) or implements list.
    fn resolve_base(&mut self, site: &BaseSite, kind: EdgeKind, text: &str) {
        let split = if site.php { split_php_name(text) } else { split_base(text) };
        let Some((receiver, name)) = split else {
            if !site.php && !text.contains('=') {
                self.open_types.insert(site.class);
            }
            return;
        };
        let item = RefItem {
            kind: RefKind::Type,
            name,
            receiver,
            enclosing: site.scope,
            span: site.span,
            decorated: None,
            arity: None,
            arg_types: None,
        };
        let Outcome::Hit(target, strategy, confidence) = self.resolve_item(site.file, &item, site.scope) else {
            // A parent, trait or interface outside the project.
            if site.php {
                self.php.open.insert(site.class);
            } else {
                self.open_types.insert(site.class);
            }
            return;
        };
        let target = self.prefer_body(target);
        if target == site.class {
            return;
        }
        let meta = EdgeMeta { confidence, strategy, evidence: Some((site.file, site.span)) };
        self.add_edge(site.class, target, kind, meta);
        // PHP: interface constants are members of implementing classes.
        if kind == EdgeKind::Inherits || site.implements_members {
            let v = self.bases.entry(site.class).or_default();
            if !v.contains(&target) {
                v.push(target);
            }
        }
    }

    // -- reference resolution ------------------------------------------------

    /// Strategy 1: a name defined by an enclosing function.
    fn local_scope(&self, f: FileId, scope: Option<LocalDefId>, name: &str) -> Option<NodeId> {
        let defs = self.defs(f);
        // ECMAScript methods of an object literal or a constructor function
        // are properties, never names in scope.
        let ecma = self.families[f as usize] == Some(LanguageFamily::EcmaScript);
        let mut cur = scope;
        while let Some(a) = cur {
            if !defs[a as usize].label.is_type()
                && let Some(&c) = self.child_by_name[f as usize].get(&(a, name))
                && !(ecma && defs[c as usize].label == Label::Method)
            {
                return Some(self.node_of(f, c));
            }
            cur = self.parents[f as usize][a as usize];
        }
        None
    }

    /// Nearest enclosing type definition.
    fn enclosing_type(&self, f: FileId, scope: Option<LocalDefId>) -> Option<NodeId> {
        let defs = self.defs(f);
        let mut cur = scope;
        while let Some(a) = cur {
            let node = self.node_of(f, a);
            if defs[a as usize].label.is_type() || self.is_constructor_function(node) {
                return Some(node);
            }
            cur = self.parents[f as usize][a as usize];
        }
        None
    }

    /// An ECMAScript function or variable used as a class: it owns methods
    /// (`View.prototype.render = function render() {}` makes `render` a
    /// method of `View`; `const api = { get() {} }` and `return { next() {} }`
    /// make `get` / `next` methods of `api` / of the function), so `this`
    /// inside it and its methods, a receiver typed `View` (`new View()`,
    /// `$type:makeCounter`) and a receiver naming it (`api.get()`, also
    /// imported) find those members.
    pub(crate) fn is_constructor_function(&self, n: NodeId) -> bool {
        let node = &self.nodes[n as usize];
        matches!(node.label, Label::Function | Label::Variable | Label::Constant)
            && node.language.map(Language::family) == Some(LanguageFamily::EcmaScript)
            && self.members.get(&n).is_some_and(|m| {
                m.values().any(|s| s.callable.is_some_and(|c| self.nodes[c as usize].label == Label::Method))
            })
    }

    /// Walks a dotted path from a starting binding.
    fn walk(&self, f: FileId, start: Target, segs: &[&str]) -> Option<Target> {
        let mut cur = start;
        for seg in segs {
            cur = match cur {
                Target::Module(m) => self.lookup_in_module(m, seg, &mut Vec::new())?,
                Target::Symbol(n) => {
                    if !self.nodes[n as usize].label.is_type() {
                        return None;
                    }
                    Target::Symbol(self.find_member(n, seg, Want::Type)?.0)
                }
                Target::Prefix(p) => {
                    let next = format!("{p}.{seg}");
                    match self.resolve_module(f, 0, &next) {
                        Some(m) => Target::Module(m),
                        None => Target::Prefix(next),
                    }
                }
                Target::External => return Some(Target::External),
            };
        }
        Some(cur)
    }

    fn resolve_item(&self, f: FileId, item: &RefItem<'_>, scope: Option<LocalDefId>) -> Outcome {
        if self.is_php(f) {
            return self.resolve_php(f, item, scope);
        }
        match item.receiver.map(str::trim) {
            None => self.resolve_bare(f, item, scope),
            Some(receiver) => self.resolve_with_receiver(f, item, scope, receiver),
        }
    }

    /// A name without receiver: strategies 1, 1b, 1c, 3, 4 and 6.
    fn resolve_bare(&self, f: FileId, item: &RefItem<'_>, scope: Option<LocalDefId>) -> Outcome {
        let name = item.name;
        // 1. Local scope.
        if let Some(n) = self.local_scope(f, scope, name) {
            return Outcome::Hit(n, Strategy::SameModule, 0.95);
        }
        // 1b. Implicit self (`implicit_self` packs): enclosing class member.
        if let Some(hit) = self.implicit_member(f, scope, name, Want::of(item.kind)) {
            return hit;
        }
        // 1c. C++: a member of an enclosing namespace.
        if let Some(hit) = self.namespace_scope(f, scope, name) {
            return hit;
        }
        // 3. Imported binding.
        if let Some(hit) = self.imported_name(f, item) {
            return hit;
        }
        // 4. Same module (the whole package for package-scoped packs).
        if let Some(n) = self.module_def(f, name) {
            return Outcome::Hit(n, Strategy::SameModule, 0.9);
        }
        // 6. Unique name.
        self.unique_name(f, item.kind, name, false)
    }

    /// Strategy 3 for a bare name: its import binding, if any.
    fn imported_name(&self, f: FileId, item: &RefItem<'_>) -> Option<Outcome> {
        match self.envs[f as usize].get(item.name) {
            Some(Some(Target::Symbol(n))) => Some(Outcome::Hit(*n, Strategy::ImportResolved, 0.95)),
            // ECMAScript: calling, constructing or typing by a module
            // binding (`const log = require("./log"); log()`) is its
            // default export (`module.exports = …`).
            Some(Some(Target::Module(m)))
                if self.families[f as usize] == Some(LanguageFamily::EcmaScript)
                    && matches!(item.kind, RefKind::Call | RefKind::New | RefKind::Type) =>
            {
                Some(match self.lookup_in_module(*m, "default", &mut Vec::new()) {
                    Some(Target::Symbol(n)) => Outcome::Hit(n, Strategy::ImportResolved, 0.9),
                    _ => Outcome::External,
                })
            }
            Some(Some(_)) => Some(Outcome::External),
            Some(None) | None => None,
        }
    }

    /// A name with a receiver: self (2), typed (5), namespace and member
    /// chain receivers, `super`, a dotted path (3, 4), unique method name (6).
    fn resolve_with_receiver(
        &self,
        f: FileId,
        item: &RefItem<'_>,
        scope: Option<LocalDefId>,
        receiver: &str,
    ) -> Outcome {
        let selfs: &[&str] = self.packs[f as usize].map_or(&[], |p| p.self_receivers());
        // 2. Self receiver (`$self`: also a method declared outside its type).
        if selfs.contains(&receiver) {
            return self.self_receiver_member(f, item, scope, receiver);
        }
        // 5. Typed receiver: `$type:T` (declared type known in the file).
        if let Some(ty) = receiver.strip_prefix(TYPE_RECEIVER_PREFIX) {
            return self.resolve_typed(f, item, scope, ty);
        }
        // C++ namespace-qualified name: `$ns:a::b`.
        if let Some(path) = receiver.strip_prefix(NAMESPACE_RECEIVER_PREFIX) {
            return self.resolve_namespaced(f, item, scope, path);
        }
        // A chain of members of the enclosing type: `$member:a.b`.
        if let Some(chain) = receiver.strip_prefix(MEMBER_RECEIVER_PREFIX) {
            return self.resolve_member_chain(f, item, scope, chain);
        }
        if receiver == "super()" || receiver == "super" {
            return self.super_member(f, scope, item.name, Want::of(item.kind));
        }
        if let Some(hit) = self.resolve_dotted(f, item, receiver, selfs) {
            return hit;
        }
        // 6. Unique method name.
        self.unique_name(f, item.kind, item.name, true)
    }

    /// Strategy 2: a member of the class enclosing `scope` (for `$self`,
    /// else the receiver type named by a method declared outside its type).
    fn self_receiver_member(
        &self,
        f: FileId,
        item: &RefItem<'_>,
        scope: Option<LocalDefId>,
        receiver: &str,
    ) -> Outcome {
        let class = self
            .enclosing_type(f, scope)
            .or_else(|| if receiver == SELF_RECEIVER { self.receiver_type_from_name(f, scope) } else { None });
        match class.and_then(|c| self.find_member(c, item.name, Want::of(item.kind))) {
            Some((n, false)) => Outcome::Hit(n, Strategy::TypeResolved, 0.9),
            Some((n, true)) => Outcome::Hit(n, Strategy::TypeResolved, 0.85),
            None => Outcome::Miss,
        }
    }

    /// A dotted receiver of identifiers: bound by an import (3) or a class
    /// of the same module (4). `None` falls through to strategy 6.
    fn resolve_dotted(&self, f: FileId, item: &RefItem<'_>, receiver: &str, selfs: &[&str]) -> Option<Outcome> {
        let segs: Vec<&str> = receiver.split('.').collect();
        if !segs.iter().all(|s| is_ident(s)) || selfs.contains(&segs[0]) {
            return None;
        }
        match self.envs[f as usize].get(segs[0]) {
            Some(Some(start)) => self.member_of_import(f, item, start, &segs[1..]),
            Some(None) => None,
            None => self.member_of_local(f, item, &segs),
        }
    }

    /// 3. Receiver bound by an import, walked along `rest`.
    fn member_of_import(&self, f: FileId, item: &RefItem<'_>, start: &Target, rest: &[&str]) -> Option<Outcome> {
        match self.walk(f, start.clone(), rest) {
            Some(Target::Module(m)) => match self.lookup_in_module(m, item.name, &mut Vec::new()) {
                Some(Target::Symbol(n)) => Some(Outcome::Hit(n, Strategy::ImportResolved, 0.9)),
                Some(_) => Some(Outcome::External),
                None => None,
            },
            Some(Target::Symbol(c)) if self.owns_members(c) => self
                .find_member(c, item.name, Want::of(item.kind))
                .map(|(n, _)| Outcome::Hit(n, Strategy::ImportResolved, 0.85)),
            Some(Target::External) | Some(Target::Prefix(_)) => Some(Outcome::External),
            _ => None,
        }
    }

    /// 4. Receiver is a class of the same module (`segs[0]` not imported).
    fn member_of_local(&self, f: FileId, item: &RefItem<'_>, segs: &[&str]) -> Option<Outcome> {
        let local = self.module_def(f, segs[0]);
        if local.is_none() && self.packs[f as usize].is_some_and(|p| p.is_builtin(segs[0])) {
            // `console.log`, `Math.max`, `str.join`: a builtin object.
            return Some(Outcome::External);
        }
        let c = local.filter(|&c| self.owns_members(c))?;
        let Some(Target::Symbol(c)) = self.walk(f, Target::Symbol(c), &segs[1..]) else { return None };
        if !self.owns_members(c) {
            return None;
        }
        self.find_member(c, item.name, Want::of(item.kind)).map(|(n, _)| Outcome::Hit(n, Strategy::SameModule, 0.85))
    }

    /// Whether `c` has members a receiver can name: a type or an ECMAScript
    /// constructor function.
    fn owns_members(&self, c: NodeId) -> bool {
        self.nodes[c as usize].label.is_type() || self.is_constructor_function(c)
    }

    /// Strategy 6.
    fn unique_name(&self, f: FileId, kind: RefKind, name: &str, with_receiver: bool) -> Outcome {
        if kind == RefKind::Write || name.is_empty() {
            return Outcome::Miss;
        }
        if self.packs[f as usize].is_some_and(|p| p.is_builtin(name)) {
            return Outcome::Miss;
        }
        if with_receiver && (name.chars().count() < 4 || BUILTIN_METHODS.contains(&name)) {
            return Outcome::Miss;
        }
        let family = self.families[f as usize];
        let Some(cands) = self.by_name.get(name) else { return Outcome::Miss };
        let mut found = None;
        for &n in cands {
            let node = &self.nodes[n as usize];
            let same_family = node.language.map(|l| l.family()) == family;
            if same_family && compatible(kind, node.label, with_receiver) {
                if found.is_some() {
                    return Outcome::Miss;
                }
                found = Some(n);
            }
        }
        match found {
            Some(n) if with_receiver => Outcome::Hit(n, Strategy::Heuristic, 0.5),
            Some(n) => Outcome::Hit(n, Strategy::UniqueName, 0.6),
            None => Outcome::Miss,
        }
    }

    /// The definition a decorator reference applies to: the first decorated
    /// definition whose name starts after the decorator.
    fn decorated_def(&self, f: FileId, r: &RefItem<'_>) -> Option<LocalDefId> {
        if r.decorated.is_some() {
            return r.decorated;
        }
        let defs = self.defs(f);
        let found = defs
            .iter()
            .enumerate()
            .filter(|(_, d)| !d.decorators.is_empty() && d.name_span.start_byte >= r.span.end_byte)
            .min_by_key(|(j, d)| (d.name_span.start_byte, *j))
            .map(|(j, _)| j as LocalDefId);
        found.or(r.enclosing)
    }

    /// If `r` names one of a class's own bases, returns the class and the edge kind.
    fn base_ref(&self, f: FileId, r: &RefItem<'_>) -> Option<(LocalDefId, EdgeKind)> {
        let defs = self.defs(f);
        let mut best: Option<LocalDefId> = None;
        for &j in &self.with_bases[f as usize] {
            let d = &defs[j as usize];
            if !d.span.contains(&r.span) {
                continue;
            }
            if best.is_some_and(|b| !defs[b as usize].span.contains(&d.span)) {
                continue;
            }
            let inside_child =
                self.children[f as usize][j as usize].iter().any(|&c| defs[c as usize].span.contains(&r.span));
            if !inside_child {
                best = Some(j);
            }
        }
        let c = best?;
        let d = &defs[c as usize];
        let matches = |text: &String| split_base(text) == Some((r.receiver, r.name));
        if d.bases.iter().any(matches) {
            Some((c, EdgeKind::Inherits))
        } else if d.implements.iter().any(matches) {
            Some((c, EdgeKind::Implements))
        } else {
            None
        }
    }

    fn resolve_references(&mut self) {
        let files: &'a [FileInput] = self.files;
        for (i, file) in files.iter().enumerate() {
            let Some(facts) = file.facts.as_ref() else { continue };
            let mut items = Self::reference_items(facts);
            // Packs that do not report decorator references: synthesize them
            // from the raw decorator texts.
            if !facts.references.iter().any(|r| r.kind == RefKind::Decorator) {
                self.synthesize_decorators(i, facts, &mut items);
            }
            for item in &items {
                self.resolve_one(i as FileId, item);
            }
        }
    }

    /// The references of a file as items to resolve.
    fn reference_items(facts: &'a FileFacts) -> Vec<RefItem<'a>> {
        let n_defs = facts.definitions.len();
        let valid = |e: Option<LocalDefId>| e.filter(|&x| (x as usize) < n_defs);
        facts
            .references
            .iter()
            .map(|r: &'a Reference| {
                let (name, arg_types) = split_arg_types(&r.name);
                RefItem {
                    kind: r.kind,
                    name,
                    receiver: r.receiver.as_deref(),
                    enclosing: valid(r.enclosing),
                    span: r.span,
                    decorated: None,
                    arity: r.arity,
                    arg_types,
                }
            })
            .collect()
    }

    /// Decorator references from the raw decorator texts of file `i`.
    fn synthesize_decorators(&self, i: usize, facts: &'a FileFacts, items: &mut Vec<RefItem<'a>>) {
        for (j, d) in facts.definitions.iter().enumerate() {
            for (receiver, name) in d.decorators.iter().filter_map(|text| split_decorator(text)) {
                items.push(RefItem {
                    kind: RefKind::Decorator,
                    name,
                    receiver,
                    enclosing: self.parents[i][j],
                    span: d.name_span,
                    decorated: Some(j as LocalDefId),
                    arity: None,
                    arg_types: None,
                });
            }
        }
    }

    fn resolve_one(&mut self, f: FileId, item: &RefItem<'_>) {
        self.stats.references_total += 1;
        let fnode = self.file_node[f as usize];
        let caller = item.enclosing.map_or(fnode, |e| self.node_of(f, e));
        if matches!(item.kind, RefKind::EnvVar | RefKind::ConfigKey | RefKind::DataRef) {
            self.resolve_bridge(f, item, caller);
            return;
        }
        let plan = self.plan_edges(f, item);
        let Some((target, strategy, confidence)) = self.count_outcome(f, item, plan.outcome) else { return };
        // Declarations → their body; overloads → by arity (RFC 0003 §10).
        let target = self.prefer_body(target);
        for (target, strategy, confidence) in self.expand_overloads(f, caller, item, target, strategy, confidence) {
            let meta = EdgeMeta { confidence, strategy, evidence: Some((f, item.span)) };
            match plan.kind {
                EdgeKind::Decorates => {
                    if let Some(dst) = plan.dst {
                        self.add_edge(target, dst, EdgeKind::Decorates, meta);
                    }
                }
                EdgeKind::Inherits | EdgeKind::Implements => {
                    let src = plan.src.unwrap_or(caller);
                    if src != target {
                        self.add_edge(src, target, plan.kind, meta);
                    }
                }
                EdgeKind::Calls => self.add_call(f, item, caller, target, meta),
                kind => self.add_edge(caller, target, kind, meta),
            }
        }
    }

    /// How a reference resolves and where its edges go.
    fn plan_edges(&self, f: FileId, item: &RefItem<'_>) -> EdgePlan {
        if item.kind == RefKind::Decorator {
            let decorated = self.decorated_def(f, item);
            let scope = match decorated {
                Some(d) if item.enclosing == Some(d) => self.parents[f as usize][d as usize],
                _ => item.enclosing,
            };
            let dst = decorated.map(|d| self.node_of(f, d));
            return EdgePlan { outcome: self.resolve_item(f, item, scope), src: None, dst, kind: EdgeKind::Decorates };
        }
        let base = match item.kind {
            RefKind::Type | RefKind::Read => self.base_ref(f, item),
            _ => None,
        };
        // A name in a class's own base / implements list.
        if let Some((c, kind)) = base {
            let scope = self.parents[f as usize][c as usize];
            let src = Some(self.node_of(f, c));
            return EdgePlan { outcome: self.resolve_item(f, item, scope), src, dst: None, kind };
        }
        let kind = match item.kind {
            RefKind::Call | RefKind::New => EdgeKind::Calls,
            RefKind::Throw => EdgeKind::Throws,
            RefKind::Write => EdgeKind::Writes,
            _ => EdgeKind::References,
        };
        EdgePlan { outcome: self.resolve_item(f, item, item.enclosing), src: None, dst: None, kind }
    }

    /// Counts an outcome in the statistics; the target of a hit.
    fn count_outcome(&mut self, f: FileId, item: &RefItem<'_>, outcome: Outcome) -> Option<(NodeId, Strategy, f32)> {
        match outcome {
            Outcome::Hit(t, s, c) => {
                self.stats.references_resolved += 1;
                *self.stats.resolved_by_strategy.entry(s.as_str().to_string()).or_default() += 1;
                Some((t, s, c))
            }
            Outcome::External => {
                self.stats.references_external += 1;
                None
            }
            Outcome::Miss => {
                self.count_miss(f, item);
                None
            }
        }
    }

    fn count_miss(&mut self, f: FileId, item: &RefItem<'_>) {
        tracing::trace!(
            kind = ?item.kind,
            receiver = item.receiver.unwrap_or("-"),
            name = item.name,
            path = %self.files[f as usize].path,
            "unresolved"
        );
        // PHP reports builtins with a namespace qualifier (`App\`, `\`).
        let unqualified = match item.receiver {
            None => true,
            Some(r) => self.is_php(f) && r.ends_with('\\'),
        };
        if unqualified && self.packs[f as usize].is_some_and(|p| p.is_builtin(item.name)) {
            self.stats.references_builtin += 1;
        }
    }

    /// `CALLS` to `target`; a call of a type also calls its constructor.
    fn add_call(&mut self, f: FileId, item: &RefItem<'_>, caller: NodeId, target: NodeId, meta: EdgeMeta) {
        self.add_edge(caller, target, EdgeKind::Calls, meta);
        if !self.nodes[target as usize].label.is_type() {
            return;
        }
        let Some((c, _)) = self.constructor_of(f, target) else { return };
        let c = self.prefer_body(c);
        for (c, s, conf) in self.expand_overloads(f, caller, item, c, meta.strategy, meta.confidence * 0.95) {
            let m = EdgeMeta { confidence: conf, strategy: s, ..meta };
            self.add_edge(caller, c, EdgeKind::Calls, m);
        }
    }

    /// The constructor a call of type `class` from file `f` reaches.
    fn constructor_of(&self, f: FileId, class: NodeId) -> Option<(NodeId, bool)> {
        match self.families[f as usize] {
            Some(LanguageFamily::Php) => self.find_member(class, "__construct", Want::Call),
            // Constructors are members named like the class.
            Some(LanguageFamily::Java | LanguageFamily::C) => self.named_constructor(class).map(|c| (c, false)),
            _ => self
                .find_member(class, "__init__", Want::Call)
                .or_else(|| self.find_member(class, "constructor", Want::Call)),
        }
    }

    /// `EnvVar` / `ConfigKey` → `READS`, `DataRef` → `REFERENCES` (see the
    /// `bridge` module); several targets are all linked.
    fn resolve_bridge(&mut self, f: FileId, item: &RefItem<'_>, caller: NodeId) {
        let (targets, external) = self.bridge_targets(f, item);
        let Some(&(_, strategy, _)) = targets.first() else {
            if external {
                self.stats.references_external += 1;
            } else {
                tracing::trace!(kind = ?item.kind, name = item.name, path = %self.files[f as usize].path, "unresolved");
            }
            return;
        };
        self.stats.references_resolved += 1;
        *self.stats.resolved_by_strategy.entry(strategy.as_str().to_string()).or_default() += 1;
        let kind = if item.kind == RefKind::DataRef { EdgeKind::References } else { EdgeKind::Reads };
        for (target, strategy, confidence) in targets {
            let meta = EdgeMeta { confidence, strategy, evidence: Some((f, item.span)) };
            self.add_edge(caller, target, kind, meta);
        }
    }

    fn tests_edges(&mut self) {
        let calls: Vec<(NodeId, NodeId, EdgeMeta)> = self
            .edges
            .iter()
            .filter(|((src, kind, dst), _)| {
                *kind == EdgeKind::Calls && self.nodes[*src as usize].is_test && !self.nodes[*dst as usize].is_test
            })
            .map(|((src, _, dst), meta)| (*src, *dst, *meta))
            .collect();
        for (src, dst, meta) in calls {
            let file_like = matches!(self.nodes[dst as usize].label, Label::File | Label::Folder | Label::Repository);
            if !file_like {
                self.add_edge(src, dst, EdgeKind::Tests, meta);
            }
        }
    }
}

#[cfg(test)]
mod tests;
