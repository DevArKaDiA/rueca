# Vendored tree-sitter queries — `rueca-lang-csharp`

Queries in this directory are compiled at runtime against
`tree-sitter-c-sharp` 0.24.0 (`LANGUAGE`, tree-sitter runtime 0.27). Each
file starts with an SPDX header, its upstream URL and commit, and the list
of Rueca adaptations. `NOTICE` at the repository root carries the
attributions.

| File | Upstream | Commit | License | Use |
|---|---|---|---|---|
| `locals.scm` | [nvim-treesitter `runtime/queries/c_sharp/locals.scm`](https://github.com/nvim-treesitter/nvim-treesitter/blob/29212d826054bb2948fa9d3a99eca9bf6a050eb2/runtime/queries/c_sharp/locals.scm) | `29212d826054bb2948fa9d3a99eca9bf6a050eb2` | Apache-2.0 | Local scopes and bindings |

Sources considered and not used:

- Upstream `tree-sitter-c-sharp` (MIT) ships `queries/tags.scm` and
  `highlights.scm` but no `locals.scm`; its tags have definitions and
  calls but no containment, qualified names, receivers or usings, all of
  which Rueca code extracts.
- bloop's `scopes.scm` for C# and Sourcegraph scip-syntax's C# locals
  (Apache-2.0 / MIT) were not needed: the nvim-treesitter query plus the
  additions listed in the file header cover every binding form the
  extractor needs, so only one upstream is vendored.
- Zed (GPL-3.0) and Helix (MPL-2.0) queries: excluded by RFC 0002 §2.

## What the query is for

C# identifiers in bodies are locals, fields, properties, types or
namespaces, and the grammar does not tell them apart (types are
`identifier`s too). The locals table answers two questions for the
extractor: **is this identifier a local variable** (so it is never
reported as a member read or a type), and **which type was it declared
with** (so `x.M()` is reported with the typed receiver `$type:T`). See
`src/locals.rs`.

## Capture contract (what `src/locals.rs` relies on)

| Capture | Meaning |
|---|---|
| `@local.scope` | block, every callable (method, constructor, destructor, operator, conversion operator, local function, accessor, indexer, lambda, anonymous method), `for`, `foreach`, `catch`, `using`, `fixed`, switch section and arm, query expression, a class or struct with a primary constructor |
| `@local.definition.<kind>` | the identifier of a binding (`var`, `parameter`); its declared type is read from the declaration node by Rust code. Record parameters are skipped: they are properties |

A binding is visible in its innermost scope from its position on.

## Upgrading the grammar

`cargo test -p rueca-lang-csharp locals` fails when the query no longer
compiles, when a required capture disappears, or when any pattern stops
matching the golden corpus in `src/locals.rs` (a renamed node or field).
Fix the query, keep the header's adaptation list current, and bump
`extractor_version()` if extraction output changes.
