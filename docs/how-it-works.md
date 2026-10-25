# How Rueca works: building the graph and answering with it

This document explains the method: what Rueca extracts from source code, how
it turns that into a graph, what the graph contains, and exactly which parts
of it each tool reads. It describes the alpha as implemented. The *why* lives
in the [RFCs](rfcs/) and the [decisions log](decisions.md); the exact tool
arguments and result shapes live in [tools.md](tools.md); byte-level formats
live in the crate docs (`rueca-segment`).

Contents:

1. [The pipeline in one picture](#1-the-pipeline-in-one-picture)
2. [Vocabulary](#2-vocabulary)
3. [A worked example](#3-a-worked-example)
4. [Building the graph](#4-building-the-graph)
5. [The graph schema](#5-the-graph-schema)
6. [How each tool uses the graph](#6-how-each-tool-uses-the-graph)
7. [Confidence: how to read an edge](#7-confidence-how-to-read-an-edge)
8. [Determinism, incrementality, limits](#8-determinism-incrementality-limits)
9. [Where the code lives](#9-where-the-code-lives)

---

## 1. The pipeline in one picture

```
                 per file (parallel, cached by blob)              per snapshot                   query time
              ┌──────────────────────────────────────┐   ┌──────────────────────────┐   ┌─────────────────────────┐
working tree ─┤ walk → read → blob SHA → tree-sitter ├──▶│ resolve: facts → graph   ├──▶│ segment (one file)      │◀── six tools
              │        → FileFacts                   │   │ nodes + edges + evidence │   │ + content packs (zstd)  │
              └──────────────────────────────────────┘   └──────────────────────────┘   └─────────────────────────┘
                 rueca-index + rueca-lang-*                 rueca-resolve                   rueca-segment, rueca-query
```

Two ideas carry the whole design:

- **Extraction is local, resolution is global.** A language pack looks at
  *one file* and reports what it sees (definitions, references by name,
  imports by specifier). It never guesses what a name points to. Linking
  names across files happens once, in the resolver, with the whole snapshot
  in view. This is what makes per-file caching possible and keeps every
  guess in one place.
- **Every edge says how it was made.** Edges carry a *strategy* and a
  *confidence*. The graph does not pretend to be a compiler; it tells the
  agent which links are certain and which are educated guesses.

## 2. Vocabulary

| Term | Meaning |
|---|---|
| **Snapshot** | One indexed state of a working tree: repo id, commit (or `worktree`), dirty flag. |
| **Blob** | A file's content, identified by its git blob SHA-1. The unit of caching and content storage. |
| **Facts** (`FileFacts`) | What one file says about itself: definitions, references, imports, parse errors, line count. Output of a language pack. |
| **Definition** | A named thing declared in a file (class, function, method, field, variable, constant, interface, type alias, enum, test…), with its span, signature, docstring and parent. |
| **Reference** | A use of a name: a call, `new`, type annotation, decorator, `raise`/`throw`, read or write. Has a name, an optional receiver (`self`, `app`, `a.b`) and the definition that encloses it. |
| **Import** | A module specifier plus imported names (`from .http import Session`). |
| **Module name** | How other files refer to a file: Python `shop.http`; ECMAScript `src/util/index` (path without extension). |
| **Qualified name** | Global name of a definition: module name + separator + local name. Python `shop.http.Session.send`; ECMAScript `src/util/index:formatAmount`. |
| **Node / edge** | The resolved graph. Nodes are definitions plus `Repository` / `Folder` / `File`; edges are typed relations between them. |
| **Strategy / confidence** | How a reference edge was resolved, and how much to trust it (0–1). |
| **Segment** | The immutable file that stores one snapshot's graph and indexes. |
| **Content pack** | Compressed file contents, keyed by blob SHA, shared across segments. |
| **Symbol id** | Stable, human-readable id: `repo@commit:path#Local.name`, e.g. `local/shop@worktree:shop/http.py#Session.send`. |

## 3. A worked example

A three-file Python package, `shop/`:

```python
# shop/__init__.py   (empty)

# shop/http.py
class Session:
    """HTTP session with connection reuse."""

    def request(self, method, url):
        return self.send(method)

    def send(self, req):
        return req

# shop/api.py
from .http import Session


def get(url):
    return Session().request("GET", url)
```

`rueca index` produces **9 nodes and 12 edges** (actual output):

```
Repository local/shop
└─CONTAINS─ Folder shop
   ├─CONTAINS─ File shop/__init__.py
   ├─CONTAINS─ File shop/http.py ─DEFINES─ Class Session ─DEFINES─┬─ Method request
   │                                                               └─ Method send
   └─CONTAINS─ File shop/api.py  ─DEFINES─ Function get
                                 ─IMPORTS─▶ File shop/http.py

Method request ─CALLS─▶ Method send     0.90  type_resolved   (self.send → member of the enclosing class)
Function get   ─CALLS─▶ Class Session   0.95  import_resolved (Session bound by `from .http import Session`)
Function get   ─CALLS─▶ Method request  0.50  heuristic       (receiver `Session()` is untyped; `request` is the only method with that name)
```

The last edge is the interesting one. A human sees that `Session()` returns a
`Session`, so `.request` is `Session.request`. The alpha has no receiver
typing (see §7), so the resolver falls back to "the only method called
`request` in the project" and marks the edge 0.5 / `heuristic`. An agent that
needs certainty can ask `trace` with `min_confidence: 0.6` and that edge
disappears. The sections below reference this example.

## 4. Building the graph

### 4.1 Walk and snapshot identity (`rueca-index`)

1. Walk the root with the `ignore` crate: `.gitignore`, `.ignore`, global git
   excludes, plus hard excludes for dependency, build and cache directories
   (`node_modules`, `.venv`, `dist`, `__pycache__`, …), minified bundles,
   source maps and lockfiles. Hidden files are kept (`.github/` matters).
2. Read each file **once**, compute its git blob SHA. Files over 2 MiB or
   binary are listed but not extracted. Every listed file gets a `File` node
   and is content-searchable if it is text, even without a language pack.
3. Identity from git via `gix`: repo id from `remote.origin.url`
   (`github.com/owner/name`) or `local/<dir>`; commit = `HEAD`; *dirty* when
   any indexed blob differs from the `HEAD` tree.

### 4.2 Extract facts per file (`rueca-lang-python`, `rueca-lang-typescript`)

Each supported file is parsed with tree-sitter under a budget (size, time,
depth). The pack walks the syntax tree and emits facts. In the example,
`shop/http.py` yields:

```
definitions:
  0 Class    Session           parent: —   span 1–8  signature "class Session"  doc "HTTP session…"
  1 Method   Session.request   parent: 0   span 4–5  signature "def request(self, method, url)"
  2 Method   Session.send      parent: 0   span 7–8  signature "def send(self, req)"
references:
  Call  name "send"  receiver "self"  enclosing: 1 (Session.request)   line 5
```

and `shop/api.py` yields the import `from .http import Session` (level 1),
the function `get`, and two calls inside it: `Session` (no receiver) and
`request` (receiver `Session()`).

What packs deliberately do and don't do:

- **Containment and local qualified names** (`Session.request`) come from the
  nesting of definitions. The parent link becomes a `DEFINES` edge later.
- **Local variables are filtered at the source.** `url`, `method` and `req`
  are parameters, so reads of them are not reported. The decision "is this
  identifier local?" is driven by vendored tree-sitter `locals.scm` queries
  (adapted from nvim-treesitter and bloop, see `NOTICE`), plus the scope rules
  queries cannot express (Python closures, `global`/`nonlocal`, class-body
  scope; JS hoisting). This removes most noise before resolution.
- **No cross-file knowledge.** `Session` in `api.py` is just a name with no
  receiver. The pack does not know it is a class.
- **Tests are definitions too**: `test_*` functions and `Test*` classes in
  Python; `describe` / `it` / `test` blocks in JS/TS become `Test` nodes
  whose qualified name is the describe chain.

Facts are cached at `<store>/facts/<sha>-<lang>-v<extractor version>.bin`.
Re-indexing an unchanged file is a cache hit; bumping a pack's
`extractor_version` invalidates its entries.

### 4.3 Module names and import targets

Two per-language hooks, still without guessing symbols:

- `module_name(path)`: Python walks up `__init__.py` packages and source
  roots (`src/`, `pyproject` package dirs) → `shop.http`. ECMAScript strips
  the extension → `src/util/index`.
- `resolve_import(from, import)`: maps a specifier to a project file or to
  *nothing* (third-party, builtin). Python follows relative levels and source
  roots (`from .http` in `shop/api.py` → `shop/http.py`). TS/JS uses
  [`oxc_resolver`](https://github.com/oxc-project/oxc-resolver) over a virtual
  file system built from the snapshot: extension probing, `index` files,
  `.js` → `.ts` aliasing, `tsconfig` `paths` / `baseUrl` / `extends`,
  `package.json` `exports`, and workspace packages mounted by name.

Every resolved import becomes an `IMPORTS` File→File edge (confidence 1.0).
An import that resolves to nothing marks its names **external**: later
references through them are never fuzzy-matched to a project symbol.

### 4.4 Resolve references (`rueca-resolve`)

**Nodes first.** One `Repository`, one `Folder` per directory, one `File` per
file, one node per definition with its global qualified name
(`shop.http` + `.` + `Session.request`). Structural edges are added with
confidence 1.0: `CONTAINS` (repository → folders → files) and `DEFINES`
(file → top-level definitions, parent → child).

**Binding environments.** For every file, the resolver computes which names
its imports bind: `from .http import Session` binds `Session` to the node
`shop.http.Session`; `import a.b` binds `a`; `import * as ns` binds a
module; star imports bind every exported name. Looking up a name in a module
checks its own definitions, then its re-exports (`export { x } from`,
`export *`), then the names its own imports bind (barrel files, package
`__init__.py`), transitively and cycle-safe up to depth 10.

**The strategy ladder.** Each reference is resolved by trying strategies in
order; the first one that succeeds wins and stamps the edge:

| Order | Strategy (edge `strategy`) | Applies when | Confidence |
|---|---|---|---|
| 1 | local scope (`same_module`) | no receiver; name defined in an enclosing function | 0.95 |
| 2 | self receiver (`type_resolved`) | receiver is `self` / `cls` / `this`: member of the enclosing class | 0.90 |
| 2 | …inherited / `super` (`type_resolved`) | member found in a base class (depth-first, left-to-right) | 0.85 |
| 3 | imported name (`import_resolved`) | no receiver; name bound by an import | 0.95 |
| 3 | imported module (`import_resolved`) | receiver is an imported module (`pkg.mod.f`, `ns.f`) | 0.90 |
| 3 | imported class (`import_resolved`) | receiver is an imported class (static member) | 0.85 |
| 4 | same module (`same_module`) | no receiver; top-level definition of the same file | 0.90 |
| 4 | same-module class (`same_module`) | receiver is a top-level class of the same file | 0.85 |
| 6 | unique name (`unique_name`) | no receiver; exactly one compatible non-test definition with that name in the project | 0.60 |
| 6 | unique method (`heuristic`) | call with an unknown receiver; exactly one method with that name (≥ 4 chars, not a common builtin method like `keys`, `then`) | 0.50 |

(Strategy 5, receiver typing from assignments, is not implemented in the
alpha.) A reference to a builtin, a name bound to an external module, or a
`self` member that does not exist **stops** resolution: it is counted as an
expected miss, never fuzzy-matched. Calls to a class also call its
`__init__` / `constructor` (confidence × 0.95).

In the example: `self.send` matches rung 2 → 0.90; `Session` matches rung 3
→ 0.95; `Session().request` has an unknown receiver, so only rung 6 applies
→ 0.50.

**From reference kind to edge kind.**

| Reference | Edge |
|---|---|
| `Call`, `New` | `CALLS` (+ `TESTS` when a test calls non-test code) |
| `Decorator` | `DECORATES` (decorator → decorated definition) |
| `Type` | `REFERENCES`, or `INHERITS` / `IMPLEMENTS` for a class's own base list |
| `Throw` | `THROWS` |
| `Read` | `REFERENCES` |
| `Write` | `WRITES` |

Duplicate edges `(src, dst, kind)` are merged keeping the best strategy, the
highest confidence and the first evidence. **Evidence** is the file and span
of the reference: this is what `trace` returns as `call_site`.

### 4.5 Write the segment (`rueca-segment`)

The resolved graph is frozen into one segment file plus content packs. Each
component exists because a tool needs it:

| Component | What it is | Used by |
|---|---|---|
| node table, file table | every node / file with its metadata | all tools |
| CSR adjacency, out and in | edges grouped by source and by destination, sorted by kind | `trace`, `get_symbol` |
| edge metadata | confidence, strategy, evidence span per edge | `trace` |
| name indexes | node ids sorted by qualified name and by name | symbol lookup (`get_symbol`, `get_snippet`, `trace`) |
| file → symbols | symbol nodes per file, sorted by position | `get_file_outline`, `search_code` (enclosing symbol) |
| tantivy symbol index | BM25 over name, qualified name, signature, path, docstring | `search_symbols` |
| trigram index | which files contain each 3-byte sequence | `search_code` |
| content packs | zstd-compressed file contents by blob SHA | `search_code`, `get_snippet` |

The segment is written to a temp file and atomically renamed; the catalog
then points the indexed root at it. Query time never reads the working tree.

## 5. The graph schema

**Node labels** produced by the alpha:

| Label | Produced from |
|---|---|
| `Repository`, `Folder`, `File` | the walk (every listed file, any language) |
| `Module` | TS `namespace` / `declare module` |
| `Class`, `Interface`, `Enum`, `TypeAlias` | class / interface / enum / type declarations |
| `Function`, `Method` | functions, methods, arrow functions assigned to names, getters/setters |
| `Field`, `Variable`, `Constant` | class fields, `self.x =` in `__init__`, module-level assignments (`UPPER_CASE` → `Constant`) |
| `Test` | JS/TS `describe` / `it` / `test` blocks (Python tests are `Function` / `Method` / `Class` with `is_test: true`) |

Every symbol node carries: name, qualified name, file, span, language,
signature, docstring, `exported`, `is_test`.

**Edge kinds** produced by the alpha: `CONTAINS`, `DEFINES`, `IMPORTS`
(structural, 1.0); `CALLS`, `REFERENCES`, `INHERITS`, `IMPLEMENTS`,
`DECORATES`, `THROWS`, `WRITES`, `TESTS` (resolved, with strategy and
confidence). `OVERRIDES` and `READS` exist in the model but are not produced
yet (reads become `REFERENCES`).

**Edge metadata**: `confidence` (0–1), `strategy`, and `evidence` (file +
span of the reference that produced it; absent for structural edges).

## 6. How each tool uses the graph

All six tools first pick the segment (the `repo` argument, the server's
`--root`, or the only indexed root) and, if they take a symbol, run the same
**symbol lookup**:

1. `id` → parse `repo@commit:path#local.name` and find that definition in
   that file (the commit is ignored, so ids survive re-indexing);
2. else `qualified_name` → exact match in the qualified-name index, then
   suffix match on a `.` / `:` / `/` boundary (`Session.send` finds
   `shop.http.Session.send`);
3. else `name` → name index, optionally narrowed by `path`.

When several nodes match, the pick is deterministic (non-test, exported,
symbol before container, shorter qualified name, lower id) and the others
are returned as `candidates`.

| Tool | Reads | Graph traversal |
|---|---|---|
| `search_symbols` | tantivy symbol index → node table | none |
| `search_code` | trigram index → content packs → file → symbols | none (positional) |
| `get_symbol` | lookup → node table → adjacency | one hop: `DEFINES`, `INHERITS`/`IMPLEMENTS`, `CALLS` counts |
| `get_snippet` | lookup → node span → content packs | none |
| `trace` | lookup → adjacency + edge metadata | breadth-first, N hops |
| `get_file_outline` | file → symbols → `DEFINES` | containment tree |

### `search_symbols` — "where is the thing called…?"

Ranks definitions, not lines. The query is split with the same code-aware
tokenizer used at index time (`user by id` → `user`, `by`, `id`, and
`getUserById` → `get`, `user`, `by`, `id`, plus the whole identifier), then
BM25 runs over five fields with boosts: name ×8, qualified name ×3,
signature ×1.5, path ×1, docstring ×0.5, plus a bonus for an exact name or
qualified-name match. Filters (`labels`, `language`) are exact terms;
`path_glob` filters after ranking. Default labels exclude `File`, `Folder`,
`Module` and `Repository`. Ties break by node id so pages are stable.

### `search_code` — "where does this text appear?"

The only tool that reads file contents for matching. The pattern (literal or
regex) is parsed; required literals are extracted and turned into an AND/OR
query over trigram postings, which yields candidate files. Candidates are
decompressed from the content packs in path order and verified with the real
regex, stopping at `limit` or the scan budget (`truncated: true`). Patterns
with no usable literal scan every text file. Each match is then **placed in
the graph**: the file's symbols (sorted by position) give the innermost
definition containing that line, returned as `symbol`. In the example,
searching `self.send` returns line 5 of `shop/http.py` with symbol
`Session.request`.

### `get_symbol` — "tell me about this symbol and its neighbourhood"

One hop around the node:

- `parent`: incoming `DEFINES` (the class or file that declares it);
- `children`: outgoing `DEFINES` (methods of a class);
- `bases`: outgoing `INHERITS` + `IMPLEMENTS`;
- `subtypes`: incoming `INHERITS` + `IMPLEMENTS`;
- `callers_count` / `callees_count`: distinct nodes across incoming /
  outgoing `CALLS`, so the agent can decide whether a `trace` is worth it.

### `get_snippet` — "show me the code"

Resolves the symbol to its file and span (or takes `path` + lines), fetches
the blob from the content packs, and returns those lines plus
`context_lines`, capped by `max_lines`. It serves the indexed version of the
file, which matches what the graph describes.

### `trace` — "who calls this, and what does it call?"

Breadth-first search from the root node over the in-adjacency (`callers`)
and/or out-adjacency (`callees`), level by level up to `depth`, following
only `edge_types` (default `CALLS`) and skipping edges below
`min_confidence`. Each direction keeps its own visited set, so a node
reachable both ways appears once per direction. Every traversed edge is
returned with its confidence, strategy and `call_site` (the evidence span).
`limit` caps nodes and sets `truncated`. Other edge kinds make `trace` a
general traversal: `["INHERITS"]` walks a class hierarchy, `["IMPORTS"]`
from a `File` node walks module dependencies, `["TESTS"]` finds the tests
that exercise a function.

### `get_file_outline` — "what is in this file?"

Takes the file's symbols (already sorted by position) and nests them by
`DEFINES` parent, falling back to span containment, children ordered by
line. No content is read; signatures come from the node table.

### Putting them together

The recommended agent loop, and why it is cheap:

1. `search_symbols` (or `search_code` for literals, config keys, error
   messages) to find candidates. Milliseconds, a few hundred tokens.
2. `get_symbol` to confirm the right one and see its neighbourhood size.
3. `get_snippet` to read only that code.
4. `trace` to follow callers / callees, filtering by `min_confidence`.

Each step returns ids that the next step accepts, so the agent navigates the
graph instead of reading whole files.

## 7. Confidence: how to read an edge

| Confidence | Strategies | Meaning |
|---|---|---|
| 1.0 | `syntactic`, `import_resolved` (IMPORTS) | Structure or a resolved import. Certain. |
| 0.85–0.95 | `type_resolved`, `import_resolved`, `same_module` | Resolved through scope, `self`, or an import. Wrong only with dynamic tricks (monkey-patching, `getattr`, re-assigned names). |
| 0.60 | `unique_name` | A bare name that matched the only compatible definition in the project. Usually right in small/medium code bases; can be wrong when a third-party symbol shadows it. |
| 0.50 | `heuristic` | Method call on an untyped receiver matched by name alone. Useful for exploration; verify before relying on it. |

Practical guidance for agents: use the default (all edges) to explore;
use `min_confidence: 0.6` to drop pure name guesses, `0.85` for
"resolved through code structure only".

Known gaps behind low-confidence or missing edges, in rough order of impact:

- **No receiver typing**: `x = Session(); x.request()` or
  `const app = new Hono(); app.get()` resolve by name only (0.50) or not at
  all. This is the largest source of misses (planned "strategy 5").
- No call-graph knowledge of third-party code; calls into dependencies stop
  at the import.
- Python MRO is approximated depth-first (differs from C3 only for diamond
  inheritance).
- Dynamic dispatch, decorators that replace functions, `getattr`,
  dependency injection containers.
- JS named function expressions passed as arguments are not definitions yet.

The benchmark ([alpha results](bench/alpha-results.md)) measures how these
gaps show up on real repositories.

## 8. Determinism, incrementality, limits

- **Deterministic**: the same files produce the same graph, byte for byte
  in node/edge order (inputs are sorted; ties break by path and id). Tests
  shuffle inputs and compare serialized output.
- **Incremental where it matters**: extraction, the expensive part, is
  cached per blob; unchanged files are never re-parsed. Resolution and the
  segment are rebuilt per snapshot (tens to hundreds of milliseconds on
  medium repositories). File contents are stored once per blob across
  segments.
- **Bounded**: per-file budgets (2 MiB, 5 s parse, depth 512, 20 000
  references); files over budget are listed with a `skipped:*` status
  instead of failing the index.
- **Immutable**: a segment is never modified after it is written; re-indexing
  writes a new one and swaps the catalog entry atomically, so queries running
  during a re-index keep working.

## 9. Where the code lives

| Step | Crate | Start reading at |
|---|---|---|
| Data model (labels, edge kinds, facts, ids) | `rueca-model` | `src/lib.rs` |
| Language-pack contract, budgeted parsing | `rueca-extract` | `LanguagePack` |
| Python extraction, module names, imports | `rueca-lang-python` | crate docs, `queries/locals.scm` |
| TS/TSX/JS extraction, `oxc_resolver` | `rueca-lang-typescript` | crate docs, `src/resolve.rs` |
| Walk, cache, snapshot identity | `rueca-index` | `index_worktree` |
| Resolution | `rueca-resolve` | crate docs (strategy table) |
| Segment, search, packs, catalog | `rueca-segment` | crate docs (format spec) |
| Tools | `rueca-query` | `src/tools.rs` |
| MCP / CLI | `rueca-mcp`, `rueca` | `serve_stdio`, `src/main.rs` |
