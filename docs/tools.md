# Tools and CLI

This is the tool contract of Rueca since RFC 0005 ([responses for
agents](rfcs/0005-agent-responses.md)), with the organization scope of RFC
0006 ([the organization graph](rfcs/0006-org-graph.md), phases 1–4: one
ranking, cross-repository edges, refs, contracts). The MCP
server, the CLI and the benchmark all use it. The alpha shapes (v0.1.0-alpha) stay available for one
release with `"format": "verbose"` ([below](#verbose-alpha-format)).

## Common conventions

- **Lines are 1-based** in every tool result.
- **Arguments every tool takes:**

  | Arg | Type | Default |
  |---|---|---|
  | `repo` | a repository id (`github.com/o/r`, `local/<dir>`) or indexed root path; a list of them; or `"*"` for every indexed repository | organization mode (`--org`): the whole organization (for `get_symbol`, `get_snippet`, `trace`: the repository of an `id` argument); otherwise the server's `--root`, else the repository of an `id` argument, else the only indexed root, else an error listing indexed roots |
  | `ref` | a branch (`main`, `feature/x`), a pull request (`pr/<n>`), a tag, or a commit (full, or a unique prefix of ≥ 7 hex digits); `HEAD` / `worktree` name the default. A string, or a list to compare refs ([below](#several-refs-of-one-repository)) | the repository's default ref (local: the working tree) |
  | `budget_tokens` | int, 100–1,000,000 | 2000 |
  | `cursor` | string: the `next_cursor` of a previous response | — |
  | `format` | `compact` or `verbose` | `compact` |

- **Refs.** Every indexed ref of a repository is a snapshot of its own. The
  default is the indexed root: its working tree, i.e. the checked-out
  branch (`main`; `HEAD` when detached; `worktree` outside git) plus
  uncommitted changes. Other refs are indexed with `rueca index [PATH] --ref
  <name>` (a branch, `pr/<n>`, a tag or a commit; in an organization also
  the member's `refs = [...]` and `rueca index --org <file> <member> --ref
  <name>`) and read with `ref`, which accepts the ref's name, the default's
  branch, `HEAD`, `worktree`, or a snapshot's commit. `ref` applies to the
  repositories `repo` selects: with `repo` (one or a list), every one of
  them must have every ref named; without `repo` (organization mode) or
  with `"*"`, each repository is read at the refs it has and the others at
  their default — the organization "as of" a release branch. Pull request
  numbers are per repository: `pr/<n>` without `repo` (or with `"*"`) is
  accepted only when exactly one selected repository you can read has it;
  otherwise name `repo`. In
  organization mode the other repositories stay in the call's view at their
  default (RFC 0006 §4: `repo: A, ref: pr/7` is the organization with A
  replaced by A at `pr/7`): search statistics, routing and `trace`'s link
  edges see that view. A ref that is not indexed is an error that lists the
  indexed refs of the repository (`ref` without `repo`: of every selected
  repository) and how to index one; a repository the caller cannot read is
  never named, nor are its refs.
- **Symbol ids** are pinned: `<repo>@<commit12>[+]:<path>#<local-qualified-name>`
  (`rueca_model::symbol_id`), with the first 12 hex digits of the commit and
  `+` when the indexed working tree has uncommitted changes (the overlay):
  `github.com/acme/billing@7696364aef89+:src/invoices/service.py#InvoiceService.issue`.
  Outside git the commit is `worktree`. Tools accept the full commit, any
  prefix of ≥ 7 hex digits, the 12-digit form (with or without `+`) or a ref
  name (`@main`). An id whose commit is that of a ref snapshot reads that
  snapshot without `ref`. An id whose commit is no longer indexed resolves
  against the repository's current snapshot (its default), so ids survive a
  re-index.
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
  `geo::scale(double, double)`; macros `GEO_PI`; GoogleTest `Suite::Name`);
  Rust `rueca_index::link::PackageLinker.analyze` (module path with `::`,
  crate name first; then the local name joined with `.`: an `impl` item is
  `Type.method` after the last segment of the impl's self type, a trait
  item `Trait.method`, an item of an inline module `tests.helper`, a nested
  function `outer.inner`, a `macro_rules!` its name; the symbol id is
  `crates/rueca-index/src/link.rs#PackageLinker.analyze`).
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
  id copied out of a response is valid on its own. `ref` is the snapshot's
  ref (`main`, `pr/7`; a ref snapshot never has an overlay); `commit` is the
  full id (`worktree` outside git); `overlay: true` means the working tree had
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

An item of a repository read at several refs ends with ` [0,1]` when it is
in several snapshots and ` changed` when it is not identical in all of them
([below](#several-refs-of-one-repository)).

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
`get_file_outline` and path snippets need the path to exist in the
snapshots of one repository.

**Several refs of one repository** (a `ref` list, RFC 0005 §6). `ref:
["main", "pr/7"]` reads both snapshots of each selected repository, side by
side; the envelope lists every requested snapshot of such a repository.
Extraction is per blob, so a symbol whose file blob is the same in two refs
has identical facts: a symbol is found in each snapshot by (label,
qualified name, path), and its versions are compared by file blob.

- An item identical in several snapshots is **one item** with `"in": [s, …]`
  (snapshot indexes); its `s` and id are those of the first. A symbol whose
  file differs appears once per distinct version, each with its own `in`.
  Every item of such a repository carries `in`; `"changed": true` marks
  those not identical in every requested ref (another version, or absent:
  added or deleted on a ref). Repositories read at one snapshot get neither,
  so their items are exactly the single-ref ones.
- `search_symbols` collapses before cutting at `limit` (an identical symbol
  counts once; `next_cursor` counts collapsed items). `get_symbol` answers
  the best match's version with its `in` / `changed` and lists the other
  versions in `"versions": [{"s", "in", "changed", "symbol", "callers",
  "callees"}]` (not as candidates). `get_file_outline` is grouped per ref:
  the first listed ref's outline with its `in` / `changed`, then each other
  distinct version of the file in `"versions": [{"s", "in", "changed",
  "line_count", "symbols"}]`, the cursor counting symbols over all of them.
  `trace` walks from every version of the root and returns the **union
  graph**: a node per symbol (its `in`: the snapshots it was reached in) and
  an edge per (source, destination, kind), whose `in` (the snapshots whose
  walk found it) ends the edge array as `{"in": [s, …], "changed": true}`
  (see [`trace`](#trace)), so "who calls `f` on the PR but not on `main`"
  is one response. `search_code` lists the matches of each snapshot and
  `get_snippet` reads the first listed ref that has the file; neither
  collapses. `format: "verbose"` never collapses.
- Text: an item, node or symbol line ends with ` [0,1]` when it is in
  several snapshots and ` changed` when it is changed; an edge not found in
  every requested ref ends with ` [s…] changed`; other versions print as
  `version [s…] changed: …`, and the first outline of a grouped
  `get_file_outline` always names its snapshots (`path (python, 17 lines)
  [0] changed`):

```
[0] github.com/acme/billing@7696364aef89+ (main)
[1] github.com/acme/billing@c0ffee00c0ff (feature/x)
[0] pkg/service.py
  Function helper L13-14  def helper() changed
[1] pkg/service.py
  Function helper L14-15  def helper() changed
[0] src/util/index.ts
  Function helper L7  function helper() [0,1]
```

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
sees everything). `ref` reads repositories at other indexed refs
([Refs](#common-conventions)); `list_repositories` lists the organization
and every indexed ref.

**Repository routing** (RFC 0006 phase 2): when a `search_symbols` call
reads several repositories of an organization, the query also scores the
repositories — where its terms occur (CORI over the organization index)
and BM25 over each repository's profile (repository and package names,
descriptions and README head, most referenced symbols, path vocabulary) —
and every symbol score of a repository is multiplied by its prior
`((0.25 + r) / 1.25)²`, `r` ∈ [0, 1] being its relevance relative to the
best visible repository, each scored at the ref the call reads (a ref
routes with its own profile). The prior reorders, it never drops a result;
with one selected repository it does nothing. `list_repositories` with `query`
returns the same repository ranking with `score`s. `RUECA_ORG_ROUTE`
overrides the parameters for experiments (`off`, `prior,alpha=2,mu=0.5`,
`top,top=2`; see `rueca_segment::org::RouteParams`).

**Cross-repository edges** (the link layer, packages): an organization
built with `rueca index --org` also links each repository to the
repositories publishing the packages it imports (npm, PyPI, Go modules,
Composer, Maven / Gradle, C / C++ `include/`), resolved with the
publishing repository's own module model: `IMPORTS`, and `CALLS`,
`INHERITS`, `REFERENCES`, `DECORATES`, … through them, with the strategy and
confidence they would have inside one repository. `trace` follows them (see
below); `list_repositories` reports `packages` and `depends_on`; `rueca
links --org` lists them all. A link edge is returned only when the caller
can read both repositories.

**Contract edges** (the link layer, contracts; RFC 0006 phase 4): every
segment keeps a contract table — the HTTP routes, OpenAPI operations and
`.proto` RPC methods its code provides and the HTTP / RPC calls it makes,
each with a canonical key (`GET /users/{}`, `pkg.Service/Method`). A
repository also depends on the repositories providing a key it calls, or
publishing a spec its server code implements, and its link set gets
`strategy: "contract"` edges: the calling definition → the handler or the
spec operation (`CALLS`), the server handler → the spec operation it
implements (`IMPLEMENTS`), with the call site (or route declaration) as
evidence. Confidence follows the match: 0.9 same key, 0.85 template (a
`{}` segment matched a literal one, or a package-less RPC key), 0.7 method
wildcard, and 0.5 each when several providers match one call equally well
— counted over the caller's own repository and every other one, so a call
that a handler of its own repository and one of another repository both
match is ambiguous, and a better match in its own repository leaves no
cross-repository edge. Code handlers and spec operations are matched
separately (reaching a handler and the operation it implements is not
ambiguous); provider tests are never targets. `trace` follows them like any
link edge (`edge_types: ["CALLS", "IMPLEMENTS"]` for both kinds);
[`list_contracts`](#list_contracts) lists the tables and the calls nothing
provides; `rueca links --org` marks them `via: "contract"`. On a view that
reads a provider at another ref, an edge whose target node (the handler,
the `Route` or `RpcMethod`) the ref no longer has is dropped; a handler
kept under a changed route keeps its edges until the consumer's link set
is recomputed (remapping is by node, not by key).

## Tools

### `search_symbols`

BM25 search over symbol names, qualified names, signatures, docstrings,
paths and each symbol's source code, with camelCase / snake_case / path
splitting; when the repository was indexed with vectors (opt-in: `rueca
index --embed` or `RUECA_EMBED=1 rueca index`), fused with embedding
similarity (`mode`, below), so questions and issue text in plain words are
welcome. A one-word query is a name lookup (exact names first); dotted
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
| `mode` | `"lexical"` (BM25), `"hybrid"` (BM25 fused with embedding similarity), `"semantic"` (embeddings only) | `hybrid` when a selected snapshot has vectors and its model loads, else `lexical` |

`mode` ([RFC 0007](rfcs/0007-semantic-search.md) §8,
[how-it-works §10](how-it-works.md#10-embeddings-rfc-0007)): an explicit
`hybrid` or `semantic` on snapshots without vectors is an error naming
`rueca index --embed`, and one whose model is not available locally an
error naming `rueca models pull`; without `mode` both cases rank lexically
(the second with a warning). In `hybrid` the score is
`(α·b̂ + (1 − α)·d̂) × prior × routing`, BM25 and cosine each min-max
normalized over its own top 100, α = 0.5 (the defaults; `[retrieval]` in
`<store>/rueca.toml` tunes them); a snapshot without vectors (an
organization member indexed without `--embed`) is ranked by BM25 alone and
named in `retrieval.lexical`. `semantic` ranks by the cosine alone and
leaves such snapshots out. Older binaries reject `mode` (arguments are
validated strictly).

The envelope gains `retrieval` (between `next_cursor` and `budget`):

```json
"retrieval": {"mode": "hybrid", "model": "qwen3-0.6b-q8:3f9a1c2b", "alpha": 0.5,
              "lexical": ["github.com/acme/legacy"]}
```

`mode` is the ranking the call got (`lexical`, `hybrid`, `semantic`);
`model` the preset and a prefix of the vectors' model key; `alpha` the BM25
weight (hybrid only); `lexical` the repositories ranked without vectors;
`warnings` why a call fell back to lexical (a model that did not load,
unreadable vectors). Empty fields are omitted: a lexical call has
`{"mode": "lexical"}`. The text rendering does not change.

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

`next_cursor` is the offset of the next result. With a `ref` list, items of
a repository read at several refs carry `"in"` and `"changed"` ([Several
refs](#several-refs-of-one-repository)).

**Language names** (`search_symbols`, `search_code`): `python`,
`typescript`, `tsx`, `javascript`, `php`, `markdown`, `go`, `java`, `cpp`, `c` (a `.h` header is `cpp` when the
repository has C++ sources), `rust`, the data languages
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

`callers` / `callees` count edges inside the symbol's repository; in
organization mode, cross-repository link edges are not counted (`trace`
follows them). `readers` / `writers` answer "is this env var / config key
used, and is it set anywhere?" before a `trace`. A canonical env var (`$env:NAME`) has no
`path` or `lines`. Over budget, the lists are cut (children, then bases,
subtypes, candidates), `truncated` is set and `"omitted": {"children": n, …}`
says what was left out; there is no cursor (use `get_file_outline` or
`trace` for the rest). With a `ref` list the result also has `"in"` /
`"changed"` and the symbol's other versions in `"versions"` ([Several
refs](#several-refs-of-one-repository)); text:

```
Method InvoiceService.issue  pkg/service.py L9-11  python exported changed
  …
  version [1] changed: Method InvoiceService.issue L9-12  def issue(self, invoice)  callers 0, callees 3
```

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

In organization mode the walk also follows cross-repository link edges
(callers of a library function in the repositories that import it, a
consumer's calls into its dependencies), only between repositories the
caller can read. Link edges are computed between default refs (a ref's own
imports are linked against its dependencies' defaults); when the call reads
a repository at another ref, the edges into it are **remapped** to that
ref's nodes by (label, qualified name, path), and an edge whose target the
ref no longer has is dropped: `trace` from a consumer's function with
`ref: "7.1.2"` (a tag only the library has, no `repo`) shows its calls
into the library as of that tag, and `repo: <consumer>, ref: "pr/7"`
traces the pull request's own calls, linked against the libraries'
defaults. Contract link edges ([above](#the-envelope), *Contract edges*)
come back like the others, with strategy `contract` and the call site:
`1 [1] out d1 Route GET /users/{id}  openapi.yaml L7-11  ←0 CALLS 0.9
contract @5`. When the answer spans several snapshots, every node
carries its snapshot `"s"` (the result's `"s"` is the root's), the
envelope lists the snapshots touched, and the text prefixes such nodes with
`[s]`:

```
[0] github.com/acme/billing@7696364aef89 (main)
[1] github.com/acme/payments@a1b2c3d4e5f6 (main)
0 [1] Function charge_card  payments/charge.py L1-2
1 [0] in d1 Method InvoiceService.issue  pkg/service.py L9-11  →0 CALLS 0.95 import_resolved @10
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
`←j` node `j` calls this node. With several refs of one repository (a `ref`
list), nodes carry `in` / `changed` and the edges of that repository end
with an object `{"in": [s, …]}` (plus `"changed": true` when a walk from
some requested ref did not find it); text:

```
[0] github.com/acme/billing@7696364aef89+ (main)
[1] github.com/acme/billing@c0ffee00c0ff (feature/x)
0 [0] Method InvoiceService.issue  pkg/service.py L9-11 [0,1] changed
2 [1] out d1 Function audit  pkg/service.py L17-18 changed  ←0 CALLS 0.95 same_module @11 [1] changed
3 [0] out d1 Function helper  pkg/service.py L13-14 [0,1] changed  ←0 CALLS 0.95 same_module @10
```

`next_cursor` is the number of non-root nodes already returned; a later
page repeats the root as node 0 and any earlier node an edge needs.

Edge types beyond calls: `["INHERITS"]` walks a class hierarchy,
`["IMPORTS"]` file dependencies (`File` → `File`, and `File` → `Folder` for
imports of a directory: a Go or Java package, a Terraform module; for Rust,
`mod` declarations and the module files `use` items come from), `["TESTS"]`
the tests of a function, `["OVERRIDES"]` the methods an override replaces
(callers: the overrides of a method; Java and C/C++, Rust trait-impl items
and the trait items they implement, and Go methods that
implicitly implement an interface method, heuristic 0.5). Go types also
get heuristic `IMPLEMENTS` edges to the interfaces whose methods they
cover, so `get_symbol` `subtypes` of a Go interface lists its implementers;
a Rust `impl Trait for Type` gives `IMPLEMENTS` `Type` → `Trait`. The code ↔ config
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
lists the rest at the top level. With a `ref` list: `"in"` / `"changed"`
and `"versions": [{"s", "in", "changed", "line_count", "symbols"}]`, one
per other distinct version of the file (text: `version [1] changed:
pkg/service.py (python, 18 lines)` and its symbols).

### `list_repositories`

The repositories this server can read, sorted by id: in organization mode
every visible repository of the organization, otherwise every indexed
repository (`repo` narrows it; `ref` picks the snapshot each summary
describes).

| Arg | Type | Default |
|---|---|---|
| `query` | string: rank the repositories for this text (organization mode; see *Repository routing*) | — |

Results: `[{"s", "repo", "default_ref", "refs": [{"ref", "commit"}, …], "languages": {"python": files, …}, "files", "symbols", "edges", "packages" (opt), "packages_more" (opt), "depends_on" (opt), "score" (opt)}]`
(`refs`: every indexed snapshot of the repository — the default ref first,
then its ref snapshots — with the first 12 hex digits of its commit;
`ref` reads any of them)
(`packages`: up to 8 package names the repository publishes, `packages_more`
the rest's count; `depends_on`: visible repositories it imports packages
from or whose contracts it calls or implements; `score`: relevance for `query`, 1 = best, results then ordered by it)
(`s` is the snapshot of the default ref; `symbols` counts nodes that are not
files, folders, modules or packages), plus `"org"` (the organization name,
or `null`) next to `next_cursor`. `next_cursor` is the number of
repositories already returned. Verbose: `{"org", "repositories": [{"repo",
"root", "default_ref", "commit", "refs", "languages", "files", "symbols",
"edges", "packages" (all), "depends_on", "score"}]}` (`refs` as above,
`commit` the default's full commit). Text (`refs` as `name@commit12`, at
most 8 then `(+n)`; `packages …`, `depends on …` and `(score)` follow when
present):

```
[0] github.com/acme/billing@7696364aef89 (main)
[1] github.com/acme/payments@a1b2c3d4e5f6 (main)
[0] github.com/acme/billing  refs main@7696364aef89, pr/7@c0ffee00c0ff  120 files, 3400 symbols, 9100 edges  python 110, yaml 10
[1] github.com/acme/payments  refs main@a1b2c3d4e5f6  80 files, 2100 symbols, 5300 edges  typescript 78, json 2
```

### `list_contracts`

The contract tables of the selected repositories (RFC 0001 §11.4, RFC 0006
phase 4): HTTP routes, OpenAPI operations and `.proto` RPC methods
provided, and the HTTP / RPC calls made, each with how many counterparts
the visible repositories have. `repo` / `ref` select what is listed as in
the other tools (organization mode: every visible repository by default;
locally: the default root); counterparts are counted over every visible
repository at the entry the call reads (in an organization also when `repo`
narrows the listing; locally, the selected roots only).

| Arg | Type | Default |
|---|---|---|
| `side` | `provided`, `consumed`, or `dangling` (consumed, and no visible repository — nor its own — provides a matching key) | provided and consumed |
| `kind` | `http`, `rpc` or `topic` | every kind |
| `query` | string: keep entries whose key, raw text or symbol contains it (case-insensitive) | — |
| `limit` | int, 1–1000 | 50 |

Results: one flat list sorted by (kind, key, repository, path, line):
`[{"s", "repo", "path", "line", "symbol", "id", "label", "kind", "side",
"key", "framework", "raw", "matches"}]` — `symbol` the qualified name of
the declaring / calling definition (the file for file-level code), `id`
its symbol id, `side` `provides` / `consumes`, `key` the canonical key,
`framework` the framework or spec (`flask`, `requests`, `openapi`,
`proto`, `grpc-python`, …), `raw` the text as written (≤ 200 characters),
`matches` the number of counterpart rows: providers matching a consumed key
(code and spec), consumers of a provided key (a row never counts itself).
Dangling entries are computed at query time, nothing is stored.
`next_cursor` is the number of entries already returned. Verbose:
`{"contracts": [{"repo", "ref", "path", "line", "symbol", "id", "label",
"kind", "side", "key", "framework", "raw", "matches"}], "truncated",
"next_offset"}`. Text, one line per entry (`key  side  framework
repo:path#local-name Lline  (matches)`, `[s]` first with several
snapshots):

```
[0] local/api@worktree (worktree)
[1] local/client@worktree (worktree)
[0] GET /users/{}  provides  openapi  local/api:openapi.yaml#paths./users/{id}.get L7  (1)
[1] GET /users/{}  consumes  requests  local/client:app/users.py#get_user L5  (1)
[1] POST /nowhere  consumes  requests  local/client:app/users.py#missing L9  (0)
```

## Over HTTP

`rueca serve` ([RFC 0008](rfcs/0008-service-mode.md) §6) serves the same
tools to many clients from a service store. The envelope, the tool
arguments and the text rendering are exactly those of `rueca mcp`; only the
transport and the caller's visibility differ.

- **Endpoint**: `POST /mcp` on `[http].listen`, one JSON-RPC message per
  request, one `application/json` response (no SSE, no session:
  `Mcp-Session-Id` is ignored and never minted). Every protocol version of
  `rueca mcp` is served statelessly: a legacy client's `initialize` and its
  later requests (version from `MCP-Protocol-Version`, `2025-03-26` when
  absent), or 2026-07-28 requests with `_meta`. Notifications answer `202`.
- **Headers**: `Authorization: Bearer <token>` (required);
  `MCP-Protocol-Version`, `Mcp-Method`, `Mcp-Name` optional, but when
  present they must match the body (`400`, -32020 `HeaderMismatch`); an
  unsupported version is `400` (-32022).
- **Auth**: static tokens (`rueca_…`, configured as `sha256:<hex>` in
  `[[auth.token]]`) or OIDC access tokens from a configured issuer
  (`[[auth.oidc]]`), with scope `rueca:read`. No token: `401` with
  `WWW-Authenticate: Bearer … resource_metadata="<PRM URL>"`; a bad token:
  `401 invalid_token`; no scope: `403 insufficient_scope`; a token in the
  query string: `400`. With an OIDC issuer the Protected Resource Metadata
  is at `/.well-known/oauth-protected-resource/mcp`.
- **Visibility**: a principal reads the repositories its rules grant
  (`repos` globs); with none it sees an empty organization (empty
  `list_repositories`, no results), and a repository it cannot read is
  reported like one that does not exist.
- **Limits**: `Origin` / `Host` not allowed `403`, body over
  `[http].max_body_bytes` `413`, `GET` / `DELETE` `405`, over a rate limit
  `429` with `Retry-After`; before the first generation is served,
  -32002.
- **Deadline**: every tool call has `[http].query_deadline` (10 s) to
  finish. The engine checks it cooperatively where work loops (per segment
  of a symbol search, per file of `search_code`'s scan, every 64 nodes of
  `trace`'s traversal, outline symbols and multi-ref versions) and stops
  there; the answer is the JSON-RPC error -32001, not a tool result, with
  a message naming the deadline:

  ```json
  {"jsonrpc":"2.0","id":7,"error":{"code":-32001,"message":"deadline of 10s exceeded; narrow `repo` or lower `limit`"}}
  ```

  Retry with a narrower `repo` (one repository instead of the whole
  organization), a tighter `path_glob`, a lower `limit` or `depth`. A
  timeout of the same length around the call is the backstop and gives
  the same error. `rueca mcp` (stdio) and `rueca query` have no deadline.
- **Operations** (`[http].ops_listen`, no auth): `/healthz`, `/readyz`
  (`503` until a generation is served and the engine warmed on it, then
  `{"generation": n, "warm": "k/m"}`), `/metrics` (Prometheus). `GET /status` on the main listener
  (`rueca:read`): generation, lag, and the caller's repositories with their
  refs and commits.

```sh
curl -s https://rueca.acme.internal/mcp \
  -H "Authorization: Bearer $RUECA_TOKEN" -H 'Content-Type: application/json' \
  -H 'MCP-Protocol-Version: 2025-06-18' -H 'Mcp-Method: tools/call' -H 'Mcp-Name: search_symbols' \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"search_symbols","arguments":{"query":"issue invoice"}}}'
```

**Container.** The repository's `Dockerfile` builds the CPU image (RFC
0008 §4.3): the release binary on `gcr.io/distroless/cc-debian13:nonroot`
with `git` and the libraries it and `git-remote-https` link copied from
Debian trixie; user `65532`, `ENTRYPOINT ["rueca"]`, default command
`serve --config /etc/rueca/rueca-service.toml`, volume `/var/cache/rueca`
(point `[store].cache_dir` there), ports 8080 and 9090; it runs with a
read-only root filesystem.

```sh
docker build -t rueca .
docker run --rm --read-only -p 8080:8080 -p 9090:9090 \
  -v "$PWD/rueca-service.toml:/etc/rueca/rueca-service.toml:ro" \
  -v rueca-cache:/var/cache/rueca rueca
```

`bench/` measures the service (`rueca-bench service`: time to ready,
latency, and every answer against `rueca query`; `bench/README.md`).

Phase 1 adds three keys to RFC 0008 §4.4's file: `[store].poll` (the
generation watcher period, default `"5s"`), `[http].ip_rate_limit` (per
client IP before authentication, default `{ per_second = 20, burst = 100
}`) and `[http].trusted_proxies` (addresses or CIDR blocks whose
`X-Forwarded-For` / `X-Real-IP` name the client). `[org].inventory`,
`[[repo]]`, `[indexing]`, `[retention]` and `[retrieval]` are parsed and
checked, used from phase 2 on.

## CLI

```
rueca index [PATH] [--store DIR] [--repo ID] [--json]
    Index the working tree at PATH (default: current dir). Prints a summary
    (or the IndexReport as JSON). Organizations holding PATH are updated
    (only its part; JSON: "orgs").

rueca index [PATH] --ref NAME [--remove] [--store DIR] [--repo ID] [--json]
    Index a ref of PATH's repository as a snapshot of its own: a branch,
    `pr/<n>`, a tag or a commit (fetched from `origin` when absent; `pr/<n>`
    always), its tree exported from the object database under
    <store>/refs/; the working tree is not touched. Re-running at the same
    commit does nothing; --remove drops the snapshot and its export.
    Organizations holding the repository list (or drop) the ref.

rueca index --org FILE [PATH] [--store DIR] [--json]
    Index every repository of the organization file (or only the one
    containing PATH), then build or update the organization layer: the
    symbol index, routing profiles and package registry, and the link
    sets of the repositories whose links changed. JSON: {"org", "indexed":
    [IndexReport], "update" ({…, "profiled", "linked", "link_edges",
    "link_errors", "profile_ms", "link_ms", "refs_added", "refs_replaced",
    "refs_removed", "costs"}), "refs" (one report per ref synced),
    "ref_errors", "repos_ms", "refs_ms", "org_ms", "duration_ms"}. Every
    member's refs are synced: its `refs = [...]` and the refs indexed
    earlier; none is removed.

rueca index --org FILE MEMBER --ref NAME [--remove] [--store DIR] [--json]
    Index (or remove) one ref of a member (its path, `id` or `url`), then
    update the organization: only that ref's entry changes (segment copy,
    profile, link set against its dependencies' defaults).

rueca query <TOOL> '<JSON ARGS>' [--root PATH | --org FILE|NAME] [--store DIR] [--pretty] [--text]
    Run one tool and print its JSON result (with --text, the compact text
    rendering MCP clients get as text content). Same semantics as MCP.

rueca mcp [--root PATH | --org FILE|NAME] [--store DIR] [--no-auto-index]
    MCP server on stdio. Indexes --root first if it has no segment; with
    --org FILE, builds the organization first if the store has none.

rueca status [--store DIR] [--json]
    List indexed roots with repo, commit, node/edge/file counts, age, the
    ref snapshots under each repository (`ref 8.5.1 @cf19179e8e2f …
    (export)`), and the organizations (generation, repositories and refs:
    `main@08b803cf32ce + 8.5.1@cf19179e8e2f`).

rueca serve [--config FILE] [--role query|indexer|all] [--check-config]
    Service mode, query role (RFC 0008 phase 1): MCP over HTTP from a
    service store (see "Over HTTP" below). The configuration is
    `rueca-service.toml` (--config, else $RUECA_CONFIG; RFC 0008 §4.4);
    $RUECA_ROLE, $RUECA_STORE_URL and $RUECA_LOG override it.
    --check-config validates the file, its `env:` references and the store,
    prints `warning:` / `error:` lines and exits 0 or 1. Logs: stdout, JSON
    by default (`[telemetry].log_format`). SIGTERM / SIGINT drain in-flight
    requests for up to `[http].request_timeout`, then exit 0.

rueca links --org FILE|NAME [--repo ID [--ref NAME]] [--store DIR] [--json]
    List the organization's cross-repository edges (RFC 0006 link layer):
    one line per edge (contract edges end with `[contract <key>]`), or JSON
    {"org", "generation", "edges": [{"consumer", "src_repo", "src",
    "src_label", "src_path", "dst_repo", "dst", "dst_name", "dst_label",
    "dst_path", "kind", "strategy", "confidence", "path", "line", "via",
    "key" (contract edges)}]} (`consumer`: the repository the edge was
    observed in, as `repo@ref` for a ref entry; `dst_name`: the target's
    name, e.g. an OpenAPI operation's `GET /users/{id}`; `via`: `package` or
    `contract`; `key`: the calling row's contract key; `--repo`: edges
    leaving that repository; `--ref`: the link set of that ref entry, into
    its dependencies' default refs).

rueca contracts [--org FILE|NAME | --root PATH] [--repo ID [--ref NAME]]
                [--side provided|consumed|dangling] [--kind http|rpc|topic]
                [--query TEXT] [--limit N] [--store DIR] [--json | --text]
    The `list_contracts` tool over an organization (or, with --root / the
    indexed root containing the current directory, one local root), every
    page of it: the pages are merged into one envelope (snapshots listed
    once) up to --limit entries (default: all). Text by default (one line
    per entry), the envelope with --json.

rueca models list | pull [PRESET] | rm PRESET | path PRESET [--store DIR] [--json]
    Embedding weights (RFC 0007 §4.4): `list` prints the presets (status,
    cache state, quantization, width, size); `pull` downloads a preset's
    GGUF file (default: `[retrieval] preset` of <store>/rueca.toml, else
    qwen3-0.6b-q8) and checks its size and sha256 — the only command that
    downloads weights; `rm` deletes it; `path` prints where it lives. The
    cache is $RUECA_MODEL_DIR, else <store>/models.

rueca embed [--preset P] [--dims N] [--max-tokens N] [--batch N] [--cpu] [--threads N]
            [--instruction TEXT] [--input FILE|-] [--output FILE|-] [--format jsonl|f32le]
            [--info | --bench | --text TEXT [--query]]
    Embed JSON lines {"id", "text", "query"} into {"id", "vector"} lines
    (or, with --format f32le, raw little-endian f32 rows plus FILE.ids), in
    input order, with weights already pulled (a missing file is an error
    naming `rueca models pull`). Documents are cut at --max-tokens (512),
    queries at 512 capped by the preset's context. --info prints the model
    record (keys, sha256, dims, budgets, runtime, backend) without loading
    the model; --bench prints throughput. Needs the `embeddings` feature
    (on by default).
```

**Organization file** (`rueca-org.toml`, RFC 0006 §7):

```toml
name = "acme"                      # ASCII letters, digits, `-`, `_`, `.`

[[repo]]
path = "~/src/billing"             # `~` and paths relative to this file

[[repo]]
url = "https://github.com/acme/payments"   # cloned under <store>/orgs/acme/clones/ with `git`
id = "github.com/acme/payments"            # optional repository id (like `index --repo`)
refs = ["release/2.x", "pr/123"]           # optional: other refs indexed next to the default
```

The default ref of a member is its root (a `path` member's working tree; a
`url` member's clone at the remote's HEAD). `refs` are indexed by `rueca
index --org` as snapshots of their own (a `url` member's branches are
fetched first); refs added with `--ref` are kept and refreshed too.

The organization lives in `<store>/orgs/<name>/` ([how it
works](how-it-works.md#46-the-organization-layer-rueca-segment-org)).
Repository ids must be unique within an organization.

Store default: `$RUECA_HOME`, else `~/.cache/rueca`.

## Verbose (alpha) format

`"format": "verbose"` returns the v0.1.0-alpha result shapes and defaults
unchanged, for one release (the next minor removes it): no envelope, long
field names, `search_symbols` limit 20, `search_code` 50, `trace` depth 2 and
200 nodes, no budget. Ids are the pinned RFC 0005 form in both formats;
`repo`, `ref` and `cursor` work in both (a `ref` list is not collapsed:
each snapshot's items are listed). The MCP text content of a verbose
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
