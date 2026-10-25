# Tools and CLI (alpha contract)

This is the contract for v0.1.0-alpha. The MCP server, the CLI and the
benchmark all use it. Results are JSON objects; every field listed is present
unless marked optional.

## Common conventions

- **Lines are 1-based** in every tool result.
- **`repo`** (optional in every tool): an indexed working-tree root path or a
  repository id (`local/<dir>`, `github.com/o/r`). Default: the server's
  `--root`, else the only indexed root, else an error listing indexed roots.
- **Symbol IDs**: `<repo>@<commit>:<path>#<local-qualified-name>`
  (`rueca_model::symbol_id`). For PHP the local qualified name is the fully
  qualified name (`app/Models/User.php#App\Models\User::save`); for C/C++
  too (`src/foo.cpp#ns::Foo::bar`). Canonical env vars have no file and use
  the pseudo-path `$env`: `local/shop@abc:$env#DATABASE_URL`. Tools
  accept an `id`, a global `qualified_name`, or a suffix of it
  (`Session.request`, `User::save`, `server.port`, `["DATABASE_URL"]`), in
  that order of precedence. For Java and C/C++ an overload's parameter
  suffix is optional: `Parser.parse` matches `Parser.parse(String)` and
  `Parser.parse(Reader)` (one is returned, the others as `candidates`).
- **Qualified names**: Python `pkg.mod.Class.method`; ECMAScript
  `src/util/index:Class.method` (module path, colon, local name); PHP
  `App\Models\User::save` (namespace, backslash, local name); Markdown
  `docs/guide.md:Install > Linux` (path, colon, heading chain). Planned for
  the RFC 0003 packs (the model and the tools already accept them):
  structured data `config/app.yaml:database.host` (path, colon, key path;
  quoted segments `paths."/v1/users".get`, list elements `steps[2]`,
  identity-keyed elements `env["DATABASE_URL"]`), Go
  `github.com/gin-gonic/gin.Engine.ServeHTTP` (import path, dot, local
  name), Java `com.example.Owner.getPets` (package, dot, local name;
  overloads `Parser.parse(String)`), C/C++ `fmt::v11::format_to`
  (namespace and class path, no module), canonical env vars
  `$env:DATABASE_URL`.
- **Brief symbol** (used inside results):
  `{ "id", "name", "qualified_name", "label", "path", "start_line", "end_line" }`
- **Truncation**: list results include `"truncated": bool`; paginated ones
  include `"next_offset"` (integer or null).
- **Errors**: MCP tool errors use `isError: true` with a message; the CLI
  prints `{"error": "..."}` to stdout and exits with status 2.

## Tools

### `search_symbols`

BM25 search over symbol names, qualified names, signatures, docstrings,
paths and each symbol's source code, with camelCase / snake_case / path
splitting. A one-word query is a name lookup (exact names first); dotted
parts (`Session.request`, `util:formatAmount`) and adjacent word pairs
(`session request`) match qualified-name suffixes; multi-word queries also
match the code itself. Fields, variables, constants and tests rank below
other definitions with the same text score. Ranking details:
[how-it-works §6](how-it-works.md#6-how-each-tool-uses-the-graph).

| Arg | Type | Default |
|---|---|---|
| `query` | string, required | — |
| `labels` | array of label names (`Function`, `Method`, `Class`, `Section`, `EnvVar`, `Key`, …) | all symbol labels except `File`, `Folder`, `Module`, `Repository`, and except `Key` / `Document` unless `language` is a data language |
| `path_glob` | string | — |
| `language` | one language name (see below) | — |
| `limit` | int, 1–200 | 20 |
| `offset` | int | 0 |

Result: `{ "results": [ brief + "signature" (opt) + "score" ], "truncated", "next_offset" }`

**Language names** (`search_symbols`, `search_code`): `python`,
`typescript`, `tsx`, `javascript`, `php`, `markdown`; and, for the packs
RFC 0003 adds, the data languages `json`, `yaml`, `toml`, `dotenv`, `ini`,
`properties`, `xml`, `hcl`, `dockerfile` and the code languages `go`,
`cpp`, `c`, `java`. A language whose pack is not in this build is accepted
and matches nothing (its files are content-searchable without a language
filter).

Structured-data keys (`Key`) and multi-document YAML documents
(`Document`) are left out of the default label set so configuration does
not crowd code out of results; name them in `labels` or pass a data
`language` to search them. `EnvVar`, `Resource`, `Image` and `Route` are
in the default set.

### `search_code`

Literal or regex search over file contents (from content packs).

| Arg | Type | Default |
|---|---|---|
| `pattern` | string, required | — |
| `regex` | bool | false |
| `case_insensitive` | bool | false |
| `path_glob` | string | — |
| `language` | one language name (as `search_symbols`) | — |
| `limit` | int (matches), 1–500 | 50 |
| `context_lines` | int, 0–10 | 0 |

Result: `{ "matches": [ { "path", "line", "text", "before" (opt), "after" (opt), "symbol" (opt brief: innermost enclosing definition) } ], "truncated", "files_scanned" }`

### `get_symbol`

| Arg | Type | Default |
|---|---|---|
| `id` / `qualified_name` / `name` | string, one required | — |
| `path` | string, narrows `name` lookup | — |

Result:

```json
{
  "symbol": { brief…, "language", "signature", "doc", "exported", "is_test" },
  "parent": brief | null,
  "children": [brief],
  "bases": [brief],          // INHERITS / IMPLEMENTS targets
  "subtypes": [brief],       // reverse INHERITS / IMPLEMENTS
  "callers_count": int,
  "callees_count": int,
  "readers_count": int,      // optional: EnvVar and Key only (distinct incoming READS)
  "writers_count": int,      // optional: EnvVar and Key only (distinct incoming WRITES)
  "candidates": [brief]      // other matches when the lookup was ambiguous
}
```

`readers_count` / `writers_count` answer "is this env var / config key
used, and is it set anywhere?" before a `trace`. A canonical env var
(`$env:NAME`) has `path`, `start_line` and `end_line` `null`.

### `get_snippet`

| Arg | Type | Default |
|---|---|---|
| `id` / `qualified_name` / `name` | string | — |
| or `path` + `start_line` (+ `end_line`) | | |
| `context_lines` | int, 0–50 | 0 |
| `max_lines` | int, 1–2000 | 300 |

Result: `{ "path", "language", "start_line", "end_line", "text", "truncated", "symbol" (opt brief) }`

### `trace`

Call graph traversal.

| Arg | Type | Default |
|---|---|---|
| `id` / `qualified_name` / `name` | string, one required | — |
| `direction` | `callers` / `callees` / `both` | `both` |
| `depth` | int, 1–10 | 2 |
| `edge_types` | array of edge kinds | `["CALLS"]` |
| `min_confidence` | float 0–1 | 0.0 |
| `limit` | int (nodes), 1–2000 | 200 |
| `labels` | array of label names: only neighbours with these labels are reported and traversed | all labels |
| `include_tests` | bool: report and traverse tests (`is_test` or label `Test`) | false, or true when `edge_types` contains `TESTS` |

Result:

```json
{
  "root": brief,
  "nodes": [ brief + { "depth": int, "direction": "callers" | "callees" } ],
  "edges": [ { "src": id, "dst": id, "kind", "confidence", "strategy",
               "call_site": { "path", "line" } | null } ],
  "truncated": bool,
  "tests_excluded": int      // distinct tests left out because include_tests is false
}
```

`nodes` is ordered by (depth, direction, qualified_name); the root is not
repeated in `nodes` and is never filtered.

Edge types beyond calls: `["INHERITS"]` walks a class hierarchy,
`["IMPORTS"]` file dependencies (`File` → `File`, and `File` → `Folder` for
imports of a directory: a Go or Java package, a Terraform module), `["TESTS"]`
the tests of a function, `["OVERRIDES"]` the methods an override replaces
(callers: the overrides of a method; Java and C/C++). The code ↔ config
bridge (RFC 0003 §7) uses `READS` / `WRITES`:

```json
{"qualified_name": "$env:DATABASE_URL", "direction": "callers", "edge_types": ["READS", "WRITES"]}
```

lists the code that reads the variable and the manifests that set it
(adding `CALLS` continues to the callers of the readers);
`{"qualified_name": "app.main", "direction": "callees", "edge_types": ["READS"]}`
lists the env vars and config keys a function depends on. Bridge edges have
strategy `contract` (0.90–0.95), `unique_name` (0.60) or `syntactic` (1.0,
a manifest setting a variable). Neighbour labels other than
functions and methods are real call sites: a `File` caller is module-level
code, a `Class` / `Variable` / `Constant` / `Field` caller is code in a class
body or initializer, and a `Class` callee is a constructor call (its
`__init__` / `constructor` is listed as well).

### `get_file_outline`

| Arg | Type | Default |
|---|---|---|
| `path` | string, required (repository-relative) | — |

Result: `{ "path", "language", "line_count", "symbols": [ brief + "signature" + "children": [ … ] ] }`
(nested by containment, ordered by line).

## CLI

```
rueca index [PATH] [--store DIR] [--repo ID] [--json]
    Index the working tree at PATH (default: current dir). Prints a summary
    (or the IndexReport as JSON).

rueca query <TOOL> '<JSON ARGS>' [--root PATH] [--store DIR]
    Run one tool and print its JSON result. Same semantics as MCP.

rueca mcp [--root PATH] [--store DIR] [--no-auto-index]
    MCP server on stdio. Indexes --root first if it has no segment.

rueca status [--store DIR] [--json]
    List indexed roots with repo, commit, node/edge/file counts, age.
```

Store default: `$RUECA_HOME`, else `~/.cache/rueca`.
