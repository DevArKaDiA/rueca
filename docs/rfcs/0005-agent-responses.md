# RFC 0005 — Responses for agents: org-wide, multi-ref, token-efficient

- Status: Draft; the local-mode part (§9) is implemented
- Created: 2026-10-25
- Depends on: RFC 0001 (§6.1 identifiers, §8.3 catalog, §11 query engine,
  §13 permissions), RFC 0004 (efficiency metric)
- Amends: `docs/tools.md` (response envelope, identifiers, `ref`, defaults),
  RFC 0004 §5 / §6 (efficiency metric)

## 1. Summary

Rueca's north star is a **service that indexes every repository of an
organization, including branches and pull requests**, queried by agents.
Local mode is the first deployment of the same engine. This RFC fixes the
shape of tool responses so that it is right for that future and cheaper in
tokens today:

1. Identifiers stay **fully pinned** (`repo@commit:path#qn`) — the repository
   and the commit are never dropped — with the commit **abbreviated to 12
   hex digits**.
2. Every response opens with an **envelope** listing the snapshots it read
   (repository, ref, commit, overlay); items refer to them instead of
   repeating them.
3. Every tool takes a **`ref`** (branch, pull request, tag, commit) and a
   `repo` that may name several repositories; results from several
   snapshots are **merged by rank fusion**.
4. A symbol identical across refs (same blob, same extraction) is returned
   **once, with the refs it appears in**.
5. Results are **grouped by repository and file**, `trace` edges point at
   **node indexes**, defaults are smaller, every tool takes a **token
   budget**, and MCP text content is a **compact text rendering** next to
   the full JSON in `structuredContent`.
6. Efficiency is measured as **tokens per correct answer**, not raw tokens.

§9 separates what lands in local mode now from what is prepared for the
service and org modes.

## 2. Motivation

Measured on the alpha (RFC 0004 efficiency suite), Rueca's answers cost
3–13× the tokens of codebase-memory-mcp's on the same queries. One
`search_symbols` result today:

```json
{"id": "github.com/DevArKaDiA/rueca@7696364aef89a22e4a2fe84f6fd23e186bef9aa8:bench/micro/php/inheritance/abstract_template/main.php#Importer::import",
 "name": "import", "qualified_name": "Importer::import", "label": "Method",
 "path": "bench/micro/php/inheritance/abstract_template/main.php",
 "start_line": 4, "end_line": 8, "signature": "public function import(): void",
 "score": 116.36391}
```

About a third is repetition: the 40-character commit, the path twice, the
qualified name twice, a five-decimal score. 20 results by default make
~2,200 tokens. `trace` repeats both full ids on every edge.

The obvious fix — drop the repository and commit from ids when one
repository is indexed — is wrong for Rueca. In an organization view the
same `path#qn` exists in many repositories, and with branches and pull
requests the same symbol exists in several commits, possibly different in
each. Ids must stay unique and stable across repositories, refs, calls and
service replicas (RFC 0001 §6.1). The savings have to come from structure,
not from losing identity.

Raw token counts are also the wrong target: a tool that returns fewer and
worse results looks cheaper. On C++ structure cbm returns fewer neighbours
with 40% callers recall where Rueca has 85%.

## 3. Identifiers

```
<repo>@<commit12>:<path>#<local-qualified-name>
github.com/acme/billing@7696364aef89:src/invoices/service.py#InvoiceService.issue
```

- `<repo>` is always present (`github.com/acme/billing`, `local/shop`).
- `<commit12>` is the first 12 hex digits of the commit (git's convention
  for unambiguous short ids in large repositories). Tools **accept** the
  full commit, any unique prefix ≥ 7, or a ref name (`@main`, `@pr/123`),
  and always **return** the 12-digit form.
- Local uncommitted changes (RFC 0001 §11.2 overlay) use the commit of the
  base plus a `+` marker: `local/shop@7696364aef89+:src/x.py#f`. The
  envelope says what the overlay contains.
- Files: `#` part omitted. Env vars: pseudo-path `$env` (unchanged).
- Host URLs (RFC 0001 §6.1) are not repeated per item: the envelope carries
  a URL template per snapshot (`https://github.com/acme/billing/blob/<commit>/{path}#L{start}-L{end}`).

## 4. The envelope

Every tool response is:

```json
{
  "snapshots": [
    {"s": 0, "repo": "github.com/acme/billing", "ref": "main",
     "commit": "7696364aef89a22e4a2fe84f6fd23e186bef9aa8", "overlay": false,
     "url": "https://github.com/acme/billing/blob/7696364aef89…/{path}#L{start}-L{end}"},
    {"s": 1, "repo": "github.com/acme/billing", "ref": "pr/123",
     "commit": "a1b2c3d4e5f6…", "overlay": false, "url": "…"}
  ],
  "results": …,
  "truncated": false,
  "next_cursor": null,
  "budget": {"limit_tokens": 2000, "used_tokens": 640}
}
```

- `snapshots` lists only snapshots the caller may read (RFC 0001 §13: views
  are filtered by grants before execution; nothing about invisible
  repositories leaks, not even their names).
- Items refer to a snapshot by `s` (an index into `snapshots`, valid within
  the response). The id of each item stays fully pinned, so an id copied
  out of a response is valid on its own.

## 5. Repositories and refs in requests

- **`repo`**: one repository id or root path (as today), a list, or `"*"`
  for every repository in the view. Default: the server's default scope
  (local: `--root`; service: the caller's configured scope; org: every
  visible repository).
- **`ref`**: branch, pull request (`pr/<n>`), tag, or commit prefix.
  Default: the repository's default branch; local mode: the working tree
  (base + overlay). A list compares refs (§6).
- An unknown or unindexed ref is an error that lists the indexed refs of
  that repository.

> *Amended by [RFC 0006](0006-org-graph.md) (2026-10-26): rank fusion
> across repositories is removed; an organization has one symbol index and
> one ranking. The paragraph below is kept for history.*

**Merging results across snapshots.** BM25 scores and the ranking priors
(RFC 0004, segment `priors`) are computed per segment and are not comparable
across repositories. Search results from several snapshots are merged by
**reciprocal rank fusion** (k = 60) over the per-snapshot rankings, then the
global priors (non-test, callable) are re-applied; `score` is the fused
rank score. Traversals follow edges inside each snapshot and cross-repository
edges from the org link segment (RFC 0001 §10).

## 6. Multi-ref results: collapse what is identical

Extraction is per blob (RFC 0001 §7), so a symbol whose file blob is the same
in two refs has identical facts. When a query reads several refs of one
repository:

- symbols with the same `(path, qualified_name, blob)` are **one item** with
  `"in": [0, 1]` (snapshot indexes) instead of two;
- symbols that differ appear once per distinct version, each with its own
  `in` list; a `changed: true` flag marks items whose content differs across
  the requested refs;
- `trace` over several refs returns the union graph with each edge's `in`
  list, so "who calls `f` on the PR but not on `main`" is answerable from one
  response.

This is what an agent reviewing a pull request needs: the unchanged code
once, the difference highlighted.

## 7. Structure and defaults

**Grouping.** List results are grouped by snapshot and file:

```json
"results": [
  {"s": 0, "path": "src/invoices/service.py", "symbols": [
     {"id": "…#InvoiceService.issue", "name": "issue", "qn": "InvoiceService.issue",
      "label": "Method", "lines": [41, 63], "sig": "def issue(self, invoice_id: str) -> Invoice"},
     {"id": "…#InvoiceService.cancel", "…": "…"}]}
]
```

- Field names shortened in the envelope form (`qn`, `sig`, `lines`); the long
  names stay available with `"format": "verbose"` for one release.
- Inside a group the item's path and snapshot are implied; `id` keeps the
  full pinned form.
- `score` is rounded to 3 significant digits and omitted unless
  `"scores": true`.

**`trace`.** Nodes are a list; edges refer to node indexes:

```json
"nodes": [{"id": "…", "qn": "…", "label": "Method", "path": "…", "lines": [7, 8], "d": 1, "dir": "in"}],
"edges": [[0, 3, "CALLS", 0.9, "type_resolved", 12]]
```

(`[src, dst, kind, confidence, strategy, call_site_line]`; the call-site path
is the source node's file unless an eighth element gives it.)

**Defaults.** `search_symbols` limit 20 → **10**; `search_code` 50 → **20**;
`trace` depth 2 → **1**, node limit 200 → **50**. Every tool takes
`budget_tokens` (default 2,000): the response is cut at that size, marked
`truncated`, with a cursor to continue.

**Text content.** MCP results carry the JSON above in `structuredContent`
and a compact text rendering as text content, one line per item:

```
acme/billing@7696364aef89 (main)
src/invoices/service.py
  Method InvoiceService.issue  L41-63  def issue(self, invoice_id: str) -> Invoice
  Method InvoiceService.cancel L65-80  def cancel(self, invoice_id: str) -> None
```

Clients that read `structuredContent` see the full data; models that read
text see the compact form. The CLI prints JSON by default and the text form
with `--text`.

## 8. Measuring efficiency

RFC 0004's efficiency metric changes from "tokens per answer" to **tokens
per correct answer**, measured on the text content an agent would read:

- `search_symbols` (localization): tokens until the first gold hit, and
  tokens per gold hit in the top k;
- `trace` (structure): tokens per true edge returned (gold edges in the
  answer);
- reported per tool and language, Rueca and cbm, with the same queries.

The target of RFC 0004 §6 ("≤ 50% of cbm") applies to the new metric. Two
new bench scenarios measure the org path: a **multi-repository store**
(several corpus repositories indexed together, queries with `repo: "*"`),
and a **multi-ref** case (a repository at two commits with a known diff:
collapse correctness and savings).

## 9. What lands when

| Part | Local mode now | Service / org |
|---|---|---|
| Ids with 12-digit commit, accept prefixes / ref names | ✅ | same |
| Envelope (`snapshots`, `s`) | ✅ (one snapshot, overlay flag) | many |
| `repo` list / `"*"` with rank fusion | ✅ across locally indexed roots | across the org view |
| `ref` argument | ✅ (working tree; other refs error until the catalog keeps several snapshots per repository) | branches, PRs, tags |
| Collapse across refs (§6) | design and tests on two local snapshots | delta segments + blob visibility |
| Grouping, short fields, `trace` edges by index, defaults, `budget_tokens` | ✅ | same |
| Text content rendering | ✅ | same |
| Host URL templates | ✅ when the remote is known | ✅ |
| Efficiency metric + multi-repo / multi-ref bench | ✅ | — |

The local catalog grows from "one segment per root" to "snapshots per
(repository, ref)" (RFC 0001 §8.3) when local multi-ref indexing lands; this
RFC only requires the tool contract to be ready for it.

**Implemented (local mode).** The "Local mode now" column landed in
`rueca-query` (`snapshot`, `compact`, `multiref` modules), `rueca-mcp`
(`structuredContent` + text rendering) and the CLI (`query --text`);
`docs/tools.md` is the contract. Choices and deviations:

- **Ref names.** A local snapshot's `ref` is its checked-out branch
  (recorded at index time in `SnapshotInfo::branch`), `HEAD` when detached,
  `worktree` outside git. `ref` accepts those, `worktree`, `HEAD`, or the
  commit (full, ≥ 7-digit prefix, 12-digit form with `+`). Outside git there
  is no base commit, so no `+` and no URL.
- **Ids.** Both formats return the pinned 12-digit form. An id whose commit
  is not indexed resolves against the repository's current snapshot (local
  mode only, so ids survive a re-index); the service should be strict.
- **Budget** (open question 1): counted on the text rendering as
  `ceil(bytes / 4)`. At least one item is always returned. Cursors are item
  offsets (the next line for `get_snippet`); `get_symbol` cuts its lists
  without a cursor and reports `omitted`.
- **Grouping**: groups are ordered by their best-ranked item, so
  `search_symbols` order inside the response is by file, not strictly by
  rank (`"scores": true` shows the ranking).
- **`trace`**: node 0 is the root (`d` 0); a node reachable both ways is one
  node with `dir: "both"`. The call-site path is a *seventh* element (§7's
  "eighth" counted one too many).
- **Text rendering** prints the local name (the id's `#` part) and the
  repository id in full in the header, so header + group path + name spell
  the id.
- **Several repositories**: the fused score is `prior / (60 + rank)` with the
  priors ×0.5 test, ×0.5 field / variable / constant. *(Replaced by RFC 0006
  phase 1: one ranking with BM25 statistics over every selected repository;
  responses list only the snapshots their answer touched.)* Name lookups take the
  best match over the selected snapshots (others as `candidates`); a path in
  several snapshots is an error asking for `repo`.
- **Collapse (§6)**: `rueca_query::multiref::{collapse, collapse_edges}`
  with unit tests on two synthetic snapshots of one repository; an item
  absent from some requested snapshot of its repository is also `changed`.
  No local tool calls it yet.
- `format: "verbose"` keeps the alpha shapes *and* alpha defaults (limits,
  depth 2, no budget).

Measured on `psf/requests`, `gin-gonic/gin` and `sindresorhus/ky` (text
content vs alpha verbose JSON, ~tokens = bytes / 4):

| Tool | requests | gin | ky |
|---|---|---|---|
| `search_symbols`, defaults (10 vs 20) | 1,834 → 310 | 1,537 → 234 | 1,683 → 277 |
| `search_symbols`, limit 10 both | 868 → 310 | 778 → 234 | 833 → 277 |
| `trace` depth 1 | 2,034 → 391 | 843 → 174 | 2,306 → 434 |
| `get_symbol` | 470 → 376 | 2,349 → 472 | 3,039 → 526 |
| `get_file_outline` | 4,087 → 1,222 | 14,640 → 3,983 (budget: 1,998) | 6,658 → 1,904 |

## 10. Compatibility

Pre-1.0, so the contract changes in one release: `docs/tools.md` is
rewritten, `"format": "verbose"` keeps the alpha field names for one release,
ids with full commits keep resolving. The bench's arm adapters are updated
in the same PR.

## 11. Alternatives considered

- **Drop repository / commit from ids in single-repository stores.** Rejected:
  ids would change meaning when a second repository or a branch is indexed,
  and the org mode is the target.
- **Response-local short aliases as ids** (`s0:path#qn`). Rejected as the
  canonical form: an id copied into a later call must be valid on its own.
  They are used only as `s` references inside a response.
- **Text-only responses.** Rejected: clients and the bench need structured
  data; text is a rendering of it.
- **Raw token count as the metric.** Rejected (§2).

## 12. Open questions

1. `budget_tokens`: which tokenizer counts (the RFC 0004 reference
   `cl100k_base`, or bytes / 4 to stay model-neutral)?
2. Should the text rendering be the default for the CLI too, with `--json`?
3. Rank fusion constant and whether per-repository priors (stars, recency,
   ownership) belong in the org ranking.
4. Collapse key: `(path, qn, blob)` is exact; should renamed files with the
   same blob also collapse?
