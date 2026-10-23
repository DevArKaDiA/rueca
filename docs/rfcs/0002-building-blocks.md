# RFC 0002 — Building blocks: what to assemble, what to build

- Status: Draft
- Created: 2026-10-22
- Depends on: RFC 0001 (architecture)
- Research date: 2026-10-22. Versions, dates and licenses were checked against
  crates.io, GitHub, Hugging Face and upstream sources on that day. Claims that
  could not be checked against a primary source are marked *(unverified)*.

## 1. Summary

For every layer of RFC 0001 this document lists existing crates, projects,
query collections, datasets and harnesses, with a verdict:

- **Dependency** — use the crate as is, pinned, behind a Telar trait.
- **Vendor + adapt** — copy files or logic into Telar with attribution.
- **Reference** — study the design; do not copy code.
- **Avoid** — license, maintenance, or fit problem.

Bottom line: roughly two thirds of Telar can be assembled from mature,
permissively licensed parts. What has **no** off-the-shelf solution, and is
therefore Telar's real engineering work, is:

1. **Name and call resolution** per language (no maintained Rust resolver
   exists; stack-graphs is archived and cannot link with current tree-sitter).
2. **Extraction query packs** at graph quality (existing `tags.scm` files are
   thin).
3. **Contract rule packs** for routes, HTTP clients, RPC and topics (no
   permissive rule library exists).
4. **A read-only Cypher executor** (parsers exist; executors do not).
5. **The segment bundle format** that ties the indexes together.
6. **The MCP resource-server auth layer** (the official SDK is client-side
   only for auth).

Section 10 lists the amendments this research implies for RFC 0001.

## 2. Licensing policy

Telar is Apache-2.0. Rules for everything that enters the repository:

| Source license | Policy |
|---|---|
| MIT, Apache-2.0, BSD, ISC, Unlicense, Zlib, CC0 (data only) | Allowed as dependency or vendored with attribution |
| MPL-2.0 | Dependency allowed. Vendored files stay separate and keep MPL headers (file-level copyleft) |
| LGPL, GPL, AGPL | Never vendored. Separate-process tools only (e.g. the `git` CLI) |
| PolyForm Noncommercial, Commons Clause, BUSL, FSL, SSPL, CC-BY-NC, "rules license" | Avoid entirely; do not read for copying |

Every vendored `.scm`, rule, or data file carries an SPDX header and an entry
in `NOTICE` (REUSE layout). CI runs a license check (`cargo-deny`) on
dependencies and on vendored files.

Sources explicitly excluded by this policy:

- GitNexus (PolyForm Noncommercial 1.0.0)
- Zed language queries (GPL-3.0-or-later)
- Serena application (GPL-3.0; its SolidLSP module is MIT)
- semgrep-rules (Semgrep Rules License, not OSI) and opengrep-rules (Commons Clause)
- Sourcebot (FSL core; closed source since v6.0.0, 2026-10-10)
- FalkorDB (SSPL)
- CORE-Bench dataset (CC-BY-NC-SA)
- Non-commercial embedding models: jina-code-embeddings-0.5b/1.5b, SFR-Embedding-Code (CC-BY-NC-4.0)

## 3. Parsing and extraction

### 3.1 Runtime constraint

The `tree-sitter` crate declares `links = "tree-sitter"`, so **one runtime
version per binary**. Any crate with a hard dependency on an older runtime
cannot be linked next to it. Modern grammar crates depend only on
`tree-sitter-language ^0.1` and are compatible. ast-grep 0.51 already builds a
large grammar set on tree-sitter 0.27, which is a useful compatibility proof.

| Component | Candidate | License | Version | Verdict |
|---|---|---|---|---|
| Runtime | `tree-sitter` | MIT | 0.27.1 (2026-10-08), ABI 13–15 | **Dependency.** Use `progress_callback` timeouts, `set_match_limit`, size/depth limits |
| Sandboxed grammars | `tree-sitter` feature `wasm` (wasmtime) | MIT | same | **Optional later** for untrusted-input hardening |
| Tags runner | `tree-sitter-tags` | MIT | 0.27.1 | Reference; its model lacks containment, imports, qualified names |
| Runtime grammar loading | `tree-sitter-loader` | MIT | 0.27.1 | Avoid (needs a C compiler at runtime, non-deterministic) |
| Grammar bundles | `tree-sitter-language-pack`, `arborium` | MIT | — | Avoid (download prebuilt parsers / forked runtime) |

### 3.2 Grammars

| Language / kind | Source | License | Verdict |
|---|---|---|---|
| Python, JS, TS/TSX, PHP, Go, Java, Ruby, C#, Rust | tree-sitter org crates | MIT | Dependency |
| Swift | `tree-sitter-swift` (alex-pinkus) 0.7.4 | MIT | Dependency, pinned; known misparses (#153, #240) |
| **Kotlin** | fwcd grammar (canonical; crates.io release incompatible with 0.27, 0.4.0 unreleased) | MIT | **Vendor at pinned commit** (the revision nvim-treesitter pins). Fallback `tree-sitter-kotlin-sg` |
| **Dart** | UserNobody14 (canonical, not on crates.io) | MIT | **Vendor at pinned commit** |
| SQL embedded in code | `tree-sitter-sequel` | MIT | Dependency |
| protobuf (spans) | `tree-sitter-proto` | MIT | Dependency |
| Markdown | `tree-sitter-md` | MIT | Dependency, or `pulldown-cmark` |
| HCL | `tree-sitter-hcl` 1.1.0 | Apache-2.0 | Dependency, or `hcl-edit` |
| **Dockerfile** | camdencheek grammar (crate needs tree-sitter ^0.20) | MIT | **Vendor at pinned commit**, or hand-written parser |
| **Helm / Go templates** | ngalaiko `tree-sitter-go-template` (not on crates.io) | MIT | **Vendor** |
| **YAML** | `tree-sitter-yaml` 0.7.2 | MIT | **Avoid on untrusted input**: unpatched out-of-bounds write (#48) and int16 overflow on long files (#49) |

All grammars live in a `telar-grammars` crate with a golden-corpus test per
grammar, so an upgrade that renames nodes fails CI instead of silently
breaking queries.

### 3.3 Pure-Rust parsers for data file kinds

Preferred over tree-sitter for formats where semantics matter and input is
untrusted:

| Kind | Crate | License |
|---|---|---|
| YAML (k8s, values, OpenAPI) | `saphyr` 0.2.2 | MIT OR Apache-2.0 |
| protobuf semantics | `protox` / `protox-parse` 0.10.0 | MIT OR Apache-2.0 |
| SQL DDL | `sqlparser` 0.63.0 | Apache-2.0 |
| HCL / Terraform | `hcl-edit` / `hcl-rs` | MIT OR Apache-2.0 |
| Markdown | `pulldown-cmark` 0.13.5 | MIT |
| OpenAPI | `oas3` 0.22 / `openapiv3` 2.2 | MIT / MIT-Apache |

### 3.4 Query collections to seed extraction packs

| Source | License | Value | Verdict |
|---|---|---|---|
| Upstream grammar `tags.scm` / `locals.scm` | MIT | Thin seed; locals only for ts, js, ruby, swift, dart | Vendor + adapt |
| nvim-treesitter `locals.scm` | Apache-2.0 | Locals for all tier-1 languages; pins grammar revisions | Vendor + adapt (strip Neovim-only predicates) |
| Sourcegraph `scip-syntax` queries (`scip-tags.scm`, locals DSL), public snapshot | MIT (directory license) | Best-designed queries for containment and locals: py, ts, js, go, java, kotlin, ruby, c#, rust, dart, sql | **Vendor + adapt** (frozen at tree-sitter 0.20.9) |
| bloop `scopes.scm` | Apache-2.0 | Scope queries for py, go, java, js, ts, php, ruby, c#, rust (no Kotlin/Swift/Dart) | Vendor + adapt |
| Helix queries | MPL-2.0 | Tags and locals | Reference (or isolated MPL files) |
| Zed queries | GPL-3.0 | — | Avoid |

**Written from scratch for every language:** imports, inheritance,
containment with qualified names, and calls with receivers. No permissive
source has these at graph quality. SQL, HCL, Dockerfile, YAML/k8s, Helm and
OpenAPI need semantic extractors, not queries.

### 3.5 Matching engine for rules

| Candidate | License | Verdict |
|---|---|---|
| `ast-grep-core` / `ast-grep-config` 0.51.0 | MIT | **Dependency (optional)** for framework and contract rules written as YAML patterns |

## 4. Name resolution and the precision layer

### 4.1 Syntactic resolution (always on)

No maintained, linkable Rust resolver exists.

| Prior art | License | Status | Verdict |
|---|---|---|---|
| stack-graphs + tree-sitter-stack-graphs | MIT OR Apache-2.0 | Archived 2025-09; needs tree-sitter ^0.24 | Reference (algorithm and paper) |
| tree-sitter-graph | MIT OR Apache-2.0 | Stalled; needs ^0.24 | Avoid |
| bloop scope graph (`scope_resolution`) | Apache-2.0 | Archived 2024-12 | **Vendor + adapt** design: per-file scopes, defs, imports, refs, hoisting |
| Sourcegraph scip-syntax locals resolver | MIT | Archived 2024-09 | **Vendor + adapt** |
| codanna resolvers + project resolvers | Apache-2.0, 0.16.0, active | Per-language resolution (e.g. Python LEGB + MRO); project config readers for tsconfig, go.mod, pom/gradle, composer PSR-4, pyproject, Package.swift, csproj | **Vendor + adapt** (closest Rust prior art) |
| `oxc_resolver` | MIT, 11.24, very active | Exact Node/TS module resolution | **Dependency** for TS/JS imports |
| ruff crates, rust-analyzer | MIT/Apache | Unstable 0.0.x APIs | Reference |

Telar resolver design (adopted from the above):

1. Per-file scope graph for locals (bloop / scip-syntax model).
2. Per-language project module resolution (codanna providers; `oxc_resolver`
   for TS/JS).
3. Global symbol table keyed by qualified name, joined through imports.
4. Name-matching fallback with an explicit confidence value.

### 4.2 Compiler-grade precision (optional, additive)

SCIP is the interchange format. Governance moved to an independent steering
committee (Meta, Uber, Sourcegraph) in March 2026; repository
`scip-code/scip`, Apache-2.0.

**SCIP gives references, not calls.** Call edges are derived by assigning each
non-definition reference to the innermost enclosing callable
(`enclosing_range`) and classifying call vs. non-call with tree-sitter. A
third-party measurement of this method on Java found about 60% precision and
36% recall for call edges, so SCIP improves resolution but does not replace
Telar's own call extraction.

| Language | Indexer | License | Status | Enclosing ranges | Verdict |
|---|---|---|---|---|---|
| Python | scip-python | MIT | Maintenance only; based on pyright 1.1.301 (2023) | Yes | Usable, stale |
| Python | **pyrefly** `--report-glean` | MIT | 1.4.0, active | — (emits explicit `FileCall`, `CalleeToCaller` facts) | **Preferred for Python call edges**; Glean JSON reader needed |
| TS / JS | scip-typescript | Apache-2.0 | 0.5.2, active; tied to the TS 6 compiler API (TS 7 has no programmatic API until 7.1) | Yes | Dependency (external tool) |
| Go | scip-go | Apache-2.0 | 0.2.9, active | Yes (since 2026-09) | Dependency (external tool) |
| Java | scip-java | Apache-2.0 | 0.13.2, active; full compile required | Yes | Dependency (external tool) |
| Kotlin | scip-java + scip-kotlinc | Apache-2.0 | Gradle only, no Android, "less mature" | Yes | Partial |
| **PHP** | scip-php | MIT | Last release 2023, no enclosing ranges, no relationships | No | **Not viable.** Build a small PHPStan extension (collectors, MIT) that emits Telar facts; watch Mago 2.0 |
| **Swift** | — (no SCIP indexer); IndexStoreDB from an Xcode/SwiftPM build | Apache-2.0 | Needs macOS build | — | **Tree-sitter only** in org mode; optional index-store import when the adopter's macOS CI uploads it |
| Ruby, Dart, Rust | scip-ruby, scip-dart, rust-analyzer scip | Apache-2.0 / MIT | Active | Yes | Tier 2 |
| C#, C/C++ | scip-dotnet, scip-clang | Apache-2.0 | Active | No | Tier 2/3 |

Importer components:

| Component | Candidate | License | Verdict |
|---|---|---|---|
| Symbol grammar | `scip` crate 0.10.0, `scip::symbol` | Apache-2.0 | **Dependency** (symbol parsing) |
| Protobuf types | own `prost` types generated from `scip.proto` | — | **Build**: the crate pins `protobuf =3.7.2` and has no streaming reader; indexes reach ~2 GB |
| Prior art | Glean `scip_to_glean` (BSD, Rust); Searchfox `scip-indexer.rs` (MPL-2.0); scip-io (MIT, orchestration) | — | Reference |
| Evaluation | scip-syntax `evaluate --candidate --ground-truth` | MIT | **Copy the idea**: score Telar's syntactic resolution against SCIP per language |

Rules:

- Telar's own ID stays the primary key. SCIP symbols are stored as aliases,
  with `<version>` dropped from the canonical key.
- Read both the deprecated and the typed range fields (a real project lost
  all scip-go edges by reading only one).
- In org mode, indexes are **uploaded from the adopter's CI**, keyed by commit.
  Telar does not run adopter builds server-side (arbitrary code execution).
- `local N` symbols are never graph nodes.

## 5. Contracts and framework detection

No maintained, permissive rule library for routes and HTTP clients exists.

| Source | License | What to take | Verdict |
|---|---|---|---|
| Gortex `internal/contracts` | Apache-2.0 | Provider/consumer contract model, matcher scoped per workspace, framework list (Flask, Django, Express, Nest, Spring, Laravel, Rails, ASP.NET, Axum…), wrapper-function inlining | **Vendor + adapt** (port logic to Rust) |
| codebase-memory-mcp `service_patterns.c`, `laravel_routing.c` | MIT | Library-identity tables for HTTP/async/gRPC clients | **Vendor data** |
| colbymchenry/codegraph `resolution/frameworks` | MIT | Framework resolver catalogue | Reference / vendor data |
| trace-mcp plugins | MIT | ~99 framework integrations taxonomy | Reference |
| CodeQL models-as-data YAML | MIT | Java/C#/Go library models | Vendor data with attribution |
| semgrep-rules, opengrep-rules | non-OSI | — | Avoid |

Rule packs are written by Telar, as `.scm` queries or ast-grep YAML.

## 6. Storage, search, graph, vectors

### 6.1 Findings that change RFC 0001

- **rusqlite cannot register a custom FTS5 tokenizer** without unsafe FFI, so
  code-aware tokenization in SQLite FTS needs workarounds.
- SQLite B-tree pages are a poor fit for range reads from object storage; in
  practice every segment must be downloaded whole.
- The proven pattern for immutable search segments in object storage is
  tantivy + a split bundle with a hotcache (Quickwit), now Apache-2.0.
- GitHub Blackbird (Rust) shards and deduplicates **by git blob**, and handles
  branches with per-blob visibility; Zoekt uses a per-blob branch bitmask.
  Both confirm Telar's blob-as-unit design.

### 6.2 Components

| Component | Candidate | License | Version | Verdict |
|---|---|---|---|---|
| Symbol full-text (BM25, custom tokenizer) | `tantivy` | MIT | 0.27.0 (2026-10-17) | **Dependency** behind a trait (breaking changes every minor) |
| Serving tantivy from object storage | Quickwit `quickwit-directories` (bundle, hotcache, storage, caching directories) | Apache-2.0 (repo; crates.io copies are stale AGPL) | — | **Vendor + adapt** from the repo |
| Regex planning / verification | `regex-syntax` literal extraction, `regex-automata`, `grep-searcher` | MIT/Apache, Unlicense/MIT | current | **Dependency** |
| n-gram index | v0: tantivy `NgramTokenizer` field (bloop did this); v1: own sparse-gram index with `fst` + `roaring` | MIT/Unlicense, MIT/Apache | — | Dependency now; build later if size/latency demand |
| Blob visibility for deltas | `roaring` 0.11.5 | MIT OR Apache-2.0 | — | **Dependency** |
| Graph adjacency | own mmap CSR arrays (`memmap2` + `bytemuck`/`epserde`) | MIT/Apache | — | **Build** (small) |
| Index-time graph algorithms | `petgraph` 0.8.3 | MIT OR Apache-2.0 | — | Dependency |
| Node/edge property tables | Arrow IPC (`arrow` 60) | Apache-2.0 | monthly majors | Dependency |
| SQL over segments | DataFusion 55 (lance-graph shows Cypher lowered to DataFusion) | Apache-2.0 | heavy | Optional feature |
| Cypher parser | `open-cypher` 0.5.0 (openCypher 2024.3, full TCK parse) | Apache-2.0 | single author | **Dependency**, pinned, ready to fork. Alternative `drasi-query-cypher` |
| Cypher executor | none mature in Rust (Kùzu archived 2025-10; LadybugDB fork is C++) | — | — | **Build**: bounded-expand planner over CSR |
| Embedded graph DBs | Kùzu (archived), CozoDB (dormant), IndraDB (low activity), Oxigraph (RDF), FalkorDB (SSPL) | — | — | Avoid |
| Vectors | flat SIMD scan by default; `usearch` 2.26 mmap view for large segments | Apache-2.0 | active | Build (flat) + optional dependency |
| Offline embeddings | `model2vec-rs` 0.3 + potion-code-16M-v2 (MIT, 256-d static) | MIT | — | **Dependency** (default) |
| Quality embeddings | `fastembed` 7.3 (ort) + jina-embeddings-v2-base-code (Apache-2.0) or CodeRankEmbed (MIT) | Apache-2.0 | ort still RC | Optional feature |
| Object storage I/O | `object_store` 0.14 (range reads, coalescing) | MIT/Apache | — | **Dependency** |
| Segment byte cache | `foyer` 0.22 (memory + NVMe) | Apache-2.0 | — | Dependency |
| Content packs | `zstd` with trained dictionaries; `zeekstd` seekable frames | BSD | — | Dependency; pack writer is **built** (~300 lines) |
| Content via git packfiles | `gix-pack` | MIT/Apache | — | Avoid as serving format (delta chains) |

### 6.3 Catalog and jobs

| Component | Candidate | License | Verdict |
|---|---|---|---|
| Catalog over SQLite and Postgres | `sqlx` 0.9 (two migration dirs, shared repository trait) or SeaORM 2 (one schema) | MIT/Apache | Dependency; pick in implementation |
| Job queue, both backends | `apalis` 1.0 (`apalis-sqlite`, `apalis-postgres`) | MIT/Apache | Dependency behind a `JobQueue` trait, or a ~300-line in-house `SKIP LOCKED` table (jobs are idempotent per blob) |
| Postgres-only queues | graphile_worker, underway, pgmq, sqlxmq | — | Reference / avoid |

## 7. Git, hosts, MCP, service

### 7.1 Git

| Need | Candidate | License | Verdict |
|---|---|---|---|
| Trees, blob SHAs, tree diff, rev-walk, status, ignore, attributes | `gix` 0.90 | MIT OR Apache-2.0 | **Dependency** for all local reads |
| Partial/blobless clone, fetch specific blobs | `git` CLI ≥ 2.49 (separate process) | GPLv2 (no linking) | **Dependency for network.** gix has no partial clone (issue #1046 closed not-planned); libgit2 cannot even open partial clones |
| Pure-Rust fetch | `gix-protocol` (supports `filter`) | MIT/Apache | Later option |
| libgit2 | `git2` | GPLv2 + linking exception | Avoid |

Fetch pattern, taken from Zoekt's indexserver: per repository a bare,
blobless, depth-1 cache; `git fetch --depth=1 --filter=blob:none <sha>`; gix
tree diff → changed blob OIDs; fetch those OIDs in one request; read with gix
under `GIT_NO_LAZY_FETCH=1`. Bitbucket Cloud does not support partial clone:
fall back to depth-1 without filter or to the API.

Host API limits that shape the design: GitHub `compare` caps at 300 files and
trees truncate at 100,000 entries, so Telar computes its own tree diffs;
GitHub does not auto-redeliver failed webhooks, so a reconcile loop is
mandatory.

### 7.2 Code-host clients

| Host | Candidate | License | Verdict |
|---|---|---|---|
| GitHub | `octocrab` 0.54 (App JWT, installation tokens, pagination) | MIT/Apache | Dependency; parse only needed webhook fields; HMAC verification in-house (`hmac` + `sha2`) |
| GitLab | `gitlab` (Kitware) 0.1904 | MIT/Apache | Dependency |
| Bitbucket Cloud / DC | no maintained crate | — | **Build** thin `reqwest` clients from the published OpenAPI specs |
| Gitea / Forgejo | `forgejo-api` 0.12 | Apache/MIT | Dependency (low priority) |

Permission sync follows Sourcegraph's documented model: user-centric first
(`GET /user/installations/{id}/repositories` on GitHub), invalidated by
membership webhooks, plus a periodic reconcile. Deny by default until a sync
has happened.

### 7.3 MCP

| Candidate | License | Version | Verdict |
|---|---|---|---|
| `rmcp` (official Rust SDK) | Apache-2.0 | 3.7.0 (2026-10-22) | **Dependency.** Implements spec 2026-07-28 (stateless) plus legacy versions; stdio and Streamable HTTP as a tower service |
| turbomcp `turbomcp-auth` | MIT | 3.6.0 | Reference for resource-server middleware |
| rust-mcp-sdk | MIT | 2.0.0 | Reference |

The 2026-07-28 spec removes the `initialize` handshake and session IDs, adds
`server/discover`, and deprecates Dynamic Client Registration in favor of
Client ID Metadata Documents. **rmcp's auth support is client-side only.**
Telar builds the resource-server side: RFC 9728 protected-resource metadata,
bearer validation with audience check, `WWW-Authenticate` challenges. The spec
forbids accepting or forwarding other tokens, so a GitHub token cannot be
Telar's MCP bearer; Telar either validates tokens from an external OIDC
provider or runs a minimal embedded authorization server (`oauth2` +
`jsonwebtoken`) that federates login to GitHub/GitLab and keeps the host token
server-side.

### 7.4 HTTP, local mode, observability, distribution

| Need | Candidate | License | Verdict |
|---|---|---|---|
| HTTP | `axum` 0.8, `tower-http` 0.7, `governor` | MIT | Dependency |
| OAuth / JWT | `oauth2` 5, `openidconnect` 4, `jsonwebtoken` 11 | MIT/Apache | Dependency |
| File watching | `notify` 8.2 + debouncer as trigger; `gix-status` as source of truth; `ignore` for walks | CC0 / MIT/Apache / Unlicense-MIT | Dependency |
| Tracing / metrics | `tracing` (stderr in stdio mode), `metrics` + Prometheus exporter; OpenTelemetry OTLP behind a feature (traces still beta) | MIT / Apache | Dependency |
| Release | `cargo-dist` 0.33, `cargo-zigbuild` for musl, native macOS runners, `mimalloc` | Apache/MIT | Dependency |

Binary size reference: difftastic ships dozens of grammars in ~11 MB
compressed; codebase-memory-mcp is ~47 MB. Grammars are gated behind cargo
features.

### 7.5 Reference designs for org-wide sync

| Project | License | Take |
|---|---|---|
| Zoekt indexserver (`zoekt-sourcegraph-indexserver`, `zoekt-mirror-*`) | Apache-2.0 | Fetch pattern, delta shards with fallback threshold, time-based clone expiry |
| Sourcegraph permission syncing docs | docs | Permission sync priorities and cost model |
| Hound, Livegrep, OpenGrok | MIT / BSD / CDDL | Anti-patterns at scale (polling, full rebuilds) |
| Sourcebot | FSL / closed | Reference only, no code |

## 8. Benchmark (telar-bench)

The repository ships **manifests and scripts only**: instance IDs, commit
SHAs, Telar's own labels, sha256-pinned tool binaries. Datasets and code are
fetched at runtime, which keeps the repository Apache-2.0 clean despite
unlicensed or copyleft upstreams.

| Suite | Reuse | License | Gaps |
|---|---|---|---|
| 1. Localization | ContextBench (1,136 tasks, human gold context spans, evaluator for agent trajectories); LocBench metrics; SWE-bench Multilingual (43 PHP); SWE-PolyBench (TS/JS); Kotlin bench by JetBrains (106) and Firebender (100); SWE-rebench V2 (Swift 362, Kotlin 889, PHP 1,445); Agent Retrieval Bench (budgeted context yield) | Apache-2.0 code / MIT / CC-BY-4.0 / several HF cards without license | **Swift** has no human-validated set; PHP and Kotlin have no gold-context set |
| 2. Cross-service edges | **CGraph cross-service bench** (MIT, merged 2026-10-14): 102 labelled edges, 5 systems, 44 repos, pinned SHAs, `score.py`, and a codebase-memory-mcp predictor; microSecEnD (385 Java connections); Online Boutique and OpenTelemetry demo (natively traced → dynamic oracle) | MIT / Apache-2.0 | Swift; TrainTicket (41 services) unlabelled |
| 3. Structural questions | No ground-truth dataset exists in any language. Generate gold from SCIP indexers at pinned SHAs; PyCG micro-benchmark (Python call graphs); SWE-QA with judge (Python); cbm question taxonomy and evaluation plan (MIT) as methodology | Apache-2.0 / MIT | All languages need generated gold |
| 4. Agent end-to-end | **Harbor** (MCP servers declared per task; Kotlin bench and LoLBench already in Harbor format) and **Inspect AI** (exact token and tool-call accounting, easy tool-subset arms) | Apache-2.0 / MIT | MCP-output → span extractor for ContextBench |
| 5. Operational | own harness; report medians of 5 runs on fixed runners | — | — |

Arms in every suite: plain grep + file reading, codebase-memory-mcp v0.12.0
(pinned by sha256), Telar at HEAD, and an issue-only / no-tools control to
measure contamination.

Label-building for the gaps: mine merged PRs with linked issues created after
model cutoffs (GraphQL `closingIssuesReferences`), filter to 1–5 non-test
files, map hunks to enclosing symbols with tree-sitter, LLM-screen, 10–20%
human spot check, refresh quarterly. Structural gold: SCIP references as the
precision oracle plus dynamic call traces from each repository's own test
suite as a recall lower bound.

Pinned corpus (excerpt): django, flask, requests (Python); vuejs/core,
nestjs/nest, express (TS/JS); laravel/framework, symfony (PHP); gin, caddy,
cli/cli (Go); gson, spring-petclinic (Java); okhttp, detekt, ktlint, ktor
(Kotlin); swift-argument-parser, swift-syntax, Alamofire, vapor (Swift).
Large repositories (vscode, kubernetes, linux) for the operational suite only.

## 9. Assemble vs. build — summary

| Layer | Assembled | Built by Telar |
|---|---|---|
| Parsing | tree-sitter runtime, ~16 grammar crates, pure-Rust data parsers | `telar-grammars` (vendoring 4 grammars, golden tests) |
| Extraction | seed queries (upstream, nvim-treesitter, scip-syntax, bloop), ast-grep core | **query packs at graph quality**, data-file extractors |
| Resolution | `oxc_resolver`, design from bloop / scip-syntax / codanna | **per-language resolver**, project-config readers |
| Precision | `scip::symbol`, external indexers, pyrefly | streaming SCIP importer, call derivation, PHPStan extension |
| Contracts | Gortex model, cbm / codegraph / CodeQL data | **rule packs**, canonicalization, org linker |
| Storage | tantivy, Quickwit directories, roaring, fst, arrow, object_store, foyer, zstd | **segment bundle format**, CSR, content packs, sparse-gram index (later) |
| Query | `open-cypher` parser, regex crates, petgraph | **Cypher executor**, views, traversal, impact |
| Vectors | model2vec-rs, usearch, fastembed | flat scan |
| Git / hosts | gix, git CLI, octocrab, gitlab crate | Bitbucket client, fetch orchestration, reconcile loop, permission sync |
| MCP / auth | rmcp, axum, oauth2, jsonwebtoken | **resource-server auth**, optional embedded authorization server |
| Ops | apalis or sqlx, tracing, metrics, cargo-dist | — |
| Benchmark | ContextBench, CGraph bench, SWE-rebench V2, Harbor, Inspect, PyCG | manifests, labelers, MCP-output extractors, Swift labels |

## 10. Amendments to RFC 0001 (applied 2026-10-22)

1. **§8.1 Segment format.** Replace "one SQLite file per segment" with a
   **bundle file per segment** modeled on Quickwit splits: manifest (format
   version, base segment, blob-visibility bitmap), mmap CSR adjacency per edge
   type, Arrow IPC property tables, a tantivy index for symbols (custom
   tokenizer) and n-grams, optional vectors, and a hotcache so an object-storage
   open takes one or two range reads. SQLite remains the catalog in local mode
   and an export format for debugging.
2. **§7 Deltas.** Express branch/PR deltas as a roaring bitmap of visible
   blobs over base + delta (Zoekt / Blackbird model), in addition to graph
   deltas.
3. **§9.3 SCIP.** Add pyrefly (Glean JSON) for Python calls, a PHPStan
   extension for PHP, and index-store import for Swift. Precision artifacts are
   uploaded from adopter CI; Telar never runs adopter builds server-side.
4. **§11.4 / §13 MCP and auth.** Target MCP spec 2026-07-28 with legacy
   support via rmcp. Telar implements the resource-server role; host tokens
   are never MCP bearers.
5. **§14 Git.** Network operations use the `git` CLI (blobless, depth-1,
   targeted OID fetch); gix for all reads. Service images therefore include
   `git`.
6. **§12 Tiers.** Mark Swift precision as tree-sitter only in org mode and PHP
   precision as PHPStan-extension based.
7. **Untrusted input.** Data file kinds (YAML, protobuf, SQL DDL, HCL) use
   pure-Rust parsers; extraction runs in isolated worker processes with
   time/memory limits; WASM grammars evaluated later.

## 11. Top risks

1. **Name resolution has no off-the-shelf solution** and is the largest and
   least certain workstream. Edge confidence must exist from day one.
2. **Grammar supply chain.** One runtime per binary, stale or forked crates
   for Kotlin, Dart, Dockerfile and Helm, and node renames on upgrade.
3. **Memory safety on untrusted input.** Runtime and external scanners are C;
   at least one widely used grammar has an open out-of-bounds write.
4. **Single-maintainer dependencies.** gix, octocrab, `open-cypher`, Swift
   grammar, several resolvers. Pin, wrap behind traits, be ready to fork.
5. **Fast-moving upstreams.** tantivy (breaking minors), arrow (monthly
   majors), rmcp (releases every 1–3 weeks), MCP spec (2026-07-28 was
   breaking), TypeScript 7 API gap.
6. **Benchmark validity.** Contamination on popular repositories, unlicensed
   datasets, and no human-validated Swift localization set.

## 12. Sources

Primary sources consulted (non-exhaustive):

- tree-sitter: https://github.com/tree-sitter/tree-sitter ; ast-grep: https://github.com/ast-grep/ast-grep
- Kotlin grammar release issue: https://github.com/fwcd/tree-sitter-kotlin/issues/242
- tree-sitter-yaml OOB write: https://github.com/tree-sitter-grammars/tree-sitter-yaml/issues/48
- nvim-treesitter: https://github.com/nvim-treesitter/nvim-treesitter
- Sourcegraph public snapshot (scip-syntax): https://github.com/sourcegraph/sourcegraph-public-snapshot
- bloop: https://github.com/BloopAI/bloop ; codanna: https://github.com/bartolli/codanna
- stack-graphs: https://github.com/github/stack-graphs ; paper: https://arxiv.org/abs/2211.01224
- Gortex: https://github.com/zzet/gortex ; codebase-memory-mcp: https://github.com/DeusData/codebase-memory-mcp
- Semgrep rules license: https://semgrep.dev/legal/rules-license
- SCIP governance: https://sourcegraph.com/blog/the-future-of-scip ; https://github.com/scip-code/scip
- SCIP calls discussion: https://github.com/scip-code/scip/issues/263
- pyrefly: https://github.com/facebook/pyrefly ; PHPStan collectors: https://phpstan.org/blog/using-phpstan-to-extract-data-about-your-codebase
- Mago: https://github.com/carthage-software/mago ; IndexStoreDB: https://github.com/swiftlang/indexstore-db
- Zoekt design: https://github.com/sourcegraph/zoekt/blob/main/doc/design.md
- GitHub Blackbird: https://github.blog/engineering/architecture-optimization/the-technology-behind-githubs-new-code-search/
- Cursor fast regex search: https://cursor.com/blog/fast-regex-search
- tantivy: https://github.com/quickwit-oss/tantivy ; Quickwit: https://github.com/quickwit-oss/quickwit
- open-cypher crate: https://crates.io/crates/open-cypher ; lance-graph: https://github.com/lance-format/lance-graph
- Kùzu → LadybugDB: https://thedataquarry.com/blog/from-kuzu-to-ladybug/
- gitoxide status: https://github.com/GitoxideLabs/gitoxide/blob/main/crate-status.md ; partial clone issue: https://github.com/GitoxideLabs/gitoxide/issues/1046
- libgit2 partial clone: https://github.com/libgit2/libgit2/issues/6880
- Zoekt git cache: https://github.com/sourcegraph/zoekt/blob/main/cmd/zoekt-sourcegraph-indexserver/git_cache.go
- Git protocol v2: https://git-scm.com/docs/gitprotocol-v2
- rmcp: https://github.com/modelcontextprotocol/rust-sdk ; MCP 2026-07-28: https://github.com/modelcontextprotocol/modelcontextprotocol/tree/main/docs/specification/2026-07-28
- GitHub REST limits: https://docs.github.com/en/rest/using-the-rest-api/rate-limits-for-the-rest-api
- Sourcegraph permission syncing: https://sourcegraph.com/docs/admin/permissions/syncing
- ContextBench: https://github.com/EuniAI/ContextBench ; LocAgent: https://github.com/gersteinlab/LocAgent
- SWE-rebench V2: https://huggingface.co/datasets/nebius/SWE-rebench-V2 ; Kotlin bench: https://github.com/Kotlin/kotlin-swe-bench
- CGraph cross-service bench: https://github.com/Nxtsoft/CGraph/pull/188 ; microSecEnD: https://github.com/tuhh-softsec/microSecEnD
- Harbor: https://github.com/harbor-framework/harbor ; Inspect AI: https://github.com/UKGovernmentBEIS/inspect_ai
- PyCG: https://github.com/vitsalis/PyCG ; SWE-Bench Illusion: https://arxiv.org/abs/2506.12286
