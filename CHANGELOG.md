# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0-alpha.1](https://github.com/DevArKaDiA/rueca/releases/tag/v0.1.0-alpha.1) - 2026-10-27

### Added

- *(org)* repository routing and cross-repository package edges (RFC 0006 phase 2) ([#36](https://github.com/DevArKaDiA/rueca/pull/36))
- *(org)* organization symbol index, scoped queries and local --org (RFC 0006 phase 1) ([#33](https://github.com/DevArKaDiA/rueca/pull/33))
- *(bench)* tokens per correct answer, multi-repo and multi-ref scenarios, vendored code out of health ([#31](https://github.com/DevArKaDiA/rueca/pull/31))
- *(query)* RFC 0005 responses — pinned ids, snapshot envelope, multi-repo, compact output ([#30](https://github.com/DevArKaDiA/rueca/pull/30))
- *(bench)* RFC 0004 scorecard, structure over every function, new gold and micro-benchmarks ([#24](https://github.com/DevArKaDiA/rueca/pull/24))
- *(bench)* Go, Java, C/C++ and data-format suites ([#21](https://github.com/DevArKaDiA/rueca/pull/21))
- *(cpp)* C and C++ language pack and resolution ([#20](https://github.com/DevArKaDiA/rueca/pull/20))
- *(java)* Java language pack and resolution ([#19](https://github.com/DevArKaDiA/rueca/pull/19))
- *(go)* Go language pack and resolution ([#17](https://github.com/DevArKaDiA/rueca/pull/17))
- *(data)* structured config formats and the code↔config bridge ([#18](https://github.com/DevArKaDiA/rueca/pull/18))
- *(bench)* multi-language harness and PHP suite ([#13](https://github.com/DevArKaDiA/rueca/pull/13))
- *(php)* PHP language pack and resolution ([#14](https://github.com/DevArKaDiA/rueca/pull/14))
- *(markdown)* Markdown language pack (sections and links) ([#12](https://github.com/DevArKaDiA/rueca/pull/12))
- alpha 1 — local mode, Python + TS/JS, six MCP tools, benchmark vs cbm

### Fixed

- *(bench)* score SCIP-ambiguous definitions as equivalence classes; recursion, constructor and matching fixes ([#35](https://github.com/DevArKaDiA/rueca/pull/35))
- *(cpp)* parse without a preprocessor, cross-file receivers — toward RFC 0004 floors ([#25](https://github.com/DevArKaDiA/rueca/pull/25))
- *(resolve)* structure floors — constructors/fields, overrides by signature, receiver typing from assignments ([#23](https://github.com/DevArKaDiA/rueca/pull/23))

### Other

- *(core)* cognitive complexity ≤ 15 in resolve/index/segment/query; fix overload self-loop ([#39](https://github.com/DevArKaDiA/rueca/pull/39))
- *(rfc)* RFC 0006 — the organization graph ([#32](https://github.com/DevArKaDiA/rueca/pull/32))
- *(rfc)* RFC 0005 — responses for agents: org-wide, multi-ref, token-efficient ([#29](https://github.com/DevArKaDiA/rueca/pull/29))
- *(rfc)* RFC 0004 — quality bar and benchmark; high-level roadmap ([#22](https://github.com/DevArKaDiA/rueca/pull/22))
- alpha.2 quality — symbol ranking, trace noise, TS extraction fixes ([#15](https://github.com/DevArKaDiA/rueca/pull/15))
- *(rfc)* RFC 0003 — structured data and next languages ([#11](https://github.com/DevArKaDiA/rueca/pull/11))
- explain how the graph is built and how each tool uses it ([#8](https://github.com/DevArKaDiA/rueca/pull/8))
- add logo and brand assets ([#6](https://github.com/DevArKaDiA/rueca/pull/6))
- open-source project scaffolding
- rename project from Telar to Rueca
- workspace skeleton, data model, extraction contract, alpha tool contract

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
