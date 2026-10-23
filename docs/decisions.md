# Decisions log

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
