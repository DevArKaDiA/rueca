# Vendored tree-sitter queries — `rueca-lang-java`

Queries in this directory are compiled at runtime against
`tree-sitter-java` 0.23.5 (`LANGUAGE`, tree-sitter runtime 0.27). Each file
starts with an SPDX header, its upstream URLs and commits, and the list of
Rueca adaptations. `NOTICE` at the repository root carries the
attributions.

| File | Upstream | Commit | License | Use |
|---|---|---|---|---|
| `locals.scm` | [nvim-treesitter `runtime/queries/java/locals.scm`](https://github.com/nvim-treesitter/nvim-treesitter/blob/29212d826054bb2948fa9d3a99eca9bf6a050eb2/runtime/queries/java/locals.scm) (base) | `29212d826054bb2948fa9d3a99eca9bf6a050eb2` | Apache-2.0 | Local-variable scopes and bindings |
| `locals.scm` | [bloop `server/bleep/src/intelligence/language/java/scopes.scm`](https://github.com/BloopAI/bloop/blob/431e9e82c5a293c40f22aadb3615c3aa387af82e/server/bleep/src/intelligence/language/java/scopes.scm) (additions: switch-case and try-with-resources scopes, resources, `instanceof` and type-pattern bindings) | `431e9e82c5a293c40f22aadb3615c3aa387af82e` | Apache-2.0 | same file |

Sources considered and not used:

- Upstream `tree-sitter-java` (MIT) ships `tags.scm` and `highlights.scm`
  but no `locals.scm`; its tags have no containment, qualified names or
  receivers, which Rueca code extracts.
- Sourcegraph scip-syntax Java queries (MIT) were not needed: the locals
  above cover every binding form of the 0.23.5 grammar.
- Zed (GPL-3.0) and Helix (MPL-2.0) queries: excluded by RFC 0002 §2.

## What the query is for

Java identifiers in method bodies are locals, fields, classes or package
names, and the grammar does not tell them apart. The locals table answers
two questions for the extractor: **is this identifier a local variable**
(so it is never reported as a field read or a class), and **which class
was it declared with** (so `x.m()` is reported with the typed receiver
`$type:T`). See `src/locals.rs`.

## Capture contract (what `src/locals.rs` relies on)

| Capture | Meaning |
|---|---|
| `@local.scope` | file, block, method, constructor, compact constructor, lambda, `for`, enhanced `for`, `catch`, try-with-resources, switch group / rule |
| `@local.definition.<kind>` | the identifier of a binding (`var`, `parameter`); its declared type is read from the declaration node by Rust code. Record components (parameters of a `record_declaration`) are skipped: they are fields |

A binding is visible in its innermost scope from its position on.

## Upgrading the grammar

`cargo test -p rueca-lang-java locals` fails when the query no longer
compiles, when a required capture disappears, or when any pattern stops
matching the golden corpus in `src/locals.rs` (a renamed node or field).
Fix the query, keep the header's adaptation list current, and bump
`extractor_version()` if extraction output changes.
