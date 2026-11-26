# Vendored tree-sitter queries — `rueca-lang-kotlin`

Queries in this directory are compiled at runtime against
`tree-sitter-kotlin-sg` 0.4.1 (`LANGUAGE`, tree-sitter runtime 0.27), the
crates.io release of fwcd/tree-sitter-kotlin by ast-grep (commit
`1a6f9b1ee1125a7357493eeb95da48d16ac302b4` of ast-grep/tree-sitter-kotlin).
Each file starts with an SPDX header, its upstream URL and commit, and the
list of Rueca adaptations. `NOTICE` at the repository root carries the
attributions.

| File | Upstream | Commit | License | Use |
|---|---|---|---|---|
| `locals.scm` | [nvim-treesitter `runtime/queries/kotlin/locals.scm`](https://github.com/nvim-treesitter/nvim-treesitter/blob/29212d826054bb2948fa9d3a99eca9bf6a050eb2/runtime/queries/kotlin/locals.scm) | `29212d826054bb2948fa9d3a99eca9bf6a050eb2` | Apache-2.0 | Local scopes and bindings |

nvim-treesitter parses Kotlin with fwcd/tree-sitter-kotlin, so its query
uses the node names of the grammar this crate links (`simple_identifier`,
`variable_declaration`, `lambda_literal`, …).

Grammars and sources considered and not used:

- `tree-sitter-kotlin-ng` 1.1.0 (tree-sitter-grammars, MIT): more files with
  parse errors on real code (moshi 2 vs 0 of 99 files, okhttp 8 vs 0 of
  620, Exposed 63 vs 17 of 1,052, kotlinpoet 5 vs 3 of 134, clikt 1 vs 0 of
  146; koin 7 vs 10 of 541), and nvim-treesitter's queries do not match its
  node names.
- `tree-sitter-kotlin` 0.3.8 (fwcd's own crate, MIT): requires
  `tree-sitter >= 0.21, < 0.23` as a normal dependency, which does not link
  with the 0.27 runtime.
- The grammar's `tags.scm` (MIT) has no containment, qualified names,
  receivers or imports, which Rueca code extracts.
- Zed (GPL-3.0) and Helix (MPL-2.0) queries: excluded by RFC 0002 §2.

## What the query is for

A Kotlin identifier in a body is a local (parameter, `val` / `var`, lambda
or `for` variable, `when` subject, `catch` parameter), a property, a class
or object, or a function. The locals table answers two questions for the
extractor: **is this identifier a local** (so it is never reported as a
property read, and a local holding a function value called like a function
is no call), and **which class does it hold** (from its declared type, or
an initializer that constructs one, so that `x.m()` is reported with the
typed receiver `$type:T`). See `src/locals.rs`.

## Capture contract (what `src/locals.rs` relies on)

| Capture | Meaning |
|---|---|
| `@local.scope` | function, anonymous function, lambda, class, secondary constructor, `init`, getter, setter, `if`, `when` and its entries, loops, `catch`, enum body and entries, string interpolation |
| `@local.definition.<kind>` | the `simple_identifier` of a binding (`parameter`, `var`, `field`); its declared type and initializer are read from the declaration node by Rust code. Properties of classes, objects and files and `val` / `var` primary-constructor parameters are members, not locals, and are skipped |

A binding is visible in its innermost scope from its position on. The
primary constructor is not a scope: its plain parameters are visible in the
whole class (property initializers and `init` blocks).

## Upgrading the grammar

`cargo test -p rueca-lang-kotlin locals` fails when the query no longer
compiles, when a required capture disappears, or when any pattern stops
matching the golden corpus in `src/locals.rs` (a renamed node or field).
Fix the query, keep the header's adaptation list current, and bump
`extractor_version()` if extraction output changes.
