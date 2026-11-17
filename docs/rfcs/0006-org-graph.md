# RFC 0006 — The organization graph

- Status: Draft; phases 1–3 (§9) are implemented
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

Phases 1–3 are implemented (§9.1–§9.3); phase 4 is next.

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
  among nodes sharing that key, and its versions compared by
  file blob. `search_symbols` fetches `limit` per snapshot, collapses, then
  cuts at `limit` (the cursor counts collapsed items); `get_symbol` answers
  the best match's version and lists the others in `versions` (`{s, in,
  changed, symbol, callers, callees}`), not as candidates;
  `get_file_outline` returns the first listed ref's outline and every other
  distinct version of the file in `versions`; `trace` walks from every
  version of the root and returns the union graph, `in` on nodes and a
  trailing `{in, changed}` object on the edges of that repository.
  `search_code`, `get_snippet` and `format: "verbose"` do not collapse;
  single-snapshot responses are unchanged.
- **`changed` is per file blob.** Facts are extracted per blob, so the
  same blob is the same symbol for certain and costs no comparison; the
  price is that a symbol in a changed file is `changed` even when its own
  text is identical (`click.types.ParamType.split_envvar_value`, lines
  205–213 in both `main` and `8.5.1`, comes back twice). Comparing a
  per-symbol content hash is open (§12).
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
|---|---|---|---|
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
7. `changed` per symbol rather than per file blob (a content hash of the
   symbol's span), so that only symbols that differ are listed twice.
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
