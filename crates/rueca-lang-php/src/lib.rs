//! PHP language pack for Rueca (`tree-sitter-php` 0.25, `LANGUAGE_PHP`:
//! PHP code plus the HTML around `<?php … ?>` tags).
//!
//! See `rueca_extract::LanguagePack` for the contract. This crate turns one
//! `.php` file into [`FileFacts`] and implements PHP name semantics:
//! compile-time name resolution against `namespace` and `use`, Composer
//! autoloading for import targets, and builtins.
//!
//! What is assembled and what is Rueca code (RFC 0002 §3.4, §4.1):
//!
//! - **Grammar**: `tree-sitter-php` (MIT), the `php` dialect, so templates
//!   mixing HTML and PHP parse (the HTML is `text`).
//! - **Variable scopes** come from a vendored, adapted `queries/locals.scm`
//!   (nvim-treesitter and bloop, Apache-2.0; provenance in
//!   `queries/README.md`, semantics in `locals.rs`). They drive receiver
//!   typing.
//! - **Autoload maps** are read from `composer.json` with `serde_json`
//!   (`composer.rs`).
//! - Definitions, containment, qualified names, name resolution, imports,
//!   inheritance and calls with receivers are Rueca code (`extract.rs`,
//!   `names.rs`). Upstream `tags.scm` (MIT) is not vendored: it has no
//!   containment, qualified names or receivers.
//!
//! # Qualified names
//!
//! PHP code refers to classes, functions and constants by **fully qualified
//! name**, never to files, and a file may declare several namespaces (or a
//! namespace that differs from its Composer path). So a definition's local
//! qualified name already carries the namespace it is declared in, and
//! [`LanguagePack::module_name`] is the empty string: the global qualified
//! name *is* the PHP name.
//!
//! | Definition | Qualified name |
//! |---|---|
//! | class / interface / trait / enum | `App\Models\User` |
//! | method | `App\Models\User::save` |
//! | property, promoted constructor parameter | `App\Models\User::$email` (`name` is `$email`) |
//! | class constant, enum case | `App\Models\User::TABLE`, `App\Enums\Status::Active` |
//! | function | `App\Support\format_money` |
//! | namespace `const` | `App\VERSION` |
//! | `define('APP_ENV', …)` | `APP_ENV` (global unless the literal contains `\`) |
//! | Pest test | describe chain joined by ` > ` (`users > creates a user`) |
//! | global namespace | no prefix: `User::save`, `helper` |
//!
//! Properties keep their `$` so a property and a method of the same name
//! (`$request` / `request()`, common in PHP) stay distinct members.
//!
//! # Definitions
//!
//! | Source | Label | Notes |
//! |---|---|---|
//! | `class`, `abstract` / `final` / `readonly class` | `Class` | `bases` = `extends` then used traits; `implements` = interfaces |
//! | `interface` | `Interface` | `bases` = extended interfaces |
//! | `trait` | `Trait` | `bases` = used traits |
//! | `enum` (pure or backed) | `Enum` | `implements` = interfaces, `bases` = used traits; cases → `Constant` children |
//! | method (static, abstract, interface, `__construct`) | `Method` | |
//! | property (each element of `public int $a, $b;`), promoted constructor parameter | `Field` | child of the class, not of the constructor |
//! | class `const`, enum `case` | `Constant` | |
//! | `function` (also conditional or nested ones, which PHP declares globally) | `Function` | `parent` = enclosing definition for containment |
//! | namespace `const`, `define('NAME', …)` with a literal name | `Constant` | |
//! | Pest `it` / `test` / `describe` with a string title and a closure | `Test` | `is_test`; references in the closure are attributed to it |
//!
//! Classes declared inside anonymous classes and closures are not
//! definitions; anonymous classes (`new class {}`) are not definitions
//! either, their code is attributed to the enclosing definition.
//!
//! - `bases` / `implements` hold **fully qualified names with a leading
//!   `\`** (`\Illuminate\Database\Eloquent\Model`), resolved against the
//!   file's namespace and imports, because a bare `Model` is meaningless
//!   without them. Traits are listed in `bases`, so the resolver emits
//!   `INHERITS` edges to them and finds trait members through them.
//! - `span` covers attributes and the whole declaration (for properties and
//!   constants, the whole declaration statement); `name_span` is the
//!   identifier (`$email` for properties).
//! - `signature`: the declaration from its first modifier or keyword to the
//!   body (`{`) or `;`, attributes and comments removed, whitespace
//!   collapsed, ≤ 200 chars: `public function show(int $id, ?Request $request = null): User|null`,
//!   `final class UserController extends Controller implements HasMiddleware`,
//!   `private readonly UserService $users`, `case Active = 'active'`.
//! - `doc`: the `/** … */` docblock right before the declaration (only
//!   whitespace in between) or right after its attributes, delimiters and
//!   leading `*` stripped, ≤ 1000 chars.
//! - `decorators`: attributes, one per attribute (`#[A, B(1)]` gives `A`
//!   and `B(1)`), without `#[` `]`.
//! - `exported`: namespace-level declarations (classes, functions,
//!   constants) are exported; members are exported when `public` (explicit
//!   or implicit) and enum cases always. Pest tests are not exported.
//! - `is_test`: classes named `*Test` or extending a class named
//!   `*TestCase`; in those classes, methods named `test*`, tagged `@test`
//!   in the docblock, or carrying `#[Test]`; Pest tests.
//!
//! # References
//!
//! `enclosing` is the innermost definition (`None` at file level). Names
//! are resolved to fully qualified names with PHP's compile-time rules
//! (`names.rs`): fully qualified `\A\B`, `namespace\A`, qualified `A\B`
//! (first segment through the `use` table), unqualified (`use` table, else
//! the current namespace; functions and constants fall back to the global
//! namespace at run time). The encoding of names and receivers is
//! documented in `names.rs` and summarized here:
//!
//! - `Call`: `foo()` → `foo` with receiver `App\` (namespace, global
//!   fallback) or `\` (global namespace); `\A\f()` / imported `use function`
//!   → receiver `\A\`; `$obj->m()` / `$obj?->m()` → `m` with the object as
//!   receiver: `$this`, `\App\Services\UserService->` when the variable or
//!   `$this->property` has a declared class (typed parameter or docblock
//!   `@param`, `catch`, `new` assignment, typed, promoted or `@var`
//!   property of the same class), else the source text (≤ 200 chars); `X::m()` → receiver `\App\X`; `self::m()`,
//!   `static::m()`, `parent::m()` → receivers `self`, `static`, `parent`.
//!   Builtins are reported; the resolver filters them.
//! - `New`: `new User` → `User` / `\App\Models\`; `new self` /
//!   `new static` / `new parent` → `__construct` with that receiver;
//!   `new class {…}` → `Type` references for its bases.
//! - `Throw`: `throw new X` → `X` (also a `New`).
//! - `Type`: classes in parameter, return, property, constant and closure
//!   types (`?A`, `A|B`, `A&B`, DNF), `catch (A | B $e)`, `instanceof A`,
//!   anonymous class bases, attributes of parameters and closures. Builtin
//!   types and `self` / `static` / `parent` are skipped.
//! - `Decorator`: attributes of definitions, attributed to the decorated
//!   definition (`#[Route('/x')]` → `Route` / `\Symfony\…\`); their
//!   arguments are visited.
//! - `Read`: `$this->prop` → `$prop` / `$this` (also the prefix of a call
//!   receiver, `$this->repo->save()`), `X::CONST`, `X::$prop`,
//!   `self::CASE`, `X::class` (a read of class `X`), and constants in
//!   expressions (`MAX_ITEMS`; builtin constants such as `PHP_EOL` and the
//!   magic constants are skipped). Deduplicated per
//!   `(enclosing, receiver, name)`.
//! - `Write`: assignments (also `+=`, `++`) to `$this->prop` and `X::$prop`.
//! - Variables (`$x`) are never references; `$this` only as a receiver.
//!   Inside anonymous classes `$this` / `self` / `static` become the
//!   receiver `class@anonymous` (unknown).
//! - `EnvVar` / `ConfigKey` (the code ↔ config bridge, RFC 0003 §7.2):
//!   `getenv('X')`, `$_ENV['X']`, `$_SERVER['X']` and Laravel `env('X')` →
//!   `EnvVar` named `X`; Laravel `config('app.name')` and
//!   `Config::get('app.name')` → `ConfigKey` `app.name`, receiver
//!   `laravel`. String literals only (module `bridge`).
//! - At most 20,000 references per file.
//!
//! # Laravel config files
//!
//! `config/<file>.php` returns an array; its string-keyed entries become
//! `Key` definitions by key path (`name`, `providers.cache.driver`; list
//! elements are not nodes) with signature `key: <value>` (secret-looking
//! names masked), so `config('app.name')` resolves to key `name` of
//! `config/app.php` (RFC 0003 §7.3). Bridge reads inside an entry
//! (`'name' => env('APP_NAME')`) are enclosed by that key, giving
//! `Key` → `READS` → `$env:APP_NAME`. PHP definitions have no module name,
//! so these keys' global qualified names are their key paths; their symbol
//! ids carry the file (`config/app.php#name`). This is the one
//! path-dependent rule of the pack (`facts_depend_on_path`).
//!
//! # Imports
//!
//! | Source | `module` | `names` | `module_alias` |
//! |---|---|---|---|
//! | `use App\Models\User;` | `App\Models\User` | | `User` |
//! | `use App\Models\User as U;` | `App\Models\User` | | `U` |
//! | `use App\{A, B as C};` | two imports: `App\A`, `App\B` | | `A`, `C` |
//! | `use function App\f;` / `use const App\C as D;` | `App` | `f` / `C` as `D` | |
//! | `require_once __DIR__ . '/../bootstrap.php';` | `__DIR__/../bootstrap.php` | | |
//! | `include 'helpers.php';` | `helpers.php` | | |
//!
//! `relative_level` is always 0, `type_only` and `reexport` false.
//! `require` / `include` (and `_once`) paths are recorded when they are
//! static: string literals, `__DIR__`, `dirname(__DIR__ [, n])`,
//! `dirname(__FILE__)` and `DIRECTORY_SEPARATOR` joined with `.`.
//!
//! # Import resolution
//!
//! [`LanguagePack::resolve_import`] maps a class import to its file through
//! Composer `autoload` / `autoload-dev` (`psr-4`, `psr-0`, `classmap`) of
//! every `composer.json` in the snapshot, falling back to the unique file
//! named after the class whose directories match the namespace; `require`
//! / `include` paths resolve relative to the file (`__DIR__`) or the
//! repository root. `use function` / `use const` imports resolve to nothing
//! (functions are not autoloaded by class name). See `composer.rs`.
//!
//! # Deviations and limitations
//!
//! - `module_name` is empty (see *Qualified names*); [`expected_namespace`]
//!   exposes what PSR-4 / PSR-0 expect for a path.
//! - Magic methods (`__call`, `__callStatic`, `__get`), Laravel facades and
//!   the service container, `$this->$name()` and other dynamic calls are not
//!   modelled; calls through them end up unresolved or as name-only guesses.
//! - Dynamic properties and properties declared only in a parent class in
//!   another file give no receiver type.
//! - Receiver typing is syntactic: a variable is typed only when every
//!   binding in its scope declares or constructs the same class; return
//!   types of called methods are not followed (`$a->b()->c()`).
//! - Docblock types are read only for `@param` of untyped parameters and
//!   `@var` of untyped properties, and only when they name one class.

mod bridge;
mod builtins;
mod composer;
mod extract;
mod locals;
mod names;
mod text;

use rueca_extract::{Budget, ExtractError, LanguagePack, ProjectFiles};
use rueca_model::{FileFacts, Import, Language};

pub use extract::MAX_REFS;

/// PHP pack. Construct with `PhpPack::default()`. Holds only a cache of the
/// Composer autoload maps of the most recent snapshots.
#[derive(Default)]
pub struct PhpPack {
    autoload: composer::AutoloadCache,
}

impl std::fmt::Debug for PhpPack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PhpPack").finish_non_exhaustive()
    }
}

/// Whether `name` is a PHP builtin function, class, interface or constant
/// (common set, see `builtins.rs`).
pub(crate) fn is_builtin_name(name: &str) -> bool {
    let name = name.trim_start_matches('\\');
    builtins::BUILTINS.binary_search(&name).is_ok()
        || (name.bytes().any(|b| b.is_ascii_uppercase())
            && builtins::BUILTINS.binary_search(&name.to_ascii_lowercase().as_str()).is_ok())
}

/// The namespace Composer's PSR-4 / PSR-0 rules expect for the file at
/// `path`, if a rule covers it. Qualified names use the namespace the file
/// declares; this is for diagnostics (namespace / path mismatches).
pub fn expected_namespace(path: &str, project: &ProjectFiles) -> Option<String> {
    composer::Autoload::build(project).expected_namespace(path)
}

impl LanguagePack for PhpPack {
    fn languages(&self) -> &'static [Language] {
        &[Language::Php]
    }
    fn extractor_version(&self) -> u32 {
        // 2: env var / config key reads and Laravel config keys (RFC 0003 §7);
        // 3: `(new Foo())->m()` and inline `/** @var Foo $x */` receivers.
        3
    }
    fn extract(
        &self,
        path: &str,
        language: Language,
        source: &[u8],
        budget: &Budget,
    ) -> Result<FileFacts, ExtractError> {
        if language != Language::Php {
            return Err(ExtractError::Unsupported(language));
        }
        let mut facts = extract::extract(source, budget)?;
        if bridge::is_config_file(path) {
            bridge::config_keys(source, budget, &mut facts)?;
        }
        Ok(facts)
    }
    /// Always empty: PHP definitions carry their namespace in their local
    /// qualified names (crate docs, *Qualified names*).
    fn module_name(&self, _path: &str, _project: &ProjectFiles) -> String {
        String::new()
    }
    fn resolve_import(&self, from_path: &str, import: &Import, project: &ProjectFiles) -> Option<String> {
        composer::resolve_import(from_path, import, project, &self.autoload)
    }
    fn is_builtin(&self, name: &str) -> bool {
        is_builtin_name(name)
    }
    fn self_receivers(&self) -> &'static [&'static str] {
        &["$this", "self", "static"]
    }
    /// Laravel `config/<file>.php` arrays become `Key`s (chosen by path).
    fn facts_depend_on_path(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests;
