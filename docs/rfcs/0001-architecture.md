# RFC 0001 — Architecture

- Status: Draft
- Created: 2026-10-22
- Supersedes: —
- Amended by: RFC 0002 (2026-10-22): segment bundle format, blob-visibility
  deltas, precision layer, MCP 2026-07-28 auth, git transport, untrusted input

## 1. Summary

Telar is a code intelligence engine for coding agents. It turns source code
into a queryable graph of symbols and relationships, plus full-text and
content indexes, and serves it over MCP.

One Rust core runs in three deployment modes:

| Mode | Shape | Storage | Typical user |
|---|---|---|---|
| **Local** | single binary | local directory + SQLite catalog | one developer, one working tree, offline |
| **Service** | one container | object storage + Postgres catalog | a team or a group of repositories |
| **Org** | indexer workers + stateless query nodes + webhook receiver | object storage + Postgres catalog | every repository in a GitHub / GitLab / Bitbucket organization |

The same extractors, segment format, and query engine run in every mode. What
changes is where segments live and how indexing is scheduled.

This RFC defines the data model, the indexing pipeline, the storage format,
the query surface, the component boundaries (as Rust traits and crates), and
the benchmark that decides when Telar is ready to replace existing tools.

## 2. Motivation

Existing code-graph tools for agents are built as **local, single-machine
programs**. Running them as a shared service exposes the same structural
problems:

1. **Clones are the source of truth.** Text search and snippet retrieval read
   files from a checkout recorded at index time. A service must keep a clone
   per repository and per indexed ref forever, or those tools silently return
   nothing. Disk grows without bound.
2. **Indexing is per checkout, not per content.** The same file in `main` and
   in ten open pull requests is parsed eleven times and stored eleven times.
3. **Multi-repo is an afterthought.** Cross-repository views are built by
   indexing a synthetic tree of every repository at once, which is slow to
   converge, memory-hungry, and lost on restart.
4. **State lives in process memory.** Restarting a service loses progress.
5. **Cross-service relationships are weak.** Public measurements show
   near-zero recall on HTTP / gRPC / topic edges between services, which is
   exactly what an organization-wide index should answer.
6. **No permission model.** Every caller sees every repository.

Telar is designed service-first, so that local mode is the degenerate case
of the same architecture rather than the other way around.

## 3. Goals and non-goals

### Goals

- G1. Answer the structural questions agents ask: where is X defined, who
  calls X, what does X call, what breaks if X changes, what services talk to
  each other, show me the code.
- G2. Serve those answers from immutable artifacts. No query depends on a
  live checkout.
- G3. Content-addressed indexing: work and storage scale with *distinct file
  contents*, not with the number of refs.
- G4. First-class cross-repository and cross-service relationships.
- G5. Same engine local and remote; local can overlay uncommitted changes on
  top of a remote organization snapshot.
- G6. Permission-aware queries in service and org modes, mirrored from the
  code host.
- G7. Deterministic extraction: same input bytes and extractor version produce
  byte-identical facts. Required for caching and for testing.
- G8. Measurable quality: every supported language has a benchmark score, and
  releases cannot regress it.

### Non-goals

- N1. Indexing document stores (Notion, Drive, wikis). Agents federate those
  through their own MCP servers. Telar exposes stable symbol IDs so results
  can be cross-referenced.
- N2. Editing or refactoring code. Telar is read-only.
- N3. Supporting every language. Languages are added in tiers (§12) and each
  one is held to the benchmark.
- N4. Writing a full type checker per language. Precision beyond syntactic
  resolution comes from importing compiler-grade indexes (SCIP) where they
  exist.
- N5. A hosted product. Telar is software adopters run.

## 4. Concepts

| Term | Meaning |
|---|---|
| **Host** | A code host: GitHub, GitLab, Bitbucket, Gitea, or the local filesystem. |
| **Repository** | `host/owner/name`. The unit of permission. |
| **Ref** | A branch, tag, or pull request head inside a repository. |
| **Snapshot** | A repository at one commit: the tree `path → blob_sha`. Immutable. |
| **Blob** | A file's content, identified by its git blob SHA. The unit of extraction. |
| **Extractor** | A pure function `blob → facts` for one language or file kind, with a version. |
| **Facts** | Everything an extractor learns from one blob in isolation: definitions, unresolved references, imports, contract hints, doc links. |
| **Resolution** | The per-snapshot step that turns unresolved references into edges, using all facts of that snapshot. |
| **Segment** | An immutable, self-contained index artifact (graph + text indexes + metadata). |
| **Content pack** | A compressed, content-addressed store of blob bytes, shared across snapshots. |
| **View** | What a query runs against: an ordered stack of segments (base + deltas) filtered by the caller's permissions. |
| **Catalog** | The small mutable database that maps repositories, refs, and snapshots to segments. |
| **Contract** | A cross-service interface a repository provides or consumes: HTTP route, gRPC method, message topic, GraphQL operation, environment variable, package. |

## 5. Architecture

```
          ┌────────────────────────── Connectors ───────────────────────────┐
          │ GitHub · GitLab · Bitbucket · Gitea · local working tree         │
          │ inventory, webhooks/polling, permissions, blob fetch             │
          └───────────────┬──────────────────────────────────────────────────┘
                          │ snapshot = (repo, commit, tree)
                          ▼
   ┌──────────────── Indexer ─────────────────────────────────────────────┐
   │ 1. Plan     diff tree vs previous snapshot → blobs to extract         │
   │ 2. Extract  blob → facts           (pure, cached by blob+extractor)   │
   │ 3. Resolve  facts of snapshot → nodes/edges   (deterministic)         │
   │ 4. Enrich   SCIP import · frameworks · infra · docs · metrics         │
   │ 5. Write    segment (graph + FTS + trigram + vectors) + content packs │
   └───────────────┬──────────────────────────────────────────────────────┘
                   │                        ┌──────── Org linker ─────────┐
                   │                        │ contracts of default-branch │
                   ▼                        │ snapshots → cross-repo edges│
   ┌──── Segment store ────┐◄───────────────┤ → org-link segment          │
   │ local dir │ S3 / GCS │                 └─────────────────────────────┘
   └──────┬────────────────┘
          │            ┌──── Catalog ────┐
          │            │ SQLite / Postgres│ repos, refs, snapshots,
          │            └────────┬────────┘ segments, ACLs, jobs
          ▼                     ▼
   ┌──────────────── Query engine ───────────────────────────────────────┐
   │ view = base + deltas + org-link, filtered by principal ACL           │
   │ lookup · search (BM25, regex, semantic) · traverse · snippet · impact│
   │ architecture · read-only openCypher subset                           │
   └──────┬──────────────────────────────────────────────────────────────┘
          ▼
   MCP (stdio · Streamable HTTP) · CLI · HTTP API
```

## 6. Data model

### 6.1 Identifiers

Every node has a stable, human-readable ID:

```
<host>/<owner>/<repo>@<commit>:<path>#<qualified-name>
github.com/acme/billing@3f2a91c:src/invoices/service.py#InvoiceService.issue
```

- `@<commit>` may be omitted to mean "the default branch head the view was
  built from". Tools always return the fully pinned form.
- `#<qualified-name>` is omitted for file nodes.
- Every result also carries a host URL (`https://…/blob/<commit>/<path>#L10-L42`)
  so other tools and humans can follow it.

Internally, nodes use a 64-bit ID that is stable within a segment only.
Cross-segment references always use the string ID or its 128-bit hash.

### 6.2 Node labels

Stable core set (v1):

- Structure: `Repository`, `Folder`, `File`, `Module`, `Package`
- Code: `Function`, `Method`, `Class`, `Interface`, `Trait`, `Struct`, `Enum`,
  `TypeAlias`, `Field`, `Variable`, `Constant`
- Interfaces between systems: `Route`, `RpcMethod`, `Topic`, `EnvVar`
- Infrastructure: `Resource` (k8s, terraform, compose…), `Image`
- Documentation: `Section`, `Decision` (ADR)
- Tests: `Test`

Labels outside the core set are allowed as `ext:<namespace>:<Label>` so
language packs can add detail without changing the schema.

### 6.3 Edge types

| Group | Edges |
|---|---|
| Containment | `CONTAINS`, `DEFINES` |
| Code | `IMPORTS`, `CALLS`, `REFERENCES`, `INHERITS`, `IMPLEMENTS`, `OVERRIDES`, `DECORATES`, `READS`, `WRITES`, `THROWS` |
| Tests | `TESTS` |
| Systems | `HANDLES` (handler → Route/RpcMethod), `CALLS_ROUTE`, `CALLS_RPC`, `PUBLISHES`, `SUBSCRIBES`, `USES_ENV` |
| Infra | `DEPLOYS`, `DEPENDS_ON`, `CONFIGURES` |
| Docs | `MENTIONS` |
| Org | `CROSS_*` variants of the systems edges, produced only by the org linker |

Every edge carries provenance:

```rust
struct EdgeMeta {
    confidence: f32,          // 0.0–1.0
    strategy: Strategy,       // Scip, TypeResolved, ImportResolved, SameModule,
                              // UniqueName, Heuristic, Contract
    evidence: Option<Span>,   // where in the source the edge was observed
}
```

When several strategies produce the same edge, the highest-precedence one wins:
`Scip > TypeResolved > ImportResolved > SameModule > UniqueName > Heuristic`.
Agents can filter by minimum confidence.

### 6.4 Facts

Facts are the extractor's output for one blob. They contain no cross-file
knowledge.

```rust
struct FileFacts {
    language: LanguageId,
    definitions: Vec<Definition>,     // label, name, qualified name, span, signature, doc
    references: Vec<UnresolvedRef>,   // kind (call, type, read, write…), text, scope, span
    imports: Vec<Import>,             // module path, imported names, aliases
    contracts: Vec<ContractHint>,     // provides / consumes, kind, raw key, span
    doc_links: Vec<DocLink>,          // markdown/comment mentions of symbols or paths
    tests: Vec<TestDecl>,
    metrics: FileMetrics,             // loc, complexity per definition
}
```

Facts are cached under `(blob_sha, extractor_id, extractor_version)`. A new
commit that touches 5 files costs 5 extractions, regardless of repository size
or how many refs share the other files.

## 7. Indexing pipeline

1. **Plan.** Given a new snapshot, diff its tree against the nearest indexed
   snapshot of the same repository. Blobs whose facts are cached are skipped.
2. **Extract.** Run extractors on missing blobs. Extractors are pure, run in
   parallel, and never touch the network or filesystem beyond the bytes given.
   A per-blob CPU and memory budget is enforced; a blob that exceeds it is
   recorded as a coverage gap, not a crash.
3. **Resolve.** Load facts for every blob in the snapshot and build edges.
   Resolution is deterministic and ordered (sorted inputs, stable tie-breaks).
   v1 re-resolves the whole snapshot from cached facts, which is cheap because
   parsing dominates cost. Incremental resolution (re-resolve only the
   dependency closure of changed files) is a later optimization behind the same
   interface.
4. **Enrich.** Optional passes over the resolved graph: SCIP import (§9.3),
   framework packs (routes, DI), infrastructure linking, doc linking, metrics.
5. **Write.** Produce a segment (§8) and append missing blobs to content
   packs. Register the segment in the catalog atomically.

**Branches and pull requests** are indexed as **delta segments**: only the
files that differ from the base snapshot, plus tombstones for deleted files.
A PR view is `base segment + delta segment`. This is also how local overlay
works (§11.2).

Delta resolution re-resolves the changed files and the files that reference
them (found through the base segment's reverse edges). Edges into unchanged
files are taken from the base.

Text indexes follow the same split (Zoekt / GitHub Blackbird model): a delta
segment indexes only blobs that are new to it, and carries a **roaring bitmap
of visible blobs** over base + delta. A view filters base postings through
that bitmap, so a PR never re-indexes unchanged files.

## 8. Storage

### 8.1 Segments

A segment is immutable and self-contained. Format: **one bundle file per
segment**, modeled on Quickwit splits — concatenated components, a footer
directory of offsets, and a **hotcache** (the small, always-needed parts) so
that opening a segment from object storage costs one or two range reads.

Components:

1. **Manifest** — format version (also encoded in the file name), repository,
   commit, base segment (for deltas), extractor versions, roaring bitmap of
   visible blobs, per-component offsets and checksums.
2. **Graph** — CSR adjacency arrays per edge type in both directions,
   memory-mapped with zero parsing (`memmap2` + `bytemuck`). Node and edge
   properties (labels, names, spans, provenance) as Arrow IPC tables.
3. **Symbol index** — a tantivy index over names, qualified names,
   signatures and docstrings, with a code-aware tokenizer (camelCase /
   snake_case splitting, path segments). Served from the bundle through a
   Quickwit-style directory stack (bundle → hotcache → storage → cache) with
   async warm-up.
4. **Content n-gram index** — over blobs new to this segment. v1: a trigram
   field in tantivy. Later, if size or latency demand it: a custom sparse-gram
   index (`fst` + `roaring` postings).
5. **Vectors** (optional) — f16 matrix scanned with SIMD by default; an
   mmap-able `usearch` HNSW when a segment exceeds a size threshold.
6. **Coverage** — files skipped or partially extracted, and why.

File contents are **not** inside the segment; they live in content packs
(§8.2) shared across segments.

All access goes through `SegmentReader` / `SegmentWriter` traits, and
third-party crates (tantivy, arrow) stay behind them, pinned per format
version. A `telar export --sqlite` command writes a segment as a SQLite file
for debugging and ad-hoc SQL.

Why not SQLite per segment (the first draft of this RFC): rusqlite cannot
register a custom FTS5 tokenizer without unsafe FFI; SQLite B-tree pages force
downloading whole files from object storage; recursive CTEs are slower than
in-memory adjacency for bounded traversal; FTS5's trigram tokenizer does not
support regex. See RFC 0002 §6.

### 8.2 Content packs

File contents are stored once per blob in **content packs**: append-only files
of seekable zstd frames (trained dictionaries per language) with a footer index
`blob_sha → (frame, offset, length)`. Packs live next to segments (local dir or
object storage) and are read with range requests. Snippets and verification of
n-gram candidates (regex re-checked with `regex-automata`) read from packs,
never from a checkout. Git packfiles are not used as the serving format
because delta chains make random access multi-hop.

### 8.3 Catalog

Small, mutable, transactional:

```
repositories(id, host, owner, name, default_branch, visibility, archived)
refs(repo_id, name, kind, head_commit, updated_at)
snapshots(repo_id, commit, tree_sha, base_snapshot, segment_uri, status, indexed_at)
segments(uri, kind[base|delta|org_link], format_version, bytes, created_at)
facts_cache(blob_sha, extractor_id, extractor_version, uri)
principals / grants(principal, repo_id, permission)
jobs(id, kind, payload, state, attempts, leased_until)
```

Local mode uses SQLite; service and org modes use Postgres. Jobs use the
catalog itself (`SELECT … FOR UPDATE SKIP LOCKED` on Postgres) so there is no
extra queue to operate.

### 8.4 Garbage collection

Segments are retained while any ref or retention policy points at them.
Content pack blobs are reference-counted through segments and compacted
offline. Closing a pull request releases its delta segment.

## 9. Extractors

### 9.1 Language packs

A language pack is data-first:

- a tree-sitter grammar (the upstream Rust crate),
- tree-sitter **query files** (`.scm`) that capture definitions, references,
  imports, and contract hints,
- a small amount of Rust for what queries cannot express (scope rules, import
  path normalization, qualified-name construction).

Keeping extraction in query files makes packs reviewable by people who know
the language but not Telar's internals, and keeps per-language code small.

Grammars are pinned in a `telar-grammars` crate. The binary links exactly one
tree-sitter runtime (0.27), so grammars whose published crates require an
older runtime (Kotlin, Dart, Dockerfile, Helm/Go templates) are vendored at
pinned commits. Every grammar has a golden-corpus test so an upgrade that
renames nodes fails CI. Seed queries come from permissively licensed sources
(upstream `tags.scm`/`locals.scm`, nvim-treesitter, Sourcegraph scip-syntax,
bloop) with SPDX provenance per file; see RFC 0002 §3.

### 9.2 Framework and file-kind packs

Separate packs recognize frameworks on top of a language pack: route
declarations (Flask, FastAPI, Django, Laravel, Express, NestJS, Spring, Ktor,
Gin…), HTTP / gRPC client calls, message producers and consumers, dependency
injection. File-kind packs cover OpenAPI, protobuf, GraphQL schemas,
Dockerfile, compose, Kubernetes, Helm, Terraform, CI pipelines, Markdown and
ADRs.

Data file kinds use pure-Rust parsers rather than tree-sitter: `saphyr` for
YAML (the tree-sitter YAML grammar has an open out-of-bounds write),
`protox-parse` for protobuf, `sqlparser` for SQL DDL, `hcl-edit` for HCL,
`pulldown-cmark` for Markdown, `oas3` for OpenAPI.

### 9.3 Precision layer

Optional, additive, per language. Every supported language must work without
it, and the benchmark reports both configurations.

| Language | Source | Notes |
|---|---|---|
| Python | pyrefly (`--report-glean`) for call edges; scip-python for references | pyrefly emits explicit caller/callee facts; scip-python is maintenance-only |
| TypeScript / JavaScript | scip-typescript | tied to the TypeScript 6 compiler API |
| Go | scip-go | |
| Java | scip-java | requires a full compile |
| Kotlin | scip-java + scip-kotlinc | Gradle only, no Android |
| PHP | Telar PHPStan extension (collectors) emitting Telar facts | scip-php is not viable |
| Swift | index-store import from an Xcode / SwiftPM build | macOS CI only; otherwise tree-sitter only |
| Ruby, Dart, Rust, C#, C/C++ | scip-ruby, scip-dart, rust-analyzer, scip-dotnet, scip-clang | tier 2 |

Rules:

- **SCIP gives references, not calls.** Call edges are derived by assigning
  each reference to the innermost enclosing callable (`enclosing_range`, or the
  tree-sitter span when absent) and classifying call vs. non-call with
  tree-sitter. Dynamic dispatch expands over `is_implementation`.
- The importer streams one `Document` at a time (indexes reach ~2 GB), reads
  both deprecated and typed range fields, and ignores occurrences for files
  whose blob differs from the indexed commit.
- Telar IDs stay primary; SCIP symbols are stored as aliases with the version
  component dropped. `local N` symbols never become nodes.
- In service and org modes, precision artifacts are **uploaded from the
  adopter's CI**, keyed by commit. Telar never runs adopter builds or
  dependency installs server-side. Local mode may run installed indexers.
- Matching edges are upgraded to `Scip` (or `TypeResolved`) strategy;
  heuristic edges contradicted by the precision layer are kept with lowered
  confidence.

### 9.4 Contracts

Extractors emit `ContractHint { side: Provides|Consumes, kind, raw_key, span }`.
Canonicalization is shared across languages:

- HTTP: method + path template with parameters normalized
  (`/users/:id`, `/users/{id}`, `/users/<id>` → `/users/{}`), base URL and
  router prefixes composed when statically known.
- gRPC: `package.Service/Method` from `.proto` and from generated stubs.
- Topics: broker kind + topic name, including constants resolved within the
  snapshot.
- Env vars: name; providers are deployment manifests, consumers are code.

## 10. Org linker

The org linker runs after base segments of default branches change. It loads
the contracts of every repository in the organization, matches consumers to
providers by canonical key, and writes an **org-link segment** with
`CROSS_*` edges and a match explanation per edge (exact key, template match,
ambiguous). Unmatched consumers are kept as dangling contracts so agents can
see "this service calls an endpoint nobody in the org provides".

The org-link segment is small and versioned independently, so adding a
repository does not reindex any other repository.

## 11. Query engine

### 11.1 Views

A view is built per request:

```
view = org_link_segment
     + for each visible repository: base segment [+ delta segments]
```

Visibility is the intersection of the caller's grants (§13) and the scope the
caller asked for. Query nodes cache opened segments (LRU by bytes) and keep no
other state.

### 11.2 Local overlay

In local mode the developer can point Telar at a remote service. The view is
the remote organization view plus a **local delta segment** built from the
working tree (including uncommitted changes) against the remote base of the
current branch. The developer sees the whole organization and their current
code without indexing the organization locally.

### 11.3 Operations

| Operation | Notes |
|---|---|
| Lookup | by ID, qualified name, suffix, or file |
| Symbol search | BM25 over the symbol FTS index, structural filters (label, path glob, degree) |
| Code search | literal and regex over content: regex literals planned into n-gram queries, candidates verified against content packs, scan budget for literal-free patterns |
| Semantic search | optional; vectors per symbol, hybrid with BM25 via reciprocal rank fusion |
| Traverse | callers / callees / dependents, bounded depth, edge-type and confidence filters, paginated |
| Snippet / outline | from content packs, with neighbours |
| Impact | diff (or list of symbols) → affected symbols, tests, routes, and cross-repo consumers |
| Architecture | languages, packages, entry points, routes, hotspots, layers, clusters, cycles |
| Graph query | read-only openCypher subset executed on the view (no writes, bounded cost) |

Traversals execute in process over adjacency loaded from segments, not as one
SQL statement per hop. All operations take a deadline and a result budget and
return explicit truncation markers and cursors.

### 11.4 MCP surface

Tools (names provisional, final list in RFC 0003):

- `search_symbols`, `search_code`, `get_symbol`, `get_snippet`, `get_file_outline`
- `trace` (calls / data flow / cross-service), `impact`
- `get_architecture`, `query_graph`, `get_schema`
- `list_repositories`, `index_status`, `check_coverage`
- `list_contracts` (provided / consumed / dangling)

Output conventions: compact by default, fully pinned IDs plus host URLs,
explicit `truncated` / `next_cursor`, token budget parameter on every tool.

Transports: stdio (local) and Streamable HTTP (service / org), built on the
official `rmcp` SDK. Target protocol: MCP 2026-07-28 (stateless, no
`initialize` handshake or session IDs, `server/discover`, deterministic
`tools/list` ordering), with legacy protocol versions still served for older
clients. In stdio mode all logs go to stderr.

## 12. Supported languages

| Tier | Meaning | Languages |
|---|---|---|
| 1 | Benchmarked, release-blocking | Python, TypeScript/JavaScript, PHP, Go, Java/Kotlin, Swift |
| 2 | Extracted, benchmarked, not release-blocking | Ruby, C#, Rust, SQL, Dart |
| 3 | Definitions and text only | anything with a tree-sitter grammar |

File kinds (OpenAPI, protobuf, YAML/k8s/Helm, Terraform, Dockerfile, Markdown)
are tier 1 for contract extraction.

Precision expectations for tier 1 in org mode: Python, TS/JS, Go, Java use
compiler-grade indexes when uploaded; Kotlin is partial (Gradle, no Android);
PHP uses the PHPStan extension; **Swift is tree-sitter only** unless an index
store is uploaded from macOS CI. Benchmark results are reported per
configuration.

## 13. Security and permissions

- **Permissions (service / org):** repositories, visibility, and
  per-principal grants are mirrored from the host (GitHub App installation,
  GitLab membership, Bitbucket workspace permissions). Sync is user-centric
  first, invalidated by membership webhooks, and backed by a periodic
  reconcile (GitHub does not redeliver failed webhooks). **Deny by default**
  until a principal has been synced. Every query view is filtered by the
  caller's grants before execution.
- **MCP authorization (service / org):** Telar is an OAuth 2.1 **resource
  server** per the MCP 2026-07-28 authorization spec: RFC 9728 protected
  resource metadata, bearer validation with audience check,
  `WWW-Authenticate` challenges and `insufficient_scope` step-up. `rmcp`
  provides only client-side auth, so this layer is Telar's. Tokens are issued
  either by an external OIDC provider (Telar validates via JWKS) or by a
  minimal embedded authorization server that federates login to GitHub /
  GitLab. **Code-host tokens are never accepted as MCP bearers** and never
  forwarded; they stay server-side for permission sync.
- **Local:** no network access unless a remote is configured.
- Content never leaves the deployment. Telar has no telemetry.
- **Untrusted input:** extraction runs in isolated worker processes with
  per-blob time, memory, size and nesting limits; tree-sitter cancellation via
  progress callbacks and match limits; data file kinds use pure-Rust parsers
  (§9.2); query files and parsers are fuzzed. Sandboxed (WASM) grammars are
  evaluated later. No adopter code or build is executed server-side (§9.3).

## 14. Deployment modes

| Component | Local | Service | Org |
|---|---|---|---|
| Connector | working tree watcher | GitHub/GitLab poller | webhooks + reconcile poller |
| Indexer | in process | in process | N worker processes |
| Segment store | `~/.cache/telar` | object storage | object storage |
| Catalog | SQLite | Postgres | Postgres |
| Query engine | in process | in process | N stateless query nodes |
| MCP | stdio | HTTP | HTTP behind a load balancer |
| Git reads | `gix` | `gix` | `gix` |
| Git network | — (working tree) | `git` CLI | `git` CLI |

All three are the same binary with different configuration
(`telar local`, `telar serve`, `telar worker` / `telar query`).

**Git transport.** All object reads (trees, blob SHAs, tree diffs, rev-walk,
status, ignore rules) use `gix`. Network operations shell out to the `git`
CLI (≥ 2.49), because gix has no partial-clone support and libgit2 cannot
open partial clones. Per repository Telar keeps a bare, blobless, depth-1
cache: fetch the new commit with `--filter=blob:none`, diff trees with gix,
fetch exactly the changed blob OIDs, read them with `GIT_NO_LAZY_FETCH=1`
(the Zoekt indexserver pattern). Caches expire by time rather than repack.
Hosts without partial clone (Bitbucket Cloud) fall back to depth-1 fetches or
the host API. Telar computes its own tree diffs (host compare APIs truncate).
Service and org images therefore include `git`; the local binary stays fully
static.

## 15. Benchmark and adoption gate

`telar-bench` is part of the repository and runs in CI.

**Corpus:** public repositories at pinned commits, at least two per tier-1
language, plus a polyglot microservice system for cross-service edges
(e.g. Google's Online Boutique). No proprietary code.

**Suites:**

1. **Localization** — issues / PRs from the corpus repositories; the gold
   answer is the set of files and functions touched by the fix. Metrics:
   Acc@1/3/5 (file), Acc@5/10 (function), recall, NDCG. Method follows
   LocBench.
2. **Cross-service edges** — hand-labelled provider/consumer pairs. Metrics:
   precision and recall per contract kind, at contract and service level.
3. **Structural questions** — callers, callees, hubs, dependency manifests,
   call chains, with gold answers derived and reviewed per repository.
4. **Agent end-to-end** — the same agent and model answer a fixed question
   set with Telar vs. with only file reading and grep. Metrics: answer quality
   (graded), tokens, tool calls.
5. **Operational** — index time, peak memory, segment and pack bytes per
   repository, query latency p50/p95.

**Baselines:** codebase-memory-mcp v0.12.0 and plain file exploration, run
outside Telar with recorded outputs.

**Adoption gate:** for a given language, Telar is recommended over the
baseline when it matches or beats it on suites 1–3 and is not worse than
10% on suite 4. Results are published per release.

## 16. Code organization

Cargo workspace:

| Crate | Responsibility |
|---|---|
| `telar-model` | IDs, labels, edge types, facts, provenance — no I/O |
| `telar-grammars` | pinned and vendored grammars, golden-corpus tests |
| `telar-extract` | extractor trait, tree-sitter runtime, query-file loader |
| `telar-lang-*` | one crate per language pack |
| `telar-packs` | framework and file-kind packs, contract canonicalization |
| `telar-scip` | SCIP import |
| `telar-resolve` | per-snapshot and delta resolution |
| `telar-segment` | segment and content-pack formats, reader/writer traits |
| `telar-catalog` | catalog trait, SQLite and Postgres implementations |
| `telar-source` | connectors: local, GitHub, GitLab, Bitbucket |
| `telar-index` | pipeline orchestration and jobs |
| `telar-orglink` | contract matching and org-link segments |
| `telar-query` | views, operations, openCypher subset |
| `telar-mcp` | MCP server, stdio and Streamable HTTP |
| `telar-cli` | the `telar` binary |
| `telar-bench` | corpus management, suites, reports |

Dependency rule: `telar-model` depends on nothing internal; extractors depend
only on `telar-model` and `telar-extract`; nothing below `telar-query` knows
about MCP.

Core traits (sketch):

```rust
trait Extractor: Send + Sync {
    fn id(&self) -> ExtractorId;
    fn version(&self) -> u32;
    fn accepts(&self, path: &str, head: &[u8]) -> bool;
    fn extract(&self, path: &str, bytes: &[u8], budget: &Budget) -> Result<FileFacts, ExtractError>;
}

trait Source: Send + Sync {
    async fn repositories(&self) -> Result<Vec<RepoRef>>;
    async fn refs(&self, repo: &RepoRef) -> Result<Vec<RefHead>>;
    async fn tree(&self, repo: &RepoRef, commit: &CommitId) -> Result<Tree>;
    async fn blobs(&self, repo: &RepoRef, shas: &[BlobSha]) -> Result<BlobStream>;
    async fn grants(&self, repo: &RepoRef) -> Result<Vec<Grant>>;
}

trait SegmentWriter { fn write(&mut self, graph: &ResolvedGraph, files: &FileTable) -> Result<SegmentMeta>; }
trait SegmentReader: Send + Sync { /* nodes, edges, fts, trigram, adjacency */ }
trait Catalog: Send + Sync { /* repos, refs, snapshots, segments, grants, jobs */ }
```

## 17. Milestones

| Milestone | Content | Exit criterion |
|---|---|---|
| M0 | RFC accepted, workspace skeleton, bench corpus pinned, baselines recorded | baseline report exists |
| M1 | Python end to end in local mode: extract → resolve → segment → query → MCP (`search_symbols`, `trace`, `get_snippet`) | suites 1–3 measured for Python |
| M2 | PHP + TypeScript, delta segments, local overlay of working tree | Python at parity with baseline |
| M3 | Service mode: object storage, Postgres catalog, GitHub connector, MCP over HTTP, permissions | indexes a 50-repo org without local clones |
| M4 | Contracts + org linker, framework packs for tier-1 languages | cross-service suite beats baseline |
| M5 | Remaining tier-1 languages, SCIP import, full tool surface | tier-1 parity on suites 1–3 |
| M6 | Org mode at scale: workers, query nodes, GitLab/Bitbucket | 500-repo org, published operational numbers |

## 18. Alternatives considered

- **Wrap codebase-memory-mcp.** Fastest path to capability, but it ties Telar
  to a large C codebase with a local-machine data model (SQLite file per
  project, sources read from checkouts). Rejected; it remains the benchmark
  baseline.
- **One SQLite file per segment.** Simplest to build and debug, but no custom
  FTS5 tokenizer from Rust, whole-file downloads from object storage, and no
  regex index. Replaced by the bundle format (§8.1); kept as an export.
- **Postgres (or another server database) as the graph store.** Simplifies
  multi-writer access, but ranking, recursive traversal, and per-hop latency
  degrade, and local mode would need a server. Rejected for segments; kept for
  the catalog only.
- **Embedded graph database.** Adds a large dependency for what is a
  read-mostly, immutable graph that fits adjacency arrays. Not needed for v1.
- **Zoekt for code search.** Mature, but adds a second index and process.
  Trigram search inside segments keeps one artifact per snapshot; revisit if
  the operational suite shows it is insufficient.
- **One synthetic mega-repository for cross-repo queries.** Slow to converge,
  memory-bound, and couples every repository's indexing. Replaced by per-repo
  segments plus a small org-link segment.

## 19. Open questions

1. Hotcache contents: which parts of each component must be in the hotcache
   for a cold open from object storage to stay within two range reads?
2. Delta resolution correctness: which reverse-dependency set is enough to keep
   PR views equal to a full re-index? Needs a differential test.
3. Embeddings: ship a small local model, or make semantic search opt-in with an
   external endpoint? Default must work offline.
4. openCypher subset: which constructs are worth supporting beyond MATCH /
   WHERE / RETURN / aggregation / variable-length paths?
5. Host permission sync for GitLab and Bitbucket: polling cost at org scale.
6. Licensing of tree-sitter grammars bundled in the binary (all candidates are
   MIT/Apache; to be verified per grammar).
7. Embedded authorization server vs. external OIDC only: is the embedded one
   worth owning, given the security surface?
8. Job queue: `apalis` 1.0 (SQLite + Postgres backends) or an in-house
   `SKIP LOCKED` table?
