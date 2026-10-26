# RFC 0006 — The organization graph

- Status: Draft; phases 1 and 2 (§9) are implemented
- Created: 2026-10-26
- Depends on: RFC 0001 (§8 storage, §10 org linker, §11 query engine,
  §13 permissions), RFC 0005 (responses, `repo` / `ref`)
- Amends: RFC 0001 §10–§11 (org linker → org layer, views), RFC 0005 §5
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
rankings with reciprocal rank fusion. The benchmark's multi-repository
scenario (21 repositories in one store) showed why that fails: with
`repo: "*"`, File Acc@5 drops to **9.5%** against **61.9%** when the right
repository is queried alone, and 94% of the top 10 comes from other
repositories. Rank fusion gives every repository's best hit the same weight,
so the 21 first places tie; BM25 scores from separate indexes cannot be
compared because each index has its own term statistics.

A federated search treats the organization as N islands. Agents in an
organization ask questions that cross them: where is this endpoint
implemented, who calls this shared library, which service reads this
variable. The answer needs one ranking and real edges between repositories.

## 3. Layers

```
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
2. **Contracts (later).** HTTP routes, RPC methods, topics, env vars and
   config keys (RFC 0001 §9.4, RFC 0003 §7) matched across repositories: the
   original org linker, now one producer of the link layer.

The link layer is recomputed only for repositories whose snapshot changed
and for their reverse dependencies (repositories that import a package
they publish), kept in the org catalog.

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

| Metric | Today (rank fusion) | Floor | Target |
|---|---|---|---|
| File Acc@5, organization scope vs the right repository alone | 9.5% vs 61.9% | ≥ 0.85 × single-repository | ≥ 0.95 × |
| Cross-repository edges (packages) precision / recall on labelled pairs | — | ≥ 90 / 75 | ≥ 97 / 90 |
| Incremental update: re-index one repository of 21 | — | org layer ≤ 2× the repository's own index time | ≤ 1× |
| Tokens per correct answer, organization scope | 831 vs 337 single-repo (top 10) | reported | ≤ 1.2 × single-repo |

Cross-repository gold: pairs of corpus repositories where one depends on
another (a library and a consumer pinned to it), labelled from the
consumer's imports.

## 9. Phasing

1. **Organization symbol index + scope API + local `--org`** (benchmark:
   organization File Acc@5, incremental update).
2. **Link layer, packages**: manifests → package registry → cross-repository
   import resolution and edges; `trace` across repositories.
3. **Refs over the organization** (§4), with the multi-ref collapse of
   RFC 0005 §6.
4. **Contracts** in the link layer (the original org linker; suite S2).

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

Measured (bench organization suite, 21 repositories, `bench/README.md`):

| Metric | Phase 1 | Floor | Target |
|---|---|---|---|
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
  searched alone in the same store (`bench/README.md`, *Org scenarios*):

  | Routing | File Acc@5 org / single | Other repositories in the top 10 | Tokens per correct answer, org / single |
  |---|---|---|---|
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

Measured (bench organization suite, `bench/README.md`):

| Metric | Phase 2 | Phase 1 | Floor | Target |
|---|---|---|---|---|
| File Acc@5, organization scope vs the right repository alone, calibration split (35 instances, 28 repositories) | 65.7% vs 71.4% (0.92×) | 45.7% (0.64×) | ≥ 0.85× | ≥ 0.95× |
| File Acc@5, organization scope vs the right repository alone, gated (21 instances, 21 repositories) | 42.9% vs 61.9% (0.69×) | 47.6% (0.77×) | ≥ 0.85× | ≥ 0.95× |
| Other repositories' share of the top 10, gated | 15.7% | 36.7% | — | — |
| Tokens per correct answer, organization scope (top 10), gated | 782 vs 529 (1.48×) | 886 (1.67×) | reported | ≤ 1.2× |
| Cross-repository edges (packages), precision / recall on 7 labelled pairs (Python, PHP, Java) | 99.7 / 95.3 (imports 100 / 100, uses 99.7 / 91.4) | — | ≥ 90 / 75 | ≥ 97 / 90 |
| Incremental update: re-index micropython of 21 (profiles, routing, links included) | 499 ms vs 3,616 ms (0.14×) | 0.05× | ≤ 2× | ≤ 1× |
| Organization layer per added repository (21 steps) | median 206 ms, max 1,413 ms (0.26× / 0.48×) | 0.10× / 0.40× | ≤ 2× | ≤ 1× |

Routing moves the calibration split past the floor but not the gated set:
of its three phase-1 misses, `fmtlib/fmt` and `coder/code-server` are now
routed to the right repository and still miss because the right file
ranks lower under organization-wide statistics than alone; `jqlang/jq`
is not routed — its distinctive terms (`NO_COLOR`, `isatty`) occur in a
handful of symbols of `src/main.c`, while other repositories hold more of
the query's generic words (`terminal`, `color`, `support`); and
`micropython/micropython` (a number-formatting question that
`fmtlib/fmt` matches better) is lost. The gated set is 21 instances (one is 4.8 points); the
next levers are per-repository ranking within a routed repository
without losing comparable scores (a score transplant: the organization
score distribution, the repository's own order) and a larger calibration
split to tune it on.

## 10. Amendments

- **RFC 0001 §10** — the org linker becomes the contract producer of the
  link layer (§3.3); §11.1 views become "organization generation + optional
  ref replacement" (§4).
- **RFC 0005 §5** — remove rank fusion across repositories; `repo` semantics
  per §6.

## 11. Alternatives considered

- **Federated search with rank fusion** (RFC 0005 as written). Rejected:
  measured 9.5% vs 61.9% (§2).
- **Global statistics injected into per-repository searches** (no shared
  index). Possible with `Bm25StatisticsProvider`, but needs every
  repository's index opened per query and gives no place for cross-repository
  edges; kept only for the ref overlay of §4.
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
3. Size limits: one tantivy index for thousands of repositories (expected
   fine; tantivy targets much larger corpora) — measure at 1k repositories.
4. Organization-level priors (archived repositories, test-only repositories)?
