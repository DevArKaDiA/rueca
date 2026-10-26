# Tools and CLI

This is the tool contract of Rueca since RFC 0005 ([responses for
agents](rfcs/0005-agent-responses.md)), with the organization scope of RFC
0006 ([the organization graph](rfcs/0006-org-graph.md), phase 1). The MCP
server, the CLI and the benchmark all use it. The alpha shapes (v0.1.0-alpha) stay available for one
release with `"format": "verbose"` ([below](#verbose-alpha-format)).

## Common conventions

- **Lines are 1-based** in every tool result.
- **Arguments every tool takes:**

  | Arg | Type | Default |
  |---|---|---|
  | `repo` | a repository id (`github.com/o/r`, `local/<dir>`) or indexed root path; a list of them; or `"*"` for every indexed repository | organization mode (`--org`): the whole organization (for `get_symbol`, `get_snippet`, `trace`: the repository of an `id` argument); otherwise the server's `--root`, else the repository of an `id` argument, else the only indexed root, else an error listing indexed roots |
  | `ref` | branch name, `HEAD`, `worktree`, or a commit (full, or a unique prefix of ≥ 7 hex digits) | the working tree |
  | `budget_tokens` | int, 100–1,000,000 | 2000 |
  | `cursor` | string: the `next_cursor` of a previous response | — |
  | `format` | `compact` or `verbose` | `compact` |

- **Refs in local mode.** Each indexed root is one snapshot: its working tree,
  i.e. the checked-out branch (`main`; `HEAD` when detached; `worktree`
  outside git) plus uncommitted changes. `ref` accepts that branch, `HEAD`,
  `worktree` or the commit; any other ref is an error that lists the indexed
  refs of the repository (index another worktree to read another branch).
  With `repo: "*"`, snapshots that do not have the ref are skipped. The
  service and org modes will hold several snapshots per repository
  (branches, `pr/<n>`, tags) under the same argument.
- **Symbol ids** are pinned: `<repo>@<commit12>[+]:<path>#<local-qualified-name>`
  (`rueca_model::symbol_id`), with the first 12 hex digits of the commit and
  `+` when the indexed working tree has uncommitted changes (the overlay):
  `github.com/acme/billing@7696364aef89+:src/invoices/service.py#InvoiceService.issue`.
  Outside git the commit is `worktree`. Tools accept the full commit, any
  prefix of ≥ 7 hex digits, the 12-digit form (with or without `+`) or a ref
  name (`@main`). An id whose commit is no longer indexed resolves against
  the repository's current snapshot (local mode), so ids survive a re-index.
  For PHP the local qualified name is the fully qualified name
  (`app/Models/User.php#App\Models\User::save`); for C/C++ too
  (`src/foo.cpp#ns::Foo::bar`). Canonical env vars have no file and use the
  pseudo-path `$env`: `local/shop@abc:$env#DATABASE_URL`. Files have no `#`
  part. Tools accept an `id`, a global `qualified_name`, or a suffix of it
  (`Session.request`, `User::save`, `server.port`, `["DATABASE_URL"]`), in
  that order of precedence. For Java and C/C++ an overload's parameter
  suffix is optional: `Parser.parse` matches `Parser.parse(String)` and
  `Parser.parse(Reader)` (one is returned, the others as `candidates`).
- **Qualified names**: Python `pkg.mod.Class.method`; ECMAScript
  `src/util/index:Class.method` (module path, colon, local name); PHP
  `App\Models\User::save` (namespace, backslash, local name); Markdown
  `docs/guide.md:Install > Linux` (path, colon, heading chain); structured
  data `config/app.yaml:database.host` (path, colon, key path; quoted
  segments `paths."/v1/users.{format}".get`, list elements `steps[2]`,
  identity-keyed elements `env["DATABASE_URL"]`, documents of a YAML stream
  `{1}.spec`, Kubernetes objects `k8s/web.yaml:Deployment/web.spec.replicas`,
  Terraform `infra/main.tf:aws_s3_bucket.logs`, `var.region`); Laravel
  config keys of `config/<file>.php` are PHP definitions, so their name is
  the key path alone (`name`, id `config/app.php#name`); canonical env vars
  `$env:DATABASE_URL`; Go `github.com/gin-gonic/gin.Engine.ServeHTTP`
  (package import path, dot, local name; methods are `Type.Method`, the
  symbol id is `gin.go#Engine.ServeHTTP`); Java
  `com.example.Owner.getPets` (package, dot, local name; nested types
  `Owner.Pet.getName`; constructors named like their class, `Owner.Owner`;
  overloads carry their parameter types, `Parser.parse(String)`,
  `Parser.parse(String,int)`; anonymous classes are named after their base
  under the enclosing definition, `Gson.fromJson.TypeAdapter$1.read`); C/C++ `geo::Circle::area` (namespace and
  class path, no module; overloads in one file carry their parameter types,
  `geo::scale(double, double)`; macros `GEO_PI`; GoogleTest `Suite::Name`).
- **Errors**: MCP tool errors use `isError: true` with a message; the CLI
  prints `{"error": "..."}` to stdout and exits with status 2.

## The envelope

Every response (default `format: compact`) is:

```json
{
  "snapshots": [
    {"s": 0, "repo": "github.com/acme/billing", "ref": "main",
     "commit": "7696364aef89a22e4a2fe84f6fd23e186bef9aa8", "overlay": true,
     "url": "https://github.com/acme/billing/blob/7696364aef89a22e4a2fe84f6fd23e186bef9aa8/{path}#L{start}-L{end}"}
  ],
  "results": "…tool-specific…",
  "truncated": false,
  "next_cursor": null,
  "budget": {"limit_tokens": 2000, "used_tokens": 312}
}
```

- `snapshots`: the snapshots the answer touched (RFC 0006 §6): a search
  over several repositories lists only those its items come from (none for
  an empty answer); a request on one snapshot always lists it. Items refer to one by `s`
  (an index valid within the response); their ids stay fully pinned, so an
  id copied out of a response is valid on its own. `commit` is the full id
  (`worktree` outside git); `overlay: true` means the working tree had
  uncommitted changes when indexed (ids carry `+`). `url` is a template for
  links on the repository's host (`{path}`, `{start}`, `{end}`), present for
  `github.com`, `gitlab.com`, `bitbucket.org` and hosts named `github.*` /
  `gitlab.*`; with an overlay it points at the base commit, whose lines may
  differ in modified files.
- `truncated`: the results stop before the end, because of a limit or the
  budget. `next_cursor`: pass it as `cursor` (with the same other
  arguments) to continue; `null` when there is nothing more or the tool
  cannot continue (`get_symbol`).
- `budget`: `limit_tokens` is `budget_tokens`; `used_tokens` is the size of
  the **text rendering** (below) as `ceil(bytes / 4)` — a model-neutral
  estimate, no tokenizer (RFC 0005 open question 1). A response over budget
  keeps the longest prefix of its items that fits, and at least one item, so
  a cursor always makes progress (a single huge item can exceed the budget).
- Fields absent from an item mean `null` / unknown (`lines` of a node
  without a span, `sig`, `doc`, `lang`).

**Brief** (inside results): `{"id", "qn", "label", "lines": [start, end]}`
(`qn` is the global qualified name; the path is in the id).

**Text rendering.** MCP results carry the envelope in `structuredContent`
and a compact text rendering as text content; the CLI prints the JSON, or
the text with `--text`. One header line per snapshot
(`repo@commit12[+] (ref)`, prefixed by `[s]` when there are several), then
one line per group (file path) and item. Items show the **local** name (the
`#` part of the id), so `<header repo@commit>:<path>#<name>` is the item's
id, and the name also works as a `qualified_name` argument:

```
github.com/acme/billing@7696364aef89+ (main)
src/invoices/service.py
  Method InvoiceService.issue L41-63  def issue(self, invoice_id: str) -> Invoice
  Method InvoiceService.cancel L65-80  def cancel(self, invoice_id: str) -> None
(truncated; continue with "cursor": "10")
```

**Several repositories.** With `repo` a list or `"*"` (or omitted in
organization mode), `search_symbols` returns **one ranking**: BM25
statistics are those of every selected repository together, so scores are
comparable and the order is the one a single index would give (RFC 0006 §6;
the rank fusion of RFC 0005 §5 is gone). In organization mode the ranking
comes from the organization symbol index; locally indexed roots without an
organization are scored with summed statistics, which gives the same
scores. `search_code` lists matches in snapshot (repository) order.
`get_symbol`, `get_snippet` and `trace` resolve an `id` in its own
snapshot, and a name in every selected snapshot (the best match wins; the
best match of each other snapshot is listed in `candidates`).
`get_file_outline` and path snippets need the path to exist in exactly one
selected snapshot.

**Organization mode** (`rueca mcp --org`, `rueca query --org`; RFC 0006).
The server reads one organization: the repositories of its current
generation (a call reads one generation even while the organization is
updated). `repo` omitted means the whole organization; one repository or a
list narrows it — the filter is applied inside the organization index
before top-k, with organization-wide statistics, so a repository's results
score the same whether it is searched alone or with the others. Callers see
only the repositories they may read: the visible set is applied before
top-k, before counts and before BM25 statistics, and a repository that is
not visible is reported exactly like one that does not exist (local mode
sees everything). `list_repositories` lists the organization.

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
| `limit` | int, 1–200 | 10 |
| `offset` | int (`cursor` takes precedence) | 0 |
| `scores` | bool: include each result's score (3 significant digits) | false |

Results, grouped by snapshot and file in rank order (a group sits where its
best result ranks):

```json
"results": [
  {"s": 0, "path": "src/invoices/service.py", "symbols": [
    {"id": "github.com/acme/billing@7696364aef89+:src/invoices/service.py#InvoiceService.issue",
     "name": "issue", "qn": "billing.invoices.service.InvoiceService.issue", "label": "Method",
     "lines": [41, 63], "sig": "def issue(self, invoice_id: str) -> Invoice"}]}
]
```

`next_cursor` is the offset of the next result.

**Language names** (`search_symbols`, `search_code`): `python`,
`typescript`, `tsx`, `javascript`, `php`, `markdown`, `go`, `java`, `cpp`, `c` (a `.h` header is `cpp` when the
repository has C++ sources), the data languages
`json` (JSONC included), `yaml`, `toml`, `dotenv`, `ini`, `properties`,
`xml` (`pom.xml`, `*.csproj`, `AndroidManifest.xml` only), `hcl`
(Terraform / OpenTofu included) and `dockerfile`. A language whose pack is not in
this build is accepted and matches nothing (its files are
content-searchable without a language filter).

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
| `limit` | int (matches), 1–500 | 20 |
| `context_lines` | int, 0–10 | 0 |

Results: `[ {"s", "path", "matches": [ {"line", "text", "before" (opt), "after" (opt), "symbol" (opt brief: innermost enclosing definition)} ]} ]`,
plus `"files_scanned"` next to `next_cursor`. Text:

```
src/http.py
  [Method Session.request L3-9]
  4- def request(self, method, url):
  5:     return self.send(method, url)
```

`next_cursor` is the number of matches already returned.

### `get_symbol`

| Arg | Type | Default |
|---|---|---|
| `id` / `qualified_name` / `name` | string, one required | — |
| `path` | string, narrows `name` lookup | — |

Results:

```json
{
  "s": 0,
  "symbol": {"id", "name", "qn", "label", "path", "lines", "lang", "sig", "doc", "exported", "test"},
  "parent": brief | null,
  "children": [brief],
  "bases": [brief],          // INHERITS / IMPLEMENTS targets
  "subtypes": [brief],       // reverse INHERITS / IMPLEMENTS
  "callers": int,            // distinct nodes over incoming CALLS
  "callees": int,
  "readers": int,            // optional: EnvVar and Key only (distinct incoming READS)
  "writers": int,            // optional: EnvVar and Key only (distinct incoming WRITES)
  "candidates": [brief]      // other matches when the lookup was ambiguous
}
```

`readers` / `writers` answer "is this env var / config key used, and is it
set anywhere?" before a `trace`. A canonical env var (`$env:NAME`) has no
`path` or `lines`. Over budget, the lists are cut (children, then bases,
subtypes, candidates), `truncated` is set and `"omitted": {"children": n, …}`
says what was left out; there is no cursor (use `get_file_outline` or
`trace` for the rest).

### `get_snippet`

| Arg | Type | Default |
|---|---|---|
| `id` / `qualified_name` / `name` | string | — |
| or `path` + `start_line` (+ `end_line`) | | |
| `context_lines` | int, 0–50 | 0 |
| `max_lines` | int, 1–2000 | 300 |

Results: `{"s", "path", "lang", "lines": [start, end], "text", "symbol" (opt brief)}`.
A snippet cut by `max_lines` or the budget is `truncated`; `next_cursor` is
the next line, and the same call with that `cursor` continues there.

### `trace`

Call graph traversal.

| Arg | Type | Default |
|---|---|---|
| `id` / `qualified_name` / `name` | string, one required | — |
| `direction` | `callers` / `callees` / `both` | `both` |
| `depth` | int, 1–10 | 1 |
| `edge_types` | array of edge kinds | `["CALLS"]` |
| `min_confidence` | float 0–1 | 0.0 |
| `limit` | int (nodes), 1–2000 | 50 |
| `labels` | array of label names: only neighbours with these labels are reported and traversed | all labels |
| `include_tests` | bool: report and traverse tests (`is_test` or label `Test`) | false, or true when `edge_types` contains `TESTS` |

Results:

```json
{
  "s": 0,
  "nodes": [
    {"id": "…#InvoiceService.issue", "qn": "…", "label": "Method", "path": "src/invoices/service.py", "lines": [41, 63], "d": 0},
    {"id": "…#Api.issue", "qn": "…", "label": "Method", "path": "src/api.py", "lines": [7, 8], "d": 1, "dir": "in"}
  ],
  "edges": [[1, 0, "CALLS", 0.9, "type_resolved", 8]],
  "tests_excluded": 2        // distinct tests left out because include_tests is false
}
```

Node 0 is the root (`d` 0, never filtered); the others are ordered by
(depth, direction, qualified_name), each listed once with `dir` `in`
(caller), `out` (callee) or `both`, and `d` its smallest depth. An edge is
`[src, dst, kind, confidence, strategy, call_site_line]` with node indexes,
plus a seventh element, the call-site path, when it is not the source
node's file (`call_site_line` is `null` without evidence). Text:

```
github.com/acme/billing@7696364aef89+ (main)
0 Method InvoiceService.issue  src/invoices/service.py L41-63
1 in d1 Method Api.issue  src/api.py L7-8  →0 CALLS 0.9 type_resolved @8
2 out d1 Function validate  src/invoices/rules.py L3-9  ←0 CALLS 0.95 import_resolved @44
(2 tests excluded; `include_tests: true` lists them)
```

Each edge is printed on its later endpoint: `→j` this node calls node `j`,
`←j` node `j` calls this node. `next_cursor` is the number of non-root
nodes already returned; a later page repeats the root as node 0 and any
earlier node an edge needs.

Edge types beyond calls: `["INHERITS"]` walks a class hierarchy,
`["IMPORTS"]` file dependencies (`File` → `File`, and `File` → `Folder` for
imports of a directory: a Go or Java package, a Terraform module), `["TESTS"]`
the tests of a function, `["OVERRIDES"]` the methods an override replaces
(callers: the overrides of a method; Java and C/C++, and Go methods that
implicitly implement an interface method, heuristic 0.5). Go types also
get heuristic `IMPLEMENTS` edges to the interfaces whose methods they
cover, so `get_symbol` `subtypes` of a Go interface lists its implementers. The code ↔ config
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

Results: `{"s", "path", "lang", "line_count", "symbols": [ {"id", "name", "qn", "label", "lines", "sig", "children" (opt): [ … ]} ] }`
(nested by containment, ordered by line). `next_cursor` is the number of
symbols already returned in pre-order; a later page nests what it can and
lists the rest at the top level.

### `list_repositories`

The repositories this server can read, sorted by id: in organization mode
every visible repository of the organization, otherwise every indexed root
(`repo` narrows it, `ref` filters snapshots).

Results: `[{"s", "repo", "default_ref", "refs": [ref, …], "languages": {"python": files, …}, "files", "symbols", "edges"}]`
(`s` is the snapshot of the default ref; `symbols` counts nodes that are not
files, folders, modules or packages), plus `"org"` (the organization name,
or `null`) next to `next_cursor`. `next_cursor` is the number of
repositories already returned. Verbose: `{"org", "repositories": [{"repo",
"root", "default_ref", "commit", "refs", "languages", "files", "symbols",
"edges"}]}`. Text:

```
[0] github.com/acme/billing@7696364aef89 (main)
[1] github.com/acme/payments@a1b2c3d4e5f6 (main)
[0] github.com/acme/billing  refs main  120 files, 3400 symbols, 9100 edges  python 110, yaml 10
[1] github.com/acme/payments  refs main  80 files, 2100 symbols, 5300 edges  typescript 78, json 2
```

## CLI

```
rueca index [PATH] [--store DIR] [--repo ID] [--json]
    Index the working tree at PATH (default: current dir). Prints a summary
    (or the IndexReport as JSON). Organizations holding PATH are updated
    (only its part; JSON: "orgs").

rueca index --org FILE [PATH] [--store DIR] [--json]
    Index every repository of the organization file (or only the one
    containing PATH), then build or update the organization layer. JSON:
    {"org", "indexed": [IndexReport], "update", "repos_ms", "org_ms",
    "duration_ms"}.

rueca query <TOOL> '<JSON ARGS>' [--root PATH | --org FILE|NAME] [--store DIR] [--pretty] [--text]
    Run one tool and print its JSON result (with --text, the compact text
    rendering MCP clients get as text content). Same semantics as MCP.

rueca mcp [--root PATH | --org FILE|NAME] [--store DIR] [--no-auto-index]
    MCP server on stdio. Indexes --root first if it has no segment; with
    --org FILE, builds the organization first if the store has none.

rueca status [--store DIR] [--json]
    List indexed roots with repo, commit, node/edge/file counts, age, and
    the organizations (generation, repositories).
```

**Organization file** (`rueca-org.toml`, RFC 0006 §7):

```toml
name = "acme"                      # ASCII letters, digits, `-`, `_`, `.`

[[repo]]
path = "~/src/billing"             # `~` and paths relative to this file

[[repo]]
url = "https://github.com/acme/payments"   # cloned under <store>/orgs/acme/clones/ with `git`
id = "github.com/acme/payments"            # optional repository id (like `index --repo`)
```

The organization lives in `<store>/orgs/<name>/` ([how it
works](how-it-works.md#46-the-organization-layer-rueca-segment-org)).
Repository ids must be unique within an organization.

Store default: `$RUECA_HOME`, else `~/.cache/rueca`.

## Verbose (alpha) format

`"format": "verbose"` returns the v0.1.0-alpha result shapes and defaults
unchanged, for one release (the next minor removes it): no envelope, long
field names, `search_symbols` limit 20, `search_code` 50, `trace` depth 2 and
200 nodes, no budget. Ids are the pinned RFC 0005 form in both formats;
`repo`, `ref` and `cursor` work in both. The MCP text content of a verbose
result is the compact JSON.

- **Brief**: `{ "id", "name", "qualified_name", "label", "path", "start_line", "end_line" }`
  (`path`, `start_line`, `end_line` are `null` for nodes without a file or
  span).
- `search_symbols`: `{ "results": [ brief + "signature" (opt) + "score" ], "truncated", "next_offset" }`.
- `search_code`: `{ "matches": [ { "path", "line", "text", "before" (opt), "after" (opt), "symbol" (opt brief) } ], "truncated", "files_scanned" }`
- `get_symbol`: `{ "symbol": { brief…, "language", "signature", "doc", "exported", "is_test" }, "parent", "children", "bases", "subtypes", "callers_count", "callees_count", "readers_count" (opt), "writers_count" (opt), "candidates" }`
- `get_snippet`: `{ "path", "language", "start_line", "end_line", "text", "truncated", "symbol" (opt brief) }`
- `trace`: `{ "root": brief, "nodes": [ brief + { "depth", "direction": "callers" | "callees" } ], "edges": [ { "src": id, "dst": id, "kind", "confidence", "strategy", "call_site": { "path", "line" } | null } ], "truncated", "tests_excluded" }`
  (`nodes` ordered by (depth, direction, qualified_name); a node reachable
  both ways appears once per direction; the root is not repeated).
- `get_file_outline`: `{ "path", "language", "line_count", "symbols": [ brief + "signature" + "children": [ … ] ] }`
