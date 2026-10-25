# Vendored tree-sitter queries — `rueca-lang-php`

Queries in this directory are compiled at runtime against
`tree-sitter-php` 0.25.1 (`LANGUAGE_PHP`, tree-sitter runtime 0.27). Each
file starts with an SPDX header, its upstream URLs and commits, and the list
of Rueca adaptations. `NOTICE` at the repository root carries the
attributions.

| File | Upstream | Commit | License | Use |
|---|---|---|---|---|
| `locals.scm` | [nvim-treesitter `runtime/queries/php_only/locals.scm`](https://github.com/nvim-treesitter/nvim-treesitter/blob/29212d826054bb2948fa9d3a99eca9bf6a050eb2/runtime/queries/php_only/locals.scm) (base; `php/locals.scm` is `; inherits: php_only`) | `29212d826054bb2948fa9d3a99eca9bf6a050eb2` | Apache-2.0 | Variable scopes, bindings, uses |
| `locals.scm` | [bloop `server/bleep/src/intelligence/language/php/scopes.scm`](https://github.com/BloopAI/bloop/blob/431e9e82c5a293c40f22aadb3615c3aa387af82e/server/bleep/src/intelligence/language/php/scopes.scm) (additions: arrow-function scope, variadic parameters, assignments, list destructuring, `static` / `global`) | `431e9e82c5a293c40f22aadb3615c3aa387af82e` | Apache-2.0 | same file |

Sources considered and not used:

- Upstream `tree-sitter-php` (MIT) ships no `locals.scm`; its
  `queries/tags.scm` has definitions and calls but no containment,
  qualified names, receivers or namespaces, all of which Rueca code
  extracts.
- Zed (GPL-3.0) and Helix (MPL-2.0) queries: excluded by RFC 0002 §2.

## What the query is for

PHP variables (`$x`) are syntactically distinct from functions, classes and
constants, and the pack never reports variables as references. The locals
table answers one question: **which class does `$x` hold?** Every binding of
a variable in its scope (typed parameter, `catch (X $e)`, `$x = new X`,
docblock `@param X $x`) must agree for `$x->m()` to be reported with the
typed receiver `\Fqn\X->`. See `src/locals.rs`.

## Capture contract (what `src/locals.rs` relies on)

| Capture | Meaning |
|---|---|
| `@local.scope` | file (`program`), function, method, closure (`anonymous_function`), arrow function |
| `@local.definition.<kind>` | a binding; the captured node may be a whole target (`list_literal`, `pair`, `by_ref`): every `variable_name` inside it is bound, except under member / subscript accesses. Kinds: `parameter`, `var`, `catch`, `global`, `capture` (closure `use`); unknown kinds are treated as `var` |
| `@local.reference` | every `variable_name` use |

## Upgrading the grammar

`cargo test -p rueca-lang-php locals` fails when the query no longer
compiles, when a required capture name disappears, or when any pattern stops
matching the golden corpus in `src/locals.rs` (a renamed node or field).
Fix the query, keep the header's adaptation list current, and bump
`extractor_version()` if extraction output changes.
