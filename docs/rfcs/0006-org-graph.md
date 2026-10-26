# RFC 0006 — The organization graph

- Status: Draft
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

1. Repository routing signals (README, package names, ownership) as extra
   fields of a per-repository document in the organization index, so a query
   can also rank repositories?
2. Package registry conflicts: two repositories publishing the same name
   (forks, monorepo vs split) — prefer the one the consumer's lockfile pins?
3. Size limits: one tantivy index for thousands of repositories (expected
   fine; tantivy targets much larger corpora) — measure at 1k repositories.
4. Organization-level priors (archived repositories, test-only repositories)?
