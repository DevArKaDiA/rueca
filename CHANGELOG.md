# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0-alpha.3](https://github.com/DevArKaDiA/rueca/compare/v0.1.0-alpha.2...v0.1.0-alpha.3) - 2026-11-26

### Added

- *(sql)* Oracle and DB2 dialects, strict parse errors, a mined localization set; preview: held-out parse errors 25.0% ([#87](https://github.com/DevArKaDiA/rueca/pull/87))
- *(shell)* dialects, zsh grammar and a mined localization set; stable under RFC 0004 ([#86](https://github.com/DevArKaDiA/rueca/pull/86))
- *(swift)* Swift language pack, stable under RFC 0004 floors ([#85](https://github.com/DevArKaDiA/rueca/pull/85))
- *(sql)* SQL language pack, preview under RFC 0004 floors ([#82](https://github.com/DevArKaDiA/rueca/pull/82))
- *(ruby)* Ruby language pack, stable under RFC 0004 floors ([#84](https://github.com/DevArKaDiA/rueca/pull/84))
- *(kotlin)* Kotlin language pack, stable under RFC 0004 floors ([#83](https://github.com/DevArKaDiA/rueca/pull/83))
- *(csharp)* C# language pack, stable under RFC 0004 floors ([#81](https://github.com/DevArKaDiA/rueca/pull/81))
- *(shell)* shell language pack, preview under RFC 0004 floors ([#80](https://github.com/DevArKaDiA/rueca/pull/80))
- *(mcp)* experimental code mode, JavaScript and Starlark (RFC 0009 phases 0–3) ([#77](https://github.com/DevArKaDiA/rueca/pull/77))
- *(query)* hybrid semantic search in search_symbols, opt-in (RFC 0007 phase 1) ([#74](https://github.com/DevArKaDiA/rueca/pull/74))
- *(embed)* embedding runtime, weights cache and the bench vector arm (RFC 0007 phase 0) ([#73](https://github.com/DevArKaDiA/rueca/pull/73))
- *(service)* service store, query role and indexer role (RFC 0008 phases 0–2) ([#66](https://github.com/DevArKaDiA/rueca/pull/66))
- *(org)* cross-service contract edges (RFC 0006 phase 4) ([#63](https://github.com/DevArKaDiA/rueca/pull/63))
- *(bench)* organization scenario with ≥ 60 instances ([#60](https://github.com/DevArKaDiA/rueca/pull/60))
- *(org)* refs over the organization (RFC 0006 phase 3) ([#61](https://github.com/DevArKaDiA/rueca/pull/61))
- *(rust)* Rust language pack, stable under RFC 0004 floors ([#56](https://github.com/DevArKaDiA/rueca/pull/56))

### Fixed

- *(rust)* macro-wrapped items, value/type namespaces, crate-scoped types, attribute macros ([#68](https://github.com/DevArKaDiA/rueca/pull/68))

### Other

- *(rfc)* RFC 0009 — code mode (experimental) ([#76](https://github.com/DevArKaDiA/rueca/pull/76))
- *(demo)* organization demo on tokio-rs ([#67](https://github.com/DevArKaDiA/rueca/pull/67))
- *(rfc)* RFC 0007 — semantic search ([#59](https://github.com/DevArKaDiA/rueca/pull/59))
- *(rfc)* RFC 0008 — service mode ([#62](https://github.com/DevArKaDiA/rueca/pull/62))
- *(mcp)* own MCP protocol layer, drop rmcp and chrono (RUSTSEC-2026-0543) ([#64](https://github.com/DevArKaDiA/rueca/pull/64))

## [0.1.0-alpha.2](https://github.com/DevArKaDiA/rueca/releases/tag/v0.1.0-alpha.2) - 2026-10-27

First public release (`0.1.0-alpha.1` was never published). Every language is
`stable` against the quality floors of RFC 0004; merges and releases are gated
by SonarQube.

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

[Unreleased]: https://github.com/DevArKaDiA/rueca/compare/v0.1.0-alpha.2...HEAD
