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
> TypeScript/JavaScript, PHP, Go, Java, C, C++, Rust, shell scripts,
> C#, Kotlin, Ruby, SQL and Swift, plus Markdown and structured configuration. Formats and tool shapes may change before 1.0. See
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
`env::var` reads, Cargo dependencies for cross-repository package edges),
shell (POSIX sh, bash, zsh, ksh and bats, one dialect per
file, zsh read with its own grammar; extension-less
scripts by their `#!` line; functions in both forms, `source` / `.` paths
through `$(dirname "$0")` / `${BASH_SOURCE%/*}` / directory variables,
sourced functions resolved transitively, scripts run by path as calls of
the script's file, `trap` handlers, bats and shunit2 tests, `$VAR` reads
and `export` providers in the env bridge), C#
(namespaces as packages across files, `using` / `using static` / aliases /
`global using`, overloads chosen by C# conversions, partial types,
extension methods, receivers typed from declarations and from declared
return types, xUnit / NUnit / MSTest tests, `Environment` and
`IConfiguration` reads into `appsettings.json`, NuGet projects for
cross-repository package edges),
Kotlin (packages bound by name wherever their files are and shared with
Java in both directions, `import` with aliases and on demand, classes,
objects and companion objects, primary / secondary / implicit constructors
called without `new`, extension functions, overloads chosen by arity,
defaults, `vararg` and argument types, receivers typed from declarations,
constructions, smart casts, scope-function lambdas and declared return
types, JUnit / kotlin.test / Kotest tests, `System.getenv` and Spring
`@Value` reads, Gradle `.kts` scripts),
Ruby (constants by path with Ruby's lookup through the lexical nesting and
the ancestors, reopened classes as one constant, `include` / `extend` /
`prepend` mixins, `module_function`, `class << self`, `super`, `Foo.new` →
`initialize`, calls without parentheses told from local variables,
receivers typed from `Foo.new`, Sorbet `sig`s and RBS comments, `require` /
`require_relative`, RSpec and Minitest tests, `ENV` reads, gemspec / Gemfile
packages for cross-repository edges),
SQL (PostgreSQL, MySQL / MariaDB, SQLite, T-SQL, Oracle with
PL/SQL, DB2, BigQuery, Snowflake, one dialect per file from a directive,
the path or the content; tables, views and their columns, functions,
procedures, triggers, packages, indexes, types; procedural code read
statement by statement; relations read and written, columns when the
table is certain,
routine calls and foreign keys as `REFERENCES` / `WRITES` / `CALLS`;
migrations resolved to the definition effective at each point; dbt models,
`ref()` / `source()` and macros; `-- name:` queries; per-dialect coverage in
[`rueca-lang-sql`](crates/rueca-lang-sql/README.md)),
Swift (SwiftPM targets as modules, from `Package.swift` and the
`Sources/<Target>` layout, every file of a target visible to the others,
`import` binding a whole module; classes, structs, enums, actors,
protocols and extensions whose members attach to the extended type in any
file or module; callables named by their selector, `init(x:y:)`, with
initializers called without `new` and overloads chosen by argument labels,
defaults, variadics and trailing closures; receivers typed from
declarations, constructions, casts, `if let` / `guard let`, array elements,
generic constraints, contextual `.member` calls and declared return types;
XCTest and Swift Testing tests; `ProcessInfo` / `getenv` reads; SwiftPM
dependencies for cross-repository package edges).
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

With Cargo (Rust 1.90+; the default `embeddings` feature builds llama.cpp
from source and needs CMake, a C/C++ compiler and libclang —
`--no-default-features --features service` builds without it, lexical only):

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

### Demo: the tokio-rs organization

[docs/demo/tokio-rs.md](docs/demo/tokio-rs.md) runs this on a public family
of repositories: tokio, mio, bytes, tracing, hyper, tower, axum and tonic,
with two refs of tokio, Cargo package edges between them and gRPC contracts
in tonic. Every output, timing and known failure on that page comes from one
script:

```sh
RUECA=$(command -v rueca) scripts/demo-tokio-rs.sh
```

### Semantic search (opt-in)

`search_symbols` can fuse BM25 with embedding similarity
([RFC 0007](docs/rfcs/0007-semantic-search.md)), so a question in plain
words — `where is the configuration file parsed` — finds code that does not
use those words. It is **off unless you turn it on**; without it nothing is
downloaded or embedded and every answer is lexical, as before. One line
enables it:

```sh
rueca models pull                 # once: the default model, Qwen3-Embedding-0.6B Q8_0 (639 MB), sha256-verified
rueca index --embed               # or: RUECA_EMBED=1 rueca index
```

Precedence is argument > environment > `[retrieval]` in
`<store>/rueca.toml`: `--no-embed` overrides `RUECA_EMBED=1`; `RUECA_EMBED`
= `0`, empty, `false` or `off` leaves it off; `RUECA_EMBED=<preset>` or
`--embed=<preset>` picks another preset; the file chooses the preset and
fusion parameters but never turns embedding on by itself. Missing weights
are an error naming `rueca models pull` (indexing never downloads).
Queries need nothing: a snapshot indexed with vectors is searched in
`hybrid` mode automatically, falls back to lexical (and says so in the
answer's `retrieval`) when its model cannot load, and `mode` (`lexical`,
`hybrid`, `semantic`) picks one explicitly. Vectors are cached in the store
by content, so a re-index, another branch or another repository embeds only
new text.

**What it costs.** Indexing Rueca itself (9,319 symbols) took 400 s on an M1
Pro with Metal, about 23 symbols/s, once; a CLI query takes about 0.25 s
including the model load (the MCP server loads it once).

**When it helps.** On the S1 calibration split (118 instances, File Acc@5 /
Function Acc@10), natural-language questions (the issue text) gain: hybrid
83.1 / 70.1 against BM25 78.8 / 57.9 (Function +12.1, 95% interval +2.8 …
+21.5). Keyword queries do not: hybrid 73.7 / 56.1 against BM25 72.9 / 57.9.
Details in [how it works, §10](docs/how-it-works.md#10-embeddings-rfc-0007)
and [the decisions log](docs/decisions.md).

`rueca models list | pull | path | rm` manage the weights, and `rueca embed`
embeds JSON lines in batch (the bench's vector arm).

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

**Code mode (experimental)**: `rueca mcp --experimental-code-mode` (or `RUECA_EXPERIMENTAL_CODE_MODE=1`) adds `execute` and `describe_api`, which run a short JavaScript script (Starlark only in a build with `--features code-mode-starlark`, off by default) against the tools and return only its answer ([details](docs/tools.md#experimental-code-mode)).

(VS Code, `.vscode/mcp.json`; Cursor: `{"mcpServers": {"rueca": {"url":
"https://rueca.acme.internal/mcp", "headers": {"Authorization": "Bearer
…"}}}}`.)

Running the service: `rueca serve --role all` indexes the configured
repositories from their URLs into an object store, keeps them current
(adaptive polling, plus signed webhooks pointed at
`https://<host>/webhooks/github`, `/gitlab` or `/bitbucket`) and serves
them; `--role query` and `--role indexer` split the two for scale
([configuration](docs/rfcs/0008-service-mode.md#44-configuration)):

```toml
# rueca-service.toml
[store]
url = "s3://acme-rueca/prod"          # or file:///var/lib/rueca/store
cache_dir = "/var/cache/rueca"

[org]
name = "acme"

[[repo]]
url = "https://github.com/acme/billing.git"
branches = ["release/*"]
tags = "v*"
prs = true

[auth]
admins = ["token:ops"]

[[auth.token]]
name = "ops"
hash = "sha256:…"                     # sha256 of the rueca_… token
repos = ["github.com/acme/*"]

[indexing]
credentials = { "github.com" = "env:GITHUB_TOKEN" }
webhooks = { github = "env:RUECA_GITHUB_WEBHOOK_SECRET" }
```

```sh
rueca serve --config rueca-service.toml --check-config
rueca serve --config rueca-service.toml --role all
rueca status --store-url s3://acme-rueca/prod      # repositories, refs, commits, closed pull requests
```

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
   `mod` / `use` through the Cargo workspace model; shell `source` paths; C# `using`s through
   namespaces, wherever their files are; Kotlin imports through
   packages, wherever their files are, shared with Java; Ruby constants through
   the lexical nesting and the ancestors, `require` through `lib/` load paths;
   SQL names over a global namespace, by migration order and nearest directory;
   Swift imports through SwiftPM targets),
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
[code mode, experimental (RFC 0009)](docs/rfcs/0009-code-mode.md),
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
- **Shell** `stable` (localization meets the floor but trails BM25 on the point estimate, the function interval ending at zero: File Acc@5 72.2 vs 77.8, Function Acc@10 64.2 vs 73.6 over 72 issues mined from GitHub by the bench — no dataset has shell instances — on nine zsh, bash and POSIX sh repositories; structure measured against the bench's code-backed labeler, not a compiler-grade oracle: callers / callees precision 100.0 / 100.0, recall 99.9 / 99.9 on nvm, acme.sh and bash-it, micro callee F1 99.1; 0.8% of files with parse errors, zsh read with its own grammar: sh 0.8%, bash 0.8%, zsh 1.1%, bats 0%; ksh read as bash, not measured)
- **C#** `stable` (localization meets the floor at BM25's level: File Acc@5 48.7 vs 48.7, Function Acc@10 26.3 vs 28.9 over 39 SWE-rebench V2 issues, paired intervals cross zero; callers / callees precision 96.9 / 95.5, recall 95.2 / 96.6 against scip-dotnet on serilog and eShopOnWeb; 0.0% of files with parse errors)
- **Kotlin** `stable` (callees precision at the floor: 90.0, interval 87.9–92.0; callers precision 96.4, recall 86.6 / 85.5 against scip-java's Kotlin indexer on moshi and clikt; localization not significantly below BM25: File Acc@5 56.5 vs 58.7, Function Acc@10 45.5 vs 31.8 over 46 Multi-SWE-bench issues; 0.15% of files with parse errors)
- **Ruby** `stable` (callers / callees precision 98.4 / 95.4, recall 96.8 / 96.7 against scip-ruby on jekyll and ruby-lsp; localization not significantly below BM25: File Acc@5 55.0 vs 55.0, Function Acc@10 24.2 vs 18.2 over 40 SWE-bench Multilingual issues; 0.2% of files with parse errors)
- **SQL** `preview` (held-out parse errors 25.0%: 1,624 of 6,496 files of 25 repositories the pack had not seen keep a strict parse error, floor ≤ 2% — T-SQL 41.9%, Oracle 40.7%, Snowflake 44.4%, MySQL 30.5%, BigQuery 19.1%, PostgreSQL 9.5%, SQLite 5.0%, dbt Jinja 3.1%; pooled with the corpus the parser was repaired on, 16.6%. The other floors are met: localization not significantly below BM25: File Acc@5 76.9 vs 78.2, Function Acc@10 72.2 vs 75.9 over 78 issues mined from GitHub by the bench — no dataset has SQL instances — on seven dbt packages and T-SQL, PL/pgSQL and BigQuery repositories; where the issue does not name the file Rueca trails clearly, File Acc@5 56.0 vs 72.0 over 25; structure measured against the bench's code-backed labeler on sqlglot, not a compiler-grade oracle: callers / callees precision 99.5 / 98.8, recall 96.1 / 95.6 on Sakila in six dialects, a dbt package, pgmq, tSQLt and mimic-code, micro callee F1 100.0; parse errors are counted strictly — a statement accepted by its first words alone is an error; on the corpus the parser was repaired on the rate is 0.8%)
- **Swift** `stable` (every structural floor met at the point estimate, two by a narrow margin: callees precision 90.8, interval 89.1–92.4, callees recall 75.5, interval 72.7–78.3; callers precision / recall 93.1 / 78.7 against the Swift compiler's index store on Alamofire and swift-algorithms; localization not significantly below BM25 but trailing it: File Acc@5 70.9 vs 80.0, Function Acc@10 51.1 vs 57.4 over 55 SWE-rebench V2 issues; 1.8% of files with parse errors)
- **Configuration** (JSON, YAML, HCL keys; env-var bridge) `stable`

**Tokens per correct answer** ([RFC 0005 §8](docs/rfcs/0005-agent-responses.md),
measured on what an agent reads, same queries as codebase-memory-mcp;
target ≤ 50% of cbm): with the RFC 0005 responses, `search_symbols` costs
0.33–0.89× cbm's tokens until the first gold hit (target met for Python,
TS/JS, PHP and Java) and `trace` 0.48–1.74× cbm's tokens per true call
edge (target met for Rust only; Rueca also returns more true edges). In an
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
| **Languages and data** | Python, TS/JS, PHP, Go, Java, C, C++, Rust, C#, Kotlin, Ruby, Swift, Markdown, structured config formats, the code ↔ config bridge ([RFC 0003](docs/rfcs/0003-structured-data-and-languages.md)); shell scripts (§10.4c), SQL (§10.4g) | ✅ done; every language `stable` except SQL, `preview` (held-out parse errors 25.0%); shell and SQL on localization sets mined by the bench and code-backed structural gold |
| **Quality bar** | Benchmark rebuilt for trustworthy numbers; every language but SQL `stable` against the floors of [RFC 0004](docs/rfcs/0004-quality-and-benchmark.md) | ✅ done, Rust included; SQL is `preview` |
| **Agent responses** | Pinned ids, snapshot envelope, `ref` and multi-repo queries, collapse across refs, compact output, tokens per correct answer ([RFC 0005](docs/rfcs/0005-agent-responses.md)) | ✅ landed (pinned ids, envelope, multi-repo, compact output, token metric; other refs with `--ref` and collapse across refs, in local mode and over an organization) |
| **First public release** | `0.1.0` on crates.io and GitHub Releases | ✅ `0.1.0-alpha.2` published; `0.1.0` follows |
| **Retrieval** | Semantic search inside `search_symbols` ([RFC 0007](docs/rfcs/0007-semantic-search.md)): Qwen3-Embedding presets (default `qwen3-0.6b-q8`), weights downloaded only on request; vectors keyed by content hash, cached in the store and reused across branches, refs, worktrees and repositories; BM25 and an exhaustive int8 vector scan fused by a convex combination (min-max, α 0.5, N 100); opt-in locally | 🚧 phases 0–1 landed (`rueca-embed`, `rueca models`, `rueca embed`, `rueca index --embed` / `RUECA_EMBED`, `search_symbols` `mode` and `retrieval`, the bench's vector and hybrid arms); next: graph expansion, centrality prior and reranker (phase 2), organization scope and service mode (phase 3) |
| **Precision layer** | Optional compiler-grade edges imported from SCIP indexes | 🗺️ planned |
| **Service mode** | One container, segments in object storage, MCP over HTTP, organization-wide indexing with GPU for embeddings ([RFC 0008](docs/rfcs/0008-service-mode.md)): one image with `query` / `indexer` roles, per-repository catalogs and packs written by compare-and-swap (no lock, no database), stateless Streamable HTTP with OIDC or static tokens and deny-by-default visibility, webhooks as hints over adaptive polling, child-process isolation per ref, retention for branches and PRs | 🚧 phases 0–2 landed (`rueca-storage`: memory / file / S3-compatible stores with compare-and-swap; `rueca-service`: publish, pull, generations, garbage collection, jobs and leases, reconciler, updater; `rueca serve`: the query role — stateless MCP over HTTP, static tokens and OIDC, visibility rules, deadlines, rate limits, health and metrics — and the indexer role — git mirrors with filtered fetches, `index-worker` child processes, webhooks for GitHub / GitLab / Bitbucket, adaptive polling, pull-request lifecycle; the container image); next: phase 3 — `embed` jobs on GPU indexers and inventory discovery, after RFC 0007 lands |
| **Organization graph** | Per-repository graphs stay the unit; one organization graph on top with a single symbol index, cross-repository edges (packages, then contracts) and scoped queries; local `--org` first ([RFC 0006](docs/rfcs/0006-org-graph.md)) | ✅ phases 1–4 landed (organization symbol index, scoped queries, local `--org`, repository routing, cross-repository package edges, Cargo included; refs / branches / PRs over the organization with `--ref` and `refs = [...]`, collapse across refs; contracts: HTTP / OpenAPI / protobuf-gRPC edges across repositories, `list_contracts`) |
| **Org mode** | Every repository of a GitHub / GitLab / Bitbucket org, mirrored permissions, cross-service edges | 🗺️ planned |
| **Agent benchmark** | End-to-end agent runs with and without Rueca (answer quality, tokens, tool calls) | 🗺️ when budget allows |

More languages follow the same gate.

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
| [`rueca-lang-csharp`](crates/rueca-lang-csharp) | C# extraction, namespaces and usings, receivers, `#if` configurations and the .NET configuration bridge |
| [`rueca-lang-kotlin`](crates/rueca-lang-kotlin) | Kotlin extraction, packages shared with Java, receivers and extension functions |
| [`rueca-lang-swift`](crates/rueca-lang-swift) | Swift extraction, SwiftPM targets as modules, extensions, selectors and labels |
| [`rueca-lang-proto`](crates/rueca-lang-proto) | Protocol Buffers services, RPC methods, messages and enums; RPC contracts |
| [`rueca-lang-shell`](crates/rueca-lang-shell) | Shell scripts (sh, bash, zsh, ksh, bats; one dialect per file): functions, `source` paths, script runs, bats / shunit2 tests, env reads and providers |
| [`rueca-lang-ruby`](crates/rueca-lang-ruby) | Ruby extraction, constant paths, receivers, `require` paths and gem manifests |
| [`rueca-lang-sql`](crates/rueca-lang-sql) | SQL in several dialects (sqlparser): tables, columns, views, routines, triggers, migrations, dbt models and macros |
| [`rueca-resolve`](crates/rueca-resolve) | cross-file resolution with confidence |
| [`rueca-segment`](crates/rueca-segment) | segment format, search, content packs, catalog |
| [`rueca-index`](crates/rueca-index) | indexing pipeline |
| [`rueca-query`](crates/rueca-query) | the tools, organization scope |
| [`rueca-mcp`](crates/rueca-mcp) | MCP server: its own protocol layer ([reference](docs/mcp-protocol.md)), stdio |
| [`rueca-codemode`](crates/rueca-codemode) | experimental code mode: sandboxed script engines (QuickJS; Starlark opt-in) behind one trait ([RFC 0009](docs/rfcs/0009-code-mode.md)) |
| [`rueca-embed`](crates/rueca-embed) | embedding presets, weights cache, llama.cpp runtime |
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
