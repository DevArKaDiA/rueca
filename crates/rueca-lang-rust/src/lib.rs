//! Rust language pack for Rueca (`tree-sitter-rust` 0.24).
//!
//! See `rueca_extract::LanguagePack` for the contract. This crate turns one
//! `.rs` file into [`FileFacts`] and implements Rust's module model (crates
//! and modules from `Cargo.toml` and the file layout), the module paths of
//! imports, and the builtins. Cross-file resolution of Rust paths, `impl`
//! blocks and trait methods is the Rust part of `rueca-resolve` (its crate
//! docs, *Rust*).
//!
//! What is assembled and what is Rueca code (RFC 0002 §3.4, §4.1):
//!
//! - **Grammar**: `tree-sitter-rust` 0.24 (MIT), linked against the
//!   tree-sitter 0.27 runtime (it depends only on `tree-sitter-language`).
//! - **Local scopes** come from a vendored, adapted `queries/locals.scm`
//!   (nvim-treesitter, Apache-2.0; provenance in `queries/README.md`,
//!   semantics in `locals.rs`). They decide which identifiers are locals.
//! - **Cargo manifests** are read with `toml_edit` (MIT OR Apache-2.0; the
//!   data pack's TOML parser).
//! - Definitions, qualified names, imports, receivers, macro bodies and
//!   arguments, attribute macros and the module model are Rueca code
//!   (`extract.rs`, `refs.rs`, `receivers.rs`, `expand.rs`, `macros.rs`,
//!   `decorators.rs`, `uses.rs`, `modules.rs`). Upstream
//!   `tags.scm` (MIT) is not vendored: it has no containment, receivers or
//!   qualified names.
//!
//! # Module names and qualified names
//!
//! A Rust module is a file (or an inline `mod m { … }`), and code refers to
//! it by **module path**. The module name of a file is its module path,
//! crate name first: `rueca_index::link` for
//! `crates/rueca-index/src/link.rs` (see `modules.rs` for crates, roots and
//! the file layout). Every file is its own module (`ModuleScope::File`).
//! Global qualified name = module path + `::` + local name:
//! `rueca_index::link::PackageLinker.analyze`.
//!
//! Local qualified names join items with `.`, like Java nested classes;
//! file-level modules are not part of them (like Go packages):
//!
//! | Definition | Label | Local qualified name |
//! |---|---|---|
//! | `fn f()` at file level | `Function` | `f` |
//! | `fn inner()` inside `fn outer` | `Function` (child of `outer`) | `outer.inner` |
//! | `fn f()` inside `mod m { … }` | `Function` (child of `m`) | `m.f` |
//! | `mod m { … }` | `Module` | `m` (its items are its children) |
//! | `struct` / `union` | `Struct`; named fields → `Field` `S.a` | `S` |
//! | `enum` | `Enum`; variants → `Constant` `E.A` | `E` |
//! | `trait Tr` | `Interface`; supertraits → bases | `Tr` |
//! | `fn m` / `type T` / `const K` in `trait Tr` | `Method` / `TypeAlias` / `Constant` (children of `Tr`) | `Tr.m` |
//! | `fn m` / `const K` in `impl [Tr for] Type` | `Method` / `Constant` | `Type.m` — `Type` is the last segment of the self type, generics and references stripped (`impl<T> Vec<T>` → `Vec.m`, `impl Display for &Foo<'a>` → `Foo.fmt`); inside `mod m`: `m.Type.m` |
//! | `type A = B` | `TypeAlias`; `B` is a base (its members are found through `A`) | `A` |
//! | `const` / `static` | `Constant` / `Variable` | `K` |
//! | `macro_rules! name` | `Function` (callable: invocations are calls), signature `macro_rules! name` | `name` |
//! | `fn` / `static` in `extern "C" { … }` | `Function` / `Variable` | `f` |
//!
//! - Items of an `impl` block are not children of the type (the block may
//!   be in another file): they are top level (or children of their inline
//!   module), with `bases = [self type]` and `implements = [trait]` (paths
//!   as written, normalized like receivers, see *Paths*); the resolver
//!   attaches them to the type and adds `IMPLEMENTS` type → trait and
//!   `OVERRIDES` method → trait method. When the file declares the type,
//!   the trait is also in the type's own `implements` (an `impl Tr for T
//!   {}` without items has no other trace). Items of a blanket impl
//!   (`impl<T: Bound> Tr for T`) are `T.m` with the base `<T>` (`<&T>` for
//!   an impl on `&T`, `&mut T` or `Box<T>`, which forwards to `T`'s impl).
//!   Associated types of an `impl` are not definitions.
//! - Items inside function bodies (functions, types, `impl` blocks) are
//!   children of the function (`outer.inner`, `outer.Local.m`). Closures
//!   are not definitions: their code belongs to the enclosing function.
//! - **Items inside macro invocations** are items where the invocation
//!   stands, when the macro's body parses as items (`cfg_net! { pub mod
//!   tcp; pub fn f() {} }` at file level → `f`; inside `mod m { … }` →
//!   `m.f`; inside `fn outer` → `outer.f`), and so are their `use` and
//!   `mod` declarations; every branch of a `cfg_if!`-shaped body `if
//!   #[cfg(…)] { … } else { … }` counts (two branches may define the same
//!   name). The invocation stays a call of the macro (see `expand.rs`).
//! - `span` is the item without its attributes and doc comment; `name_span`
//!   the identifier; `signature` the item up to its body on one line
//!   (≤ 200 chars); `doc` the `///` / `/** */` comments above it (≤ 1000
//!   chars, attributes between them skipped).
//! - `decorators`: the item's attribute macros as written inside `#[…]`
//!   (`tokio::main(flavor = "current_thread")`), see *References*.
//! - `exported`: the item has a `pub` visibility (any form); trait members
//!   and variants follow their trait or enum; items of trait impls are
//!   exported.
//! - `is_test`: functions with a test attribute (`#[test]`,
//!   `#[tokio::test]` and any `…::test`, `#[bench]`, `#[rstest]`,
//!   `#[test_case(…)]`), everything inside a `#[cfg(test)]` module, and
//!   every definition of a file under a `tests/` or `benches/` directory.
//!   A module file declared by `#[cfg(test)] mod x;` (or `all(test, …)`,
//!   not `any(test, …)`: such a module is also built without tests) is
//!   test code too, which the file alone does not show: the declaring
//!   file's `$test` marker import (*Imports*) lets the resolver mark its
//!   definitions.
//!   Because this depends on the path, [`LanguagePack::facts_depend_on_path`]
//!   is true.
//!
//! # References
//!
//! `enclosing` is the innermost definition: functions and methods for their
//! signatures and bodies (closures included), fields and variants for their
//! types, `const` / `static` items for their values, `None` at file level.
//! The bodies of `macro_rules!` rules are token patterns, not code: no
//! references (rust-analyzer records none there either).
//!
//! - `Call`: `f()` (not a local closure), `a::b::f()`, `T::new()`,
//!   `Self::f()`, `x.m()` (receiver below), `f::<T>()`, and invocations of
//!   macros that are not standard (`my_macro!()`, `log::info!()`; a
//!   standard macro is a call only when the file defines a `macro_rules!`
//!   with its name).
//! - `Read`: non-local names in value position (`CONST`, a function passed
//!   as a value, `a::B`), `F(…)` / `E::V(…)` (tuple-struct and variant
//!   constructors are uses of the type, not calls), upper-case names and
//!   paths in patterns (`E::A =>`, `None` excluded), `self.field`.
//! - `Type`: named types in signatures, `let` annotations, casts, generic
//!   arguments and bounds, `impl` headers, struct expressions and patterns
//!   (`T { … }` is a reference to `T`); `a::T` → `T` with receiver `a`.
//!   Primitives, `Self`, generic parameters and standard types are not
//!   references, except in field types (where the resolver reads the first
//!   one as the field's type; smart pointers are skipped there).
//! - `Write`: assignments to non-local names and to `self.field`.
//! - `Decorator`: an attribute macro of an item (`#[tokio::main]` → `main`
//!   with receiver `tokio`, `#[rstest]` → `rstest`), enclosed by the item;
//!   the resolver adds `DECORATES` from the macro's function (a
//!   `#[proc_macro_attribute] pub fn`) to the item. Built-in attributes
//!   (`#[derive]`, `#[cfg]`, `#[test]`, `#[inline]`, `#[doc]`, …), tool
//!   attributes (`#[rustfmt::skip]`) and, on an item with a `#[derive]`,
//!   single-segment helper attributes (`#[serde(…)]`) are not references
//!   (`decorators.rs`).
//! - `EnvVar`: `std::env::var("X")`, `env::var("X")` / `var("X")` with
//!   `std::env` / `std::env::var` imported (also `var_os`), `env!("X")`,
//!   `option_env!("X")` — literal names only (RFC 0003 §7.2).
//! - **Inside macro arguments** (`println!("{}", f())`, `assert_eq!(a.len(),
//!   3)`, `vec![make()]`, `write!(f, …)`, any macro of any crate): the
//!   argument list is parsed again as code and visited like the file's own,
//!   at the original positions (see `macros.rs`). A body that is items is
//!   not re-parsed as an expression: its items are visited as items.
//! - `Read` / `Write` / `Type` are deduplicated per `(enclosing, receiver,
//!   name)`; at most 20,000 references per file.
//!
//! # Paths
//!
//! A path qualifier is written as a receiver with `.` separators after
//! normalization at its site: `self::` and `super::` that stay inside the
//! file become the chain of inline modules (`tests.helper`, or no receiver
//! at file level); `crate::a::f()` keeps `crate`, `super::f()` beyond the
//! file becomes `super$1` (`super$2` two levels up), and a lower-case first
//! segment that the file neither declares nor imports (`serde_json::…`, a
//! crate of the workspace) stays as written. Each such anchor gets one
//! synthetic import (`type_only: true`, below) so the resolver binds it.
//!
//! # Receivers
//!
//! | `x` in `x.m()` is | Receiver |
//! |---|---|
//! | `self` (also `Self::f()`) | `$self` (listed in [`LanguagePack::self_receivers`]) |
//! | typed from the file (see `receivers.rs`: annotations, constructors `T { … }` / `T(…)` / `T::new()`, return types of the file's functions and methods, `?` / `unwrap()`, struct fields, `for` elements, `Some(x)` patterns, generic bounds and `impl Tr` / `dyn Tr`) | `$type:T` (path with `.`; smart pointers stripped) |
//! | a unit struct of the file used as a value (`Local.m()`) | `$type:Local` |
//! | `<T as Tr>::f()` | `$type:T`, or `$type:Tr` when `T` is generic or `Self` |
//! | `self.a.b` with `a` not typed in the file | `$member:a.b` |
//! | the result of a call the file cannot type (`let s = Store::open(p)?; s.m()`, `make().build()`) | `$ret:<receiver>#<name>`, `?` appended when unwrapped (`returns.rs`; typed by the resolver from the callee's signature) |
//! | an untyped local | `(x)` (never bound to an item named like it) |
//! | anything else | `(…)` (the unique-method rung) |
//!
//! # Imports
//!
//! | Source | `module` | `relative_level` | `names` | `module_alias` |
//! |---|---|---|---|---|
//! | `use crate::a::b::C;` | `crate::a::b` | 0 | `C` | |
//! | `use super::x as y;` (file level) | *(empty)* | 2 | `x as y` | |
//! | `use self::m::*;` | `m` | 1 | `*` | |
//! | `use serde::de::{self, Visitor};` | `serde` / `serde::de` | 0 | `de` / `Visitor` | |
//! | `use log;` / `use ::log::info;` | *(empty)* / `::log` | 0 | `log` / `info` | |
//! | `pub use …` | as above | | | `reexport: true` |
//! | `use …` inside `mod m { … }` | as above | | | `$in:m` (the module, for lookups of `m::name`) |
//! | `mod x;` (inside `mod m { … }`: `m::x`) | `x` | 1 | | `x` |
//! | `use` / `mod` inside a macro body that is items (`cfg_net! { pub mod tcp; pub use tcp::X; }`) | as above, at their place | | | |
//! | `#[path = "p.rs"] mod x;` | `"p.rs"` | 1 | | `x` |
//! | `#[cfg(test)] mod x;` (or `all(test, …)`; also inside such a module) | the `mod x;` import, then a marker: as above | 1 | | `$test`, `type_only: true` |
//! | `extern crate foo as bar;` | `::foo` | 0 | | `bar` |
//! | a path anchor used in code (`crate`, `super`, `serde_json`) | `crate` / *(empty)* / `serde_json` | 0 / 2 / 0 | | the anchor (`super$1` for `super`), `type_only: true` |
//!
//! `relative_level` ≥ 1 is relative to the file's module (1 = the module
//! itself, 2 = its parent, …); `self::` and `super::` written inside inline
//! modules are folded into it at extraction. [`LanguagePack::resolve_import`]
//! maps such a module path to the module **file** that holds it: `crate`
//! is the file's crate root, a first segment that is a module the file
//! declares or a crate of the snapshot (a dependency of the file's package,
//! renames and `path` dependencies followed; else any crate with that name,
//! same workspace first) continues from there, and each further segment is
//! `<dir>/<s>.rs` or `<dir>/<s>/mod.rs`; the walk stops at the first
//! segment that is not a file (an inline module, a type) and returns the
//! deepest module file. A `mod x;` inside inline modules (`mod net { mod
//! tcp; }`) is the file below their directories (`<dir>/net/tcp.rs`),
//! whether or not a file holds the inline modules. `std`, `core`, `alloc`
//! and third-party crates resolve to nothing (external).
//!
//! # Limitations
//!
//! - Types are file-local: return types of functions of other files and
//!   field types of structs declared elsewhere are not followed in the pack
//!   (the resolver follows `$member:` chains through declared field types).
//! - Macros are not expanded. Items a macro **passes through** are items
//!   (a body that parses as items, or `cfg_if!`'s branches); items a macro
//!   **generates** (`bitflags!`, derives, a `macro_rules!` that builds an
//!   `impl` from its arguments, bodies in a macro's own syntax such as
//!   `stable: { … }`) are not definitions (their arguments are still
//!   visited for references), and what a `macro_rules!` body calls is not
//!   a call of the macro's callers. A pass-through invocation written with
//!   no whitespace around its opening delimiter (`m!{fn f() {}}`) is not
//!   opened (`expand.rs`).
//! - A method called on a return-position `impl Trait` binds the trait's
//!   method, not the concrete type's.
//! - `#[cfg]` is ignored: every item is indexed; `#[path]` is honoured on
//!   file-level `mod` declarations only.
//! - Trait-method calls on receivers of unknown type resolve by name only
//!   (see `rueca-resolve`, *Rust*).

mod builtins;
mod contracts;
mod decorators;
mod expand;
mod extract;
mod locals;
mod macros;
pub mod modules;
mod receivers;
mod refs;
mod returns;
mod types;
mod uses;

use rueca_extract::{Budget, ExtractError, LanguagePack, ProjectFiles, SELF_RECEIVER};
use rueca_model::{FileFacts, Import, Language};

pub use extract::MAX_REFS;

/// Rust pack. Construct with `RustPack::default()`. Holds only a cache of
/// the crate tables (`Cargo.toml` files and the file layout) of the most
/// recent snapshots.
#[derive(Default)]
pub struct RustPack {
    crates: modules::CratesCache,
}

impl std::fmt::Debug for RustPack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RustPack").finish_non_exhaustive()
    }
}

impl LanguagePack for RustPack {
    fn languages(&self) -> &'static [Language] {
        &[Language::Rust]
    }
    fn extractor_version(&self) -> u32 {
        // 2: contract hints (HTTP routes and calls, tonic; RFC 0006 phase 4).
        // 3: items inside macro bodies, attribute macros as decorators.
        3
    }
    fn extract(
        &self,
        path: &str,
        language: Language,
        source: &[u8],
        budget: &Budget,
    ) -> Result<FileFacts, ExtractError> {
        if language != Language::Rust {
            return Err(ExtractError::Unsupported(language));
        }
        extract::extract(path, source, budget)
    }
    /// The module path of the file (crate docs, *Module names*).
    fn module_name(&self, path: &str, project: &ProjectFiles) -> String {
        self.crates.get(project).module_path(path)
    }
    /// The module file a module path names (crate docs, *Imports*).
    fn resolve_import(&self, from_path: &str, import: &Import, project: &ProjectFiles) -> Option<String> {
        self.crates.get(project).resolve(from_path, import)
    }
    fn is_builtin(&self, name: &str) -> bool {
        builtins::is_builtin(name)
    }
    fn self_receivers(&self) -> &'static [&'static str] {
        &[SELF_RECEIVER]
    }
    /// `is_test` depends on `tests/` and `benches/` directories.
    fn facts_depend_on_path(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests;
