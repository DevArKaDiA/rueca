# RFC 0006 — The organization graph

- **Status:** Draft; phases 1–4 (§9) are implemented
- **Created:** 2026-10-26
- **Updated:** 2026-11-24
- **Depends on:** [RFC 0001](0001-architecture.md) (§8 storage, §10 org linker, §11 query engine,
  §13 permissions), [RFC 0005](0005-agent-responses.md) (responses, `repo` / `ref`)
- **Amends:** [RFC 0001](0001-architecture.md) §10–§11 (org linker → org layer, views), [RFC 0005](0005-agent-responses.md) §5
  (no rank fusion across repositories)

## 1. Summary

Rueca keeps two layers:

1. **The repository graph is the unit of work** — one graph per repository
   and ref: indexed, versioned, cached and garbage-collected on its own
   (RFC 0001 §8). A branch or pull request is a delta over its repository's
   base. Nothing in this layer changes.
2. **The organization graph is built on top** — when a set of repositories
   is configured as an organization, Rueca also maintains one graph that
   merges them: a **single symbol index** with organization-wide statistics,
   the union of the repository graphs, and a **link layer of
   cross-repository edges** (shared packages, then contracts). It is updated
   incrementally, one repository at a time.

The API exposes the scope: one repository, a list, or the whole
organization (the default in organization mode), at any indexed ref.

## 2. Motivation

RFC 0005 searched several repositories by querying each one and merging the
rankings with reciprocal rank fusion. The benchmark's first multi-repository
scenario (21 repositories in one store, one issue each) showed why that
fails: with `repo: "*"`, File Acc@5 drops to **9.5%** against **61.9%** when
the right repository is queried alone, and 94% of the top 10 comes from
other repositories. Rank fusion gives every repository's best hit the same
weight, so the 21 first places tie; BM25 scores from separate indexes cannot
be compared because each index has its own term statistics. The
organization suite that replaced that scenario (§8; 120 issues over 42
repositories) measures the same failure: per-repository search merged by
rank fusion keeps **24.2%** against **66.7%** alone, with 91.7% of the top
10 from other repositories.

A federated search treats the organization as N islands. Agents in an
organization ask questions that cross them: where is this endpoint
implemented, who calls this shared library, which service reads this
variable. The answer needs one ranking and real edges between repositories.

## 3. Layers

```text
                         organization layer (per organization, per generation)
   ┌───────────────────────────────────────────────────────────────────────┐
   │ org symbol index   one tantivy index, one internal segment per repo   │
   │ org link layer     cross-repository edges (packages, contracts)       │
   │ org catalog        repositories, default refs, snapshot per repo      │
   └───────────────────────────────────────────────────────────────────────┘
          ▲ built from                     ▲ built from
   ┌──────────────┐ ┌──────────────┐ ┌──────────────┐
   │ repo A @ main│ │ repo B @ main│ │ repo C @ main│   repository layer (unchanged):
   │  + deltas    │ │  + deltas    │ │  + deltas    │   segment per repo and ref,
   └──────────────┘ └──────────────┘ └──────────────┘   content packs, facts cache
```

### 3.1 Repository layer (unchanged)

Segments per repository and ref (base + deltas), content packs, facts cache,
the per-repository symbol and trigram indexes. A repository can be queried
alone, with or without an organization around it.

### 3.2 Organization symbol index

- **One tantivy index per organization.** Each repository's default-branch
  snapshot contributes its symbol documents as **one internal tantivy
  segment** (documents carry `repo`, the snapshot id and the segment-local
  node id, plus every field of the per-repository index).
- tantivy computes BM25 statistics over all segments of an index, so scores
  are **comparable across repositories by construction**.
- **Incremental:** re-indexing repository A deletes A's documents (by the
  `repo` term) and adds the new ones; other repositories are not touched.
  The merge policy keeps segments aligned to repositories so a replacement
  is cheap.
- **Scoped queries use the same index:** `repo: X` or a list adds a term
  filter before top-k. Priors (RFC 0004: tests, declarations, amalgamations,
  vendored code) are stored per document and apply unchanged.
- Code search (`search_code`) needs no global scoring: it runs on the
  per-repository trigram indexes in parallel and merges by (repo, path,
  line).

### 3.3 Organization link layer

Cross-repository edges, stored as edges between pinned symbol ids (or
`(snapshot, node)` pairs internally):

1. **Packages (first).** A package manifest maps a published name to a
   repository: `package.json` `name`, `pyproject.toml` / `setup.cfg`
   project name, `go.mod` module path, `composer.json` name, Maven
   `groupId:artifactId`, Gradle project names. An import in repository B that
   resolves to nothing inside B but names a package published by repository
   A is resolved against A's graph with A's own module resolution (the
   language pack's `resolve_import` over A's files), producing `IMPORTS` and,
   through the normal strategy ladder, `CALLS` / `INHERITS` / … edges into A.
   These edges carry the strategy and confidence they would have inside one
   repository, plus the evidence.
2. **Contracts.** HTTP routes, RPC methods, topics, env vars and
   config keys (RFC 0001 §9.4, RFC 0003 §7) matched across repositories: the
   original org linker, now one producer of the link layer. Implemented
   (§9.4) for HTTP routes and calls, OpenAPI operations and protobuf /
   gRPC services (topics: Kafka clients in Python only): every segment
   keeps a contract table of canonical keys; a repository that calls a key
   another one provides, or whose server code implements a spec another
   one publishes, depends on it, and the `CALLS` / `IMPLEMENTS` edges
   between them (strategy `contract`) go into the same link sets as the
   package edges. Env vars and config keys stay within one repository (the
   code ↔ config bridge).

The link layer is recomputed only for repositories whose snapshot changed
and for their reverse dependencies (repositories that import a package
they publish, or call or implement a contract they provide), kept in the
org catalog.

### 3.4 Organization catalog and generations

The organization catalog records the repositories, their default refs, the
snapshot each contributes and a **generation** number. An update (one
repository re-indexed) produces a new generation: new org-index segment for
that repository, recomputed link edges, then one atomic pointer swap.
Queries read one generation for their whole duration.

## 4. Refs over the organization

The organization layer is built from default branches. A query with a
`ref` on one repository (a pull request of A, a local working tree) sees:

> the organization at its current generation, with repository A replaced by
> A at that ref.

- **Search:** the org index with A's documents filtered out, plus A's ref
  segment scored **with the organization's term statistics** (tantivy
  `Bm25StatisticsProvider`), so its scores stay comparable; one ranking.
- **Graph:** A's ref graph replaces A's base graph; link edges whose A-side
  symbol still exists at the ref are kept, edges into removed symbols are
  dropped, and new imports of org packages in the ref are resolved on the
  fly.
- This is the local overlay of RFC 0001 §11.2 generalized: "the
  organization, seen from my branch".

As built (§9.3): the ref is a segment of the same organization index, so
the overlay is a selection of segments (no second statistics provider), and
a ref's new imports are linked when the ref is indexed, not on the fly.

## 5. Permissions

The organization index holds every repository; a caller sees only the
repositories they can read (RFC 0001 §13). The visible set is applied as a
filter **inside** the query before top-k and before any count, so results,
totals, truncation markers and link edges never reveal a repository the
caller cannot read. Link edges are returned only when both ends are
visible.

## 6. API

- **`repo`**: one repository, a list, or omitted. In organization mode an
  omitted `repo` means the whole organization; in local mode with a single
  root it means that root. `"*"` stays as an explicit "everything visible".
- **`ref`**: as RFC 0005; on the organization it applies to the repository it
  names (`ref: "pr/123"` with `repo: "acme/billing"` → §4 view).
- **`trace`** follows link edges across repositories; nodes carry their
  snapshot index in the RFC 0005 envelope, which lists the snapshots the
  answer touched (not every snapshot of the organization).
- A new tool, **`list_repositories`** (RFC 0001 §11.4), lists the
  organization's repositories, their default refs, indexed refs and
  languages, so an agent can narrow the scope when it wants to.
- Phase 4 adds **`list_contracts`** (RFC 0001 §11.4): the routes, spec
  operations and RPC methods the selected repositories provide, the calls
  they make, and the calls no visible repository provides (§9.4).

RFC 0005 §5's rank fusion across repositories is removed; a single ranking
comes from the organization index. Rank fusion remains only where scores
truly come from different engines (e.g. a future vector arm, RFC 0004 §11).

## 7. Local mode

An organization is a configuration, not a deployment:

```toml
# rueca-org.toml
name = "acme"
[[repo]]
path = "~/src/billing"      # or url = "https://github.com/acme/billing"
[[repo]]
path = "~/src/payments"
```

- `rueca index --org rueca-org.toml` indexes every repository (unchanged
  repository layer) and builds the organization layer in
  `<store>/orgs/<name>/`.
- `rueca mcp --org rueca-org.toml` serves the organization; `--root` keeps
  working for a single repository.
- Re-indexing one repository updates the organization incrementally (§3.2,
  §3.4).

## 8. Measuring it

The benchmark's multi-repository scenario becomes the organization suite:

| Metric | Today (gated split, n = 120; `docs/bench/alpha-results.md`) | Floor | Target |
|---|---|---|---:|
| File Acc@5, organization scope vs the right repository alone | 56.7% vs 66.7% (0.85×, 95% CI 0.74–0.96 ⚠; rank fusion: 24.2%) | ≥ 0.85 × single-repository | ≥ 0.95 × |
| Cross-repository edges (packages) precision / recall on labelled pairs | 12 pairs: 99.4 / 94.7; without Rust (8 pairs) 100.0 / 96.2; Rust alone (4 pairs) 98.3 / 93.1 | ≥ 90 / 75 | ≥ 97 / 90 |
| Incremental update: re-index one repository of 42 | micropython/micropython: organization layer 504 ms vs 2,681 ms (0.19×) | org layer ≤ 2× the repository's own index time | ≤ 1× |
| Tokens per correct answer, organization scope | 701 vs 542 single-repo (1.29×, 95% CI 1.13–1.50; top 10) | reported | ≤ 1.2 × single-repo |

The suite's population (`bench/README.md`, *Org scenarios*). Members are
every gated S1 repository (42, eight languages), each **pinned** at the
base commit of its first gated S1 instance and indexed one at a time into
one organization; the calibration organization adds 8 calibration members
pinned the same way, the gated members acting as distractors. **Issue**
instances: per member, the first 3 S1 instances (calibration: 5) whose gold
— the files and functions of the real fix patch — is present at the pin
and whose fix is not already there (`patch_at_pin != reverse`); the gold is
carried to the pin and the query is the issue's keyword query. Gated: 120
issue instances over 42 members (python 14, ts/js 15, php 21, go 15, java
15, c 12, cpp 10, rust 18); calibration: 29, never gated. **Impact**
instances (16 gated) ask the issue of a library member (tokio for axum;
carbon, monolog and commonmark for laravel/framework; gson for dubbo and
apollo) and score, besides its own gold, the consumer files that use a
symbol the fix changes, labelled at the pins by the independent
cross-repository labeler: they measure whether one organization ranking
surfaces the consumers of a changed library symbol (consumer hit@10 /
hit@20, recall@20). Every arm answers the same questions natively and per
repository merged by rank fusion, against its own single-repository answer
in the same run: Rueca (`--org`), BM25 over the union of the members (the
plain-text floor arm) and cbm (per project + rank fusion). Rates
carry Wilson intervals; Rueca − BM25 and the other pairs are paired
bootstraps that also state the discordance and the minimum detectable
effect (RFC 0004 §7.6): at n = 120 a File Acc@5 difference below 12.1
points can go undetected (29.0 at the 21 instances of the first scenario).

Cross-repository gold: pairs of corpus repositories where one depends on
another (a library and a consumer pinned to it), labelled from the
consumer's imports and uses by an independent labeler (Python `ast`, PHP
and Java `use` / `import`, Rust Cargo packages and module trees); pooled
over all 12 pairs, over the pairs without Rust and over the Rust pairs
alone.

## 9. Phasing

1. **Organization symbol index + scope API + local `--org`** (benchmark:
   organization File Acc@5, incremental update).
2. **Link layer, packages**: manifests → package registry → cross-repository
   import resolution and edges; `trace` across repositories.
3. **Refs over the organization** (§4), with the multi-ref collapse of
   RFC 0005 §6.
4. **Contracts** in the link layer (the original org linker; suite S2).

Phases 1–4 are implemented (§9.1–§9.4).

### 9.1 Phase 1 — implemented

The organization symbol index, the scope API and local `--org` landed in
`rueca-segment` (`org` module), `rueca-query` (organization mode,
`Visibility`, `list_repositories`), `rueca-index` (`org`: `rueca-org.toml`,
`index_org`) and the CLI / MCP server (`--org`). Choices and deviations:

- **Segments are the repositories' own.** The organization index does not
  re-index documents with extra `repo` / snapshot fields: every repository
  segment already embeds a tantivy index of one tantivy segment with doc id
  = node id, and the organization index lists those segments, copied
  unchanged (same schema, tokenizer, `body` text, priors). The repository,
  snapshot and store segment of a document are attributes of its tantivy
  segment, recorded in the generation; the hit is (repository, node id).
  "Delete by `repo` term, add the new documents" (§3.2) is therefore
  "drop that repository's segment from the list, add the new one" — the
  degenerate, cheapest form of a merge policy aligned to repositories — and
  an update is a file copy, no tokenization.
- **The `repo` filter is a segment filter.** Applied inside tantivy before
  top-k (the collector skips unselected segments without scoring them),
  which is what a term filter on `repo` would select. BM25 statistics are
  summed over the **visible** repositories (a `Bm25StatisticsProvider` over
  a subset of segments), so a repository the caller cannot read changes
  neither results, totals nor scores (§5), while narrowing `repo` keeps
  organization-wide scores.
- **Generations** (§3.4): `orgs/<name>/org.json` (pointer) →
  `generations/<n>.json` (repositories, snapshots, store and tantivy
  segments, the index schema) → `index/<uuid>.*` (tantivy files shared
  between generations). The index's `meta.json` is synthesized from the
  generation by a read-only tantivy directory. The current and previous
  generations are kept; older ones are collected with the files and store
  segments only they used, and `Store::register` keeps store segments a kept
  generation still reads. A member re-indexed in the store but not yet
  propagated stays served from its generation segment.
- **No rank fusion anywhere** (§6): without an organization, several locally
  indexed roots are ranked with BM25 statistics summed over their segments
  (`rueca_segment::search_symbols_merged`) — the same scores the
  organization index gives (tested for equality).
- **Envelope**: a response lists only the snapshots its items come from; a
  request on one snapshot always lists it.
- **`list_repositories`** reports, per repository: default ref, indexed
  refs, files per language, files, symbols (non-container nodes) and edges.
- **Local `--org`**: `rueca index --org <file> [PATH]` (PATH: one member;
  members never indexed are left out of the generation until they are), a
  plain `rueca index <member>` updates the organizations holding it, `rueca
  query --org` / `rueca mcp --org` take the file or the organization's name,
  `rueca status` lists organizations. `url` members are cloned with `git`
  under `orgs/<name>/clones/`. Repository ids must be unique within an
  organization (`id = "…"` overrides one).
- **Permissions hook**: `rueca_query::Visibility` (local mode: `All`) is
  applied in every organization path: the snapshot selection, the index
  statistics and collector, `list_repositories`, and errors (an invisible
  repository named in `repo` or in an id is reported like one that does not
  exist).

Measured at the time (the first organization scenario, 21 instances over
21 repositories; another population than today's suite, §8 — kept as the
record of phase 1, not comparable with §9.2):

| Metric | Phase 1 | Floor | Target |
|---|---|---|---:|
| File Acc@5, organization scope vs the right repository alone | 47.6% vs 61.9% (0.77×; rank fusion: 9.5%) | ≥ 0.85× | ≥ 0.95× |
| Other repositories' share of the top 10 | 36.7% (rank fusion: 94.3%) | — | — |
| Incremental update: re-index micropython (95k symbols) of 21 | organization layer 123 ms vs 2,713 ms (0.05×) | ≤ 2× | ≤ 1× |
| Organization layer per added repository (21 steps) | median 74 ms, max 137 ms (0.10× median, 0.40× max) | ≤ 2× | ≤ 1× |
| Tokens per correct answer, organization scope (top 10) | 886 vs 529 single (1.67×) | reported | ≤ 1.2× |

The localization floor is not met: three issues whose file the single
repository finds (`coder/code-server`, `jqlang/jq`, `fmtlib/fmt`) lose it to
the same words in other repositories of the same language family, and
answers that span several repositories cost more header lines. Repository
routing signals (open question 1) are the next lever; the link layer
(phase 2) does not change localization.

### 9.2 Phase 2 — implemented

Repository routing (open question 1) and the link layer for packages
(§3.3.1) landed in `rueca-segment` (`org::route`, `org::link`),
`rueca-index` (`link`: the `PackageLinker`), `rueca-query` (routing in
`search_symbols`, `list_repositories` `query`, `trace` across
repositories) and the CLI (`rueca links --org`). Choices and deviations:

- **Routing is a prior, two signals.** Every repository of a generation
  has a profile (`profiles/<segment>.json`): repository and package
  names, manifest descriptions and the README head, the 64 most referenced
  symbols and the 64 most frequent path components, plus its package
  registry entries. A query scores the visible repositories by (a) CORI
  over the organization index — the documents of each repository holding
  each query term are a dictionary lookup in its tantivy segment, so the
  term-distribution signal needs no extra index — and (b) BM25 over the
  profiles; both are min-max normalized and averaged (`mu` 0.5), then
  divided by the best. Each symbol score is multiplied inside the top-k by
  `((0.25 + r) / 1.25)²`. A prior keeps a strong match in another
  repository reachable; the two-stage alternative (`top`: only the best
  repositories) and per-repository statistics (`stats=local`) were
  evaluated and not kept. Profiles are documents of the generation, not
  documents of the symbol index: the per-repository documents are tokenized
  once per opened generation (≈ 28 repositories: milliseconds) instead of
  being added to the tantivy index, so the repositories' segments stay
  copied unchanged (§9.1).
- **Tuned on the calibration split only** (RFC 0004 §7.7): the 35
  calibration S1 instances asked of an organization of the 7 calibration
  repositories plus the gated organization's 21 repositories as
  distractors (their documents only), against each instance's repository
  searched alone in the same store (`bench/README.md`, *Org scenarios*).
  The sweep below is the historical record of that choice: it was run on
  the earlier 35-instance calibration organization, before the
  organization suite of §8 (whose calibration split, 29 instances over a
  50-member organization, measures the chosen default):

  | Routing | File Acc@5 org / single | Other repositories in the top 10 | Tokens per correct answer, org / single |
  |---|---:|---:|---:|
  | off (phase 1) | 45.7 / 71.4 (0.64×) | 34.6% | 1.69× |
  | prior α 0.5 / 1 / **2** / 3 | 0.84× / 0.88× / **0.92×** / 0.88× | 15.4% / 11.1% / **9.1%** / 8.3% | 1.29× / 1.27× / **1.25×** / 1.24× |
  | prior α 2, document only / terms only | 0.84× / 0.88× | 13.4% / 13.7% | 1.32× / 1.26× |
  | top 1 / top 2 | 0.88× / 0.92× | 11.4% / 14.0% | 1.20× / 1.27× |
  | prior α 2, per-repository statistics | 0.88× | 10.9% | 1.27× |

  CORI's constants (50 / 150, binary presence, 500 / 1500), the field
  weights and `eps` moved nothing beyond one instance; the defaults stay.
- **Package registry** from manifests (`package.json`, `pyproject.toml` /
  `setup.cfg` / `setup.py` with the distribution's top-level import
  packages, `go.mod`, `composer.json` PSR-4 / PSR-0, `pom.xml`, Gradle
  settings, and C / C++ `include/` directories). A repository's imports
  that its own pack cannot resolve are recorded as keys (`npm:vue`,
  `py:click.core`, `go:…`, `java:<package>`, `php:<name>`, `c:<include>`);
  a repository depends on another when an import key continues one of the
  other's package keys, or when its manifests require one of its packages
  by name (PHP code needs no import to use a class).
- **Resolution by joint resolve.** Instead of a second resolver, the
  consumer and its dependencies are resolved together by the normal one,
  the dependencies' files mounted under a private prefix and every pack
  wrapped so that a dependency keeps its own module names and import
  resolution (its project: workspaces, `exports`, source roots, `go.mod`,
  PSR-4), and a consumer import nobody resolved inside its own repository
  is resolved by the publishing repository's pack against that
  repository's project. The whole ladder then applies across repositories
  (imported bindings, re-exports, typed receivers, inherited members,
  PHP's fully qualified names, includes), so link edges carry the
  strategy, confidence and evidence they would have inside one
  repository. Kept: edges between a consumer node and a dependency node,
  of every kind but `CONTAINS` / `DEFINES` — `DECORATES` points into the
  consumer (a library decorator on its function) — never through the
  name-matching rungs, never into a dependency's tests or into a name the
  consumer defines itself (Carbon ships `Illuminate\…` test stubs that
  would otherwise capture Laravel's own classes). Nodes are mapped back to
  the repositories' own segments by (label, qualified name, path).
- **Versioned with the generation, recomputed for reverse dependencies.**
  A consumer's link set is one file named by its segment, its
  dependencies' segments and the linker version; the generation records
  what each set was computed against, so an update recomputes the
  consumers whose own segment, one of whose dependencies or whose
  dependency set changed, and nothing else (tested: re-indexing a library
  re-links its one consumer, re-indexing an unrelated repository links
  nothing).
- **Query side.** `trace` walks link edges in both directions and opens
  the other repository's snapshot on demand; nodes carry their snapshot
  `s`. Link edges are filtered by visibility at both ends (`OrgIndex::link_edges`).
  `get_symbol` counts stay per repository.
- **Open question 2** (two repositories publishing one name) is handled
  only by order: a consumer's import is tried against every dependency
  publishing a matching key, first match wins; lockfiles are not read.

Measured on the organization suite of §8 (`docs/bench/alpha-results.md`,
*Org scenarios*; method in `bench/README.md`). The 21-instance rows
published here before 2026-11-16 are dropped: another population, not
comparable and no longer carried (§10).

| Metric | Phase 2 | Floor | Target |
|---|---:|---:|---:|
| File Acc@5, organization scope vs the right repository alone, gated (120 issue instances, 42 members) | 56.7% (47.7–65.2) vs 66.7% (0.85×, 0.74–0.96 ⚠) | ≥ 0.85× | ≥ 0.95× |
| File Acc@5, organization scope vs the right repository alone, calibration split (29 issue instances, 50 members; routing tuned here, never gated) | 62.1% (44.0–77.3) vs 79.3% (0.78×, 0.60–0.95) | ≥ 0.85× | ≥ 0.95× |
| Function Acc@10, organization scope vs alone, gated (n = 115) | 37.4% (29.1–46.5) vs 48.7% | — | — |
| BM25 over the union of the members, gated: File Acc@5 / Function Acc@10 | 52.5% (43.6–61.2) / 26.1% (18.9–34.8) | — | — |
| Rueca − BM25 over the union, gated, paired: File Acc@5 / Function Acc@10 | +4.2 (−4.2…+12.5) ⚠, d = 22.5%, MDE 12.1 / +11.3 (+4.3…+19.1) ✅, d = 18.3%, MDE 11.2 | reported (status as RFC 0004 §6.1) | — |
| Rueca organization − Rueca per repository + rank fusion, gated, File Acc@5 | +32.5 (+23.3…+41.7) ✅ (rank fusion: 24.2%) | — | — |
| Other repositories' share of the top 10, gated | 11.3% (BM25 over the union 28.7%; rank fusion 91.7%) | — | — |
| Tokens per correct answer, organization scope (top 10), gated | 701 vs 542 (1.29×, 1.13–1.50) | reported | ≤ 1.2× |
| Impact (16 gated): consumer hit@10, Rueca vs BM25 over the union | 12.5% vs 37.5% (−25.0, −50.0…−6.2 ❌) | — | — |
| Impact: own File Acc@5, Rueca vs BM25 over the union | 81.2% vs 62.5% | — | — |
| Cross-repository edges (packages), precision / recall, 12 labelled pairs pooled | 99.4 (99.1–99.6) / 94.7 (93.2–95.9) ✅ (2026-11-18; 98.3 / 74.9 ❌ before the Rust fixes) | ≥ 90 / 75 | ≥ 97 / 90 |
| — without Rust (8 pairs: Python, PHP, Java) | 100.0 / 96.2 (99.8 / 96.2 before) | ≥ 90 / 75 | ≥ 97 / 90 |
| — Rust alone (4 pairs) | 98.3 (97.4–98.9) / 93.1 (90.6–95.0) ✅ (94.5 / 53.2 ❌ before) | ≥ 90 / 75 | ≥ 97 / 90 |
| Incremental update: re-index micropython (95,342 symbols) of 42 (profiles, routing, links included) | 504 ms vs 2,681 ms (0.19×) | ≤ 2× | ≤ 1× |
| Organization layer per added repository (42 steps) | median 273 ms, max 1,598 ms (0.30× / 3.62× ❌) | ≤ 2× (max) | ≤ 1× |

cbm (per project + rank fusion; it has no cross-project search): File Acc@5
11.7 (7.1–18.6) against 56.7 alone (0.21×), Function Acc@10 6.1; Rueca
native − cbm +45.0 (+35.8…+54.2) ✅ on File Acc@5, +31.3 (+22.6…+40.0) ✅
on Function Acc@10; impact consumer hit@10 12.5 vs 12.5.

At 120 gated instances the organization ranking sits at the floor: 0.85×
the single-repository File Acc@5, with an interval (0.74–0.96) that
crosses it (⚠), and the 0.95× target not met. On the calibration split,
where routing was tuned, the same default gives 0.78× (0.60–0.95) over 29
instances of a 50-member organization: the 0.92× of the earlier
35-instance calibration organization did not carry over. Routing does its
part — 11.3% of the top 10 comes from other repositories, against 28.7%
for BM25 over the same union — so the lead to check on the misses stays
the one phase 2 found: the right file ranking lower inside a routed
repository under organization-wide statistics than alone. Against the
plain-text floor arm, Rueca is ahead on Function Acc@10 (+11.3, interval
above zero) and not distinguishable on File Acc@5 (+4.2; a true difference
under 12.1 points can go undetected at this n). The per-step maximum over
the floor (3.62×) is one small repository whose own index is fast. The
impact kind is a loss: the organization ranking returns the library's own
files (own File Acc@5 81.2%) and almost never the consumers that use the
changed symbol (consumer hit@10 12.5%), which BM25 over the union reaches
more often (37.5%) (§12, question 6). The next levers stay per-repository
ranking within a routed repository without losing comparable scores (a
score transplant: the organization score distribution, the repository's
own order) and, for impact, signals from the link layer.

Open findings from the Rust pairs (about Rueca, not the labeler; §12,
question 10):

- ~~**Attribute macros** (`#[tokio::main]`, `#[tokio::test]`) produce no
  link edge: 70 of the 176 labelled axum → tokio uses.~~ Fixed 2026-11-17
  (PR fix/rust-tokio-gaps): an attribute macro is a `Decorator` reference,
  `DECORATES` from the `#[proc_macro_attribute] pub fn` to the item.
- ~~**Items inside macro invocation bodies** — `mod`, `use` and items in
  `cfg_net! { … }` and similar — are not extracted, so re-exports such as
  `tokio::net::TcpListener` and `tokio::spawn` are invisible to
  consumers.~~ Fixed 2026-11-17 (PR fix/rust-tokio-gaps): a body that
  parses as items (and every `cfg_if!` branch) is opened in place; Rust
  lookups also keep the value and type namespaces apart (`mod sleep; pub
  use sleep::sleep;`).
- ~~**Cross-repository method resolution by type name** can pick a
  same-named type of another crate (`TcpStream.peek` → mio): most of the
  38 wrong tokio → mio edges.~~ Fixed 2026-11-17 (PR fix/rust-tokio-gaps):
  a type binds in the file's scope, another crate's same-named type only
  as a guess (`heuristic`, dropped by the link layer); tokio → mio 200 →
  166 edges, 165 counted correct (the one left is `Registry.register` on
  a `mio::Registry` field, right but invisible to the labeler).
- The linker matches Cargo dependencies by package name only, never by
  version.
- Methods of an extension trait implemented for every type with another
  trait (`impl<T: Service> ServiceExt for T`: `.oneshot()`, `StreamExt`)
  bind to nothing: 20 of the 36 Rust uses still missed (axum → tower).

After the fixes (2026-11-18): axum → tokio 98.0 / 95.6 (was 92.9 / 14.1),
axum → hyper 90.4 / 94.7 (100 / 54.4), axum → tower 100 / 89.8 (100 /
82.7), tokio → mio 99.4 / 93.8 (81.0 / 87.5). The edges counted wrong are
methods of values a library call returned (`Server::bind(..).serve(..)`,
`tokio::spawn(..).abort()`), right but unseen by the labeler.

### 9.3 Phase 3 — implemented

Refs land as snapshots of their own on the unchanged repository layer; the
organization layer lists them and a query picks one per repository. They
landed in `rueca-index` (`refs`: resolution, export, `index_ref`,
`remove_ref`; `org`: `refs = [...]`, `index_org_ref`), `rueca-segment`
(`org::select`: `OrgSlot`, `OrgSelection`; ref entries in generations,
per-ref profiles and link sets, remapped link edges), `rueca-query`
(`ref` lists, organization views, collapse across refs) and the CLI
(`index --ref`, `index --org … --ref`, `links --ref`, `status`). The
design below resolves what §4 and RFC 0005 leave open; the choices made
while building it and the measurements follow.

- **A ref is a snapshot.** Every indexed ref of a repository is one store
  root, one segment and one catalog entry, exactly like the repository's
  own root: `rueca index [PATH] --ref <name>` exports the tree of the
  commit the ref names (blobs written byte for byte from the object
  database with gix — not `git archive`, whose `export-ignore` /
  `export-subst` and filters would change blob ids and lose facts-cache
  hits; no checkout, nothing written into the user's repository except
  fetched objects) under `<store>/refs/<repo-dir>/<ref-slug>/`, indexes that
  tree with the repository's id, the commit and the ref name recorded in
  `SnapshotInfo` (`branch` holds the ref name; `dirty` is always false),
  and registers it. The facts cache is keyed by blob, so a second ref
  costs the walk, the hashing and the resolution plus extraction of the
  blobs that differ. `--ref <name> --remove` drops the snapshot and its
  export. The catalog stays keyed by root; the repository id groups the
  snapshots of one repository.
- **Ref names.** `main` / `feature/x` (`refs/heads/…`, then
  `refs/remotes/origin/…`, fetched when absent), `pr/<n>` (`refs/pull/<n>/head`,
  then `refs/merge-requests/<n>/head`, fetched from `origin`), `v1.2`
  (`refs/tags/…`), or a commit (full or a unique prefix ≥ 7; recorded as
  its 12-digit form). The name given is the snapshot's `ref`; the
  envelope carries the commit.
- **The default ref** of a repository is the member's own root (a `path`
  member: its working tree, as in phase 1; a `url` member: the clone at
  the remote's HEAD). Its `ref` is the branch checked out there. Other
  refs are the member's `refs = ["feature/x", "pr/123"]` in
  `rueca-org.toml` plus any ref added from the command line
  (`rueca index --org <file> <member> --ref <name>`); `rueca index --org
  <file>` syncs the file's refs for every member, refreshes the refs added
  earlier, and removes nothing by itself (`--ref <name> --remove` does).
- **Generations list every ref.** `OrgRepo` keeps describing the default
  ref (the generation format is unchanged, `format` 1) and gains
  `refs: [OrgRef]`, each with the fields of a snapshot entry (root,
  segment, snapshot, tantivy segment, docs, bytes, `since_generation`,
  profile, links). The organization tantivy index lists the default
  **and** the ref segments; a refreshed ref replaces only its own entry
  (segment copy, profile, link set), nothing else of the generation is
  rebuilt. Garbage collection treats ref entries like repository entries.
- **A query selects one segment per repository**, the default unless `ref`
  names another: `repo: A, ref: pr/123` is §4's view (A replaced by A at
  that ref, every other repository at its default); `ref` without `repo`
  applies to every repository that has the ref and leaves the others at
  their default (the organization "as of" a release branch); a ref no
  selected repository has is an error that lists the indexed refs. BM25
  statistics are summed over the **selected** segments (the organization as
  seen from that ref), which is what the phase 1 collector already does;
  §4's "org statistics with A filtered out, plus A's ref segment" is this
  selection, not a second `Bm25StatisticsProvider`. Routing profiles are
  per segment, so a ref routes with its own profile. Ids whose commit is
  that of a ref snapshot resolve to it.
- **Several refs of one repository** (`ref: ["main", "pr/123"]`, RFC 0005
  §5–§6) select several segments of that repository; `search_symbols`,
  `get_symbol` and `trace` collapse items identical across them
  (`rueca_query::multiref`, key `(repo, path, qn, blob)`) into one item
  with `in: [s, …]`, and flag `changed` the ones that differ or are
  absent from one of the refs. The compact text marks such items.
- **Link edges and refs.** A ref snapshot's outgoing link set is computed
  at index time against its dependencies' default snapshots (a new import
  of an organization package on the branch is resolved then, not on the
  fly) and versioned like any link set. On a ref view, incoming edges
  (consumers at their default → the repository's default) are remapped to
  the ref's nodes by (label, qualified name, path); edges into symbols
  the ref no longer has are dropped (§4). Reverse dependencies are not
  recomputed when a non-default ref changes.
- **`list_repositories`** reports `default_ref`, its `commit`, and
  `refs: [{"ref", "commit"}]` for every indexed snapshot of the
  repository; `rueca status` lists refs per repository.
- **Local mode** gets the same: `rueca index . --ref feature/x` then
  `ref: "feature/x"` or `ref: ["main", "feature/x"]` on any tool; RFC 0005
  §9's "other refs error until the catalog keeps several snapshots" is
  lifted.
- **Service mode** (later) will materialize refs from the object store
  instead of exporting trees under the store; the contract above does not
  change.

Choices and deviations:

- **Exported from the object database, not checked out.** The commit's tree
  is written blob by blob with `gix` into a temporary directory swapped in
  at `<store>/refs/<repo-dir>/<ref-slug>/`; symlinks and submodules are
  skipped (the working-tree walk skips them too). A commit with no file
  under the root's path (a subdirectory root that does not exist at that
  ref) is an error, not an empty snapshot. A `git worktree` would
  write into the user's repository and lock the branch; `git archive`
  applies `export-ignore`, `export-subst` and content filters, which change
  files and their blob ids and so lose the facts-cache hits. The only
  writes into the repository are fetched objects, remote-tracking refs,
  tags fetched by name and `refs/rueca/…`; a shallow repository is fetched
  with `--depth 1`, and `git` runs with `GIT_TERMINAL_PROMPT=0` and no
  askpass, so a fetch fails instead of waiting on a prompt.
- **Repository directories** (`<repo-dir>`) are the repository id's slug,
  `--<path slug>` for a root below the top of its repository, then the
  first 8 hex digits of the hash of the id and that path
  (`github-com-o-r-<hash>`, `github-com-o-r--web-<hash>`): repository slugs
  are not injective (`acme/my.lib`, `acme/my-lib`), and two subdirectory
  roots of one repository carry the same id and must not share a ref's
  export. An owner marker `<repo-dir>/.owners/<ref-slug>.json` (repository
  id, path, ref) is checked before an export is overwritten, and
  `--remove` deletes only an export that its catalog entry or its marker
  says is that ref of that root.
- **Ref slugs** keep ASCII letters, digits, `.`, `_` and `-`, turn every
  other run into one `-`, and append the first 8 hex digits of the name's
  hash when anything was replaced or the name has upper-case letters, so
  distinct names never share a directory, even on a case-insensitive file
  system (`8.5.1` stays `8.5.1`; `pr/123` becomes `pr-123-<hash>`).
- **Resolution order:** `refs/heads/<n>`, `refs/remotes/origin/<n>`,
  `refs/remotes/<n>` (`origin/main` works), `refs/tags/<n>` (annotated tags
  peeled), `HEAD`, a full `refs/…` name, a commit (full or a unique prefix
  of ≥ 7 hex digits, recorded under its 12-digit form); a name found
  nowhere is fetched from `origin` (a full commit id into
  `refs/rueca/commit/<id>`, else a branch, else a tag), so tags and
  commits work in the shallow clone of a `url` member. `pr/<n>` is always fetched,
  `refs/pull/<n>/head` then `refs/merge-requests/<n>/head`, into
  `refs/rueca/pr/<n>`, which keeps its objects reachable and is the offline
  fallback. A ref whose commit, repository id and Rueca version did not
  change, with its export still there, is not re-indexed (a `pr/<n>` still
  pays its fetch).
- **Generation format unchanged.** `OrgRepo` is `{repo, url, default:
  OrgEntry, refs: [OrgRef {ref_name, entry}]}` with `default` flattened
  into the repository object, so `format` 1 generations read and write as
  before. A ref export belongs to the member whose repository id it
  carries (member roots are canonicalized first, so a path through a
  symbolic link derives the same id); a ref `rueca index --org` indexed
  whose id is no indexed member's is reported in `OrgUpdate::unowned_refs`
  and the CLI summary, not dropped silently. Entries are reused by (root, segment); `OrgUpdate` reports
  `refs_added` / `refs_replaced` / `refs_removed` / `refs_unchanged` and,
  per entry, the copy, profile and link costs.
- **Link sets per ref, against the defaults.** A ref's outgoing link set is
  its own imports resolved against its dependencies' default entries, one
  file named by the segments it was computed from, like a default's. The
  incoming side is remapped at read time: `OrgIndex::link_edges(at, node,
  dir, kind, visible, view)` maps the default's endpoints to the ref's
  nodes by (label, qualified name, path) — several nodes with one key, a
  property's getter and setter or overloads, pair by position in source
  order — through a correspondence built on
  first use per (repository, ref) slot, for the nodes link edges target
  only, and cached with the opened generation; an endpoint the ref no
  longer has drops the edge. Resolving a consumer's imports against a
  library ref on the fly was not built: it would run the joint resolver
  (§9.2) inside a query.
- **`find_ref` precedence:** a ref entry with that name (a commit by any
  prefix of ≥ 7 digits), else the default entry when the name is the
  branch checked out at the member root, else any entry whose commit starts
  with the name. A ref entry wins over the default when both carry the
  name: `--ref main` exports the commit `main` points to, the default may
  be dirty. `HEAD` and `worktree` name the default. Local mode follows the
  same order (a ref snapshot recorded under the name, then the working
  tree).
- **`ref` with and without `repo`.** With `repo` (one or a list) every
  selected repository must have every named ref; without it (or `"*"`)
  each repository is read at the refs it has and the others at their
  default, and naming a ref no repository has is an error. Pull request
  numbers are per repository, so `pr/<n>` without `repo` (or with `"*"`)
  is accepted only when exactly one selected repository has it; otherwise
  the error lists the visible ones that do (`` `pr/7` exists in several
  repositories (A, B): name `repo` ``). Errors list the
  indexed refs (`main@08b803cf32ce, 7.1.2@1784558ed7c7, …`, per repository
  when `repo` is omitted) and the command that indexes another one; a
  repository the caller cannot read is never named, nor are its refs.
- **Collapse per tool.** A symbol is matched across the snapshots of one
  repository by (label, qualified name, path), by position in source order
  among nodes sharing that key, and its versions (the items, with their
  `in`) collapsed by file blob. `search_symbols` fetches `limit` per snapshot, collapses, then
  cuts at `limit` (the cursor counts collapsed items); `get_symbol` answers
  the best match's version and lists the others in `versions` (`{s, in,
  changed, symbol, callers, callees}`), not as candidates;
  `get_file_outline` returns the first listed ref's outline and every other
  distinct version of the file in `versions`; `trace` walks from every
  version of the root and returns the union graph, `in` on nodes and a
  trailing `{in, changed}` object on the edges of that repository.
  `search_code`, `get_snippet` and `format: "verbose"` do not collapse;
  single-snapshot responses are unchanged.
- **`changed` is per symbol** *(2026-11-24; it was per file blob)*. A
  symbol is `changed` when its own text — the bytes of its span — is not
  the same in every requested snapshot of its repository, or it is absent
  from one; equal blobs are equal without reading, and a changed file is
  read once per call. A symbol whose file changed elsewhere (moved by lines
  added above it) still comes back once per version — its lines differ —
  but not `changed` (`click.types.ParamType.split_envvar_value`, lines
  205–213 in both `main` and `8.5.1`, used to be flagged). `trace` nodes
  are `changed` by the same rule (where the walk reached them is their
  `in`; edges keep `changed` = not found in every ref); `get_file_outline`
  marks each symbol of each version the same way, and its own `changed`
  stays the file's. The file-level signal is still there: an item's `in`
  is the snapshots whose file blob it shares.
- **`rueca index --org <file>` refreshes every ref** of each member it
  indexes: the file's `refs`, then the refs indexed earlier from the
  command line (the store's ref entries of that repository id); a ref
  that fails is reported in `ref_errors` and the run goes on; nothing is
  removed without `--remove`. A plain `rueca index <path> --ref <name>`
  updates the organizations that hold the repository.

Measured on a demonstration organization (`pallets/click` + `pallets/flask`,
which imports `click`; 151 link edges flask → click), **debug build**, CLI
wall times; with the hands' measurements on this repository and on
`aiokpl` + `pulumi-mlflow` (also debug). Refs: click `8.5.1` and `7.1.2`
(tags) and `stable` (branch); flask `3.1.3` (tag) and `pr/5918` (fetched
from GitHub; its head is based on an older `main`).

| Metric | Measured | Floor | Target |
|---|---|---:|---:|
| Indexing a second ref vs the first snapshot: files extracted (of them from the facts cache), index time | click: default 156 (4), 1,312 ms; `8.5.1` 156 (104), 1,200 ms; `stable` 156 (143), 1,219 ms. flask: default 105 (5), 875 ms; `3.1.3` 104 (60), 903 ms; `pr/5918` 105 (95), 917 ms. Rueca: working tree 1,454 extracted, 6.2 s; `origin/main` 1,451 (1,434), 4.6 s | — | — |
| Export of a ref (blobs from the object database) | click 47 ms (179 files, 1.6 MB), flask 98–112 ms, Rueca ≈ 0.5 s; `pr/5918` fetch 577 ms | — | — |
| Re-indexing an unchanged ref | 225 ms (Rueca, local); `pr/5918` 547 ms, of which 509 ms its fetch | — | — |
| Organization layer per added ref vs the ref's own indexing (resolve + export + index) | click `8.5.1` 225 / 1,250 ms (0.18×), click `7.1.2` 142 / 613 ms (0.23×), flask `3.1.3` 631 / 1,003 ms (0.63×; linking 440 ms), flask `pr/5918` 645 / 1,608 ms (0.40×); pulumi-mlflow second ref 186 / 846 ms (0.22×) | ≤ 2× | ≤ 1× |
| Query latency at a ref vs the default (`search_symbols`, median of 7 CLI calls) | whole organization 99 ms vs `ref: "8.5.1"` 97 ms; `repo: flask` 50 ms vs `repo: flask, ref: "pr/5918"` 49 ms; click 62 ms vs `ref: ["main", "8.5.1"]` 110 ms (two snapshots and the collapse) | — | — |
| Collapse savings on a `ref` list (10 queries, `limit` 50): items returned vs per-ref items they stand for | click `[main, 8.5.1]` 473 vs 648 (0.73×; 298 marked `changed`); flask `[main, pr/5918]` 500 vs 678 (0.74×; 322); flask `[main, 3.1.3]` 500 vs 533 (0.94×; 467) | — | — |

The facts cache removes extraction, not the rest: a ref costs its walk,
resolution and above all the segment write (symbol index, content packs:
≈ 70% of the time in the debug build), so a second snapshot costs about
as much as the first here (0.91–1.05×) and 0.74× on this repository. The organization layer stays
well under the floor for every added ref; linking (a consumer ref's joint
resolve) is its largest part. A ref adds no query cost: reading a ref
instead of the default is the same segment selection. Collapse saves about
a quarter of the items when the two refs share most files; between refs
whose files mostly changed (flask `3.1.3`, a release branch, against
`main`), blob-level comparison leaves little to collapse.

The demonstration showed a `trace` across repositories at a ref: from
`flask.cli.with_appcontext.decorator`, callees `Context.ensure_object`,
`Context.invoke` and `Context.with_resource` in click at `main`; with
`ref: "7.1.2"` the first two are remapped to the tag's lines
(`src/click/core.py` L525-532 instead of L779-786) and `with_resource`
(added in click 8.0) is dropped. It also found two problems, open for the
next change: an `index --org <file> <member> --ref` whose member path goes
through a symbolic link (macOS `/tmp`) records the ref under
`local/<dir>` instead of the origin's id, so the organization silently
leaves it out; and `rueca index --org <file>` with nothing changed writes
new default segments, which replaces every default entry and re-links
every consumer ref (1.7 s of organization layer for two repositories and
five refs).

### 9.4 Phase 4 — implemented

Contracts landed in `rueca-model` (`contract`: hints, canonical keys,
matchers), a new pack `rueca-lang-proto`, a `contracts` module in every
code pack, `rueca-lang-data` (OpenAPI operations provide), `rueca-resolve`
(`contracts`: the table and the edges inside one snapshot),
`rueca-segment` (the `contracts` component; `org::link`: `ContractKey`),
`rueca-index` (`contract_link`), `rueca-query` (`list_contracts`) and the
CLI (`rueca contracts`, contract edges in `rueca links`). What was built:

- **Contract hints are facts**, as RFC 0001 §6.4 planned. A pack emits a
  `ContractHint { side, kind, key, raw, span, owner, framework }` per route
  declaration, spec operation or call site: `side` `Provides` /
  `Consumes`, `kind` `Http` / `Rpc` / `Topic`, `key` the canonical key,
  `raw` the text as written (≤ 200 characters, the evidence), `owner` the
  handler, `Route` or `RpcMethod` of a provider and the enclosing
  definition of a consumer (none: file level), `framework` a short name
  (`flask`, `requests`, `openapi`, `proto`, `grpc-go`, …).
  `FileFacts.contracts` is appended last; `FACTS_FORMAT` 2 → 3 invalidates
  every facts-cache entry once, and every pack that emits hints bumped its
  extractor version.
- **Canonical keys** (`rueca_model::contract`, pure functions). HTTP:
  `METHOD /path`, the method upper-cased, `*` when it is unknown, not a
  literal or not an HTTP method. The scheme, host and port of an absolute
  URL are dropped, and so is anything before the first `/` that looks like
  a base URL (an interpolation, or text with `.` or `:`: `${host}/api/x` →
  `/api/x`; `f"{base}/x"` and `localhost:8080/x` → `/x`), so a client's
  base URL never becomes a segment; the query string, fragment, empty segments and a
  trailing `/` are dropped. Every parameter segment becomes `{}`: `:id`,
  `{id}`, `<id>`, `<int:id>`, `[id]`, `${x}`, `%s` / `%d`, `*` / `**`,
  a concatenation or interpolation gap, and a segment only partly a
  parameter (`sha256:{digest}`). Literal segments stay literal (`GET
  /users/42`), keys are case-sensitive, and a path made only of gaps has
  no key. RPC: `pkg.Service/Method` from `.proto`; code that does not know
  the package writes `Service/Method`, and code that knows only the method
  writes `*/Method` (a Go unary server method taking `*pb.<Method>Request`,
  a call through a client field typed in another file). Topic: the name.
- **Matching** (`match_keys`, `best_matches`, shared by the resolver, the
  linker and `list_contracts`): exact (the same key; `{}` against `{}` is
  exact) > template (a provider `{}` segment stands for a literal consumer
  segment; for RPC, a key without a package on either side) > method
  wildcard (either HTTP method `*`, either RPC service `*`). Segment counts
  must be equal and a consumer's `{}` never matches a provider's literal
  segment (`GET /users/{}` does not reach `GET /users/me`). Confidence 0.9 /
  0.85 / 0.7; when several distinct providers share the best quality, 0.5
  each (`AMBIGUOUS_CONFIDENCE`). Providers are bucketed by kind and shape
  (HTTP: segment count; RPC: method name; topic: the name), so a call is
  compared with one bucket, not with every provider.
- **The segment contract table.** Every hint is a row `(node, side, kind,
  key, raw, file, span, framework)` on its owner's node, else the file
  node, stored in an optional bundle component `contracts` (written when
  there are rows; an older segment reads as an empty table; the segment
  format stays 2). The linker and `list_contracts` read the table; nothing
  parses files again.
- **Edges inside one snapshot** (`rueca-resolve` `contracts`, like the
  bridge of RFC 0003 §7): consumer → code provider (the handler) `CALLS`,
  consumer → spec provider (an OpenAPI `Route`, a `.proto` `RpcMethod`)
  `CALLS`, code provider → spec provider with a matching key `IMPLEMENTS`;
  all `Strategy::Contract`, with the consumer's span as evidence (the
  handler's hint for `IMPLEMENTS`). Code and spec providers are matched
  separately, so a call that reaches a handler and the operation it
  implements is not ambiguous; spec-to-spec and code-to-code providers
  never link, a node never links to itself, and a consumer that matches
  nothing gets no edge and stays in the table.
- **Specifications.** OpenAPI 3 and Swagger 2 operations
  (`paths.<path>.<method>`) were already `Route` nodes named `GET
  /users/{id}`; each also provides its key now (framework `openapi`).
  `.proto` files are a language of their own (`Language::Proto`, not a
  data file kind), read by `rueca-lang-proto` over
  `protox-parse` 0.10 — pure Rust, no `protoc`, MIT OR Apache-2.0; types
  are not resolved and imported files are not read: `service` →
  `Interface` `pkg.Service`, `rpc` → `RpcMethod` `pkg.Service.Method` with
  signature `rpc Method(Req) returns (Resp)`, messages → `Struct` with
  `Field`s, enums → `Enum` with `Constant`s, `import` → `IMPORTS`. Every
  `rpc` provides `pkg.Service/Method` (framework `proto`).
- **What the packs recognize**, from file-local evidence (the router,
  client or stub module the file imports or binds; prefixes composed when
  the file builds them):

  | Language | HTTP providers | HTTP consumers | gRPC (framework); topics |
  |---|---|---|---|
  | Python | Flask, Quart, FastAPI, aiohttp, Sanic, Litestar decorators (with a `Blueprint` / `APIRouter` prefix of the file); `add_url_rule`; Django `path()` / `re_path()` / `url()` (method `*`); aiohttp `add_get` / `add_route` / `web.get`; Starlette `Route` | calls whose callee is an HTTP verb (`get`, `_post`, `get_api_list`) or `request` / `_request` / `urlopen` with a path-like argument; method from the callee, a literal argument or `method=` | `x.Method` on a name bound to `<mod>.XStub` consumes, the PascalCase methods of an `XServicer` subclass provide (`grpc-python`); Kafka topics (kafka-python, aiokafka) |
  | TS / JS | express, koa-router, fastify, hono, restify, hapi routes in a file importing the framework (or on `app` / `router` / `server` with an inline handler); `app.route("/x").get(h)`; NestJS `@Controller` + `@Get` | `fetch`, axios, got and other clients, verb wrappers and `request` with a path-like argument; the one-hop wrapper rule | `new GreeterClient(…)` calls consume, `server.addService(GreeterService, {…})` provides (`grpc-node`) |
  | PHP | Laravel `Route::` (chained, `match`, `any`, `resource`, `prefix` / `group` closures); Symfony `#[Route]` (class prefix); Slim / Lumen `$app->get` | Guzzle-style `->get('/x')` / `->request('POST', …)`, the `Http::` facade, `CURLOPT_URL` | — |
  | Go | `net/http` `HandleFunc` / `Handle("GET /x")`, gin, echo, chi, gorilla/mux, fiber (group, sub-router and `Route` prefixes of the file) | `net/http`, resty, SDK wrappers called with a method then a path | types embedding `pb.Unimplemented<S>Server` or registered with `pb.Register<S>Server`, stream parameters `pb.<S>_<M>Server`, unary signatures taking `*pb.<M>Request` (`*/M`) provide; calls on `pb.<S>Client` consume (`grpc-go`); generated files have no hints |
  | Java | Spring `@GetMapping` / `@RequestMapping`, JAX-RS `@GET` + `@Path`, Micronaut `@Get` (class prefix) | the same annotations on `@FeignClient` / `@RegisterRestClient` / Micronaut `@Client` types, `@GetExchange`, Feign `@RequestLine`; `RestTemplate`, `WebClient` / `RestClient`, `HttpRequest`, OkHttp | `XGrpc.XImplBase` / `AsyncService` subclasses provide, calls on `XGrpc` stubs consume (`grpc-java`) |
  | Rust | actix-web / rocket attributes, actix `.route` / `web::resource`, axum `.route` (with `web::scope` / `.nest` prefixes) | reqwest | the methods of `impl <Service> for <Type>` provide (snake_case back to the proto's PascalCase), calls on `<S>Client` consume (`tonic`) |
  | OpenAPI 3 / Swagger 2 | every operation (`openapi`) | — | — |
  | `.proto` | — | — | every `rpc` (`proto`) |

- **The organization linker.** `RepoDeps`, the analysis stored with each
  profile, gains `contracts: [ContractKey {side, kind, key, spec}]`, the
  distinct keys of the contract table (serde default: an older profile has
  none). `RepoDeps::depends_on` adds `depends_on_contracts`: a repository
  depends on another when one of its consumed keys matches a key the other
  provides, or one of its code-provided keys matches a spec key of the
  other (a server implementing a spec published elsewhere); a provider
  never depends on a consumer, and two servers or two specs never depend
  on each other. A consumer's contract edges come from the contract tables
  alone (`rueca-index` `contract_link`), without the joint resolve of
  §9.2: a dependency reached only through contracts is not mounted, and
  contract edges of the joint graph are dropped there, so they are not
  counted twice. Each row of the consumer is matched against the provider
  rows of its dependencies' default entries and of its own segment, with
  the matcher of the resolver; only edges into a dependency are kept. They
  go into the **same link set** as the package edges (`LinkEdge`,
  `meta.strategy = Contract`), so versioning, recomputing reverse
  dependencies, refs, the remap of incoming edges and visibility apply
  unchanged. `LINKER_VERSION` 1 → 2 recomputes every profile and link set
  once.
- **Ambiguity across repositories.** The best quality and the number of
  providers sharing it are counted over the consumer's own providers and
  every dependency's: a call that a handler of its own repository and one
  of another repository match equally well gets a 0.5 link edge, and a
  better match in its own repository (an exact route against the other's
  template) leaves no link edge. The in-snapshot edge keeps the confidence
  its resolver gave it, which cannot see other repositories. A provider
  that is a test or sits in a test file is not a link target.
- **Refs.** A ref's contract edges are part of its link set, computed when
  it is indexed from its own contract table against its dependencies'
  defaults (§9.3). On a view that reads a provider at a ref, incoming
  contract edges are remapped like package edges, by node (label,
  qualified name, path): an edge whose handler, `Route` or `RpcMethod` the
  ref no longer has is dropped, and a handler kept under a changed route
  keeps its edge until the consumer's link set is recomputed.
- **Surfaces.** `trace` follows contract link edges like any link edge
  (`edge_types: ["CALLS", "IMPLEMENTS"]`), strategy `contract`, the call
  site as `call_site`. `list_contracts` (MCP) and `rueca contracts` (CLI,
  every page merged) list the contract tables of the selected repositories
  with `side` `provided` / `consumed` / `dangling`, `kind`, `query` and
  `limit`; each entry carries `matches`, its counterparts among the visible
  repositories, and a dangling entry is a consumed row with none, computed
  per query and not stored. `rueca links --org` ends contract edges with
  `[contract <key>]`; its JSON adds `via` (`package` / `contract`), `key`
  (the calling row's key) and `dst_name` (the target's name, an OpenAPI
  operation's `GET /users/{id}`). `list_repositories`' `depends_on` names
  contract dependencies too.
- **RFC 0001 §10 as built.** There is no org-link segment of `CROSS_*`
  edges: contract edges are the normal edge kinds (`CALLS`, `IMPLEMENTS`)
  in the link sets, and dangling contracts are computed at query time, not
  stored. RFC 0001 §6.3's systems edges (`HANDLES`, `CALLS_ROUTE`,
  `CALLS_RPC`) are not produced.

Choices and deviations:

- **Hints on definitions, not route nodes.** A code route is a row on its
  handler, not a node of its own with `HANDLES` (RFC 0001 §6.3): the
  handler is the code an agent reads, so `trace` from a client reaches it
  in one hop, and a route node would be a second hop with no source of its
  own. Spec operations were nodes already, since the spec file is their
  source. The row keeps the raw text and the framework for
  `list_contracts`.
- **Spec and code providers both get `CALLS`.** A client of a service that
  publishes a spec reaches both the handler (the behaviour) and the
  operation (the documented contract). As competing providers, every call
  into a specified service would be ambiguous (0.5); matched separately,
  each keeps its confidence, and `IMPLEMENTS` ties the handler to the
  operation.
- **No suffix- or prefix-tolerant matching.** A tolerant match
  (`/api/v1/users/{}` against `/users/{}`) would recover the mount
  prefixes the packs do not compose, but it makes every short route (`GET
  /health`, `GET /{}`) a provider for many calls, against the
  precision-first rule (RFC 0004 §3.4). The gap is recorded below and
  shows as recall.
- **`spec` in `ContractKey`.** Dependencies are decided from keys, before
  any row is read, and whether a provided key comes from a spec decides
  whether a server depends on that repository (`IMPLEMENTS` goes code →
  spec only). The field is serialized only when true.
- **Test doubles.** A mock server or fake handler in a test (a test
  definition, a test path, a Go `_test.go` file:
  `rueca_model::contract::is_test_source`) is never a match candidate,
  inside one snapshot or across repositories, as package edges never
  point into a dependency's tests; measured on dapr's Python SDK, 83 of
  99 stub calls would otherwise have matched `FakeDaprSidecar`. The rows
  stay in the contract table, and `list_contracts` still counts them as
  counterparts, so a call matched only by a test double is not listed as
  dangling (open question 18).
- **`*` for an unknown method or service.** A call whose method is not a
  literal (`http.NewRequest(method, …)`), Django and `app.all` routes, and
  a Go server method whose service its file does not name get `*` and
  match at 0.7: dropping them loses the edge, guessing makes it wrong.
- **The one-hop wrapper rule** (TS / JS). Client libraries call through
  one function that interpolates its parameter into the URL
  (`` `${host}/api/${endpoint}` ``); every call of that function in the
  same file that passes a string literal there is a consumer too, at the
  caller, with the literal in place. One hop, same file. Go and Python reach the same shape at the call site: an
  SDK wrapper called with a method and a path (`c.do(ctx,
  http.MethodPost, "/api/generate")`, `self._request("POST",
  "/api/generate")`).
- **Not composed**: mount prefixes across files (a router mounted under
  `/api` in another file, `include_router(prefix=…)` elsewhere, an
  OpenAPI `servers` path or a Swagger `basePath`); routes registered from
  runtime tables (dapr's HTTP API, go-swagger's generated handlers);
  Next.js file routes (`[id]` canonicalizes, but files are not routes);
  route registrations inside Rust macros. Topics come only from
  kafka-python / aiokafka; no other broker client or language emits them.

Measured (bench S2, `bench/README.md`, *Cross-service edges*: groups
ollama, qdrant, dapr and weaviate gated, openai diagnostic; precision /
recall pooled over the gated groups per kind, the *all* row shown, not
gated; RFC 0004 §5):

| Kind | Measured (precision / recall) | Floor | Target |
|---|---|---:|---:|
| http-code | 98.3 / 98.0 (n=118 predicted, 99 gold) | ≥ 90 / 75 | ≥ 97 / 90 |
| http-spec | 100.0 / 100.0 (n=131 predicted, 110 gold) | ≥ 90 / 75 | ≥ 97 / 90 |
| rpc-spec-client | 96.1 / 100.0 (n=228 predicted, 212 gold) | ≥ 90 / 75 | ≥ 97 / 90 |
| rpc-spec-server | 100.0 / 90.0 (n=18 predicted, 20 gold) | ≥ 90 / 75 | ≥ 97 / 90 |
| rpc-code | 95.5 / 95.4 (n=290 predicted, 283 gold) | ≥ 90 / 75 | ≥ 97 / 90 |
| all (not gated) | 96.9 / 97.7 (n=785 predicted, 724 gold) | — | — |
| Organization layer time per group | ollama 579 ms (links 214 ms), qdrant 3,957 ms (links 2,767 ms), dapr 1,821 ms (links 1,144 ms), weaviate 2,554 ms (links 1,072 ms) | — | — |
| Dangling consumers per group | ollama 32 (labelers 11), qdrant 11 (labelers 0), dapr 185 (labelers 10), weaviate 136 (labelers 12) | — | — |

## 10. Amendments

- **RFC 0001 §10** — the org linker becomes the contract producer of the
  link layer (§3.3); as built (§9.4) it writes no org-link segment and no
  `CROSS_*` edges (nor RFC 0001 §6.3's `HANDLES` / `CALLS_ROUTE` /
  `CALLS_RPC`): contract edges are normal edge kinds (`CALLS`,
  `IMPLEMENTS`, strategy `contract`) in the link sets, and dangling
  contracts are computed at query time, not stored; §11.1 views become
  "organization generation + optional ref replacement" (§4).
- **RFC 0005 §5** — remove rank fusion across repositories; `repo` semantics
  per §6.
- **2026-11-16 — organization suite** (§8, §9.2). The 21-instance
  scenario (one issue per repository) is replaced by 42 gated members
  pinned at the base commit of their first gated S1 instance, up to 3
  issue instances each whose real-fix gold is present at the pin and whose
  fix is not already there (120 gated; a calibration organization of 50
  members with 29 instances, never gated), plus 16 impact instances
  scored on the consumers of the changed library symbols; BM25 over the
  union of the members joins as the floor arm; every rate carries a Wilson
  interval and every paired comparison its discordance and minimum
  detectable effect (RFC 0004 §7.6). Cross-repository edges grow to 12
  labelled pairs (Rust included) and are reported over all pairs, without
  Rust and Rust alone. The earlier 21-instance numbers are another
  population and are no longer carried.
- **2026-11-18 — Rust pairs after the Rust fixes** (§8, §9.2, §12 q.10).
  Items inside macro bodies, attribute macros as decorators, value and
  type namespaces and crate-scoped types (PR fix/rust-tokio-gaps) lift the
  cross-repository edges to 99.4 / 94.7 over the 12 pairs (Rust alone 98.3
  / 93.1), over the floor and at the recall target; three of the four
  Rust findings of §9.2 are closed.

## 11. Alternatives considered

- **Federated search with rank fusion** (RFC 0005 as written). Rejected:
  measured 9.5% vs 61.9% (§2); 24.2% vs 66.7% on the organization suite
  (§8).
- **Global statistics injected into per-repository searches** (no shared
  index). Possible with `Bm25StatisticsProvider`, but needs every
  repository's index opened per query and gives no place for cross-repository
  edges. §4 planned it for the ref overlay; as built (§9.3) the overlay
  is a selection of segments of the one index, so it is not used.
- **One monolithic organization graph, no repository layer.** Rejected:
  re-indexing any repository would rebuild everything, and branches,
  pull requests and per-repository use would lose their unit.

## 12. Open questions

1. ~~Repository routing signals (README, package names, ownership) as extra
   fields of a per-repository document in the organization index, so a query
   can also rank repositories?~~ Done in phase 2 (§9.2): a profile per
   repository plus the term distribution of the organization index, used as
   a prior; ownership is not a signal yet.
2. Package registry conflicts: two repositories publishing the same name
   (forks, monorepo vs split) — prefer the one the consumer's lockfile pins?
   The same pin could pick the library's matching ref (a consumer locked
   to `click==8.5.1` linked against click's `8.5.1` entry) instead of its
   default (§9.3 links every ref against the defaults).
3. Size limits: one tantivy index for thousands of repositories (expected
   fine; tantivy targets much larger corpora) — measure at 1k repositories.
4. Organization-level priors (archived repositories, test-only repositories)?
5. Service mode: materialize refs from the object store (or as delta
   segments over the base) instead of exporting trees under the store, and
   share the segment write between refs, which the facts cache does not
   (§9.3).
6. Lifecycle of `pr/<n>` refs: they are fetched again on every `rueca index
   --org` and kept until `--remove`. Drop merged or closed pull requests,
   or refs not read for a while, by a policy?
7. ~~`changed` per symbol rather than per file blob~~ — done (2026-11-24,
   span text compared directly; §9.3 *Collapse per tool*). Symbols that
   only moved are still listed once per version (their lines differ).
8. Unchanged re-indexing: skip writing a new segment when a working tree
   has the commit, blobs and Rueca version of its current one, or key link
   sets by content, so that consumers and their refs are not re-linked
   (§9.3, measured).
9. A store lock for concurrent indexers: two `rueca index` runs (or an
   index and an organization update) read-modify-write `catalog.json`, the
   organization pointer and garbage collection without one, so the last
   writer wins and a collection can race a registration. Local mode runs
   one indexer at a time; service mode needs the lock (or a transactional
   catalog).
10. Rust in the link layer (§9.2, open findings): ~~attribute macros
   (`#[tokio::main]`, `#[tokio::test]`) as uses, items and `mod` / `use`
   inside macro invocation bodies (`cfg_net! { … }`), method resolution
   across repositories that does not pick a same-named type of another
   crate~~ (fixed 2026-11-17, PR fix/rust-tokio-gaps: Rust pairs 94.5 /
   53.2 → 98.3 / 93.1, all pairs 98.3 / 74.9 → 99.4 / 94.7), and Cargo
   dependencies matched by version as well as name; methods of blanket
   extension traits (`ServiceExt::oneshot`) are the largest share of the
   Rust misses left.
11. Impact: the consumers of a changed library symbol are not surfaced by
   the organization ranking (consumer hit@10 12.5% vs 37.5% for BM25 over
   the union, 16 gated impact instances). Which signals answer "who uses
   what this fix changes" inside one ranking — semantic search (RFC 0007)
   and graph signals from the link layer (callers of the matched library
   symbols across repositories) — is the question the impact kind
   measures.
12. Contract version pinning across refs: a client pinned to a version of
    a service (a lockfile, a generated client's spec version) linked to the
    provider's matching ref instead of its default, as open question 2
    asks for packages (§9.4 links every ref against the defaults).
13. Mount prefixes across files: a resolver pass over the rows' `raw` text
    and the files that mount a router (`app.use("/api", router)`,
    `include_router(prefix=…)`, an OpenAPI `servers` path, a Swagger
    `basePath`) that composes the prefix into the keys before matching,
    rather than a prefix-tolerant matcher (§9.4).
14. Topics beyond kafka-python / aiokafka: other clients and languages,
    the broker in the key, topic names from constants (RFC 0001 §9.4).
15. An `ambiguous` marker in the evidence: today confidence 0.5 is the only
    signal; RFC 0001 §10 asked for a match explanation per edge (exact,
    template, ambiguous), which the edge metadata has no field for.
16. Remap at refs by key as well as by node, so that a route moved to
    another handler at the ref keeps its callers and a handler whose route
    changed loses them (§9.4 remaps by node).
17. GraphQL: schema fields as spec providers, resolvers as code providers
    and client operations as consumers.
18. Test doubles and `list_contracts`: `matches` counts test providers as
    counterparts although they are never match candidates, so a call that
    only a test double answers is neither linked nor listed as dangling;
    count counterparts with the same filter, or list the test match as
    such.
