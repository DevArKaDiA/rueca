# Vendored tree-sitter queries — `rueca-lang-shell`

Queries in this directory are compiled at runtime against
`tree-sitter-bash` 0.25.1 (`LANGUAGE`, tree-sitter runtime 0.27). Each file
starts with an SPDX header, its upstream URL and commit, and the list of
Rueca adaptations. `NOTICE` at the repository root carries the
attributions.

| File | Upstream | Commit | License | Use |
|---|---|---|---|---|
| `locals.scm` | [nvim-treesitter `runtime/queries/bash/locals.scm`](https://github.com/nvim-treesitter/nvim-treesitter/blob/29212d826054bb2948fa9d3a99eca9bf6a050eb2/runtime/queries/bash/locals.scm) | `29212d826054bb2948fa9d3a99eca9bf6a050eb2` | Apache-2.0 | Variable bindings and function scopes |

Sources considered and not used:

- Upstream `tree-sitter-bash` (MIT) ships only `queries/highlights.scm`:
  no locals, no tags.
- bloop has no shell scope query. Zed (GPL-3.0) and Helix (MPL-2.0)
  queries: excluded by RFC 0002 §2.

## What the query is for

A shell expansion `$NAME` reads either a variable the script itself sets
(an assignment anywhere in the file, `local` / `declare` / `export`
without a value, a `for` variable, or one of the binding commands below)
or a value the process inherited — an environment variable. The table
built from this query (`src/locals.rs`) answers three questions for the
extractor:

1. **Is `NAME` bound anywhere in the file?** If not, and the name is
   upper case and not maintained by the shell itself (`BASH_SOURCE`,
   `LINENO`, `OPTARG`, …), `$NAME` is an environment read (`EnvVar`
   reference, RFC 0003 §7.2).
2. **Is `NAME` assigned at file level?** Then it is a `Variable` (or
   `Constant` for `readonly` / `declare -r`) definition, and `$NAME` in a
   function is a `Read` reference to it.
3. **Is `NAME` local to the enclosing function** (`local x`, or
   `declare` / `typeset` without `-g` inside a function)? Then `$NAME`
   there is neither.

Commands that bind names (not syntax, so not in the query): `read [-opts]
NAME…`, `getopts SPEC NAME`, `mapfile` / `readarray [-opts] NAME`,
`printf -v NAME`, `select NAME in …` (parsed as a command by the
grammar).

## Capture contract (what `src/locals.rs` relies on)

| Capture | Meaning |
|---|---|
| `@local.scope` | a `function_definition` |
| `@local.definition.var` | a variable name bound by an assignment, a declaration command or a `for` loop |

## Upgrading the grammar

`cargo test -p rueca-lang-shell locals` fails when the query no longer
compiles, when a required capture name disappears, or when any pattern
stops matching the corpus in `src/locals.rs` (a renamed node or field).
Fix the query, keep the header's adaptation list current, and bump
`extractor_version()` if extraction output changes.
