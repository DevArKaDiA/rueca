# Vendored tree-sitter queries — `rueca-lang-swift`

Queries in this directory are compiled at runtime against
`tree-sitter-swift` 0.7.4 (`LANGUAGE`, tree-sitter runtime 0.27;
alex-pinkus/tree-sitter-swift, MIT, which ships its generated parser at
ABI 15). Each file starts with an SPDX header, its upstream URL and commit,
and the list of Rueca adaptations. `NOTICE` at the repository root carries
the attributions.

| File | Upstream | Commit | License | Use |
|---|---|---|---|---|
| `locals.scm` | [nvim-treesitter `runtime/queries/swift/locals.scm`](https://github.com/nvim-treesitter/nvim-treesitter/blob/29212d826054bb2948fa9d3a99eca9bf6a050eb2/runtime/queries/swift/locals.scm) | `29212d826054bb2948fa9d3a99eca9bf6a050eb2` | Apache-2.0 | Local scopes and bindings |

nvim-treesitter parses Swift with alex-pinkus/tree-sitter-swift, so its
query uses the node names of the grammar this crate links (`statements`,
`property_declaration`, `lambda_literal`, …). The grammar ships the same
file as `queries/locals.scm`.

Sources considered and not used:

- Upstream's query has scopes only (no variable bindings); the bindings
  are Rueca additions listed in the file header.
- The grammar's `tags.scm` (MIT) has definitions and calls but no
  containment, qualified names, receivers, argument labels or imports, which
  Rueca code extracts.
- Zed (GPL-3.0) and Helix (MPL-2.0) queries: excluded by RFC 0002 §2.

## What the query is for

Swift identifiers in value position are either locals (parameters, `let` /
`var`, `for`, `if let` / `guard let` / `while let`, `catch let`, `case let`
and closure bindings) or members, module-level declarations and builtins.
The locals table answers two questions for the extractor: **is this
identifier a local** (then a call of it is a closure value, not a function
reference), and **what type does it hold** (inferred on the Rust side, see
`src/locals.rs`; a typed local becomes a `$type:T` receiver).

## Capture contract (what `src/locals.rs` relies on)

| Capture | Meaning |
|---|---|
| `@local.scope` | `statements`, loops, `if`, `switch` and its entries, `do`, `catch`, function / `init` / `deinit` / `subscript` / protocol-function declarations, closures, accessor blocks |
| `@local.definition.parameter` | a function, `init`, `subscript` or closure parameter (the Rust side keeps the bound name, not the external label) |
| `@local.definition.var` | an identifier bound by `let` / `var` (also inside a tuple pattern), `for`, `if let` / `guard let` / `while let`, `catch let` or `case let` (the Rust side drops properties of types and files) |

No `@local.reference` captures: the extractor asks the table for every
identifier it meets.

## Upgrading the grammar

`cargo test -p rueca-lang-swift locals` fails when the query no longer
compiles, when a required capture name disappears, or when any pattern stops
matching the golden corpus in `src/locals.rs` (a renamed node or field).
Fix the query, keep the header's adaptation list current, and bump
`extractor_version()` if extraction output changes.
