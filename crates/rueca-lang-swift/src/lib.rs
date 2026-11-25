//! Swift language pack for Rueca (`tree-sitter-swift` 0.7.4, RFC 0003).
//!
//! See `rueca_extract::LanguagePack` for the contract. This crate turns one
//! `.swift` file into [`FileFacts`] and implements Swift's module
//! semantics: a module is a SwiftPM target, every file of a target sees
//! the others without imports, and `import Foo` makes a whole module
//! visible. It opts into the resolver hooks Swift needs: package scope
//! ([`ModuleScope::Package`], one package per target), imports by module
//! name ([`LanguagePack::namespace_imports`]), implicit `self`, overloads
//! chosen by argument labels and arity, and `OVERRIDES` edges.
//!
//! What is assembled and what is Rueca code (RFC 0002 §3.4, §4.1):
//!
//! - **Grammar**: `tree-sitter-swift` 0.7.4 (MIT, alex-pinkus), the grammar
//!   nvim-treesitter uses; it ships its generated parser (ABI 15) and
//!   depends only on `tree-sitter-language ^0.1`.
//! - **Local bindings** come from a vendored, adapted `queries/locals.scm`
//!   (nvim-treesitter, Apache-2.0; provenance in `queries/README.md`,
//!   semantics in `locals.rs`).
//! - Definitions, containment, qualified names, selectors, imports,
//!   supertypes, receivers, tests and the environment bridge are Rueca code
//!   (`extract.rs`, `refs.rs`); module names come from the path and
//!   `Package.swift` (`project.rs`).
//!
//! # Qualified names
//!
//! The module name is the **target** (see `project.rs`), the separator is
//! `.`, and the local name is the path inside the file:
//!
//! | Definition | Label | Local qualified name |
//! |---|---|---|
//! | `class`, `actor` | `Class` | `Session`, `Session.Request` |
//! | `struct` | `Struct` | `Point` |
//! | `enum`; its `case`s are `Constant` children | `Enum` | `Color`, `Color.red` |
//! | `protocol`; requirements are its members | `Interface` | `Shape`, `Shape.draw(at:)` |
//! | method, `init`, `deinit`, `subscript`, operator | `Method` | `Point.draw(at:_:)`, `Point.init(x:y:)`, `Point.deinit`, `Point.subscript(_:)` |
//! | top-level or local function | `Function` | `parse(_:)`, `run().helper()` |
//! | stored / computed / `static` property of a type | `Field` | `Point.x` |
//! | top-level `let` / `var` | `Constant` / `Variable` | `defaultTimeout` |
//! | `typealias`, `associatedtype` | `TypeAlias` | `Shape.Unit` |
//! | member of `extension T` (any file) | as above, top-level | `T.m(x:)`, `T.Nested` |
//!
//! **Selectors**: every callable but `deinit` carries its argument labels
//! (`(at:_:)`, `()` without parameters) — Swift's own full name, and how it
//! overloads: `init(x:)` and `init(y:)` are two overloads, told apart by
//! the labels of a call. Lookups by qualified name treat the selector as
//! optional (`Point.init` finds every initializer).
//!
//! **Extensions** are not definitions: their members are top-level
//! definitions named `T.m`, so the resolver attaches them to type `T`
//! wherever `T` is declared (another file of the module, or another module
//! of the snapshot), like Go methods and Rust `impl` items. Conformances
//! written on an extension are `Type` references.
//!
//! - `span` covers the declaration with its attributes and modifiers;
//!   `name_span` is the identifier (`init`, `deinit`, `subscript` keyword
//!   for those).
//! - `signature`: the header up to the body, attributes and comments
//!   removed, whitespace collapsed, ≤ 200 chars.
//! - `doc`: the `///` lines or `/** … */` block right before the declaration.
//! - `decorators`: attributes without `@` (`MainActor`, `Test("works")`).
//! - `bases`: a class's supertypes (its superclass and protocols: the
//!   resolver tells them apart by what they resolve to), a protocol's
//!   inherited protocols; `implements`: a struct's or enum's protocols.
//! - `exported`: not `private` / `fileprivate`.
//! - `is_test`: classes inheriting `XCTestCase` (also Quick's `QuickSpec` /
//!   `AsyncSpec`), types annotated `@Suite`, types named `*Tests` that
//!   declare a `@Test` function, and every member of those; functions
//!   annotated `@Test` (Swift Testing). [`is_test_path`] adds test targets
//!   (`Tests/…`) for package publishing.
//!
//! # References
//!
//! `enclosing` is the innermost definition (`None` at file level). A
//! property's initializer and accessors (`get`, `set`, `willSet`,
//! `didSet`) belong to the property; closures belong to the function that
//! writes them; local `let` / `var` are not definitions.
//!
//! - `Call` with `arity` (arguments plus trailing closures) and the
//!   argument labels in the name (`draw$args:at,_`; `_` unlabeled, `?` a
//!   trailing closure, read by the resolver to choose among overloads):
//!   `f()` → no receiver (the resolver tries locals, implicit `self`,
//!   imports, then the module); a type called like a function (`Point(x:
//!   1)`, `Box<Int>(…)`) resolves to the type and its initializer, Swift
//!   having no `new`; `self.f()` and `Self.f()` → `$self`; `super.f()` →
//!   `super`; `Type.f()` / `A.B.f()` → the path as written; `x.f()` /
//!   `x?.f()` / `x!.f()` where `x` is a parameter, local, `if let` /
//!   `guard let` / `for` / `catch` / closure binding declared with type `T`
//!   or initialized with `T(…)`, `x as? T` or a literal → `$type:T`; a
//!   property of the enclosing type declared with a type in this file →
//!   `$type:T`, else `$member:x` (the resolver reads its declared type);
//!   `self.a.b.f()` → `$member:a.b`; `Foo.shared.f()` with `shared`
//!   declared in this file → its type; `f().g()` and a local initialized
//!   by a call → `$ret:<receiver>#f` (the resolver types it from `f`'s
//!   declared return type; an initializer returns its type);
//!   `T(…).g()`, `(x as T).g()`, `"s".g()` → `$type:T` / `$type:String`;
//!   `.f(…)` → `$type:T` when it initializes a property declared `: T`,
//!   else no reference; anything else → the receiver text (≤ 200 chars,
//!   resolved by the unique-method rung only). A call of a local closure
//!   is no reference. Subscripts (`a[i]`) are not calls.
//! - `Type`: parameter, return, property, supertype, cast, `is` and
//!   generic-argument types; generic parameters in scope are skipped.
//!
//! ## Code ↔ config bridge (RFC 0003 §7.2)
//!
//! Only with string literals: `ProcessInfo.processInfo.environment["X"]`
//! and `getenv("X")` → `EnvVar` `X`.
//!
//! # Imports
//!
//! | Source | `module` | `names` |
//! |---|---|---|
//! | `import Foundation` | `Foundation` | `*` |
//! | `@testable import ArgumentParser` | `ArgumentParser` | `*` |
//! | `import struct Foo.Bar` | `Foo` | `Bar` |
//! | `import Foo.Sub` (a submodule) | `Foo` | `*` |
//!
//! # Module model and import resolution
//!
//! [`LanguagePack::module_name`] is the target of the file (`project.rs`:
//! `Package.swift` custom paths, else `Sources/<Target>/…`, else the
//! top-level directory). [`LanguagePack::namespace_imports`]: an import
//! binds the package (target) of that name wherever its files are;
//! modules that are not in the snapshot (Foundation, UIKit, dependencies)
//! bind their names as external. [`LanguagePack::resolve_import`] answers
//! the target's directory, which tells the organization linker that the
//! repository resolves the import itself; other imports are reported as
//! `swift:<module>`.
//!
//! [`LanguagePack::is_builtin`]: standard-library functions and types,
//! common Foundation types, XCTest assertions and the members every
//! collection or string has (`builtins.rs`).
//!
//! # Deviations and limitations
//!
//! - Receiver types are declared, constructed or returned in the file (or
//!   read from the callee's declared return type); inferred types of
//!   closures' parameters, generic element types (`[Foo]` → `Array`) and
//!   key paths are not followed.
//! - Protocol conformances declared in an extension are not bases of the
//!   type (their members are still found when the type has the method).
//! - `#if` branches are all extracted; macros (`#expect`, `@Observable`)
//!   and property-wrapper synthesized members (`$x`, `_x`) are invisible;
//!   result builders' implicit calls are not modeled.
//! - `.member` implicit expressions other than a typed property's
//!   initializer, enum-case constructors and dynamic member lookup are not
//!   references.

mod builtins;
mod extract;
mod locals;
mod manifest;
mod project;
mod refs;
mod types;

use rueca_extract::{Budget, ExtractError, LanguagePack, ModuleScope, ProjectFiles};
use rueca_model::{FileFacts, Import, Language};

pub use extract::MAX_REFS;
pub use manifest::{Manifest, Target, manifest_targets, package_identity, parse_manifest};
pub use project::{conventional_module, is_test_path, module_dir, module_of_path};
pub use refs::{RET_RECEIVER_PREFIX, TRAILING_LABEL};

/// Swift pack. Stateless.
#[derive(Debug, Default)]
pub struct SwiftPack;

/// Whether `name` is a standard-library, Foundation or XCTest name (see
/// `builtins.rs`).
pub(crate) fn is_builtin_name(name: &str) -> bool {
    builtins::BUILTINS.binary_search(&name).is_ok()
}

impl LanguagePack for SwiftPack {
    fn languages(&self) -> &'static [Language] {
        &[Language::Swift]
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
        if language != Language::Swift {
            return Err(ExtractError::Unsupported(language));
        }
        extract::extract(source, budget)
    }
    /// The SwiftPM target of `path` (see the crate docs).
    fn module_name(&self, path: &str, project: &ProjectFiles) -> String {
        project::module_of_path(path, project)
    }
    /// The directory of the imported target, when the snapshot has it.
    fn resolve_import(&self, _from_path: &str, import: &Import, project: &ProjectFiles) -> Option<String> {
        project::module_dir(&import.module, project)
    }
    fn is_builtin(&self, name: &str) -> bool {
        is_builtin_name(name)
    }
    fn self_receivers(&self) -> &'static [&'static str] {
        &[rueca_extract::SELF_RECEIVER]
    }
    fn module_scope(&self, _language: Language) -> ModuleScope {
        ModuleScope::Package
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
    fn namespace_imports(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests;
