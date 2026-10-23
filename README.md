# Telar

Code intelligence engine for coding agents. One core, three deployment modes:

- **Local** — single binary, indexes a working tree, MCP over stdio.
- **Service** — one container, snapshots in object storage, MCP over HTTP.
- **Org brain** — indexer workers + stateless query nodes over every repo in
  GitHub / GitLab / Bitbucket, with per-repo permissions mirrored from the host.

Local mode can overlay uncommitted changes on top of the org snapshot.

## Scope

In: source code and what lives in repositories (README, ADRs, OpenAPI, infra
manifests, diagrams). Pull requests and issues are a v2 candidate.

Out: Notion, Drive, and other document stores. Agents federate those through
their own MCP servers; Telar exposes stable symbol IDs
(`repo@sha:path#symbol`) so results can be cross-referenced.

## Parity contract

Telar is built from scratch. codebase-memory-mcp v0.12.0 is the reference
baseline: for every supported language, Telar must match or beat it on the
benchmark (localization, cross-service edges, structural questions) before it
is recommended as a replacement.

See `docs/decisions.md` and `docs/rfcs/`.

## Status: v0.1.0-alpha.1 (local mode)

One binary, MCP over stdio, Python and TypeScript/JavaScript. Indexes a
working tree (no service, object storage, permissions or PR deltas yet).
Six tools: `search_symbols`, `search_code`, `get_symbol`, `get_snippet`,
`trace`, `get_file_outline` — contract in `docs/tools.md`.

```sh
cargo build --release -p telar-cli
target/release/telar index path/to/repo
target/release/telar query search_symbols '{"query":"session request"}' --root path/to/repo
target/release/telar mcp --root path/to/repo      # MCP server on stdio
```

Benchmark against cbm v0.12.0: `docs/bench/alpha-results.md` (method in
`bench/README.md`). The alpha does **not** meet the parity gate yet: it leads
on TS/JS call edges and query latency, trails on localization ranking and on
Python structural questions.

### Crates

| Crate | Role | Assembled from |
|---|---|---|
| `telar-model` | data model, symbol IDs, blob SHA | — |
| `telar-extract` | `LanguagePack` contract, budgeted parsing | tree-sitter 0.27 |
| `telar-lang-python` | Python extraction, module resolution | tree-sitter-python; locals query adapted from nvim-treesitter + bloop |
| `telar-lang-typescript` | TS/TSX/JS extraction | tree-sitter-typescript/javascript; locals from nvim-treesitter; imports via `oxc_resolver` |
| `telar-resolve` | cross-file resolution with strategy + confidence | design from bloop scope graph, scip-syntax, codanna |
| `telar-segment` | segment bundle, symbol + trigram search, content packs, catalog | tantivy, roaring, regex-syntax, zstd, memmap2, blake3 |
| `telar-index` | worktree walk, facts cache, parallel extraction | ignore, rayon, gix |
| `telar-query` | the six tools | schemars |
| `telar-mcp` | MCP stdio server | rmcp 3.7 |
| `telar-cli` | `telar` binary | clap |
