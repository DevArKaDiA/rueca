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

> **Status: alpha (`0.1.0-alpha.2`).** Local mode (one repository, or an
> organization of local repositories with `--org`); Python,
> TypeScript/JavaScript, PHP, Go, Java, C, C++ and Rust, plus Markdown and
> structured configuration. Formats and tool shapes may change before 1.0. See
> [Benchmark](#benchmark) for where it stands today — including where it loses.

## Features

- **One static binary**, no services: `rueca index` builds a segment,
  `rueca mcp` serves it on stdio.
- **Eight tools** for agents: `search_symbols`, `search_code`, `get_symbol`,
  `get_snippet`, `trace`, `get_file_outline`, `list_repositories`,
  `list_contracts` ([contract](docs/tools.md)).
- **Organizations**: several repositories indexed as one organization
  (`rueca-org.toml`) share one symbol index, so a question asked of all of
  them gets one ranking, weighed by which repositories it is about; a
  repository importing a package another one publishes is linked to it
  (imports, calls, inheritance across repositories, followed by `trace`),
  and so is a service calling the HTTP routes or RPC methods another one
  provides or specifies (`list_contracts`); re-indexing one repository
  replaces only its part.
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
tests), Go (`go.mod` / `go.work` import paths, packages across files,
methods with typed receivers, embedding, implicit interfaces as heuristic
`IMPLEMENTS`, generics, `go test` functions), Java (packages, Maven /
Gradle multi-module source roots, overloads, `OVERRIDES`, JUnit and TestNG
tests, Spring `@Value` and `System.getenv` reads), C and C++ (namespaces,
classes, out-of-line bodies linked to their declarations, overloads,
templates, macros, `#include` through `compile_commands.json` include
directories, GoogleTest and Catch2 tests), Rust (Cargo workspaces and crate
roots, `mod` files / `mod.rs` / inline modules, `use` with `crate::` /
`super::` / `self::`, globs and `pub use` re-export chains, `impl` and
trait items named `Type.method` / `Trait.method`, receivers from `self` /
`Self`, annotations, constructors, fields and return types, calls through
`dyn Trait` / `impl Trait` / generic bounds bound to the trait item,
`macro_rules!` as callables and calls recovered inside macro arguments,
`#[test]` / `#[cfg(test)]` / `tests/` tests, `env!` / `option_env!` /
`env::var` reads, Cargo dependencies for cross-repository package edges).
Documents: Markdown headings become `Section` symbols and file links become
`IMPORTS` edges, in the same graph as the code.
Configuration: JSON/JSONC, YAML, TOML, `.env`, INI, `.properties`, XML
(Maven, MSBuild, Android manifests), HCL/Terraform and Dockerfile become
keys addressed by key path (`k8s/web.yaml:Deployment/web.spec.replicas`),
with file kinds — docker compose, Kubernetes, GitHub Actions, OpenAPI,
JSON Schema, Terraform, Dockerfile, `package.json`, tsconfig,
`pyproject.toml`, Spring — and the **code ↔ config bridge**: `trace` on
`$env:DATABASE_URL` lists the code that reads it (`os.getenv`,
`process.env`, `getenv`, Laravel `env()`) and the manifests that set it;
`config('app.name')` reads its key in `config/app.php`.

## Install

Prebuilt binaries (macOS and Linux, x86_64 and arm64):

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/DevArKaDiA/rueca/releases/download/v0.1.0-alpha.2/rueca-installer.sh | sh
```

With Cargo (Rust 1.90+):

```sh
cargo install rueca@0.1.0-alpha.2 --locked
```

From source:

```sh
git clone https://github.com/DevArKaDiA/rueca && cd rueca
cargo build --release -p rueca      # binary at target/release/rueca
```

## Quick start

```sh
rueca index path/to/repo
rueca query search_symbols '{"query":"session request","limit":3}' --root path/to/repo --text
rueca query trace '{"qualified_name":"Session.request","direction":"callers"}' --root path/to/repo --text
rueca status
```

`--text` prints what an agent reads over MCP (without it, `query` prints the
JSON envelope; see [docs/tools.md](docs/tools.md)). On
[psf/requests](https://github.com/psf/requests):

```
github.com/psf/requests@611c6162cbc4 (main)
src/requests/sessions.py
  Method Session.request L557-653  def request(self, method: str, url: _t.UriType, params: …
  Method Session.prepare_request L511-555  def prepare_request(self, request: Request) -> PreparedRequest
  Function session L908-920  def session() -> Session
(truncated; continue with "cursor": "3")

github.com/psf/requests@611c6162cbc4 (main)
0 Method Session.request  src/requests/sessions.py L557-653
1 in d1 Function request  src/requests/api.py L24-71  →0 CALLS 0.5 heuristic @71
2 in d1 Method Session.delete  src/requests/sessions.py L742-750  →0 CALLS 0.9 type_resolved @750
…
```

Ids are `repo@commit:path#name` (here
`github.com/psf/requests@611c6162cbc4:src/requests/sessions.py#Session.request`),
and every tool takes `repo` (a list, or `"*"` for every indexed repository),
`ref` and a token `budget_tokens`.

Indexes live in `$RUECA_HOME` (default `~/.cache/rueca`). Re-run
`rueca index` after changes; only changed files are re-extracted.

Other refs are snapshots of their own, next to the working tree: `rueca
index . --ref pr/123` (also a branch, a tag or a commit; `--remove` drops
it) exports that commit's tree under the store and indexes it, reusing the
facts of every file it shares with what is already indexed. Then `ref:
"pr/123"` reads it, and `ref: ["main", "pr/123"]` compares: identical
symbols come back once, marked ` [0,1]`, and the ones that differ are
marked ` changed`.

### Several repositories: an organization

```toml
# rueca-org.toml
name = "acme"

[[repo]]
path = "~/src/billing"

[[repo]]
path = "~/src/payments"
refs = ["release/2.x", "pr/123"]             # other refs to index next to the default

[[repo]]
url = "https://github.com/acme/shared-lib"   # cloned under the store with git
```

```sh
rueca index --org rueca-org.toml                  # every repository, then the organization layer
rueca query search_symbols '{"query":"refund invoice"}' --org acme --text   # one ranking over all of them
rueca query list_repositories '{"query":"refund invoice"}' --org acme --text   # which repositories it is about
rueca query trace '{"qualified_name":"payments.charge","direction":"callers"}' --org acme --text   # callers in every repository
rueca links --org acme                            # the cross-repository edges
rueca contracts --org acme --side dangling        # HTTP / RPC calls no repository provides
rueca index ~/src/payments                        # re-index one: only its part of the organization changes
rueca index --org rueca-org.toml ~/src/billing --ref pr/7   # one more ref of a member: only that entry changes
rueca query search_symbols '{"query":"refund invoice","repo":"github.com/acme/billing","ref":"pr/7"}' --org acme --text   # the organization seen from that PR
rueca mcp --org rueca-org.toml                    # serve the organization (builds it first if needed)
```

In an organization, `repo` omitted means every repository and `repo: "…"`
narrows the same ranking; the query also ranks the repositories (names,
packages, README, central symbols, where its words occur) and weighs each
repository's results by that relevance. Repositories that import a package
another one publishes (npm, PyPI, Go modules, Composer, Maven / Gradle,
C / C++ headers, Cargo) are linked: imports, calls, inheritance and references
across repositories, resolved with the publishing repository's own module
model, which `trace` follows. So are a client's HTTP calls and gRPC stub
calls and the routes, OpenAPI operations or `.proto` RPC methods another
repository provides, matched by canonical key (`GET /users/{}`,
`pkg.Service/Method`); `rueca contracts` lists them and the calls nothing
provides. A ref of one repository (`repo: A, ref:
"pr/7"`) is the organization with A replaced by A at that ref; `ref`
alone reads every repository that has it (a release branch across
repositories) and the others at their default, and the cross-repository
edges into a ref follow the symbols it still has
([RFC 0006](docs/rfcs/0006-org-graph.md),
[how it works](docs/how-it-works.md#46-the-organization-layer-rueca-segment-org)).

## Use it from an agent

`rueca mcp --root <repo>` speaks MCP over stdio and indexes the root on first
start if needed. It serves protocol versions 2024-11-05 to 2025-11-25 through
`initialize` and 2026-07-28 without it
([protocol reference](docs/mcp-protocol.md)).

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

**Over HTTP**: a shared `rueca serve` (service mode, [RFC
0008](docs/rfcs/0008-service-mode.md); [details](docs/tools.md#over-http))
serves an organization to many clients at `https://<host>/mcp` with a
bearer token (a static `rueca_…` token or an OIDC access token):

```sh
claude mcp add --transport http rueca https://rueca.acme.internal/mcp --header "Authorization: Bearer $RUECA_TOKEN"
```

```json
{ "servers": { "rueca": { "type": "http", "url": "https://rueca.acme.internal/mcp",
  "headers": { "Authorization": "Bearer ${input:rueca-token}" } } } }
```

(VS Code, `.vscode/mcp.json`; Cursor: `{"mcpServers": {"rueca": {"url":
"https://rueca.acme.internal/mcp", "headers": {"Authorization": "Bearer
…"}}}}`.)

Recommended agent flow: `search_symbols` → `get_symbol` / `get_snippet` →
`trace`; `search_code` for literals and config.

## How it works

```
working tree ──walk──▶ files ──tree-sitter──▶ facts ──resolve──▶ graph ──▶ segment ──▶ MCP tools
              (ignore)        (per language,     (per blob,     (imports,    (CSR + tantivy
                               cached by blob)    cached)        scopes, MRO)  + trigrams + packs)
```

1. **Extract** — each code file is parsed with tree-sitter (Markdown and
   data files with pure-Rust parsers); a language pack emits definitions,
   references (including env var and config key reads), and imports. Local
   scopes come from adapted `locals.scm` queries. Facts are cached by git
   blob SHA.
2. **Resolve** — references are linked across files through imports
   (TS/JS module resolution by [`oxc_resolver`](https://github.com/oxc-project/oxc-resolver),
   including `tsconfig` paths and workspace packages; PHP names through
   `namespace` / `use` and Composer autoload; Go import paths through
   `go.mod`; Java packages and imports through Maven / Gradle source roots; C/C++
   `#include` through `compile_commands.json` include directories; Rust
   `mod` / `use` through the Cargo workspace model),
   re-export chains, `self`/`this` with base classes and traits,
   receiver types the file declares or constructs (every code language:
   `c: Context`, `const app = new Hono()`, `x = Session()`), declarations to
   bodies (C/C++), overloads by argument count and type, package and
   same-module scope (implicit `this` in Java), each with a confidence.
3. **Store** — the graph is written as one immutable segment file: CSR
   adjacency, an embedded [tantivy](https://github.com/quickwit-oss/tantivy)
   symbol index with code-aware tokenization, and a trigram index over
   zstd-compressed, content-addressed file packs.
4. **Serve** — the tools run against the memory-mapped segment (and, for an
   organization, its shared symbol index).

The full method — what is extracted, how references are resolved, the graph
schema, and exactly what each tool reads — is in
[docs/how-it-works.md](docs/how-it-works.md).

Design documents: [architecture (RFC 0001)](docs/rfcs/0001-architecture.md),
[building blocks (RFC 0002)](docs/rfcs/0002-building-blocks.md),
[structured data and the next languages (RFC 0003)](docs/rfcs/0003-structured-data-and-languages.md),
[quality bar and benchmark (RFC 0004)](docs/rfcs/0004-quality-and-benchmark.md),
[responses for agents: org-wide, multi-ref, token-efficient (RFC 0005)](docs/rfcs/0005-agent-responses.md),
[the organization graph (RFC 0006)](docs/rfcs/0006-org-graph.md),
[semantic search (RFC 0007)](docs/rfcs/0007-semantic-search.md),
[service mode (RFC 0008)](docs/rfcs/0008-service-mode.md),
[decisions log](docs/decisions.md).

## Benchmark

Every language is measured against the absolute floors of
[RFC 0004](docs/rfcs/0004-quality-and-benchmark.md) — issue localization
(SWE-bench family, ≥ 30 issues per language), callers / callees over every
function of two or more SCIP / PHPStan gold repositories per language, own
hand-labelled micro-benchmarks, parse-error rate (vendored, copied and
misdetected files left out; the raw rate is published next to it), latency,
and configuration key recall and env-var bridge — with 95% intervals;
[codebase-memory-mcp](https://github.com/DeusData/codebase-memory-mcp)
v0.12.0 and a BM25 baseline are published alongside as reference. Current
scorecard (`stable` = every floor met; full tables in
[docs/bench/alpha-results.md](docs/bench/alpha-results.md), method in
[bench/README.md](bench/README.md)):

- **Python** `stable`
- **TS/JS** `stable`
- **PHP** `stable`
- **Go** `stable`
- **Java** `stable`
- **C** `stable` (1.1% of files with parse errors, vendored code excluded)
- **C++** `stable` (callees precision 92.5 with SCIP-ambiguous definitions scored as one class; 0.5% of files with parse errors, vendored code excluded)
- **Rust** `stable` (localization meets the floor but trails BM25 on the point estimate: File Acc@5 62.8 vs 67.4, Function Acc@10 39.0 vs 46.3 over 43 issues, paired intervals cross zero; callers / callees precision 98.5 / 98.0, recall 86.8 / 87.1 against rust-analyzer on serde_json and ripgrep; 0.1% of files with parse errors)
- **Configuration** (JSON, YAML, HCL keys; env-var bridge) `stable`

**Tokens per correct answer** ([RFC 0005 §8](docs/rfcs/0005-agent-responses.md),
measured on what an agent reads, same queries as codebase-memory-mcp;
target ≤ 50% of cbm): with the RFC 0005 responses, `search_symbols` costs
0.33–0.89× cbm's tokens until the first gold hit (target met for Python,
TS/JS, PHP and Java) and `trace` 0.88–1.74× cbm's tokens per true call
edge (target not met; Rueca also returns more true edges). In an
organization of 42 pinned repositories (120 gated issues), one ranking
over the organization index weighed by repository routing keeps the right
repository's file in the top 5 for 56.7% of the issues vs 66.7% when
asking that repository alone (0.85×, at the floor with its interval
crossing it; per-repository rank fusion gives 24.2%; BM25 over the union
of the repositories 52.5%); it does not yet surface the consumers of a
changed library symbol (12.5% vs 37.5% for BM25). Cross-repository edges
on 12 labelled consumer–library pairs have 99.4% precision and 94.7%
recall (100.0 / 96.2 without the Rust pairs, 98.3 / 93.1 Rust alone);
re-indexing one repository of 42 updates the organization layer in 0.5 s,
0.19× the repository's own index time
([RFC 0006 §8, §9.2](docs/rfcs/0006-org-graph.md)).

Contributions that move these numbers are very welcome.

## Roadmap

Where Rueca is going, in order. Each stage has a design document; the
quality bar every language must meet is defined in
[RFC 0004](docs/rfcs/0004-quality-and-benchmark.md) (deterministic metrics,
absolute floors and aspirational targets).

| Stage | What it delivers | Status |
|---|---|---|
| **Local mode** | One binary, MCP over stdio, the tools; segments, content packs, facts cache ([RFC 0001](docs/rfcs/0001-architecture.md)) | ✅ done |
| **Languages and data** | Python, TS/JS, PHP, Go, Java, C, C++, Rust, Markdown, structured config formats, the code ↔ config bridge ([RFC 0003](docs/rfcs/0003-structured-data-and-languages.md)) | ✅ done, every language `stable` |
| **Quality bar** | Benchmark rebuilt for trustworthy numbers; every language `stable` against the floors of [RFC 0004](docs/rfcs/0004-quality-and-benchmark.md) | ✅ done, Rust included |
| **Agent responses** | Pinned ids, snapshot envelope, `ref` and multi-repo queries, collapse across refs, compact output, tokens per correct answer ([RFC 0005](docs/rfcs/0005-agent-responses.md)) | ✅ landed (pinned ids, envelope, multi-repo, compact output, token metric; other refs with `--ref` and collapse across refs, in local mode and over an organization) |
| **First public release** | `0.1.0` on crates.io and GitHub Releases | ✅ `0.1.0-alpha.2` published; `0.1.0` follows |
| **Retrieval** | Semantic search inside `search_symbols` ([RFC 0007](docs/rfcs/0007-semantic-search.md)): embedding presets (Qwen3-Embedding 0.6B / 4B / 8B in several quantizations, CodeRankEmbed, remote APIs opt-in; default preset `qwen3-0.6b-q8`, which phase 0 may change by a stated rule), weights downloaded on first use; vectors keyed by content hash, cached in the store and reused across branches, worktrees and repositories; two stages — BM25 + vectors + graph neighbours fused by a convex combination, then an optional reranker, off by default locally and on in service mode only if it passes its gate — measured on CoIR, the S1 localization suite (`raw` and `kw`) and the organization suite (localization and impact) | 🗺️ next (design) |
| **Precision layer** | Optional compiler-grade edges imported from SCIP indexes | 🗺️ planned |
| **Service mode** | One container, segments in object storage, MCP over HTTP, organization-wide indexing with GPU for embeddings ([RFC 0008](docs/rfcs/0008-service-mode.md)): one image with `query` / `indexer` roles, per-repository catalogs and packs written by compare-and-swap (no lock, no database), stateless Streamable HTTP with OIDC or static tokens and deny-by-default visibility, webhooks as hints over adaptive polling, child-process isolation per ref, retention for branches and PRs | 🚧 phase 0 landed (`rueca-storage`: memory / file / S3-compatible stores with compare-and-swap; `rueca-service`: publish, pull, generations, garbage collection; `rueca status --store-url`); next: `rueca serve` query role over HTTP |
| **Organization graph** | Per-repository graphs stay the unit; one organization graph on top with a single symbol index, cross-repository edges (packages, then contracts) and scoped queries; local `--org` first ([RFC 0006](docs/rfcs/0006-org-graph.md)) | ✅ phases 1–4 landed (organization symbol index, scoped queries, local `--org`, repository routing, cross-repository package edges, Cargo included; refs / branches / PRs over the organization with `--ref` and `refs = [...]`, collapse across refs; contracts: HTTP / OpenAPI / protobuf-gRPC edges across repositories, `list_contracts`) |
| **Org mode** | Every repository of a GitHub / GitLab / Bitbucket org, mirrored permissions, cross-service edges | 🗺️ planned |
| **Agent benchmark** | End-to-end agent runs with and without Rueca (answer quality, tokens, tool calls) | 🗺️ when budget allows |

More languages (Kotlin, Swift, Ruby, C#, …) follow the same gate.

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
| [`rueca-lang-data`](crates/rueca-lang-data) | structured data: keys, file kinds, data pointers, env var providers |
| [`rueca-lang-go`](crates/rueca-lang-go) | Go extraction, receivers and the `go.mod` module model |
| [`rueca-lang-java`](crates/rueca-lang-java) | Java extraction, packages and Maven / Gradle source roots |
| [`rueca-lang-cpp`](crates/rueca-lang-cpp) | C and C++ extraction, macro masking and include resolution |
| [`rueca-lang-rust`](crates/rueca-lang-rust) | Rust extraction, receivers, macro arguments and the Cargo module model |
| [`rueca-lang-proto`](crates/rueca-lang-proto) | Protocol Buffers services, RPC methods, messages and enums; RPC contracts |
| [`rueca-resolve`](crates/rueca-resolve) | cross-file resolution with confidence |
| [`rueca-segment`](crates/rueca-segment) | segment format, search, content packs, catalog |
| [`rueca-index`](crates/rueca-index) | indexing pipeline |
| [`rueca-query`](crates/rueca-query) | the tools, organization scope |
| [`rueca-mcp`](crates/rueca-mcp) | MCP server: its own protocol layer ([reference](docs/mcp-protocol.md)), stdio |
| [`rueca-storage`](crates/rueca-storage) | object storage behind a `Storage` trait: `memory://`, `file://`, `s3://`, `gs://`, conditional writes |
| [`rueca-service`](crates/rueca-service) | service store: publish, pull, organization generations, garbage collection |
| [`rueca-serve`](crates/rueca-serve) | `rueca serve`: MCP over HTTP, authentication, visibility, rate limits, health and metrics |

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
and queries adapted from
[nvim-treesitter](https://github.com/nvim-treesitter/nvim-treesitter) and
[bloop](https://github.com/BloopAI/bloop).
