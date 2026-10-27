# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0-alpha.2] - 2026-10-27

First public release (`0.1.0-alpha.1` was never published). Local mode,
organization mode on one machine, every language `stable` against the
quality floors of RFC 0004.

### Added

- `rueca` binary: `index`, `query`, `mcp`, `status`, `links`; MCP over stdio
  (protocol versions 2024-11-05 through 2026-07-28).
- Tools: `search_symbols`, `search_code`, `get_symbol`, `get_snippet`,
  `trace`, `get_file_outline`, `list_repositories`.
- Languages: Python, TypeScript / TSX / JavaScript, PHP, Go, Java, C, C++;
  Markdown sections and links; structured configuration (JSON/JSONC, YAML,
  TOML, `.env`, INI, `.properties`, XML kinds, HCL/Terraform, Dockerfile)
  with file kinds (compose, Kubernetes, GitHub Actions, OpenAPI, Terraform).
- Code ↔ config bridge: env var and config key reads linked to their
  providers (`$env:NAME` nodes, `READS` / `WRITES`).
- Cross-file resolution with per-edge strategy and confidence; receiver
  typing from declarations and assignments; overloads by argument type;
  `OVERRIDES`; C/C++ lightweight preprocessor and include resolution.
- Responses for agents (RFC 0005): pinned ids with 12-digit commits, snapshot
  envelope, `ref` / multi-repository `repo`, grouped compact output, token
  budget, compact text rendering.
- Organization graph (RFC 0006): `--org` with one symbol index over every
  repository, repository routing, cross-repository package edges, scoped and
  permission-filtered queries, incremental generations.
- Segment format v2 (body-aware symbol ranking, priors for declarations,
  amalgamations and vendored code), trigram content index, zstd content
  packs, content-addressed facts cache.
- Benchmark (`bench/`): localization, structure over every function with SCIP
  / PHPStan gold and micro-benchmarks, configuration, health, efficiency
  (tokens per correct answer), organization scenarios; scorecard with floors
  and targets (RFC 0004); weekly regression check.
- Quality: SonarQube gate (all Rust rules, cognitive complexity ≤ 15) required
  for merges and releases; clippy, cargo-deny, MSRV 1.90, DCO.
- Automated releases: release-plz (versions, changelog, crates.io, tag) and
  cargo-dist (binaries, installer, GitHub release).

[Unreleased]: https://github.com/DevArKaDiA/rueca/compare/v0.1.0-alpha.2...HEAD
[0.1.0-alpha.2]: https://github.com/DevArKaDiA/rueca/releases/tag/v0.1.0-alpha.2
