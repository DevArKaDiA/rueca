//! Java language pack for Rueca (`tree-sitter-java` 0.23.5, RFC 0003 §10.4).
//!
//! See `rueca_extract::LanguagePack` for the contract. This crate turns one
//! `.java` file into [`FileFacts`] and implements Java's module semantics:
//! packages, single-type / on-demand / static imports, Maven and Gradle
//! source roots, and the `java.lang` builtins. It opts into every RFC 0003
//! resolver hook a Java program needs: package scope
//! ([`ModuleScope::Package`]), implicit `this`, overloads chosen by arity and
//! `OVERRIDES` edges.
//!
//! What is assembled and what is Rueca code (RFC 0002 §3.4, §4.1):
//!
//! - **Grammar**: `tree-sitter-java` 0.23.5 (MIT). It depends only on
//!   `tree-sitter-language ^0.1` (its `tree-sitter` requirement is a
//!   dev-dependency), so it links with the 0.27 runtime.
//! - **Local variables** come from a vendored, adapted `queries/locals.scm`
//!   (nvim-treesitter and bloop, Apache-2.0; provenance in
//!   `queries/README.md`, semantics in `locals.rs`). They keep parameters
//!   and locals out of the references and give receivers their declared
//!   types.
//! - Definitions, containment, qualified names, overload suffixes, imports,
//!   inheritance, receivers, tests and the Spring bridge are Rueca code
//!   (`extract.rs`, `project.rs`). Upstream `tags.scm` (MIT) is not
//!   vendored: it has no containment, qualified names or receivers.
//!
//! # Qualified names
//!
//! The module name is the **package** (from the `package` clause; the
//! default package is empty), the separator is `.`, and the local name is
//! the type path inside the file:
//!
//! | Definition | Local qualified name | Global |
//! |---|---|---|
//! | top-level type | `Owner` | `com.example.Owner` |
//! | member type (static nested or inner) | `Owner.Pet`, `Outer.Inner.Deeper` | `com.example.Owner.Pet` |
//! | method, field, enum constant | `Owner.getPets`, `Owner.pets`, `Color.RED` | `com.example.Owner.getPets` |
//! | constructor (named like its class) | `Owner.Owner` | `com.example.Owner.Owner` |
//! | overloaded method or constructor | `Parser.parse(String)`, `Parser.parse(String,int)`, `Owner.Owner()` | |
//! | local class | `Owner.method.Local` (child of the method) | |
//!
//! **Overloads** (RFC 0003 §10.1, open question 4 left as the RFC states
//! it): when a type declares several callables with one name, *all* of
//! them get a parameter suffix — types with generic arguments, annotations
//! and package qualifiers removed, array dimensions and varargs kept,
//! joined with `,` and no spaces (`copy(byte[],int)`, `of(Object...)`,
//! `put(Entry)` for `java.util.Map.Entry<K, V>`). A callable that is not
//! overloaded stays bare. Lookups by qualified name treat the suffix as
//! optional (`rueca-query`). A compact record constructor takes the
//! record components' types. Fields never get a suffix; a field and a
//! method may share a qualified name (`Color.code` and `Color.code()`).
//!
//! # Definitions
//!
//! | Source | Label | Notes |
//! |---|---|---|
//! | `class`, `record` | `Class` | `bases` = `extends`; `implements` = interfaces |
//! | `interface`, `@interface` | `Interface` | `bases` = extended interfaces; members implicitly public |
//! | `enum` | `Enum` | `implements` = interfaces; constants → `Constant` children |
//! | method, interface method (abstract, `default`, `static`, `private`), annotation element | `Method` | `Function` only for top-level methods of scripts |
//! | constructor, compact record constructor | `Method` named like the class | |
//! | field, record component | `Field` | each declarator of `int a, b;` |
//! | `static final` field (or interface field) with an `UPPER_CASE` name | `Constant` | |
//! | enum constant | `Constant` | exported; arguments call the enum's constructor |
//! | local class (in a method body) | its label | child of the method |
//!
//! Not definitions: **anonymous classes** (`new T() {…}`) and enum constant
//! bodies — their members' code is attributed to the enclosing definition
//! and `this` inside them is the unknown receiver `class@anonymous`;
//! lambdas; static and instance initializer blocks (their code belongs to
//! the class); `package` and `module` declarations.
//!
//! - `span` covers the declaration with its annotations; `name_span` is the
//!   identifier (for constructors, the class name).
//! - `signature`: the header from the first modifier keyword to the body,
//!   annotations and comments removed, whitespace collapsed, ≤ 200 chars:
//!   `public static <T> T fromJson(String json, Class<T> classOfT) throws JsonSyntaxException`;
//!   fields `private final List<Pet> pets`.
//! - `doc`: the `/** … */` Javadoc right before the declaration (only
//!   whitespace in between), delimiters and leading `*` removed, ≤ 1000
//!   chars.
//! - `decorators`: annotations without `@` (`GetMapping("/owners")`).
//! - `exported`: `public` or `protected`; members of interfaces and
//!   annotation types unless `private`; enum constants; record components
//!   when the record is exported. Package-private is not exported, so an
//!   on-demand import binds only public types (as Java does).
//! - `is_test`: methods annotated `@Test` (JUnit 4 / 5, TestNG),
//!   `@ParameterizedTest`, `@RepeatedTest`, `@TestFactory`,
//!   `@TestTemplate`; fixture methods (`@BeforeEach`, `@Before`,
//!   `@AfterAll`, …) of test classes; JUnit 3 `test*` / `setUp` /
//!   `tearDown` in a subclass of `*TestCase`; public methods of a class
//!   annotated `@Test` (TestNG). Classes named `*Test`, `*Tests`, `*IT`,
//!   `*TestCase`, `*ITCase`, annotated `@Nested` / `@Test`, or declaring a
//!   test method are tests.
//!
//! Fields are emitted after the other members of their type: the resolver
//! keeps one member per name, and a call `c.code()` must find the method
//! `code()`, not the field `code`. For the same reason a field that shares
//! its name with a method of its type is never reported as read or written.
//!
//! # References
//!
//! `enclosing` is the innermost definition (`None` at file level).
//!
//! - `Call` with `arity` (argument count): `m()` → no receiver (the resolver
//!   tries implicit `this`, then static imports, then the package);
//!   `this.m()` → `this`; `super.m()` → `super`; `Type.m()` / `a.b.Type.m()`
//!   / `Outer.Inner.m()` → the type path as written; `x.m()` where `x` is
//!   a parameter, local, `var x = new T()`, pattern variable, `catch`
//!   parameter, resource, or a field of an enclosing type (also
//!   `this.x.m()`) declared with class `T` → `$type:T`; `new T().m()`,
//!   `((T) e).m()` → `$type:T`; `"s".m()` → `$type:String`; `X.super.m()`
//!   → `$type:X`; anything else → the receiver text (≤ 200 chars, resolved
//!   by the unique-method rung only). `this(…)` / `super(…)` → a call to
//!   the constructor of this class / of the superclass (receiver `this` /
//!   `super`). Method references: `T::m`, `x::m`, `this::m` → `Call`
//!   without arity; `T::new` → `New`.
//! - `New` with `arity`: `new T(…)` → `T` (receiver = qualifier); an
//!   anonymous class body makes it a `Type` reference to its base.
//! - `Type`: field, local, parameter, return and pattern types, generic
//!   arguments, bounds, casts, `instanceof`, `catch`, class literals, array
//!   creation, annotations of parameters and types, generic arguments of
//!   `extends` / `implements`. Primitives, `var` and the type parameters in
//!   scope are skipped.
//! - `Throw`: `throw new X(…)` and every type of a `throws` clause.
//! - `Decorator`: annotations of definitions, attributed to the decorated
//!   definition; their arguments are visited.
//! - `Read` / `Write` (deduplicated per `(enclosing, receiver, name)`):
//!   bare identifiers that name a field of an enclosing type declared in
//!   the file (unless a local shadows it) or an `UPPER_CASE` constant;
//!   `this.x`, `Type.FIELD`, `x.f` on a typed local; assignments to them.
//!   Other bare identifiers (locals, inherited fields, package names) are
//!   not reported.
//!
//! **Receivers that would reach a constructor.** Constructors are members
//! named like their class (wave R), and the resolver tries implicit `this`
//! before imports, so a bare type name inside a class that *is* that class,
//! an outer class, or its superclass would resolve to that class's
//! constructor. The pack avoids it: types declared in the file are written
//! fully qualified in `$type:` receivers (`$type:com.x.Outer.Node`), the
//! direct superclass becomes `super`, and type references to the
//! enclosing class itself or to an enclosing class's superclass are not
//! reported (`DEFINES` / `INHERITS` already link them); a member type
//! naming itself is qualified by its outer type. `new` keeps the bare name:
//! reaching the constructor is right there.
//!
//! ## Code ↔ config bridge (RFC 0003 §7.2)
//!
//! Emitted only with file-local evidence, string literals only; Spring
//! relaxed binding stays off (RFC 0003 open question 8):
//!
//! - `System.getenv("X")` → `EnvVar` `X` (not when the file declares or
//!   imports another `System`).
//! - `@Value("${a.b}")` / `@Value("${a.b:default}")` (every placeholder in
//!   the literal, `value = …` too) with `Value` imported from
//!   `org.springframework.beans.factory.annotation` → `ConfigKey` `a.b`,
//!   receiver `spring`, attributed to the annotated field, method or
//!   (for a parameter) the constructor / method.
//! - `env.getProperty("a.b")`, `getRequiredProperty`, `containsProperty`
//!   on a receiver declared as `Environment`, `ConfigurableEnvironment`,
//!   `PropertyResolver` or `StandardEnvironment` from
//!   `org.springframework.core.env` → `ConfigKey` `a.b` (`spring`).
//!
//! # Imports
//!
//! | Source | `module` | `names` |
//! |---|---|---|
//! | `package com.acme;` | `com.acme` | *(none)* — the package clause, always the first import; [`declared_package`] reads it |
//! | `import com.acme.Owner;` | `com.acme` | `Owner` |
//! | `import com.acme.Owner.Pet;` | `com.acme` | `Owner.Pet` (binds `Pet`) |
//! | `import com.acme.*;` | `com.acme` | `*` |
//! | `import static com.acme.Util.max;` | `com.acme` | `Util.max` (binds member `max`) |
//! | `import static com.acme.Util.*;` | `com.acme` | `Util.*`, plus one synthetic `Util.<name>` per bare call / `UPPER_CASE` read whose name the file neither declares nor imports by a single static import |
//! | a fully qualified type, `com.acme.Owner` in code | `com.acme` | `Owner` (synthetic), and the reference is written `Owner` |
//!
//! Packages and classes are told apart by Java's naming convention (package
//! segments start lower-case, classes upper-case). Synthetic imports make
//! fully qualified names and static on-demand imports resolve like their
//! explicit forms; members of enclosing and inherited classes still win,
//! because implicit `this` is tried before imports.
//!
//! # Module model and import resolution
//!
//! [`LanguagePack::module_name_for`] returns the declared package (empty for
//! the default package); for a file without facts, the path under its
//! source root. Files of one package form one module for lookups
//! ([`ModuleScope::Package`]): same-package names resolve across files
//! (and across `src/main/java` and `src/test/java`) without imports.
//!
//! [`LanguagePack::resolve_import`] (see `project.rs`): a class import
//! resolves to `C.java` in the package directory under a source root
//! (Maven / Gradle conventions `…/src/<set>/java` at any depth, so every
//! module of a multi-module build; `<sourceDirectory>` /
//! `<testSourceDirectory>` of every `pom.xml`; literal `srcDir(s)` of every
//! `build.gradle(.kts)`), else under any directory whose path ends with the
//! package path (non-standard layouts); an on-demand import resolves to
//! the package directory (`IMPORTS` File → Folder, binding the package).
//! Ties prefer the importer's source root, then its Maven / Gradle module
//! (`<modules>`, `settings.gradle` `include`s), then source roots, then
//! main over test, then proximity and path order. JDK (`java.*`, `javax.*`,
//! `jakarta.*`) and dependency classes are not in the snapshot: their
//! imports resolve to nothing and bind their names as external.
//!
//! [`LanguagePack::is_builtin`]: `java.lang` types, common JDK types and the
//! JDK package heads (`java`, `javax`, `jakarta`, `jdk`, `sun`), and method
//! names every object or JDK collection / stream / string has (`toString`,
//! `equals`, `stream`, `isEmpty`, …) so that untyped receivers never
//! fuzzy-match them to a project method (`builtins.rs`).
//!
//! # Measured
//!
//! `tests/real_repo.rs` on pinned shallow clones (debug build for the
//! counts, `rueca index` release build for the time, cold cache):
//!
//! | Repository (commit) | Files | Defs | Refs | Parse errors | Resolved | Builtin | External | Unresolved | `rueca index` |
//! |---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
//! | google/gson (`57731049`) | 264 | 5,162 | 40,270 | 0 | 45.9% | 11.3% | 30.3% | 12.5% | 0.42 s |
//! | FasterXML/jackson-core (`3b0a5f50`) | 453 | 8,423 | 64,830 | 0 | 66.6% | 11.7% | 18.6% | 3.1% | 0.63 s |
//!
//! Resolved by strategy: gson `type_resolved` 29.7%, `import_resolved`
//! 9.5%, `same_module` 5.7%, `heuristic` 0.9%, `unique_name` 0.1%;
//! jackson-core 47.6%, 16.7%, 1.4%, 0.9%, 0.0%. Most unresolved
//! references are calls on the result of another call (`assertThat(x)
//! .isEqualTo(y)`, builder chains), whose receiver type is unknown.
//!
//! # Deviations and limitations
//!
//! - Receiver types are declared types only: return types of calls
//!   (`a.b().c()`), fields inherited from a class in another file, and
//!   generic type arguments (`List<Foo>` element types) are not followed.
//! - Anonymous classes are not modelled: calls to their own methods or to
//!   members inherited by them stay unresolved or resolve against the
//!   enclosing class.
//! - An override that is not overloaded in its own class (no suffix, so no
//!   arity) gets `OVERRIDES` to every same-named overload of the base.
//! - Nested types imported on demand (`import a.b.Outer.*`) bind nothing;
//!   the import still links the file.
//! - Dependency injection, reflection, Lombok-generated members and other
//!   code generation are invisible (RFC 0003 §10.4 *Heuristic*).
//! - `module-info.java` is parsed but produces no definitions.

mod builtins;
mod extract;
mod locals;
mod project;
mod types;

use rueca_extract::{Budget, ExtractError, LanguagePack, ModuleScope, ProjectFiles};
use rueca_model::{FileFacts, Import, Language};

pub use extract::MAX_REFS;

/// Java pack. Construct with `JavaPack::default()`. Holds only a cache of
/// the project layouts (source roots, modules) of the most recent snapshots.
#[derive(Default)]
pub struct JavaPack {
    layouts: project::LayoutCache,
}

impl std::fmt::Debug for JavaPack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JavaPack").finish_non_exhaustive()
    }
}

/// Whether `name` is a `java.lang` type, a common JDK type or method name,
/// or a JDK package head (see `builtins.rs`).
pub(crate) fn is_builtin_name(name: &str) -> bool {
    builtins::BUILTINS.binary_search(&name).is_ok()
}

/// The package declared by facts (the package clause is recorded as an
/// import without names), if any.
pub fn declared_package(facts: &FileFacts) -> Option<&str> {
    facts.imports.iter().find(|i| i.names.is_empty() && i.module_alias.is_none()).map(|i| i.module.as_str())
}

/// Source roots the pack detected in a snapshot (Maven / Gradle conventions
/// and declared roots), for diagnostics.
pub fn source_roots(project: &ProjectFiles) -> Vec<String> {
    project::Layout::build(project).roots.into_iter().collect()
}

impl LanguagePack for JavaPack {
    fn languages(&self) -> &'static [Language] {
        &[Language::Java]
    }
    fn extractor_version(&self) -> u32 {
        2
    }
    fn extract(
        &self,
        _path: &str,
        language: Language,
        source: &[u8],
        budget: &Budget,
    ) -> Result<FileFacts, ExtractError> {
        if language != Language::Java {
            return Err(ExtractError::Unsupported(language));
        }
        extract::extract(source, budget)
    }
    /// The package implied by the path under a source root (used only when
    /// the file has no facts; see [`LanguagePack::module_name_for`]).
    fn module_name(&self, path: &str, project: &ProjectFiles) -> String {
        self.layouts.get(project).package_of_path(path)
    }
    /// The declared `package` (empty for the default package); the path
    /// under a source root when the file was not extracted.
    fn module_name_for(&self, path: &str, facts: Option<&FileFacts>, project: &ProjectFiles) -> String {
        match facts {
            Some(f) => declared_package(f).unwrap_or("").to_string(),
            None => self.module_name(path, project),
        }
    }
    fn resolve_import(&self, from_path: &str, import: &Import, project: &ProjectFiles) -> Option<String> {
        self.layouts.get(project).resolve_import(from_path, import)
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
}

#[cfg(test)]
mod tests;
