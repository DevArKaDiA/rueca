# Vendored tree-sitter queries — `telar-lang-python`

Queries in this directory are compiled at runtime against
`tree-sitter-python` 0.25 (tree-sitter runtime 0.27). Each file starts with
an SPDX header, its upstream URL and commit, and the list of Telar
adaptations. `NOTICE` at the repository root carries the attributions.

| File | Upstream | Commit | License | Use |
|---|---|---|---|---|
| `locals.scm` | [nvim-treesitter `runtime/queries/python/locals.scm`](https://github.com/nvim-treesitter/nvim-treesitter/blob/29212d826054bb2948fa9d3a99eca9bf6a050eb2/runtime/queries/python/locals.scm) (base) | `29212d826054bb2948fa9d3a99eca9bf6a050eb2` (`main`; file last changed in `692b051b09935653befdb8f7ba8afdb640adf17b`) | Apache-2.0 | Scopes, local bindings, references |
| `locals.scm` | [bloop `server/bleep/src/intelligence/language/python/scopes.scm`](https://github.com/BloopAI/bloop/blob/431e9e82c5a293c40f22aadb3615c3aa387af82e/server/bleep/src/intelligence/language/python/scopes.scm) (additions: lambda / generator scopes, lambda parameters, `global`) | `431e9e82c5a293c40f22aadb3615c3aa387af82e` | Apache-2.0 | same file |

Sources considered and not used:

- Sourcegraph `scip-syntax` (`docker-images/syntax-highlighter/crates/syntax-analysis/queries/python/locals.scm`
  in `sourcegraph-public-snapshot@c864f15a`, MIT): the Python file is empty
  in the public snapshot.
- Upstream `tree-sitter-python` `queries/tags.scm` (MIT): definitions and
  calls are already extracted by Telar code with containment, qualified
  names and receivers, which `tags.scm` lacks.
- Zed (GPL-3.0) and Helix (MPL-2.0) queries: excluded by RFC 0002 §2.

## Capture contract (what `src/locals.rs` relies on)

| Capture | Meaning |
|---|---|
| `@local.scope` | module, class, function, lambda, list/set/dict comprehension, generator expression |
| `@local.definition.<kind>` | a binding; the captured node may be a whole target pattern (every identifier inside it is bound, except under attribute / subscript). Kinds: `function`, `type` (bind in the parent scope), `type_parameter`, `parameter`, `var`, `iter`, `alias`, `walrus`, `case`, `import`; unknown kinds are treated as `var` |
| `@local.global`, `@local.nonlocal` | names declared `global` / `nonlocal` (Telar extension) |
| `@local.reference` | every identifier use |

## Upgrading the grammar

`cargo test -p telar-lang-python locals` fails when the query no longer
compiles, when a required capture name disappears, or when any pattern stops
matching the golden corpus in `src/locals.rs` (a renamed node or field).
Fix the query, keep the header's adaptation list current, and bump
`extractor_version()` if extraction output changes.
