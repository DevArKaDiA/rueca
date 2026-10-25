# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

First public alpha: local mode.

### Added

- `rueca` binary with `index`, `query`, `mcp` and `status` commands.
- MCP server over stdio (protocol versions 2024-11-05 through 2026-07-28)
  with six tools: `search_symbols`, `search_code`, `get_symbol`,
  `get_snippet`, `trace`, `get_file_outline`.
- Python and TypeScript / TSX / JavaScript extraction with tree-sitter and
  vendored locals queries; TS/JS module resolution with `oxc_resolver`
  (tsconfig paths, workspace packages).
- Cross-file resolution with per-edge strategy and confidence.
- Segment format: CSR adjacency, embedded tantivy symbol index, trigram
  content index, zstd content packs; content-addressed facts cache.
- Benchmark harness against codebase-memory-mcp v0.12.0 and a BM25 baseline
  (`bench/`), with published alpha results.
- Project logo and brand assets (`assets/logo/`).
- Automated releases: release-plz (versions, changelog, crates.io, tag) and
  cargo-dist (binaries, installer, GitHub release).

[Unreleased]: https://github.com/DevArKaDiA/rueca/commits/main
