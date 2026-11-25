//! Kotlin language pack for Rueca (`tree-sitter-kotlin-sg` 0.4.1, RFC 0003).
//!
//! See `rueca_extract::LanguagePack` for the contract. This crate turns one
//! `.kt` / `.kts` file into [`FileFacts`] and implements Kotlin's module
//! semantics: packages, imports by package name, default imports as
//! builtins. Kotlin joins the **JVM family** of the model
//! ([`rueca_model::LanguageFamily::Java`]): Kotlin and Java files declaring
//! one package form one package for the resolver, so a Kotlin file uses a
//! Java class of its package (or an imported one) and Java code reaches
//! Kotlin classes the same way. It opts into the resolver hooks a JVM
//! language needs: package scope ([`ModuleScope::Package`]), imports by
//! namespace, implicit `this`, overloads chosen by arity and argument
//! types, and `OVERRIDES` edges.
//!
//! What is assembled and what is Rueca code (RFC 0002 §3.4, §4.1):
//!
//! - **Grammar**: `tree-sitter-kotlin-sg` 0.4.1 (MIT), ast-grep's crates.io
//!   release of fwcd/tree-sitter-kotlin, the grammar nvim-treesitter uses.
//!   It depends only on `tree-sitter-language ^0.1`, so it links with the
//!   0.27 runtime (fwcd's own crate pins `tree-sitter < 0.23`). Chosen over
//!   `tree-sitter-kotlin-ng` 1.1.0 by parse-error rate on real code (files
//!   with errors: moshi 0 vs 2 of 99, okhttp 0 vs 8 of 620, Exposed 17 vs 63
//!   of 1,052, clikt 0 vs 1 of 146) and because nvim-treesitter's queries
//!   are written for it.
//! - **Local bindings** come from a vendored, adapted `queries/locals.scm`
//!   (nvim-treesitter, Apache-2.0; provenance in `queries/README.md`,
//!   semantics in `locals.rs`).
//! - Definitions, containment, qualified names, overload suffixes, imports,
//!   supertypes, receivers, tests and the Spring bridge are Rueca code
//!   (`extract.rs`, `refs.rs`).
//!
//! # Qualified names
//!
//! The module name is the **package** (from the `package` header; empty
//! without one), the separator is `.`, and the local name is the path
//! inside the file, as in the Java pack:
//!
//! | Definition | Label | Local qualified name |
//! |---|---|---|
//! | `class`, `data` / `sealed` / `value` / `inner` / `abstract` class, `object` | `Class` | `Order`, `Order.Line` |
//! | `interface`, `fun interface`, `annotation class` | `Interface` | `Shape` |
//! | `enum class` | `Enum`; entries are `Constant` children | `Color`, `Color.RED` |
//! | `companion object` (named or not) | `Class` child of its class | `Order.Companion`, `Order.Factory` |
//! | member function (also an extension declared in a class) | `Method` | `Order.total` |
//! | top-level or local function, top-level extension function | `Function` | `shout`, `main.local` |
//! | primary / secondary constructor | `Method` named like the class | `Order.Order` |
//! | class property, `val` / `var` constructor parameter | `Field` (`Constant` when `const` or an `UPPER_CASE` `val`) | `Order.total` |
//! | top-level property (also extension properties) | `Variable` / `Constant` | `DEFAULT_TIMEOUT` |
//! | `typealias` | `TypeAlias` | `Handler` |
//! | anonymous object `object : T { … }` | `Class` named `T$N`, child of the enclosing definition | `main.Runnable$1` |
//!
//! **Overloads**: when a class declares several callables with one name,
//! all of them get a parameter suffix of their declared types, generic
//! arguments, nullability and package qualifiers removed (`Order(Long)`,
//! `Order(Long,Int)`, `of(String...)` for `vararg`). Lookups by qualified
//! name treat the suffix as optional. Top-level functions keep bare names
//! (the resolver links every same-named candidate of a package).
//!
//! - `span` covers the declaration with its annotations and modifiers;
//!   `name_span` is the identifier (a primary constructor's is the class
//!   name, a secondary constructor's the declaration).
//! - `signature`: the header up to the body, annotations and comments
//!   removed, whitespace collapsed, ≤ 200 chars.
//! - `doc`: the `/** … */` KDoc right before the declaration.
//! - `decorators`: annotations without `@`.
//! - `exported`: not `private` (Kotlin's default visibility is public;
//!   `internal` is visible to the whole module).
//! - `is_test`: functions annotated `@Test` (JUnit 4 / 5, kotlin.test,
//!   TestNG), `@ParameterizedTest`, `@RepeatedTest`, `@TestFactory`,
//!   `@TestTemplate`; fixture functions (`@BeforeEach`, `@BeforeTest`, …)
//!   of test classes; local functions of tests. Classes named `*Test`,
//!   `*Tests`, `*Spec`, `*IT`, `*TestCase`, annotated `@Nested`,
//!   `@RunWith` or `@ExtendWith`, extending a Kotest spec (`StringSpec`,
//!   `FunSpec`, `DescribeSpec`, `BehaviorSpec`, …) or declaring a test
//!   function are tests. [`is_test_path`] adds the source sets (`src/test`,
//!   `src/jvmTest`, `src/androidTest`, …) for package publishing.
//!
//! # References
//!
//! `enclosing` is the innermost definition (`None` at file level). Code of
//! `init` blocks and of the supertype list belongs to the primary
//! constructor (else the class); a property's initializer, getter, setter
//! and delegate belong to the property; local `val` / `var` are not
//! definitions.
//!
//! - `Call` with `arity` (value arguments plus a trailing lambda): `f()` →
//!   no receiver (the resolver tries implicit `this`, then imports, then
//!   the package); a class called like a function (`Order(1)`) resolves to
//!   the class and its constructor, Kotlin having no `new`; `this.f()` →
//!   `this`; `super.f()` → `super`; `super<T>.f()` / `this@Outer.f()` →
//!   `$type:T`; `Type.f()` / `a.b.Type.f()` → the path as written (a
//!   companion object's members are found through its class); `x.f()` /
//!   `x?.f()` / `x!!.f()` where `x` is a parameter, local, `catch` /
//!   lambda / `for` variable or property of an enclosing class declared
//!   with class `T` or initialized with `T(…)` → `$type:T`; a local
//!   initialized by a call of one of the file's functions with a known
//!   return class → that class; a property of an enclosing class whose
//!   class the file does not show → `$member:x` (the resolver reads its
//!   declared type); `Foo(…).f()`, `(e as T).f()`, `"s".f()`, `own().f()`
//!   → `$type:Foo` / `$type:T` / `$type:String` / the declared return
//!   class; anything else → the receiver text (≤ 200 chars, resolved by
//!   the unique-method rung only). Callable references `::f`, `T::f`,
//!   `x::f` → `Call` without arity. A call of a local holding a function
//!   value is not a reference.
//! - **Argument types** (`rueca_extract::with_arg_types`): literals
//!   (`String`, `Int`, `Long`, `Double`, `Float`, `Boolean`, `Char`),
//!   typed locals and constructions; `?` otherwise.
//! - Supertypes: a supertype called like a constructor (`Base(…)`) is the
//!   superclass (`bases`) and a `Call` of its constructor; the others are
//!   interfaces (`implements`; `bases` of an interface). `: this(…)` /
//!   `: super(…)` in a secondary constructor call the class's / the
//!   superclass's constructor. Enum entries with arguments call the
//!   enum's constructor.
//! - `Type`: parameter, return, property, receiver, supertype, cast,
//!   `is`, `catch` and generic-argument types and bounds; type parameters
//!   in scope are skipped.
//! - `Decorator`: annotations of definitions; annotations of parameters
//!   are `Type` references.
//! - `Read` / `Write` (deduplicated per `(enclosing, receiver, name)`):
//!   bare identifiers naming a property of an enclosing class or of the
//!   file (unless a local shadows it) or an `UPPER_CASE` constant;
//!   `this.x`, `Type.X`, `x.f` on a typed value; assignments to them.
//!
//! Scope functions (`apply`, `run`, `with`, `let`, `also`) are builtins:
//! calls inside their lambdas on the implicit receiver are resolved like
//! any receiver-less call (implicit `this`, imports, package), and `it` is
//! an untyped receiver. Extension functions are found by the resolver when
//! the receiver's class has no member of that name (see `rueca-resolve`).
//!
//! ## Code ↔ config bridge (RFC 0003 §7.2)
//!
//! As in the Java pack, only with file-local evidence and string literals:
//!
//! - `System.getenv("X")` → `EnvVar` `X` (not when the file declares or
//!   imports another `System`).
//! - `@Value("\${a.b}")` / `@Value("\${a.b:default}")` with `Value`
//!   imported from `org.springframework.beans.factory.annotation` →
//!   `ConfigKey` `a.b`, receiver `spring`, attributed to the annotated
//!   property or (for a parameter) the constructor / function.
//! - `env.getProperty("a.b")`, `getRequiredProperty`, `containsProperty`
//!   on a value declared as a Spring `Environment`, `ConfigurableEnvironment`,
//!   `PropertyResolver` or `StandardEnvironment` → `ConfigKey` `a.b`
//!   (`spring`).
//!
//! # Imports
//!
//! | Source | `module` | `names` |
//! |---|---|---|
//! | `package com.acme` | `com.acme` | *(none)* — always the first import; [`declared_package`] reads it |
//! | `import com.acme.Owner` | `com.acme` | `Owner` |
//! | `import com.acme.Owner.Pet` | `com.acme` | `Owner.Pet` (binds `Pet`) |
//! | `import com.acme.util.max` | `com.acme.util` | `max` |
//! | `import com.acme.*` | `com.acme` | `*` |
//! | `import com.acme.Owner as O` | `com.acme` | `Owner` as `O` |
//!
//! Packages and declarations are told apart by Kotlin's naming convention
//! (package segments lower-case, classes upper-case; a path without an
//! upper-case segment imports its last segment).
//!
//! # Module model and import resolution
//!
//! [`LanguagePack::module_name_for`] returns the declared package; for a
//! file without facts, the path under a `src/<set>/kotlin` or
//! `src/<set>/java` root. [`LanguagePack::namespace_imports`]: an import
//! binds the package of the JVM family whose name is its `module`
//! wherever its files are (Kotlin does not tie packages to directories),
//! Java files included. Classes of the JDK, the Kotlin standard library and
//! dependencies are not in the snapshot: their imports bind their names as
//! external. [`LanguagePack::resolve_import`] answers nothing (imports are
//! bound by package name); the organization linker then reports every
//! import of a package the repository does not declare as `java:<package>`.
//!
//! [`LanguagePack::is_builtin`]: the default imports (`kotlin.*`,
//! `kotlin.collections.*`, …, `java.lang.*`), common JDK types, package
//! heads, standard-library functions and the members every object,
//! collection or string has (`builtins.rs`).
//!
//! # Deviations and limitations
//!
//! - Receiver types are declared or constructed in the file; inferred
//!   types of other files' functions, generic element types (`List<Foo>`)
//!   and smart casts are not followed.
//! - Implicit receivers of lambdas with receiver (`apply { … }`, builders,
//!   DSLs) are not modeled.
//! - Gradle Kotlin scripts (`.kts`) are parsed like any file: their calls
//!   are references of the file; `build.gradle.kts` is also read as a
//!   manifest (RFC 0006).
//! - Annotation processing, compiler plugins (kotlinx.serialization,
//!   all-open, kapt / KSP output) and reflection are invisible.

mod builtins;
mod extract;
mod locals;
mod project;
mod refs;
mod types;

use rueca_extract::{Budget, ExtractError, LanguagePack, ModuleScope, ProjectFiles};
use rueca_model::{FileFacts, Import, Language};

pub use extract::MAX_REFS;
pub use project::is_test_path;

/// Kotlin pack. Stateless.
#[derive(Debug, Default)]
pub struct KotlinPack;

/// Whether `name` is a default-imported, standard-library or common JDK
/// name (see `builtins.rs`).
pub(crate) fn is_builtin_name(name: &str) -> bool {
    builtins::BUILTINS.binary_search(&name).is_ok()
}

/// The package declared by facts (the package header is recorded as an
/// import without names), if any.
pub fn declared_package(facts: &FileFacts) -> Option<&str> {
    facts.imports.iter().find(|i| i.names.is_empty() && i.module_alias.is_none()).map(|i| i.module.as_str())
}

impl LanguagePack for KotlinPack {
    fn languages(&self) -> &'static [Language] {
        &[Language::Kotlin]
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
        if language != Language::Kotlin {
            return Err(ExtractError::Unsupported(language));
        }
        extract::extract(source, budget)
    }
    /// The package implied by the path under a source root (used only when
    /// the file has no facts).
    fn module_name(&self, path: &str, _project: &ProjectFiles) -> String {
        project::package_of_path(path)
    }
    /// The declared `package` (empty without one); the path under a source
    /// root when the file was not extracted.
    fn module_name_for(&self, path: &str, facts: Option<&FileFacts>, project: &ProjectFiles) -> String {
        match facts {
            Some(f) => declared_package(f).unwrap_or("").to_string(),
            None => self.module_name(path, project),
        }
    }
    /// Imports name packages, bound by the resolver (see the crate docs).
    fn resolve_import(&self, _from_path: &str, _import: &Import, _project: &ProjectFiles) -> Option<String> {
        None
    }
    fn is_builtin(&self, name: &str) -> bool {
        is_builtin_name(name)
    }
    fn self_receivers(&self) -> &'static [&'static str] {
        &["this"]
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
