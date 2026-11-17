# Vendored tree-sitter queries — `rueca-lang-rust`

Queries in this directory are compiled at runtime against
`tree-sitter-rust` 0.24.2 (`LANGUAGE`, tree-sitter runtime 0.27). Each file
starts with an SPDX header, its upstream URL and commit, and the list of
Rueca adaptations. `NOTICE` at the repository root carries the
attributions.

| File | Upstream | Commit | License | Use |
|---|---|---|---|---|
| `locals.scm` | [nvim-treesitter `runtime/queries/rust/locals.scm`](https://github.com/nvim-treesitter/nvim-treesitter/blob/29212d826054bb2948fa9d3a99eca9bf6a050eb2/runtime/queries/rust/locals.scm) | `29212d826054bb2948fa9d3a99eca9bf6a050eb2` | Apache-2.0 | Local scopes and bindings |

Sources considered and not used:

- Upstream `tree-sitter-rust` (MIT) ships no `locals.scm`; its
  `queries/tags.scm` has definitions and calls but no containment,
  qualified names, receivers or imports, all of which Rueca code extracts.
- bloop's `server/bleep/src/intelligence/language/rust/scopes.scm`
  (Apache-2.0, commit `431e9e82c5a293c40f22aadb3615c3aa387af82e`) was
  considered: most of it is `@local.reference` and item patterns, which
  Rueca extracts in Rust code; the nvim-treesitter query plus the additions
  listed in the file header cover the bindings the extractor needs, so only
  one upstream is vendored.
- Zed (GPL-3.0) and Helix (MPL-2.0) queries: excluded by RFC 0002 §2.

## What the query is for

Rust identifiers in value position are either locals (parameters, `let`,
`for`, `match` arm, `if let` / `while let` and closure bindings) or items,
imports and builtins. The locals table answers two questions for the
extractor: **is this identifier a local** (then it is not a reference; a
local closure called like a function is no call), and **what type does it
hold** (types are inferred on the Rust side, see `src/receivers.rs`, and a
typed local becomes a `$type:T` receiver). Generic parameters are bindings
too, so `T` in `fn f<T: Tr>` is never a type reference and stands for its
bound `Tr` as a receiver. Macro arguments re-parsed by `src/macros.rs` get
their own table (closure parameters inside `println!(…)`).

## Capture contract (what `src/locals.rs` relies on)

| Capture | Meaning |
|---|---|
| `@local.scope` | `block`, function, closure, `while`, `for`, `loop`, `if`, `match` and each match arm |
| `@local.definition.parameter` | a function or closure parameter |
| `@local.definition.var` | an identifier bound by a pattern: `let`, `for`, a match arm, `if let` / `while let`, at any depth of tuple, tuple-struct, struct, slice, reference, `ref` / `mut`, `x @ …` and or-patterns (the Rust side decides the binding site and skips paths: the type of a tuple-struct pattern, upper-case names) |
| `@local.definition.type_parameter` | a type or const generic parameter (its item is computed on the Rust side) |

No `@local.reference` captures: the extractor visits every identifier and
asks the table.

## Upgrading the grammar

`cargo test -p rueca-lang-rust locals` fails when the query no longer
compiles, when a required capture name disappears, or when any pattern stops
matching the golden corpus in `src/locals.rs` (a renamed node or field).
Fix the query, keep the header's adaptation list current, and bump
`extractor_version()` if extraction output changes.
