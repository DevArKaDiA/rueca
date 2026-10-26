# Vendored tree-sitter queries — `rueca-lang-cpp`

Queries in this directory are compiled at runtime: `c-locals.scm` against
`tree-sitter-c` 0.24.2 (`LANGUAGE`), and `c-locals.scm` followed by
`cpp-locals.scm` against `tree-sitter-cpp` 0.23.4 (`LANGUAGE`), both on the
tree-sitter 0.27 runtime. Each file starts with an SPDX header, its
upstream URL and commit, and the list of Rueca adaptations. `NOTICE` at the
repository root carries the attributions.

| File | Upstream | Commit | License | Use |
|---|---|---|---|---|
| `c-locals.scm` | [nvim-treesitter `runtime/queries/c/locals.scm`](https://github.com/nvim-treesitter/nvim-treesitter/blob/29212d826054bb2948fa9d3a99eca9bf6a050eb2/runtime/queries/c/locals.scm) | `29212d826054bb2948fa9d3a99eca9bf6a050eb2` | Apache-2.0 | Block scopes, parameter and variable bindings (C, and the base of C++) |
| `cpp-locals.scm` | [nvim-treesitter `runtime/queries/cpp/locals.scm`](https://github.com/nvim-treesitter/nvim-treesitter/blob/29212d826054bb2948fa9d3a99eca9bf6a050eb2/runtime/queries/cpp/locals.scm) (`; inherits: c`) | `29212d826054bb2948fa9d3a99eca9bf6a050eb2` | Apache-2.0 | C++ additions: template type parameters, lambdas, `try` / `catch`, range `for`, structured bindings; Rueca's `T x(a, b);` read as a function declaration in a block, and `using std::get;` in a block |

Sources considered and not used:

- Upstream `tree-sitter-c` and `tree-sitter-cpp` (MIT) ship no
  `locals.scm`; their `queries/tags.scm` has definitions and calls but no
  containment, qualified names, receivers or namespaces, all of which
  Rueca code extracts.
- bloop and scip-syntax have no C / C++ scope queries.
- Zed (GPL-3.0) and Helix (MPL-2.0) queries: excluded by RFC 0002 §2.

## What the queries are for

The pack never reports locals as references, and it types receivers from
declarations. The locals table answers two questions (see `src/locals.rs`):
is the identifier at this position a parameter, a local variable or a
template type parameter, and which type does it declare (`Foo* p`,
`const Foo& f`, `auto f = new Foo`)? Definitions, namespaces, classes and
macros are extracted by Rueca code, so nvim's function, type, field,
namespace, macro and label captures are removed, as are the file, class
and namespace scopes (names there are definitions, not locals).

## Capture contract (what `src/locals.rs` relies on)

| Capture | Meaning |
|---|---|
| `@local.scope` | a block-like scope: function, compound statement, `for` / `if` / `while` / `do` / `switch` / `case`, range `for`, lambda, `try` body, `catch`, template, `requires` |
| `@local.definition.parameter` | a parameter's identifier |
| `@local.definition.var` | a local variable's identifier (also reference, variadic and structured-binding declarators) |
| `@local.definition.type` | a template type parameter (C++ only) |

A binding is visible from its position to the end of its innermost scope
and in nested scopes (template type parameters in their whole template);
bindings outside every scope are ignored.

## Upgrading a grammar

`cargo test -p rueca-lang-cpp locals` fails when a query no longer
compiles, when a required capture name disappears, or when any pattern
stops matching the golden corpora in `src/locals.rs` (a renamed node or
field). Fix the query, keep the header's adaptation list current, and bump
`extractor_version()` if extraction output changes.
