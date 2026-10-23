# Vendored queries — telar-lang-typescript

Tree-sitter queries vendored and adapted per RFC 0002 §3.4. They only find
**local bindings**; definitions, containment, qualified names, imports,
inheritance and calls with receivers are Telar code (`src/extract.rs`).

| File | Upstream | Commit | License |
|---|---|---|---|
| `ecma-locals.scm` | nvim-treesitter `runtime/queries/ecma/locals.scm` | `29212d826054bb2948fa9d3a99eca9bf6a050eb2` | Apache-2.0 |
| `ecma-locals.scm` (one pattern: `(pattern/identifier)`) | tree-sitter-javascript `queries/locals.scm` (v0.25.0) | `44c892e0be055ac465d5eeddae6d3e194424e7de` | MIT |
| `javascript-locals.scm` | nvim-treesitter `runtime/queries/javascript/locals.scm` | `29212d826054bb2948fa9d3a99eca9bf6a050eb2` | Apache-2.0 |
| `typescript-locals.scm` | nvim-treesitter `runtime/queries/typescript/locals.scm` | `29212d826054bb2948fa9d3a99eca9bf6a050eb2` | Apache-2.0 |
| `tsx-locals.scm` | nvim-treesitter `runtime/queries/tsx/locals.scm` | `29212d826054bb2948fa9d3a99eca9bf6a050eb2` | Apache-2.0 |

Upstream URLs:

- https://github.com/nvim-treesitter/nvim-treesitter/tree/29212d826054bb2948fa9d3a99eca9bf6a050eb2/runtime/queries
- https://github.com/tree-sitter/tree-sitter-javascript/blob/44c892e0be055ac465d5eeddae6d3e194424e7de/queries/locals.scm

Grammars they are compiled against (golden test
`locals_queries_compile_against_pinned_grammars`): `tree-sitter-javascript`
0.25.0 and `tree-sitter-typescript` 0.23.2 (`LANGUAGE_TYPESCRIPT`,
`LANGUAGE_TSX`), runtime `tree-sitter` 0.27.

Excluded sources (RFC 0002 §2): Zed queries (GPL-3.0), Helix queries
(MPL-2.0).

## `; inherits:`

nvim-treesitter composes languages with a `; inherits: ecma` /
`; inherits: typescript` first line. Telar resolves it in `src/locals.rs` by
concatenation before compiling:

| Grammar | Query source |
|---|---|
| JavaScript | `ecma-locals.scm` + `javascript-locals.scm` |
| TypeScript | `ecma-locals.scm` + `typescript-locals.scm` |
| TSX | `ecma-locals.scm` + `typescript-locals.scm` + `tsx-locals.scm` |

## Adaptations

Each file's header lists its changes; new patterns are marked `Telar:`.

- Removed every `@local.reference` pattern: Telar's walker emits references.
- Removed `method_definition` / `field_definition` / `this.x = …` definitions:
  they are properties, not bindings.
- Added scopes: generator functions; body-less callables (TS function /
  method / abstract method signatures, function types, call and construct
  signatures) so their parameters do not leak into the enclosing function.
- Added definitions: destructuring at any depth (`pattern/identifier`, rest
  and shorthand patterns), `catch` parameters, `for … of / in` targets,
  generator / function-expression / class / abstract-class names.
- Added `@telar.namespace_body` (TS only): namespace and ambient-module bodies
  are not local scopes.

The `#set! definition.var.scope parent` property from upstream is honoured
(read with `Query::property_settings`); no Neovim-only predicates remain.

## Attribution (for NOTICE)

```
telar-lang-typescript/queries/*-locals.scm are adapted from nvim-treesitter
(https://github.com/nvim-treesitter/nvim-treesitter, commit 29212d82),
Copyright the nvim-treesitter contributors, licensed under the Apache
License 2.0.

telar-lang-typescript/queries/ecma-locals.scm contains a pattern from
tree-sitter-javascript (https://github.com/tree-sitter/tree-sitter-javascript,
v0.25.0), Copyright (c) 2014 Max Brunsfeld, licensed under the MIT License.
```
