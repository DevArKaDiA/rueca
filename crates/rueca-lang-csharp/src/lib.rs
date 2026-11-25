//! C# language pack for Rueca (`tree-sitter-c-sharp` 0.24.0, RFC 0003 §10.4d).
//!
//! See `rueca_extract::LanguagePack` for the contract. This crate turns one
//! `.cs` file into [`FileFacts`] and declares C#'s module semantics:
//! namespaces as modules shared by every file that declares them
//! ([`ModuleScope::Package`]), `using` directives that import namespaces
//! by name ([`LanguagePack::namespace_imports`]), implicit `this`,
//! overloads chosen by arity and argument types, and `OVERRIDES` edges.
//! The resolver adds what C# needs on top of the Java-like hooks (the
//! `csharp` module of `rueca-resolve`): enclosing namespaces, `global
//! using` per project, `using static`, namespace-qualified names, partial
//! types and extension methods.
//!
//! What is assembled and what is Rueca code (RFC 0002 §3.4, §4.1):
//!
//! - **Grammar**: `tree-sitter-c-sharp` 0.24.0 (MIT). It depends only on
//!   `tree-sitter-language ^0.1` (its `tree-sitter` requirement is a
//!   dev-dependency), so it links with the 0.27 runtime.
//! - **Local variables** come from a vendored, adapted `queries/locals.scm`
//!   (nvim-treesitter, Apache-2.0; provenance in `queries/README.md`,
//!   semantics in `locals.rs`). They keep parameters and locals out of the
//!   references and give receivers their declared types.
//! - Definitions, containment, qualified names, overload suffixes, usings,
//!   inheritance, receivers, tests and the configuration bridge are Rueca
//!   code (`extract.rs`, `refs.rs`, `bridge.rs`). Upstream `tags.scm` (MIT)
//!   is not vendored: it has no containment, qualified names or receivers.
//!
//! # Qualified names
//!
//! The module name is the **namespace** every type of the file shares
//! (block `namespace A.B { … }`, nested blocks, or file-scoped `namespace
//! A.B;`; the global namespace is empty), the separator is `.`, and the
//! local name is the type path inside it:
//!
//! | Definition | Local qualified name | Global |
//! |---|---|---|
//! | top-level type | `Store` | `Acme.Shop.Store` |
//! | nested type | `Store.Options` | `Acme.Shop.Store.Options` |
//! | method, property, field, event, enum member | `Store.Add`, `Store.Name`, `Color.Red` | `Acme.Shop.Store.Add` |
//! | constructor (named like its type), destructor | `Store.Store`, `Store.~Store` | |
//! | overloaded method or constructor | `Store.Add(Item)`, `Store.Add(Item,int)`, `Store.Add(object...)` | |
//! | explicit interface implementation | `Store.IStore.Explicit` | |
//! | operator, conversion | `Vec.operator+`, `Vec.implicit operator string` | |
//! | indexer | `Store.this[]` | |
//! | local function | `Store.Add.Local` (child of the method) | |
//!
//! A file whose types live in several namespaces (`namespace A.B {…}
//! namespace A.C {…}`) gets the longest common namespace as module and the
//! rest in the local names (`B.Store`, `C.Other`); such types are found
//! by their global names and through the file, not as members of `A.B`'s
//! namespace from other files (rare in practice).
//!
//! **Namespaces are not definitions**: as Java packages, they are module
//! names, visible in every global qualified name and in
//! `ResolvedGraph::modules`.
//!
//! **Overloads** (as Java, RFC 0003 §10.1): when a type declares several
//! callables with one name, *all* of them get a parameter suffix — types
//! with generic arguments, namespace qualifiers, nullable marks and
//! `ref` / `out` / `in` / `this` modifiers removed, array ranks kept, a
//! `params T[]` written `T...` — joined with `,` and no spaces. A callable
//! that is not overloaded stays bare. The resolver reads default values
//! (`int n = 0`) and `params` from the signature to choose by arity.
//!
//! # Definitions
//!
//! | Source | Label | Notes |
//! |---|---|---|
//! | `class`, `record` (class) | `Class` | `bases` = the first base not named like an interface (`I` + upper case); `implements` = the others |
//! | `struct`, `record struct` | `Struct` | `implements` = the base list |
//! | `interface` | `Interface` | `bases` = extended interfaces; members implicitly public |
//! | `enum` | `Enum` | members → `Constant` children |
//! | `delegate` | `TypeAlias` | a named function type |
//! | method (also interface, abstract, extension, explicit interface implementation), constructor, destructor, operator, conversion operator | `Method` | |
//! | local function | `Function` | child of the enclosing callable |
//! | field, property, event, indexer, record parameter | `Field` | each declarator of `int a, b;` |
//! | `const` field | `Constant` | |
//!
//! Not definitions: lambdas and anonymous methods (their code belongs to
//! the enclosing definition), namespaces (module names), top-level
//! statements (file level), accessors (their code belongs to the property).
//!
//! - `span` covers the declaration with its attributes; `name_span` is the
//!   identifier (for constructors, the type name; operators, the operator).
//! - `signature`: the header from the first modifier to the body,
//!   attributes and comments removed, whitespace collapsed, ≤ 200 chars:
//!   `public static IServiceCollection AddShop(this IServiceCollection services, string name = "x")`;
//!   fields `private readonly IRepo _repo`.
//! - `doc`: the `///` lines right before the declaration, XML tags
//!   removed (`<see cref="X"/>` → `X`), ≤ 1000 chars.
//! - `decorators`: attributes as written (`HttpGet("{id}")`).
//! - `exported`: not `private` / `file`: `public`, `internal` and
//!   `protected` members, top-level types (internal by default; visible to
//!   the assembly's other namespaces), interface members and enum members.
//! - `is_test`: methods with xUnit `[Fact]` / `[Theory]`, NUnit `[Test]` /
//!   `[TestCase]` / `[TestCaseSource]`, MSTest `[TestMethod]` /
//!   `[DataTestMethod]` (and `Skippable*`); fixture methods (`[SetUp]`,
//!   `[TearDown]`, `[OneTimeSetUp]`, `[TestInitialize]`, `[ClassInitialize]`,
//!   …) of test classes. Classes with `[TestClass]` / `[TestFixture]` or a
//!   test method are tests. Every definition of a file in a test project
//!   directory (`*.Tests`, `*.Test`, `*.UnitTests`, `*.IntegrationTests`,
//!   `*.FunctionalTests`, `*.Specs`, `*.Spec`) or under `test/` / `tests/`
//!   is test code (so [`LanguagePack::facts_depend_on_path`] is true).
//!
//! # References
//!
//! `enclosing` is the innermost definition (`None` at file level).
//!
//! - `Call` with `arity` (argument count): `M()` → no receiver (the resolver
//!   tries the enclosing callables' local functions, implicit `this`, then
//!   `using static`, the namespaces and usings); `this.M()` → `this`;
//!   `base.M()` → `base`; `Type.M()` / `A.B.Type.M()` / `Outer.Inner.M()`
//!   → the type path as written; `x.M()` / `x?.M()` where `x` is a
//!   parameter, local, `var x = new T()` / cast / `as T`, pattern, `out`,
//!   `foreach` or `catch` variable, primary-constructor parameter, or a
//!   field / property / event of an enclosing type (also `this.x.M()`)
//!   declared with class `T` → `$type:T` (`T` as written; a type parameter
//!   is never a receiver type); `new T().M()`, `((T) e).M()`, `(e as T).M()`
//!   → `$type:T`; `"s".M()` → `$type:string`; `Own().M()` where `Own` is a
//!   method of an enclosing type of this file declaring class `T` as its
//!   return type (every overload agreeing) → `$type:T`; anything else → the
//!   receiver text (≤ 200 chars, resolved by the unique-method rung only).
//!   Generic arguments of the called name are `Type` references. A call of
//!   a local variable (a delegate) is not a reference; `nameof(…)` is not a
//!   call. `: this(…)` / `: base(…)` and a primary constructor's `: Base(…)`
//!   → a call to the constructor of this type / of the base class
//!   (receiver `this` / `base`).
//! - **Argument types**: a `Call` or `New` with arguments carries their
//!   static types in its name when the file knows at least one
//!   (`Add$args:int,?`; `rueca_extract::with_arg_types`): literals
//!   (`string`, `char`, `int` / `long` / `uint` / `ulong`, `double` /
//!   `float` / `decimal`, `bool`, `Type` for `typeof`), locals,
//!   parameters, fields and properties with a declared type, `this`, `new
//!   T(…)`, casts, `as`, `out` declarations, string concatenations,
//!   comparisons, `nameof` and calls of this file's methods with a
//!   declared return type; `?` otherwise.
//! - `New` with `arity`: `new T(…)` → `T` (receiver = qualifier); `T x =
//!   new(…)` → `T`. Object initializers (`new T { A = 1 }`) write `A` on
//!   `$type:T`.
//! - `Type`: field, property, local, parameter, return, pattern, cast,
//!   `as` / `is`, `typeof`, `default`, constraint and base-list generic
//!   types; generic arguments; `catch` types. Keyword types, `var` and the
//!   type parameters in scope are skipped.
//! - `Throw`: `throw new X(…)` (statement or expression).
//! - `Decorator`: attributes of definitions, attributed to the decorated
//!   definition and named like the attribute class (`[HttpGet]` →
//!   `HttpGetAttribute`); their arguments are visited. Attributes of
//!   parameters and assembly attributes are `Type` references.
//! - `Read` / `Write` (deduplicated per `(enclosing, receiver, name)`):
//!   bare identifiers that name a field / property / event of an enclosing
//!   type declared in the file (unless a local shadows it); `this.x`,
//!   `Type.Member`, `x.f` / `x?.f` on a typed local or field; assignments
//!   to them.
//!
//! ## Code ↔ config bridge (RFC 0003 §7.2)
//!
//! Emitted only with file-local evidence, string literals only:
//!
//! - `Environment.GetEnvironmentVariable("X")` (also `System.Environment`)
//!   → `EnvVar` `X` (not when the file declares another `Environment`).
//! - On a receiver declared as `IConfiguration`, `IConfigurationRoot`,
//!   `IConfigurationManager` or `ConfigurationManager`, or a property named
//!   `Configuration` (`builder.Configuration`, `context.Configuration`):
//!   `config["A:B"]`, `GetValue<T>("A:B")`, `GetSection("A:B")`,
//!   `GetRequiredSection("A:B")` → `ConfigKey` `A:B`;
//!   `GetConnectionString("Db")` → `ConfigKey` `ConnectionStrings:Db`;
//!   receiver `dotnet` (the bridge matches `appsettings*.json` keys, `:`
//!   read as `.`).
//!
//! # Imports
//!
//! | Source | `module` | `names` |
//! |---|---|---|
//! | the file's namespace | `Acme.Shop` | *(none)* — always the first import; [`declared_namespace`] reads it |
//! | `using A.B;` | `A.B` | `*` |
//! | `using static A.B.Util;` | `A.B` | `Util.*`, plus one synthetic `Util.<name>` per bare call / read (≤ 512 names) |
//! | `using X = A.B.Type;` | `A.B` | `Type` as `X` |
//! | `using X = A;` (a single segment) | `A` | *(none)*, `module_alias` `X` |
//! | `global using …` | as above | `reexport` = true: applies to every file of its project (the nearest `.csproj` directory) |
//!
//! # Module model and import resolution
//!
//! [`LanguagePack::module_name_for`] returns the recorded namespace (empty
//! for the global namespace). Files of one namespace form one module for
//! lookups ([`ModuleScope::Package`]): same-namespace names resolve across
//! files and directories without usings. [`LanguagePack::resolve_import`]
//! returns nothing: with [`LanguagePack::namespace_imports`] the resolver
//! binds a `using` to the namespace whose module name it names, wherever
//! its files are, and links `IMPORTS` to the directory holding most of its
//! files. Base class library and NuGet namespaces are not in the snapshot:
//! their usings bind nothing and names through them are external.
//!
//! [`LanguagePack::is_builtin`]: C# keyword types, base class library
//! types, the `System` / `Microsoft` / `Windows` namespace heads, and
//! method names every object, collection, LINQ sequence, string or task
//! has (`builtins.rs`).
//!
//! # Deviations and limitations
//!
//! - Receiver types are declared in the file: return types of methods of
//!   other files (`a.B().C()`), members inherited from a type in another
//!   file, properties of other types (`a.B.C()`) and generic type
//!   arguments (`List<Foo>` elements) are not followed.
//! - Base classes and interfaces are told apart by the `I` + upper-case
//!   naming convention only.
//! - Extension methods are found by the resolver by name and receiver type
//!   name (best effort; generic `this T` receivers are not matched).
//! - Source generators, reflection, dependency injection and `dynamic` are
//!   invisible (RFC 0003 §10.4 *Heuristic*).
//! - `#if` / `#else` branches are all walked (both alternatives' definitions
//!   are recorded), except in a file where a directive splits a declaration
//!   (`void F() #if X => G() #endif ;`) and breaks the syntax: there only
//!   the first branch of each conditional is read, if that parses with
//!   fewer errors (`preproc.rs`; offsets and lines unchanged).

mod bridge;
mod builtins;
mod extract;
mod locals;
mod preproc;
mod refs;
mod types;

use rueca_extract::{Budget, ExtractError, LanguagePack, ModuleScope, ProjectFiles};
use rueca_model::{FileFacts, Import, Language};

pub use extract::{MAX_REFS, is_test_path};

/// C# pack. Construct with `CSharpPack::default()`; stateless.
#[derive(Debug, Default)]
pub struct CSharpPack;

/// Whether `name` is a C# keyword type, a base class library type or
/// method name, or a .NET namespace head (see `builtins.rs`).
pub(crate) fn is_builtin_name(name: &str) -> bool {
    builtins::BUILTINS.binary_search(&name).is_ok()
}

/// The namespace recorded by facts (the file's first import, without names
/// or alias), if any.
pub fn declared_namespace(facts: &FileFacts) -> Option<&str> {
    facts.imports.iter().find(|i| i.names.is_empty() && i.module_alias.is_none()).map(|i| i.module.as_str())
}

impl LanguagePack for CSharpPack {
    fn languages(&self) -> &'static [Language] {
        &[Language::CSharp]
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
        if language != Language::CSharp {
            return Err(ExtractError::Unsupported(language));
        }
        extract::extract(path, source, budget)
    }
    /// Unknown without the file's facts (C# namespaces are declared, not
    /// implied by paths): the global namespace.
    fn module_name(&self, _path: &str, _project: &ProjectFiles) -> String {
        String::new()
    }
    /// The namespace the file's types share (empty for the global namespace).
    fn module_name_for(&self, path: &str, facts: Option<&FileFacts>, project: &ProjectFiles) -> String {
        match facts {
            Some(f) => declared_namespace(f).unwrap_or("").to_string(),
            None => self.module_name(path, project),
        }
    }
    /// Usings name namespaces, not files: bound by the resolver
    /// ([`LanguagePack::namespace_imports`]).
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
    fn facts_depend_on_path(&self) -> bool {
        true
    }
    fn namespace_imports(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests;
