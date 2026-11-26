# Vendored tree-sitter queries — `rueca-lang-ruby`

Queries in this directory are compiled at runtime against
`tree-sitter-ruby` 0.23.1 (`LANGUAGE`, tree-sitter runtime 0.27). Each file
starts with an SPDX header, its upstream URL and commit, and the list of
Rueca adaptations. `NOTICE` at the repository root carries the
attributions.

| File | Upstream | Commit | License | Use |
|---|---|---|---|---|
| `locals.scm` | [nvim-treesitter `runtime/queries/ruby/locals.scm`](https://github.com/nvim-treesitter/nvim-treesitter/blob/29212d826054bb2948fa9d3a99eca9bf6a050eb2/runtime/queries/ruby/locals.scm) | `29212d826054bb2948fa9d3a99eca9bf6a050eb2` | MIT (Copyright (c) 2016 Rob Rix, in the file) | Local scopes and bindings |

Sources considered and not used:

- Upstream `tree-sitter-ruby` (MIT) ships `queries/locals.scm` too; the
  nvim-treesitter file is the same query (it carries Rob Rix's MIT header
  from the grammar) with the parameter kinds split out, so it is the one
  vendored.
- Upstream `queries/tags.scm` (MIT) has definitions and calls but no
  containment, qualified constant paths, receivers or imports, all of
  which Rueca code extracts.
- Zed (GPL-3.0) and Helix (MPL-2.0) queries: excluded by RFC 0002 §2.

## What the query is for

A bare Ruby identifier is either a local variable or a method call with an
implicit receiver (`helper` is `self.helper()` unless `helper` was
assigned or is a parameter). Ruby decides it at parse time: a name is a
local from the first assignment to it onwards, in the current `def`,
`class` or `module` body and in the blocks inside it. The locals table
answers that question for the extractor (`src/locals.rs`): an identifier
that is not a local is a `Call` with no receiver. It also tells which
names a typed assignment (`x = Foo.new`) may type.

## Capture contract (what `src/locals.rs` relies on)

| Capture | Meaning |
|---|---|
| `@local.scope.hard` | `def`, `def self.x`, `class`, `module`, `class << x`: a new local table |
| `@local.scope` | `{ … }` / `do … end` blocks and lambdas: see the enclosing locals |
| `@local.definition.parameter` | a method, block or lambda parameter (any kind) |
| `@local.definition.var` | an assignment target (`x = …`, `x ||= …`, `a, b = …`, `*rest`), `rescue => e`, a `for` variable |

No `@local.reference` captures: the extractor visits every identifier and
asks the table.

## Upgrading the grammar

`cargo test -p rueca-lang-ruby locals` fails when the query no longer
compiles, when a required capture name disappears, or when any pattern
stops matching the golden corpus in `src/locals.rs` (a renamed node or
field). Fix the query, keep the header's adaptation list current, and bump
`extractor_version()` if extraction output changes.
