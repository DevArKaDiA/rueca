# Vendored tree-sitter queries — `rueca-lang-go`

Queries in this directory are compiled at runtime against
`tree-sitter-go` 0.25.0 (`LANGUAGE`, tree-sitter runtime 0.27). Each file
starts with an SPDX header, its upstream URLs and commits, and the list of
Rueca adaptations. `NOTICE` at the repository root carries the
attributions.

| File | Upstream | Commit | License | Use |
|---|---|---|---|---|
| `locals.scm` | [nvim-treesitter `runtime/queries/go/locals.scm`](https://github.com/nvim-treesitter/nvim-treesitter/blob/29212d826054bb2948fa9d3a99eca9bf6a050eb2/runtime/queries/go/locals.scm) (base) | `29212d826054bb2948fa9d3a99eca9bf6a050eb2` | Apache-2.0 | Local scopes and bindings |
| `locals.scm` | [bloop `server/bleep/src/intelligence/language/go/scopes.scm`](https://github.com/BloopAI/bloop/blob/431e9e82c5a293c40f22aadb3615c3aa387af82e/server/bleep/src/intelligence/language/go/scopes.scm) (additions: function-literal parameters, type parameters, type-switch aliases, `select` receive bindings, case scopes) | `431e9e82c5a293c40f22aadb3615c3aa387af82e` | Apache-2.0 | same file |

Sources considered and not used:

- Upstream `tree-sitter-go` (MIT) ships no `locals.scm`; its
  `queries/tags.scm` has definitions and calls but no containment,
  qualified names, receivers or imports, all of which Rueca code extracts.
- Sourcegraph `scip-syntax` Go queries (MIT): frozen at tree-sitter 0.20.9
  (RFC 0002 §3.4); nvim-treesitter tracks the current grammar.
- Zed (GPL-3.0) and Helix (MPL-2.0) queries: excluded by RFC 0002 §2.

## What the query is for

Go identifiers in value position are either locals (parameters, `:=`,
`var` inside a function, range / type-switch / `select` bindings) or
package-level names, imports and builtins. The locals table answers two
questions for the extractor: **is this identifier a local** (then it is not
a reference), and **what type does it hold** (a typed local becomes a
`$type:T` receiver; the receiver of a method becomes `$self`). Type
parameters are bindings too, so `T` in `func Map[T any]` or
`func (l *List[T])` is never reported as a type reference. See
`src/locals.rs`.

## Capture contract (what `src/locals.rs` relies on)

| Capture | Meaning |
|---|---|
| `@local.scope` | `source_file`, function, method, function literal, `block`, `if`, `for`, `switch` (expression and type), each case clause and `select` case |
| `@local.definition.parameter` | a parameter, named result or method receiver (the Rust side tells receivers apart and ignores parameters of function *types*) |
| `@local.definition.type_parameter` | a type parameter of a function, generic type or (through the receiver's type arguments) method |
| `@local.definition.var` | `x := …`, `var x …` |
| `@local.definition.const` | `const x = …` |
| `@local.definition.range` | `for k, v := range …` |
| `@local.definition.type_switch` | `switch v := x.(type)`; typed per `case` clause |
| `@local.definition.receive` | `case v := <-ch` |
| `@local.definition.type` | a type declared in a function (package-level types are ignored: bindings directly in `source_file` are not locals) |
| `@local.definition.function` | a function declaration (package-level, ignored by the locals table) |

No `@local.reference` captures: the extractor visits every identifier and
asks the table.

## Upgrading the grammar

`cargo test -p rueca-lang-go locals` fails when the query no longer
compiles, when a required capture name disappears, or when any pattern stops
matching the golden corpus in `src/locals.rs` (a renamed node or field).
Fix the query, keep the header's adaptation list current, and bump
`extractor_version()` if extraction output changes.
