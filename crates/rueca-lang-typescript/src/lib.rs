//! TypeScript / TSX / JavaScript language pack for Rueca.
//!
//! Grammars: `tree-sitter-typescript` 0.23 (`LANGUAGE_TYPESCRIPT` for
//! `.ts/.mts/.cts`, `LANGUAGE_TSX` for `.tsx`) and `tree-sitter-javascript`
//! 0.25 (`.js/.jsx/.mjs/.cjs`, JSX included). See
//! `rueca_extract::LanguagePack` for the contract.
//!
//! What is assembled and what is Rueca code (RFC 0002 §3.4, §4.1, §9):
//!
//! - **Local bindings** come from vendored, adapted `locals.scm` queries
//!   (nvim-treesitter, Apache-2.0, plus one tree-sitter-javascript pattern,
//!   MIT); provenance in `queries/README.md`, semantics in `locals.rs`.
//! - **Import resolution** is `oxc_resolver` over a virtual file system built
//!   from `ProjectFiles` (no disk access); see `resolve.rs`.
//! - Definitions, containment and qualified names, imports, inheritance and
//!   calls with receivers are Rueca code (`extract.rs`).
//!
//! # Definitions
//!
//! Qualified names are module-relative (`UserService.findById`); the
//! resolver prefixes the module (`src/user/service:UserService.findById`).
//!
//! | Source | Label | Notes |
//! |---|---|---|
//! | `function f` / `function* g` (also `declare function`) | `Function` | TS overload signatures followed by their implementation yield one definition |
//! | `const f = () => …` / `= function …` / `= forwardRef(…)` / `memo` / `observer` | `Function` | |
//! | `class` / `abstract class` / `const A = class …` | `Class` | `bases` = `extends` expression text, `implements` = implemented type names (type arguments dropped) |
//! | method, getter / setter (a get/set pair is one), constructor, abstract / signature member | `Method` | |
//! | class field | `Field`; `Method` when initialised with a function (`onClick = () => …`) | TS parameter properties (`constructor(private repo: Repo)`) are `Field`s too |
//! | `interface` | `Interface` | `bases` = `extends` types; property signatures → `Field`, method signatures → `Method` |
//! | `type X = …` | `TypeAlias` | object-type members become `Field` / `Method` children |
//! | `enum` | `Enum` | members → `Constant` children |
//! | `namespace N {}` / `module N {}` / `declare module "m" {}` | `Module` | inner declarations are qualified by it (`N.helper`); everything inside `declare module "m"` is exported |
//! | other module-level `const` | `Constant` if `UPPER_CASE`, else `Variable` | `let` / `var` → `Variable`; destructuring gives one per name |
//! | methods / function-valued pairs of a module-level object literal (`export const api = { get() {} }`) | `Method` children of the variable | |
//! | CommonJS `module.exports = function f`, `exports.x = function`, `X.prototype.y = function`, `X.y = function` | `Function`, or `Method` of a module-level `X` | |
//! | named function expression anywhere else: call arguments (`defineGetter(req, "query", function query() {})`, `app.use(function handler() {})`), `return function timing() {}`, operands | `Function` child of the enclosing definition | span = the function expression; doc = JSDoc before it or before the statement holding it. Anonymous ones are not definitions |
//! | `describe` / `it` / `test` (also `context`, `suite`, `specify`, `fit`, `xit`, `.each`, `.only`, `.skip`, `.todo`) with a string first argument and a function argument | `Test` | `is_test`; qualified name = describe chain joined by ` > ` (dots in titles become `_`); references inside get the test as `enclosing` |
//!
//! - `exported`: `export` declarations, `export { a, b as c }`,
//!   `export default x`, `export = x`, `module.exports = x` /
//!   `module.exports = { a, b }` / `exports.x = …`. Members inherit their
//!   owner's flag. `export default function foo` is `foo`; anonymous defaults
//!   are named `default`.
//! - `span` covers the `export` keyword and decorators; `name_span` is the
//!   identifier.
//! - `signature`: one line, from the declaration start to its body (`{` or
//!   `=>`), whitespace collapsed, parameter decorators removed, ≤ 200 chars.
//! - `doc`: the `/** … */` JSDoc block immediately before the declaration
//!   (only whitespace in between), delimiters and leading `*` stripped,
//!   ≤ 1000 chars.
//! - `decorators`: decorator expressions without `@`, whitespace collapsed.
//!
//! # References
//!
//! `enclosing` is the innermost definition (`None` at module level).
//!
//! - `Call`: `f()`, `a.b.c()`, `a?.b()` → name = last identifier, receiver =
//!   object text (≤ 200 chars). Calls whose root is a global builtin
//!   (`console.log`, `Promise.all`, `JSON.parse`) are skipped unless the name
//!   is shadowed locally. Capitalized JSX tags (`<Button/>`, `<Foo.Bar/>`)
//!   are `Call`s too.
//! - Builtins come in two lists (`builtins.rs`): ECMAScript / TypeScript /
//!   Node / test globals that are never emitted, and web-platform and runtime
//!   globals that projects may also define (`Request`, `Response`,
//!   `Headers`, `FormData`, `Blob`, `ReadableStream`, `WebSocket`, `Event*`,
//!   `crypto`, `Element`, …). References to the latter are emitted (an
//!   imported or same-module `Request` still resolves), but
//!   [`LanguagePack::is_builtin`] reports both lists, so the resolver never
//!   matches an unbound `Request` to a project symbol by unique name.
//! - `New`: `new Foo()`; `Throw`: `throw new X()` (also a `New`).
//! - `Type`: type identifiers in annotations, generics, `implements`,
//!   `extends` (classes and interfaces), `satisfies`, `as`, skipping builtin
//!   types and type parameters in scope. Type parameters are scoped by the
//!   declaration that introduces them: functions, methods, classes,
//!   interfaces, type aliases, call / construct signatures, function and
//!   constructor types, mapped types (`[K in …]`) and `infer`. Parse-error
//!   regions (tree-sitter-typescript 0.23 splits interfaces whose generic call
//!   signatures start on new lines into `ERROR` fragments) keep the type
//!   parameters of the declaration they continue and of earlier fragments,
//!   and their references are attributed to that declaration.
//! - `Decorator`: `@Injectable()` → `Injectable`; `@a.b()` → `b` / `a`.
//! - `Read`: identifiers and plain member chains (`a.b`, `this.x`) in value
//!   position (arguments, return values, initialisers, assignment right-hand
//!   sides, JSX expressions, spreads, ternaries, binary operands) whose root
//!   is neither a local binding nor a global builtin. Deduplicated per
//!   `(enclosing, receiver, name)`.
//! - `Write`: `this.x = …` → name `x`, receiver `this`.
//! - `EnvVar` / `ConfigKey` (the code ↔ config bridge, RFC 0003 §7.2):
//!   `process.env.X`, `process.env["X"]`, `const { X } = process.env`,
//!   `import.meta.env.X` (also destructured) and `Deno.env.get("X")` →
//!   `EnvVar` named `X`; `config.get("a.b")` / `config.has("a.b")` with
//!   `config` imported or required from the `config` package → `ConfigKey`
//!   `a.b`, receiver `node-config`. Only literal names; `process` / `Deno`
//!   must not be local bindings; assignment and `delete` targets are not
//!   reads. The resolver links them with `READS` (see `rueca-resolve`).
//! - At most 20,000 references per file.
//!
//! # Imports
//!
//! `module` is the raw specifier, `relative_level` is always 0.
//!
//! | Source | `names` | `module_alias` | flags |
//! |---|---|---|---|
//! | `import x from "m"` | `default` as `x` | | |
//! | `import { a, b as c } from "m"` | `a`, `b` as `c` | | |
//! | `import * as ns from "m"` | | `ns` | |
//! | `import "m"` | | | |
//! | `import type { T } from "m"` | `T` | | `type_only` |
//! | `import { a, type T } from "m"` | two imports: `a`, and `T` with `type_only` | | |
//! | `import x = require("m")` | | `x` | |
//! | `export { a as b } from "m"` | `a` as `b` | | `reexport` |
//! | `export * from "m"` | `*` | | `reexport` |
//! | `export * as ns from "m"` | | `ns` | `reexport` |
//! | `export default x` / `module.exports = x` (`x` an identifier) | `x` as `default` | | `reexport`, module = `./<own file name>` |
//! | `export { a as b }` (no `from`, `a` ≠ `b`) | `a` as `b` | | `reexport`, module = `./<own file name>` |
//! | `const x = require("m")` / `await import("m")` | | `x` | |
//! | `const { a, b: c } = require("m")` | `a`, `b` as `c` | | |
//! | `const y = require("m").y` | `y` | | |
//! | `require("m")` / `import("m")` elsewhere | | | |
//!
//! Mixed value/type imports are split so `type_only` stays exact per name.
//! Self re-exports (module `./<own file name>`) resolve to the file itself, so the
//! resolver binds `import x from "./m"` and `import { b } from "./m"` to the
//! exact local definition instead of guessing the default export.
//!
//! # Locals
//!
//! A read is local when the name is bound by the current function or an
//! enclosing one: parameters, `const` / `let` / `var` (block scopes are
//! flattened into their function), destructuring at any depth, `catch`
//! parameters, `for … of / in` targets, nested function and class names,
//! named function expressions. Module-level declarations are module symbols,
//! not locals, except inside blocks. Type parameters are tracked separately.
//!
//! # Module names and import resolution
//!
//! [`LanguagePack::module_name`] is the path without its final extension
//! (`src/util/index.ts` → `src/util/index`, `x.d.ts` → `x`).
//!
//! [`LanguagePack::resolve_import`] runs `oxc_resolver` with extensions
//! `.ts .tsx .d.ts .js .jsx .mjs .cjs .mts .cts .json`, extension aliases
//! `.js → .ts .tsx .js`, `.jsx → .tsx .jsx`, `.mjs → .mts .mjs`,
//! `.cjs → .cts .cjs`, main fields `types module main`, conditions
//! `types import require default node`, and automatic tsconfig discovery
//! (nearest `tsconfig.json`, or `jsconfig.json`, owning the file; `extends`,
//! `baseUrl`, `paths`, project references). Workspace packages (any
//! `package.json` with a `name`) resolve by name, through their `exports` /
//! `types` / `module` / `main`; when those point at an uncommitted build
//! directory (`dist/`, `lib/`, …) the matching `src/` file is used. Third-party
//! packages, Node builtins and anything outside `ProjectFiles::paths` are
//! `None`. Details in `resolve.rs`.
//!
//! # Deviations and limitations
//!
//! - Class fields initialised with a function (`onClick = () => …`) are
//!   `Method`s, not `Field`s: they are called like methods.
//! - Dots in test titles become `_` in qualified names (`.` is the member
//!   separator); `name` keeps the title.
//! - Block scoping is flattened to function level for locals (a `const` in an
//!   inner block shadows module symbols in the whole function).
//! - Calls to local bindings are still emitted (the resolver can link them to
//!   nested definitions); only reads of locals are suppressed.
//! - `.d.ts` files are extracted like any TypeScript file.
//! - Grammar gaps (tree-sitter-typescript 0.23.2, the latest release): newer
//!   syntax such as `export type * from` yields parse errors; extraction
//!   continues around them and `parse_errors` counts them.
//! - `oxc_resolver` is pinned to the 11.x line compatible with the workspace
//!   MSRV (Rust 1.90): 11.23 (11.24 requires Rust 1.95).

mod bridge;
mod builtins;
mod extract;
mod locals;
mod resolve;

use std::time::Instant;

use rueca_extract::{Budget, ExtractError, LanguagePack, ProjectFiles, count_errors, line_count};
use rueca_model::{FileFacts, Import, Language};

pub use extract::MAX_REFS;

/// ECMAScript pack (TypeScript, TSX, JavaScript/JSX). Stateless apart from a
/// process-wide resolver cache; construct with `TypeScriptPack`.
#[derive(Debug, Default)]
pub struct TypeScriptPack;

impl LanguagePack for TypeScriptPack {
    fn languages(&self) -> &'static [Language] {
        &[Language::TypeScript, Language::Tsx, Language::JavaScript]
    }

    fn extractor_version(&self) -> u32 {
        // 3: env var / config key reads (RFC 0003 §7.2).
        3
    }

    fn extract(
        &self,
        path: &str,
        language: Language,
        source: &[u8],
        budget: &Budget,
    ) -> Result<FileFacts, ExtractError> {
        let grammar = locals::grammar(language).ok_or(ExtractError::Unsupported(language))?;
        let query = locals::locals_query(language)?;
        let tree = rueca_extract::parse(&grammar, source, budget)?;
        let root = tree.root_node();
        let locals = locals::Locals::compute(query, root, source, Instant::now() + budget.max_parse_time);
        let mut facts = extract::extract_tree(root, source, path, budget.max_depth, &locals);
        facts.parse_errors = count_errors(&tree);
        facts.line_count = line_count(source);
        Ok(facts)
    }

    fn module_name(&self, path: &str, _project: &ProjectFiles) -> String {
        resolve::module_name(path)
    }

    fn resolve_import(&self, from_path: &str, import: &Import, project: &ProjectFiles) -> Option<String> {
        resolve::resolver_for(project).resolve(from_path, &import.module)
    }

    fn is_builtin(&self, name: &str) -> bool {
        builtins::is_global(name)
    }

    fn self_receivers(&self) -> &'static [&'static str] {
        &["this"]
    }
}

#[cfg(test)]
mod tests;
