# Decisions log

## 2026-10-22 — Alpha 1 (local mode) shipped; deviations from RFC 0001/0002

Alpha scope: local mode, one binary, MCP over stdio, Python + TS/JS, six tools.
Built by assembling the RFC 0002 components; deviations, all documented in the
crate docs:

- Node/file tables are postcard, not Arrow IPC (behind the reader; swap later).
- Trigram content index is Telar's own (sorted keys + roaring postings) rather
  than a tantivy n-gram field; `fst` not used (keys are fixed width).
- tantivy symbol index is embedded in the bundle and served through a
  read-only bundle directory (Quickwit pattern); doc id = node id for
  deterministic tie-breaks.
- Local catalog is `catalog.json`, not SQLite.
- `oxc_resolver` pinned to 11.23 (11.24 needs Rust 1.95; MSRV is 1.90), run
  over a virtual file system built from the snapshot's paths and configs.
- Local-binding detection uses vendored locals queries (nvim-treesitter,
  bloop; see NOTICE). scip-syntax Python locals is empty upstream.
- Git reads via gix; `dirty` is computed by comparing indexed blob SHAs with
  the HEAD tree instead of gix status.
- Benchmark Python call gold: scip-python + PyCG, not pyrefly. Function
  localization gold reuses LocAgent and SWE-PolyBench labels where available.

Results in `docs/bench/alpha-results.md`: parity gate not met. Telar leads
cbm on TS/JS callers/callees F1 and on query latency (~10 ms vs ~440 ms per
call), trails on S1 localization ranking (and both graph tools trail BM25), on
Python callees, and on JS named function expressions passed as arguments
(express definition-found 56%).

## 2026-10-20 — Scope: code only

Telar indexes code and repository-resident docs. External document stores
(Notion, Drive) are out of scope: the industry converged on per-source MCP
servers that handle their own permissions and freshness. Telar composes with
them instead of absorbing them.

## 2026-10-20 — Zero capability loss vs cbm (superseded 2026-10-21)

~~Migration uses a cbm-backed extractor adapter so every release keeps full
capability.~~ Replaced by the entry below.

## 2026-10-21 — Built from scratch; cbm is a baseline, not a dependency

Telar does not embed, wrap, or call codebase-memory-mcp. cbm v0.12.0 is the
reference baseline that the benchmark measures against. "No capability loss"
is an adoption gate, not a development constraint: Telar may be partial while
it is built, and an adopter switches only when Telar matches or beats cbm on
the benchmark for the languages and capabilities that adopter needs. Parity is
declared per language (see RFC 0001, supported-language tiers).

## 2026-10-22 — RFC 0002 amendments applied to RFC 0001

Segments become a bundle file (tantivy + mmap CSR + Arrow IPC + hotcache)
instead of SQLite; branch/PR deltas use blob-visibility bitmaps; precision
layer is SCIP plus pyrefly (Python calls), a PHPStan extension (PHP) and
index-store import (Swift), uploaded from adopter CI; MCP targets the
2026-07-28 spec with Telar as OAuth resource server; git network operations
use the `git` CLI and all reads use gix; data file kinds use pure-Rust
parsers.

## 2026-10-20 — Storage model (segment format superseded 2026-10-22)

Indexing unit is the git blob (content-addressed), so identical files across
branches and PRs are parsed once. Output is immutable segments (graph, full
text, trigram, vectors, file contents) stored in a local directory or object
storage, plus a small catalog (SQLite locally, Postgres as a service). Postgres
as the primary graph store was rejected: it breaks parity on BM25 ranking,
recursive traversal semantics, and per-node query latency.

## 2026-10-20 — Core language

Rust core (tree-sitter bindings, single static binary for local mode).

## 2026-10-20 — Open source, vendor-neutral

Telar is an independent open-source project. No company-specific code,
names, or data in this repository; adopters deploy it with their own
configuration.
