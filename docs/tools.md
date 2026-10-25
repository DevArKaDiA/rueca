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
  (`rueca_model::symbol_id`). Tools accept an `id`, a global
  `qualified_name`, or a suffix of it (`Session.request`), in that order of
  precedence.
- **Qualified names**: Python `pkg.mod.Class.method`; ECMAScript
  `src/util/index:Class.method` (module path, colon, local name); PHP
  `App\Models\User::save` (namespace, backslash, local name); Markdown
  `docs/guide.md:Install > Linux` (path, colon, heading chain).
- **Brief symbol** (used inside results):
  `{ "id", "name", "qualified_name", "label", "path", "start_line", "end_line" }`
- **Truncation**: list results include `"truncated": bool`; paginated ones
  include `"next_offset"` (integer or null).
- **Errors**: MCP tool errors use `isError: true` with a message; the CLI
  prints `{"error": "..."}` to stdout and exits with status 2.

## Tools

### `search_symbols`

BM25 search over symbol names, qualified names, signatures, docstrings and
paths, with camelCase / snake_case / path splitting.

| Arg | Type | Default |
|---|---|---|
| `query` | string, required | — |
| `labels` | array of label names (`Function`, `Method`, `Class`, …) | all symbol labels |
| `path_glob` | string | — |
| `language` | `python` / `typescript` / `tsx` / `javascript` / `php` / `markdown` | — |
| `limit` | int, 1–200 | 20 |
| `offset` | int | 0 |

Result: `{ "results": [ brief + "signature" (opt) + "score" ], "truncated", "next_offset" }`

### `search_code`

Literal or regex search over file contents (from content packs).

| Arg | Type | Default |
|---|---|---|
| `pattern` | string, required | — |
| `regex` | bool | false |
| `case_insensitive` | bool | false |
| `path_glob` | string | — |
| `language` | string | — |
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
  "candidates": [brief]      // other matches when the lookup was ambiguous
}
```

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

Result:

```json
{
  "root": brief,
  "nodes": [ brief + { "depth": int, "direction": "callers" | "callees" } ],
  "edges": [ { "src": id, "dst": id, "kind", "confidence", "strategy",
               "call_site": { "path", "line" } | null } ],
  "truncated": bool
}
```

`nodes` is ordered by (depth, direction, qualified_name); the root is not
repeated in `nodes`.

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
