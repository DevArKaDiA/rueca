<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/DevArKaDiA/rueca/main/assets/logo/rueca-on-dark.svg">
    <img src="https://raw.githubusercontent.com/DevArKaDiA/rueca/main/assets/logo/rueca.svg" width="160" alt="Rueca logo: an R made of a spinning wheel and a bobbin of code braces">
  </picture>
</p>

# Rueca

[![CI](https://github.com/DevArKaDiA/rueca/actions/workflows/ci.yml/badge.svg)](https://github.com/DevArKaDiA/rueca/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![MSRV](https://img.shields.io/badge/rustc-1.90+-orange.svg)](Cargo.toml)
[![crates.io](https://img.shields.io/crates/v/rueca.svg)](https://crates.io/crates/rueca)

**Code intelligence for coding agents.** Rueca indexes a repository into a
symbol and call graph and serves it to agents over [MCP](https://modelcontextprotocol.io):
find a symbol, read it, and follow who calls it — without grepping the whole
tree into the context window.

*Rueca* is Spanish for spinning wheel: it spins raw source into the thread an
agent can follow.

> **Status: alpha (`0.1.0-alpha.1`).** Local mode only, Python,
> TypeScript/JavaScript and PHP. Formats and tool shapes may change before 1.0. See
> [Benchmark](#benchmark) for where it stands today — including where it loses.

## Features

- **One static binary**, no services: `rueca index` builds a segment,
  `rueca mcp` serves it on stdio.
- **Six tools** for agents: `search_symbols`, `search_code`, `get_symbol`,
  `get_snippet`, `trace`, `get_file_outline` ([contract](docs/tools.md)).
- **Call graph with provenance**: every edge carries the strategy that resolved
  it (scope, `self`/MRO, import, re-export, same module, unique name,
  heuristic) and a confidence, so agents can filter guesses out.
- **Fast**: a medium repository indexes in well under a second; queries take
  milliseconds. Unchanged files are served from a content-addressed facts
  cache on re-index.
- **Content search** over a trigram index with regex support, scoped by glob
  and language.
- **Stable symbol IDs** (`repo@commit:path#Class.method`) that other tools can
  cross-reference.

Languages: Python, TypeScript, TSX, JavaScript (incl. JSX, CommonJS, ESM),
PHP (incl. HTML templates, Composer PSR-4 / PSR-0 autoload, PHPUnit and Pest
tests).
Documents: Markdown headings become `Section` symbols and file links become
`IMPORTS` edges, in the same graph as the code.

## Install

Prebuilt binaries (macOS and Linux, x86_64 and arm64):

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/DevArKaDiA/rueca/releases/download/v0.1.0-alpha.1/rueca-installer.sh | sh
```

With Cargo (Rust 1.90+):

```sh
cargo install rueca@0.1.0-alpha.1 --locked
```

From source:

```sh
git clone https://github.com/DevArKaDiA/rueca && cd rueca
cargo build --release -p rueca      # binary at target/release/rueca
```

## Quick start

```sh
rueca index path/to/repo
rueca query search_symbols '{"query":"session request"}' --root path/to/repo --pretty
rueca query trace '{"qualified_name":"Session.request","direction":"callers","depth":2}' --root path/to/repo
rueca status
```

Indexes live in `$RUECA_HOME` (default `~/.cache/rueca`). Re-run
`rueca index` after changes; only changed files are re-extracted.

## Use it from an agent

`rueca mcp --root <repo>` speaks MCP over stdio and indexes the root on first
start if needed.

**Claude Code**

```sh
claude mcp add rueca -- rueca mcp --root "$PWD"
```

**VS Code** (`.vscode/mcp.json`)

```json
{
  "servers": {
    "rueca": { "command": "rueca", "args": ["mcp", "--root", "${workspaceFolder}"] }
  }
}
```

**Cursor and other clients** (`mcpServers` format)

```json
{
  "mcpServers": {
    "rueca": { "command": "rueca", "args": ["mcp", "--root", "/absolute/path/to/repo"] }
  }
}
```

Recommended agent flow: `search_symbols` → `get_symbol` / `get_snippet` →
`trace`; `search_code` for literals and config.

## How it works

```
working tree ──walk──▶ files ──tree-sitter──▶ facts ──resolve──▶ graph ──▶ segment ──▶ MCP tools
              (ignore)        (per language,     (per blob,     (imports,    (CSR + tantivy
                               cached by blob)    cached)        scopes, MRO)  + trigrams + packs)
```

1. **Extract** — each file is parsed with tree-sitter; a language pack emits
   definitions, references, and imports. Local scopes come from adapted
   `locals.scm` queries. Facts are cached by git blob SHA.
2. **Resolve** — references are linked across files through imports
   (TS/JS module resolution by [`oxc_resolver`](https://github.com/oxc-project/oxc-resolver),
   including `tsconfig` paths and workspace packages; PHP names through
   `namespace` / `use` and Composer autoload), re-export chains,
   `self`/`this` with base classes and traits, declared receiver types
   (PHP), and same-module scope, each with a confidence.
3. **Store** — the graph is written as one immutable segment file: CSR
   adjacency, an embedded [tantivy](https://github.com/quickwit-oss/tantivy)
   symbol index with code-aware tokenization, and a trigram index over
   zstd-compressed, content-addressed file packs.
4. **Serve** — the six tools run against the memory-mapped segment.

The full method — what is extracted, how references are resolved, the graph
schema, and exactly what each tool reads — is in
[docs/how-it-works.md](docs/how-it-works.md).

Design documents: [architecture (RFC 0001)](docs/rfcs/0001-architecture.md),
[building blocks (RFC 0002)](docs/rfcs/0002-building-blocks.md),
[structured data and the next languages (RFC 0003)](docs/rfcs/0003-structured-data-and-languages.md),
[decisions log](docs/decisions.md).

## Benchmark

Rueca is benchmarked against [codebase-memory-mcp](https://github.com/DeusData/codebase-memory-mcp)
v0.12.0 and a BM25 baseline, on issue localization (SWE-bench Lite/Verified,
SWE-PolyBench, 81 instances) and structural questions (callers/callees against
SCIP-derived gold and PyCG). Full results:
[docs/bench/alpha-results.md](docs/bench/alpha-results.md); method:
[bench/README.md](bench/README.md).

| Overall, % | BM25 | cbm 0.12.0 | Rueca |
|---|---:|---:|---:|
| Localization (keywords): file Acc@5 | 60.5 | 56.8 | **69.1** |
| Localization (keywords): function Acc@10 | 47.9 | 35.6 | **54.8** |
| Localization (issue text): file Acc@5 | 67.9 | 65.4 | **74.1** |
| Localization (issue text): function Acc@10 | **52.1** | 39.7 | **52.1** |
| Callers F1 | 82.8 | 82.2 | **86.4** |
| Callees F1 | 62.7 | 69.9 | **74.8** |
| Definition found | 100 | 97.1 | 98.3 |
| Median query latency | ~0 s | 0.43 s | **0.01 s** |

Rueca ranks definitions by their code as well as their names, leads on
TS/JS call edges and on latency, and still trails cbm on Python structural
questions (callers/callees F1, bare-name lookup) and BM25 on file Acc@1.
Contributions that move these numbers are very welcome.

## Roadmap

- Receiver typing from assignments; structural tie-breaks for same-name
  definitions; Python call-edge recall.
- More languages (Go, Java, Kotlin, Swift, Rust, …).
- Optional compiler-grade precision from SCIP indexes.
- Service mode (object storage, MCP over HTTP) and an org-wide mode over
  GitHub / GitLab / Bitbucket with mirrored permissions.

See [RFC 0001](docs/rfcs/0001-architecture.md) for the full plan.

## Workspace

| Crate | Role |
|---|---|
| [`rueca`](crates/rueca-cli) | the `rueca` binary (CLI + MCP) |
| [`rueca-model`](crates/rueca-model) | data model, symbol IDs |
| [`rueca-extract`](crates/rueca-extract) | language-pack contract, budgeted parsing |
| [`rueca-lang-python`](crates/rueca-lang-python) | Python extraction and module resolution |
| [`rueca-lang-typescript`](crates/rueca-lang-typescript) | TS/TSX/JS extraction and module resolution |
| [`rueca-lang-markdown`](crates/rueca-lang-markdown) | Markdown sections and file links |
| [`rueca-lang-php`](crates/rueca-lang-php) | PHP extraction, name resolution and Composer autoload |
| [`rueca-resolve`](crates/rueca-resolve) | cross-file resolution with confidence |
| [`rueca-segment`](crates/rueca-segment) | segment format, search, content packs, catalog |
| [`rueca-index`](crates/rueca-index) | indexing pipeline |
| [`rueca-query`](crates/rueca-query) | the six tools |
| [`rueca-mcp`](crates/rueca-mcp) | MCP stdio server |

## Contributing

Issues and pull requests are welcome — see [CONTRIBUTING.md](CONTRIBUTING.md).
Commits must be signed off ([DCO](https://developercertificate.org/)). This
project follows the [Contributor Covenant](CODE_OF_CONDUCT.md). Report
vulnerabilities privately as described in [SECURITY.md](SECURITY.md).

## License

Licensed under the [Apache License, Version 2.0](LICENSE). Vendored
tree-sitter queries keep their original licenses; see [NOTICE](NOTICE).

Built on [tree-sitter](https://tree-sitter.github.io), [tantivy](https://github.com/quickwit-oss/tantivy),
[oxc_resolver](https://github.com/oxc-project/oxc-resolver), [gix](https://github.com/GitoxideLabs/gitoxide),
[rmcp](https://github.com/modelcontextprotocol/rust-sdk), and queries adapted from
[nvim-treesitter](https://github.com/nvim-treesitter/nvim-treesitter) and
[bloop](https://github.com/BloopAI/bloop).
