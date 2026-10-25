# RFC 0003 — Structured data and the next languages

- Status: Draft; the shared groundwork (§9.2 contract, §9.3 model, §10.1
  resolver, wave R of §12) is **implemented**, and so are the data formats,
  file kinds and the code ↔ config bridge (waves 1, 2a and 2b:
  `rueca-lang-data` plus the Python, TS/JS and PHP packs) — see the
  *Implemented* notes in §3, §4.5, §5, §6, §7.2 and §8. Go (§10.2), Java
  (§10.4) and C/C++ (§10.3) have landed as `preview`.
- Created: 2026-10-24
- Depends on: RFC 0001 (architecture), RFC 0002 (building blocks)
- Research date: 2026-10-24. Crate versions, licenses and dependency
  requirements were checked against the crates.io API and the crate sources;
  dataset licenses against the Hugging Face API; baseline behaviour against
  the codebase-memory-mcp v0.12.0 README at its tag. Claims that could not be
  checked against a primary source are marked *(unverified)*.

## 1. Summary

The maintainer's order for languages before the first public release is:
**PHP, Markdown, JSON, YAML, the other structured config formats, Go, C++,
C, Java.** PHP and Markdown are being implemented. This RFC designs the rest:

1. A **release gate** that says when a language or format is "supported"
   (§3), with a per-language parity row against codebase-memory-mcp
   (cbm) v0.12.0.
2. A **data model for structured files** (§4–§6): config keys become `Key`
   nodes with qualified names `path:key.path`; well-known file kinds
   (compose, Kubernetes, GitHub Actions, OpenAPI, Terraform, Dockerfile,
   package manifests) add `Resource`, `Image`, `Route` and `EnvVar` nodes;
   pointers inside data (`$ref`, `extends`, YAML aliases, module sources)
   become `REFERENCES` / `IMPORTS` / `INHERITS` edges.
3. **The code ↔ config bridge** (§7), the main reason to index data at all:
   code that reads `os.getenv("DATABASE_URL")`, `process.env.X`,
   `System.getenv`, `@Value("${server.port}")` or `config('app.name')`
   gets a `READS` edge to the env var or config key, and the manifests that
   set an env var get a `WRITES` edge to it. `trace` on an env var answers
   "who reads it and who sets it".
4. **Parsers** (§8): pure-Rust and span-preserving — `jsonc-parser` (JSON,
   JSONC), `saphyr-parser` (YAML, event level, aliases never expanded),
   `toml_edit` (TOML), `roxmltree` (XML), `hcl-edit` (HCL); `.env`, INI,
   `.properties` and Dockerfile are small line-oriented parsers Rueca
   builds, because no maintained crate gives spans.
5. **One crate, `rueca-lang-data`** (§9), implementing the existing
   `LanguagePack` contract. The contract gains only defaulted hooks.
6. **Go, C/C++ and Java** (§10): grammars, module models, qualified names,
   receiver semantics, bench gold sources and risks, plus the shared
   resolver work they need (package-scoped modules, implicit `this`,
   receivers typed from declarations).
7. **Phasing** (§12) in the maintainer's order, with a recommended release
   point.

Every model change is additive (enum variants appended) except one flag on
`Definition` for C/C++ declarations (§9.3).

## 2. Scope

In scope: JSON/JSONC, YAML (multi-document), TOML, `.env`, INI,
`.properties`, XML (known kinds only), HCL/Terraform, Dockerfile; the file
kinds of §5; env and config reads in the code packs (Python, TS/JS, PHP, and
each new language); Go, C, C++, Java.

Out of scope here: Kotlin, Swift, Ruby, C#, Rust (RFC 0001 §12 tiers stand);
protobuf, GraphQL and SQL DDL (RFC 0001 §9.2, a later RFC); cross-repository
linking (RFC 0001 §10 org linker; this RFC produces the facts it will
consume); the final tool list. RFC 0001 §11.4 pointed at "RFC 0003" for the
final tool list; that list moves to a later RFC, and the six-tool contract in
[`docs/tools.md`](../tools.md) stands.

## 3. What "supported" means

### 3.1 The gate

> *Amended by [RFC 0004](0004-quality-and-benchmark.md) (2026-10-25):
> "supported" is renamed `stable` and also requires the metric floors of
> RFC 0004 §6; the parity row becomes the scorecard row (RFC 0004 §9) and
> "at parity" is no longer a gate.*

A language or data format is **supported** when all of the following ship in
the same release. Each item is a checkbox in the language's tracking issue.

| # | Requirement | Code language | Data format |
|---|---|---|---|
| 1 | **Extraction** | definitions for every construct in the pack's construct table (crate docs), references, imports; parse-error rate < 1% of files on the smoke repositories | keys, kind semantics of §5 for that format, pointers of §6 |
| 2 | **Module and import resolution** | `module_name` + `resolve_import` per the module model of §10; IMPORTS edges | relative file pointers (§6) |
| 3 | **Tests** | fixture tests per construct (one fixture per row of the construct table), resolver fixtures for each strategy the language uses, determinism (shuffled inputs), and an `#[ignore]` smoke test on ≥ 2 pinned real repositories (no panics, counts within ±5% of the recorded baseline, a list of known symbols found) | fixtures per format and kind; key-recall property test against a reference parser (§3.3); fuzz target for the parser front end; smoke test on pinned repositories |
| 4 | **Benchmark entry** | S1 localization when ≥ 20 instances exist (§10); S3 structural when a SCIP gold exists | bridge precision/recall on the labelled set (§3.3) |
| 5 | **Parity row** | published in the results file, Rueca vs cbm v0.12.0 (§3.2) | same table, capability and bridge rows |
| 6 | **Docs** | `docs/how-it-works.md` (extraction, qualified names, resolution rungs used), `docs/tools.md` (`language` enum, qualified-name convention), README language list, crate docs, `NOTICE` for vendored queries | same |

"Supported" is the **release gate**: everything is measured and published,
regressions are written down. **At parity** is the separate **adoption gate**
of the decisions log (2026-10-21): Rueca matches or beats cbm on S1–S3 for
that language. Python and TS/JS today are supported but not at parity.

Statuses used in the README and `docs/tools.md`:

| Status | Meaning |
|---|---|
| `supported` | Gate met. |
| `preview` | Extraction ships and is documented; one or more of items 3–5 is missing. Results are flagged in tool output docs. |
| `text` | No pack: `File` node and content search only (any text file). |

*Implemented* (data formats, 2026-10-25): items 1, 2 and 6 of the gate
for every data format, and the parts of item 3 that run in CI — fixtures per
format and kind, the key-recall test against `serde_json`, `saphyr`'s
loader and `toml` (≥ 99%, on the fixture corpus), determinism, adversarial
inputs (billion-laughs YAML, 100,000-deep brackets, random and mutated
bytes) and an `#[ignore]` smoke test (`rueca-lang-data/tests/real_repo.rs`).
Still missing, so every data format is `preview`: `cargo fuzz` targets
(the randomized tests stand in for now), pinned smoke baselines, the bridge
precision/recall labels of §3.3 (item 4) and the parity row (item 5).

### 3.2 Parity table format

One table per release in `docs/bench/<release>-results.md`, generated by
`rueca-bench report`, one row per language and one per data format. Code
languages:

| Language | Status | S1 n | File Acc@5 R / cbm / Δ | Func Acc@10 R / cbm / Δ | S3 gold | S3 n | Def. found R / cbm / Δ | Callers F1 R / cbm / Δ | Callees F1 R / cbm / Δ | Index s (median) R / cbm | At parity |
|---|---|---:|---|---|---|---:|---|---|---|---|---|
| go | preview | 42 | … | … | scip-go 0.2.9 (gin, cli) | 100 | … | … | … | … | ❌ (S1 func) |

Data formats (cbm v0.12.0 indexes all of them; its bridge edges are
`CONFIGURES` with strategies `env_access`, `env_set`, `key_symbol`):

| Format | Status | Key recall vs reference | Span line accuracy | Kinds covered | Bridge P / R (Rueca) | Bridge P / R (cbm) | At parity |
|---|---|---|---|---|---|---|---|
| yaml | preview | … | … | compose, k8s, GHA, OpenAPI | … | … | … |

Δ is Rueca − cbm in points; a cell is ✅ when Δ ≥ 0 or within the noise band
the report prints for that sample size. Rows are never dropped: a language
that regressed keeps its row with the regression.

### 3.3 Data-format measurements

- **Key recall** (CI test, not a bench suite): for a pinned corpus of
  config files, a reference loader (`serde_json`, `saphyr`'s loader, `toml`)
  flattens every file into leaf key paths; Rueca's `Key` nodes must cover
  ≥ 99% of them, excluding paths cut by the budgets of §4.7. Span line
  accuracy compares each `Key`'s start line with the reference parser's
  marker.
- **Bridge precision/recall** (part of S2, the contracts suite, env and
  config kinds): hand-labelled `(reader, env var or key, provider)` triples
  on pinned public repositories with real deployment manifests — the
  OpenTelemetry demo and Online Boutique (already in RFC 0001 §15),
  spring-petclinic, the laravel/laravel skeleton, plus one compose-based
  Python and one Node service. Both arms are scored on the same labels;
  cbm's `CONFIGURES` edges are mapped to `READS` / `WRITES` by direction.
- **S1 regression check**: indexing data files adds symbols that compete in
  `search_symbols`; S1 is re-run with data packs enabled and must not
  regress beyond noise (§11 keeps `Key` out of the default label set for
  this reason).

## 4. Structured data model

### 4.1 Languages and family

New `Language` variants, appended in landing order: `Json`, `Yaml`, `Toml`,
`DotEnv`, `Ini`, `Properties`, `Xml`, `Hcl`, `Dockerfile` (serialized
`json`, `yaml`, `toml`, `dotenv`, `ini`, `properties`, `xml`, `hcl`,
`dockerfile`). One variant per format, not one `Data` variant, so that the
`language` filter of `search_symbols` / `search_code` can say `yaml`.

All of them belong to a new `LanguageFamily::Data` with module separator
`:` and **module name = repository-relative path with extension** (as
Markdown): `config/app.yaml:database.host`.

Detection becomes name-aware (`Language::from_path`): extension first,
then exact names and patterns — `Dockerfile`, `Dockerfile.*`, `*.Dockerfile`,
`Containerfile`; `.env`, `.env.*`; `*.tfvars`, `*.tofu`; `*.jsonc`;
`*.yml`/`*.yaml`; `*.properties`; `*.ini`, `*.cfg` (`setup.cfg` included;
it also stays a Python resolver input). XML files are
detected only for the kinds of §5 (`pom.xml`, `*.csproj`, `*.fsproj`,
`AndroidManifest.xml`, …); generic XML stays `text`.

### 4.2 Labels

Appended to `Label`:

| Label | Meaning | Produced from |
|---|---|---|
| `Key` | A key in structured data, at any depth | every format (§4.4) |
| `Document` | One document of a multi-document YAML stream that has no kind identity | YAML streams with ≥ 2 documents |
| `Resource` | A declared deployable or infrastructure object (RFC 0001 §6.2) | k8s objects, compose services, Terraform `resource` / `data` / `module`, Helm chart |
| `Image` | A container image or build stage (RFC 0001 §6.2) | Dockerfile stages, compose `build` |

Reused: `EnvVar` (env var providers and the canonical env var node, §7),
`Route` (OpenAPI operations), `Variable` (Terraform `variable`, Dockerfile
`ARG`), `Section` stays reserved for document headings (Markdown).

INI sections and TOML tables are `Key`, not `Section`: `database.host`
must have the same shape whether it lives in YAML, TOML, INI or
`.properties`, so that the bridge (§7) matches one key path everywhere.

### 4.3 Qualified names: key paths

Global qualified name = `path` + `:` + **key path**. The key path is the
`Definition.qualified_name` (local); `name` is the last segment.

| Element | Syntax | Example |
|---|---|---|
| Mapping key | bare segment, joined with `.` | `database.host` |
| Key that needs quoting | `"…"` with JSON string escapes, when the key is empty, contains `.`, `[`, `]`, `"`, `\`, whitespace or control characters, or starts with `{` | `paths."/v1/users.{format}".get`, `"a.b".c` |
| Array element | `[N]` (0-based), only for elements that are mappings (§4.5) | `jobs.test.steps[2].run` |
| Identity-keyed list element | `["name"]`, used by kinds whose lists carry an identity field (k8s `name`, GitHub Actions step `id`, XML `artifactId`) | `containers["web"].env["DATABASE_URL"]` |
| Document in a multi-document stream | `{N}` (0-based) at the root, only when the stream has ≥ 2 documents and the document has no kind identity | `{1}.spec.replicas` |
| Kind-named document | the kind identity, e.g. `Kind/name` for k8s | `Deployment/web.spec.replicas` |

Bare segments allow `/`, `-`, `$`, `@`, `{` (except first), `}` and
non-ASCII,
so OpenAPI paths (`paths./users/{id}.get`), npm scopes
(`dependencies.@types/node`) and JSON Schema keywords (`$defs.User`) read
naturally. A `.properties` or Spring key `server.port` is split on `.` into a
key path (§4.4), so `application.properties:server.port` and
`application.yml:server.port` have the same local name. Lookup by suffix
(`server.port`) already works on `.` / `:` boundaries.

Examples: `package.json:scripts.build`,
`.github/workflows/ci.yml:jobs.test`, `k8s/web.yaml:Deployment/web`,
`infra/main.tf:aws_s3_bucket.logs`, `Dockerfile:build` (a stage),
`.env.example:DATABASE_URL`.

### 4.4 What becomes a node, per format

| Format | Nodes | Nesting (`DEFINES`) |
|---|---|---|
| JSON / JSONC | every object member → `Key` | parent member; top level from the `File` |
| YAML | every mapping entry → `Key`; documents → `Document` (or a kind node) | parent entry / document |
| TOML | tables, array-of-tables entries (`[[bin]]` → `bin[0]`), dotted keys and key/value pairs → `Key` | by key path, even when a table is reopened later in the file (span = first occurrence) |
| `.env` | `NAME=value` (optional `export`) → `EnvVar` (provider, §7) | top level |
| INI | `[section]` and `key = value` → `Key` | section |
| `.properties` | `a.b.c=value` → one `Key` with key path `a.b.c`; intermediate segments are not nodes (they have no span) | top level |
| XML (kinds only) | elements selected by the kind (§5) → `Key` / `Resource` | element nesting |
| HCL | blocks → `Resource` / `Variable` / `Key` by block type; attributes → `Key` | block |
| Dockerfile | stages (`FROM … AS x`, unnamed stages `stage[N]`) → `Image`; `ENV` → `EnvVar`; `ARG` → `Variable` | stage |

Scalar-only arrays produce no element nodes: the array's `Key` carries a
preview. Values are never nodes.

### 4.5 What is not indexed

Data files are cheap to parse but can flood the graph. Rules, applied in
order; a skipped file keeps its `File` node and stays content-searchable,
with status `skipped:<reason>`.

1. **Already excluded** by the walk (lockfiles, minified files, source maps,
   ignored paths). The lockfile list grows: `npm-shrinkwrap.json`,
   `go.sum`, `.terraform.lock.hcl`, `packages.lock.json`, `gradle.lockfile`,
   `flake.lock`, `Podfile.lock`, `Package.resolved`, `pubspec.lock`,
   `mix.lock`.
2. **Not config by name or place** (`skipped:data`): `*.ipynb`, `*.geojson`,
   `*.har`, `*.min.json`, and data files under `fixtures/`, `__fixtures__/`,
   `testdata/`, `__snapshots__/`, `test/data/`.
3. **Generated** (`skipped:generated`): a `Code generated … DO NOT EDIT` or
   `@generated` marker in the first 512 bytes.
4. **Size** (`skipped:data-size`): over 256 KiB, unless the file is a known
   kind (OpenAPI specs and k8s bundles are large and valuable; they get the
   general 2 MiB budget).
5. **Shape** (`skipped:data-shape`), decided after parsing, before emitting:
   root is an array of > 100 elements; or the file would emit > 5,000 keys;
   or more than half of its keys are array elements (a data dump, not
   config).

Within an indexed file: array elements that are mappings are indexed up to
64 per array; key depth is capped at 32 (deeper subtrees fold into the
ancestor's preview); a file emits at most 5,000 `Key`s (status `partial`
beyond).

*Implemented* with these refinements: rule 5 applies to files of no known
kind (a Kubernetes bundle or an OpenAPI spec is capped and `partial`, never
skipped), and its "mostly array elements" test needs at least 20
definitions (a three-element config list is not a dump); the 5,000-key cap
counts keys as the generic walk emits them, before kinds relabel some as
`Route` / `Resource` / `EnvVar`; Helm templates (YAML under `templates/`
containing `{{`) are `skipped:template`; HCL deeper than 128 brackets is
`skipped:data-shape`; unrecoverable JSON / TOML / HCL / XML syntax errors
are `skipped:parser`, while YAML keeps what it read (`partial`).

### 4.6 Spans, signatures, docs, secrets

- **Spans**: definition span = from the key's first byte to the end of its
  value; `name_span` = the key token. Every chosen parser reports byte
  offsets; one shared `LineIndex` (newline offsets) converts them to the
  `Span` convention (0-based lines, byte columns). End offsets from event
  parsers (YAML) are trimmed back to the last non-whitespace, non-comment
  byte of the value.
- **Signature**: `key: <preview>`. Scalars as written, truncated to 80
  chars; mappings `server: {port, host, … 3 more}`; arrays
  `hosts: [3 items]`. Kind nodes use the kind's own form:
  `GET /users/{id} (getUser)`, `FROM node:20 AS build`,
  `Deployment web (apps/v1)`.
- **Doc**: the comment block immediately above the key plus a same-line
  trailing comment (YAML, TOML, INI, `.properties`, `.env`, HCL, JSONC,
  Dockerfile); JSON Schema / OpenAPI `description` for schema properties
  and operations (`summary` first).
- **Secrets**: values of `EnvVar` providers are never put in signatures,
  and neither are values of keys whose name matches
  `(?i)pass(word)?|secret|token|api[_-]?key|private[_-]?key|credential`
  (`DATABASE_PASSWORD=•••`). Content search still reads the raw file (it is
  in the repository); Rueca only avoids copying secrets into the symbol
  index. Ignored files (a local `.env`) are never read.

### 4.7 Budgets and untrusted input

Data parsers are pure Rust (no C), but still face hostile input:

| Threat | Mitigation |
|---|---|
| YAML alias expansion ("billion laughs") | Rueca consumes `saphyr-parser` **events** and never expands aliases: an alias becomes a `REFERENCES` fact to the anchored key (§6). `saphyr`'s own loader clones the anchored node per alias and is not used. Cap: 1,000 anchors per file. |
| Deep nesting (stack) | `jsonc-parser` stops at depth 512; `toml_edit` has a recursion guard unless its `unbounded` feature is enabled (it must not be); `saphyr-parser` bounds flow nesting; `hcl-edit` has no guard, so Rueca pre-scans bracket/brace depth and refuses > 128 before parsing. Rueca's own tree walk stops at depth 32 (§4.5). |
| XML entities | `roxmltree` rejects DTDs by default (`allow_dtd: false`) and has `nodes_limit`; Rueca sets it to 200,000. |
| Huge files, many documents | 2 MiB per file (existing budget), 1,000 YAML documents per stream, key and element caps of §4.5. |
| Time | Pure-Rust parsers take no progress callback; size caps bound time, and RFC 0001 §13 worker isolation applies in service mode. |

Each format front end gets a `cargo fuzz` target (RFC 0001 §13).

## 5. File kinds

A **kind** is a semantic layer over the parsed tree, chosen by path and
shape inside `extract` (which has both). Kinds add labels, edges and
signatures; they never remove generic `Key`s, so an unrecognized variant of
a kind degrades to plain keys.

| Kind | Detection | Extra semantics | Wave |
|---|---|---|---|
| `package.json` | name | `scripts.*` keys with the command as signature; `main` / `module` / `types` / `bin` / `exports` → IMPORTS to the entry files; `workspaces` | 1 |
| tsconfig / jsconfig | `tsconfig*.json`, `jsconfig.json` (JSONC) | `extends` → IMPORTS; `compilerOptions.paths` keys | 1 |
| `composer.json` | name | `autoload.psr-4` keys; `scripts` | 1 |
| `pyproject.toml` | name | generic keys; `[project.scripts]` / `[project.entry-points]` values `pkg.mod:func` → REFERENCES to the Python definition (wave 2) | 1 / 2 |
| `.env*` | name | `EnvVar` providers → canonical env var (§7) | 2 |
| docker compose | `compose.y(a)ml`, `docker-compose*.y(a)ml`, or root `services:` whose entries have `image` / `build` | services → `Resource`; `environment` → `EnvVar` providers; `env_file` → IMPORTS; `build` → IMPORTS of the context folder / Dockerfile; `depends_on` → REFERENCES; `extends` → INHERITS | 2 |
| Kubernetes | per document: `apiVersion` + `kind` + `metadata.name` | document → `Resource` `Kind/name`; container `env` → `EnvVar` providers; `envFrom` / `configMapKeyRef` / `secretKeyRef` → REFERENCES to the ConfigMap / Secret `Resource` or key; ConfigMap `data` keys → `Key` (Secret values never stored) | 2 |
| GitHub Actions | `.github/workflows/*.y(a)ml`, `action.y(a)ml` | jobs → `Key` with `runs-on` signature; steps keyed by `id`; `needs` → REFERENCES; local `uses: ./…` (actions and reusable workflows) → IMPORTS; `env` → `EnvVar` providers | 2 |
| OpenAPI / Swagger | root `openapi: 3.*` or `swagger: "2.0"` (JSON or YAML) | `paths.<p>.<method>` → `Route` named `GET /users/{id}`, path canonicalized as in RFC 0001 §9.4; `components.schemas.*` / `definitions.*` keys; `$ref` → REFERENCES | 2 |
| JSON Schema | root `$schema` or `*.schema.json` | `$defs` / `definitions`; `$ref` → REFERENCES | 2 |
| Dockerfile | §4.1 names | stages → `Image`; `COPY --from=<stage>` → REFERENCES; `FROM <stage>` → INHERITS; `ENV` → `EnvVar` providers; `ARG` → `Variable` | 2 |
| Terraform / OpenTofu | `.tf`, `.tofu`, `.tfvars` | `resource "t" "n"` → `Resource` `t.n`; `data` → `data.t.n`; `variable` → `Variable` `var.x`; `locals` → `local.x`; `output`; `module "m"` → `Resource` `module.m` + IMPORTS of the source folder | 2 (definitions), 3 (references, needs §10.1 package scope) |
| Spring config | `application*.{yml,yaml,properties}`, `bootstrap*.*` | config namespace `spring` for the bridge; dotted YAML keys split; `${ENV:default}` → READS of the env var | 2 |
| .NET config | `appsettings*.json` | config namespace `dotnet` (`A:B:C` ≡ `A.B.C`) | 2 |
| Maven / .NET projects | `pom.xml`, `*.csproj` | `modules`, `dependencies` keyed `groupId:artifactId`; `ProjectReference` → IMPORTS; source roots for Java (§10.4) | 3 / with Java |
| Helm | `Chart.yaml`, `values*.yaml`; `templates/**` | chart → `Resource`; values → `Key`s. Templates are Go templates, not YAML: `File` only in waves 1–2; `.Values.a.b` paths scanned from `{{ }}` actions → REFERENCES to values keys in wave 3 | 2 / 3 |
| `go.mod`, `go.work` | name | module path, `replace` with local paths — input to the Go resolver | with Go |
| Android manifest | `AndroidManifest.xml` | activities / services → `Resource` with class REFERENCES | later |
| i18n catalogs | `locales/**`, `*.po` | translation keys + `t('home.title')` bridge | later (open question) |

*Implemented* (waves 1–2): every kind of the table except the Helm row
(chart `Resource`, `values*.yaml` scoping and `.Values` references stay for
wave 3), `go.mod` / `go.work` (with Go) and Android components (later).
Maven and MSBuild projects get keys (`dependencies.dependency["g:a"]`,
`ItemGroup.PackageReference["Serilog"]`) and imports (`modules/module`,
`parent/relativePath`, `ProjectReference`); Java source roots come with
Java. Additions: `pyproject.toml` entry points include
`[project.gui-scripts]`, `[project.entry-points.*]` and
`[tool.poetry.scripts]`; Kubernetes `secretKeyRef` points at `data` and
`stringData`; compose `build` also imports `<context>/Dockerfile`;
GitHub Actions `${{ env.X }}` and Kubernetes `$(X)` are env var reads by
the key that holds them. Terraform references (wave 3 in the table) are
implemented too, since directory scope landed in wave R:
`var.x` / `local.x` / `module.m` / `data.t.n` / `t.n` traversals resolve
within the module directory.

## 6. References inside data

Data pointers use the same facts as code: `Import` for file targets,
`Reference` (new kind `DataRef`, §9.3) for key targets, `Definition.bases`
for inheritance-like merges.

| Source | Fact | Edge | Resolution | Confidence / strategy |
|---|---|---|---|---|
| `$ref: '#/components/schemas/User'` | `DataRef`, name = key path `components.schemas.User` (JSON Pointer decoded: `~1` → `/`, `~0` → `~`) | `REFERENCES` key → key | same file, exact key path | 1.0 `syntactic` |
| `$ref: './user.yaml#/User'` | `Import` `./user.yaml` + `DataRef` with receiver = specifier | `IMPORTS` file → file; `REFERENCES` key → key | file relative to the source file; key path in the target | 1.0 / 0.95 `import_resolved` |
| YAML alias `*defaults` | `DataRef` to the anchored key path | `REFERENCES` | same document | 1.0 `syntactic` |
| YAML merge `<<: *defaults` | `bases` of the mapping key | `INHERITS` key → key | same document | 1.0 `syntactic` |
| tsconfig / eslint `extends` | `Import` | `IMPORTS` | relative path; package specifiers are external (no `node_modules` in the snapshot) | 1.0 |
| compose `extends`, `env_file`, `build` | `Import` / `bases` | `IMPORTS`, `INHERITS` | relative to the compose file | 1.0 |
| Terraform `module` `source = "./modules/net"` | `Import` | `IMPORTS` file → **folder** | relative path; registry and git sources external | 1.0 |
| Terraform `aws_s3_bucket.logs.arn`, `var.x`, `module.m.out` | `Reference` (`Read`) | `REFERENCES` | same module = all `.tf` files of the directory (§10.1) | 0.95 `same_module` |
| Dockerfile `COPY --from=build`, `FROM build` | `DataRef` / `bases` | `REFERENCES` / `INHERITS` stage → stage | same file | 1.0 |
| Dockerfile `COPY src/ /app/src` | — | — | build context is unknown statically; later, with compose/CI context (open question) | — |
| GitHub Actions `uses: ./.github/actions/x` | `Import` | `IMPORTS` | repository root | 1.0 |
| `pyproject` `pkg.mod:main` | `DataRef` with a global qualified name | `REFERENCES` key → Python function | exact global qualified name | 0.95 `import_resolved` |

The resolver handles `DataRef` outside the strategy ladder: exact matches
only, no name fallback, so a data pointer never fuzzy-matches.

*Implemented*: every row except Dockerfile `COPY src/` build contexts
(open question 5). A `$ref` JSON Pointer into an array becomes `[N]` (or the
element's identity) by walking the document; a pointer into another file
keeps its tokens as keys.

## 7. The code ↔ config bridge

### 7.1 Graph shape

Env vars are a process-global namespace with many providers and often no
defining file. Config keys are defined in files a framework loads. The two
are modelled differently:

- **Canonical env var node**: one `EnvVar` node per name in the snapshot,
  file-less, qualified name `$env:NAME`, symbol id
  `<repo>@<commit>:$env#NAME`. It exists if anything reads or sets the name.
- **Provider entries** (`.env` lines, compose / k8s / GitHub Actions `env`,
  Dockerfile `ENV`) are `EnvVar` definitions in their files
  (`.env.example:DATABASE_URL`,
  `k8s/web.yaml:Deployment/web.spec.template.spec.containers["web"].env["DATABASE_URL"]`)
  with a `WRITES` edge to the canonical node.
- **Config keys** are the `Key` definitions themselves; there is no
  canonical key node.
- **Code readers** get a `READS` edge: to the canonical env var, or to every
  `Key` the read resolves to.
- **Config that reads env** (Spring `${DB_HOST:localhost}`, compose
  `${DATABASE_URL}`, Laravel `env('X')` inside `config/*.php`) gets
  `READS` from the `Key` to the canonical env var, so chains compose:

```
Method OwnerController.find ─READS─▶ Key application.yml:spring.datasource.url
Key spring.datasource.url    ─READS─▶ EnvVar $env:DB_URL
EnvVar k8s/app.yaml:…env["DB_URL"] ─WRITES─▶ EnvVar $env:DB_URL
EnvVar .env.example:DB_URL          ─WRITES─▶ EnvVar $env:DB_URL
```

`READS` exists in the model and is not produced yet; this RFC gives it its
meaning: *reads the value of an env var or config key*. `WRITES` keeps its
meaning (sets a value). RFC 0001 §6.3 `USES_ENV` and `CONFIGURES` are not
added: direction-specific `READS` / `WRITES` answer "who reads" and "who
sets" with a plain `trace` edge filter.

### 7.2 Facts emitted by code packs

Two new `RefKind` variants (appended): `EnvVar` and `ConfigKey`. The name
must be a **string literal** (or a same-file string constant, wave 3);
dynamic names emit nothing. Packs emit them only for APIs identified with
file-local evidence (the import of `os`, the builtin `process`, the PHP
builtin `getenv`), never by function name alone.

| Language | Env var reads (`EnvVar`, name = var) | Config key reads (`ConfigKey`, name = key path, receiver = namespace) |
|---|---|---|
| Python | `os.environ["X"]`, `os.environ.get("X")`, `os.getenv("X")`, `environ[...]` after `from os import environ` | Django `settings.X` (namespace `django`, later); pydantic `BaseSettings` fields (later) |
| TS / JS | `process.env.X`, `process.env["X"]`, `const { X } = process.env`, `import.meta.env.X`, `Deno.env.get("X")` | `config.get("a.b")` with `config` imported from `config` (namespace `node-config`) |
| PHP | `getenv('X')`, `$_ENV['X']`, `$_SERVER['X']`, Laravel `env('X')` | Laravel `config('app.name')`, `Config::get(...)` (namespace `laravel`) |
| Go | `os.Getenv("X")`, `os.LookupEnv("X")` | viper `viper.GetString("a.b")` (namespace `viper`) |
| Java | `System.getenv("X")` | Spring `@Value("${a.b}")`, `env.getProperty("a.b")`; `@ConfigurationProperties(prefix)` fields (later) (namespace `spring`) |
| C / C++ | `getenv("X")`, `std::getenv("X")`, `secure_getenv` | — |

*Implemented* for Python, TS / JS and PHP as listed, except Django
`settings.X` and pydantic `BaseSettings` (later). Evidence: Python needs the
file's `import os` / `from os import …` (aliases followed); TS / JS needs
`process` / `Deno` not to be a local binding and, for node-config, an
import or `require` of `config`; PHP matches the unqualified builtin /
helper names (`getenv`, `env`, `config`) and the `Config` facade, so a
project-defined namespaced `env()` would be misread. Assignment and
`del` / `delete` targets are writes, not reads. Laravel config files are
handled in the PHP pack as §7.3 says: `config/<file>.php` arrays become
`Key`s (PHP has no module name, so their global name is the key path,
`name`; symbol id `config/app.php#name`), and `env()` inside an entry is
read by that key. The PHP pack therefore sets `facts_depend_on_path`.

Why new kinds instead of `Read` with a marker receiver: `Read` goes through
the strategy ladder, whose last rung matches any symbol by name; an env var
name must never fuzzy-match a code symbol, and the resolver must know the
namespace without parsing receiver text. Appending variants is additive.

### 7.3 Resolution and confidence

| Read | Target | Strategy | Confidence |
|---|---|---|---|
| `EnvVar` | canonical `$env:NAME` (created if absent) | `contract` (new, `Strategy::Contract`) | 0.95 |
| `ConfigKey`, namespace with a known config scope (`spring`: `application*` / `bootstrap*` files; `laravel`: `config/<file>.php` → prefix `<file>`; `dotnet`: `appsettings*.json`; `node-config`: `config/*.{json,yaml,yml}`; `viper`: files named by a literal `SetConfigName`, else `config.*`) | every `Key` with that exact key path in the scope (profiles such as `application-prod.yml` all match) | `contract` | 0.90 |
| `ConfigKey`, no scope found | the only `Key` in the snapshot with that exact key path | `unique_name` | 0.60 |
| `ConfigKey`, several candidates outside a scope | none (reported unresolved) | — | — |
| Provider entry | canonical `$env:NAME` | `syntactic` | 1.0 |

Names match exactly (case-sensitive). Spring relaxed binding
(`SERVER_PORT` ↔ `server.port`) and cbm-style identifier matching
(`maxRetryCount` = `max_retry_count`, scoped to the config file's
component) are a later, opt-in `heuristic` rung at 0.5. Laravel config files
are PHP: the PHP pack (or its Laravel layer) emits `Key`s for the array
returned by `config/*.php`, so the same resolution applies.

`Strategy::Contract` is appended (RFC 0001 §6.3 already names it). Bridge
edges never compete with ladder edges for the same `(src, dst, kind)`, so its
position in the derived order does not matter.

### 7.4 How tools expose it

- `search_symbols {"query": "DATABASE_URL"}` finds the canonical
  `EnvVar` and its providers (`EnvVar` is in the default label set).
- `get_symbol {"qualified_name": "$env:DATABASE_URL"}` returns the node
  plus, for `EnvVar` and `Key`, `readers_count` and `writers_count`
  (new fields; `docs/tools.md` change).
- `trace {"qualified_name": "$env:DATABASE_URL", "direction": "callers",
  "edge_types": ["READS", "WRITES"]}` lists the code that reads it and the
  manifests that set it; adding `CALLS` to `edge_types` continues to the
  callers of the reading functions.
- `trace` on a function with `direction: "callees"` and
  `edge_types: ["READS"]` lists the configuration it depends on.

### 7.5 Compared to cbm v0.12.0

cbm already links code to env vars and config keys (`CONFIGURES`, shared
`EnvVar` per name, per-container env vars, `key_symbol` name matching) and
goes further on deployment topology (image → Dockerfile → program,
Kustomize, Helm rendering, Terragrunt). Rueca's first waves aim at the
bridge (§3.3 measures it against cbm) and at file-level kinds; topology
chains (`DEPLOYS`, image scoping of env reads through `COPY`) are later
work for a framework/infra RFC.

### 7.6 Alternatives rejected

- **No canonical env var node; edges from readers to every provider.**
  Loses env vars that are read but set nowhere in the repository — exactly
  the ones an agent needs to list ("what does this service need to run?").
- **A canonical node for config keys too.** Keys are file-scoped by nature
  (profiles, per-environment files); a canonical node would hide which file
  sets the value.
- **One edge kind (`CONFIGURES`, as cbm and RFC 0001 §6.3).** Direction is
  the whole question; two existing edge kinds express it without schema
  growth.
- **Matching identifiers to keys by name by default.** High recall, low
  precision on common words (`timeout`, `host`); kept as an opt-in rung.

## 8. Parsers

| Format | Crate | Version | License | Spans | Limits | Verdict |
|---|---|---|---|---|---|---|
| JSON, JSONC | `jsonc-parser` | 0.34.0 | MIT | byte `Range` on every node; comments collected | depth 512 built in | **Dependency.** Lenient options (comments, trailing commas) for every `.json`; JSON5 not supported |
| YAML | `saphyr-parser` | 0.2.2 | MIT OR Apache-2.0 | `SpannedEventReceiver`: byte index, 1-based line/col per event | flow nesting bounded; Rueca bounds the rest | **Dependency** (event level). `saphyr` (loader) only as the reference parser in tests |
| TOML | `toml_edit` | 0.25.17 | MIT OR Apache-2.0 | `Document::parse` keeps byte spans on keys and items; decor keeps comments | recursion guard (default features) | **Dependency** |
| XML | `roxmltree` | 0.21.2 | MIT OR Apache-2.0 | `Node::range()` byte ranges, attribute ranges | DTD off by default, `nodes_limit`, entity loop detection, `forbid(unsafe_code)` | **Dependency** (kinds only) |
| HCL | `hcl-edit` | 0.9.8 | MIT OR Apache-2.0 | `Span::span()` byte ranges, decor | none: Rueca pre-scans depth | **Dependency** |
| `.env`, INI, `.properties` | — | — | — | — | — | **Build**: ~100 lines each; `dotenvy` 0.15.7, `rust-ini` 0.21.3 and `java-properties` 2.0.0 are MIT but expose no positions |
| Dockerfile | — | — | — | — | — | **Build**: line-oriented (continuations, parser directives, JSON-form arguments, heredocs), ~400 lines, against the BuildKit reference |
| Reference parsers (tests only) | `serde_json` 1.0.154, `saphyr` 0.2.2, `toml` 1.1.8 | | MIT OR Apache-2.0 | | | Dev-dependencies for the key-recall test |

*Implemented* with the crates and versions of the table (versions and
licenses re-checked on crates.io on 2026-10-25; `cargo deny` passes; no
new license). Two facts found while building: `saphyr-parser` markers count
**characters**, not bytes, so offsets are mapped back to bytes for
non-ASCII sources; and its block-collection end events sit at the next
token, so block collections end where their last child ends.

Not chosen:

| Candidate | Reason |
|---|---|
| `serde_json` (main parser) | No spans, no comments |
| `tree-sitter-json` 0.24.8, `tree-sitter-toml-ng` 0.7.0, `tree-sitter-xml` 0.7.0, `tree-sitter-hcl` 1.1.0 | Linkable (`tree-sitter-language ^0.1`), but RFC 0002 §3.3 prefers pure Rust for data on untrusted input, and semantic extraction needs typed values, not CST nodes |
| `tree-sitter-yaml` 0.7.2 | Unpatched out-of-bounds write (RFC 0002 §3.2) |
| `tree-sitter-dockerfile` 0.2.0 | Normal dependency on `tree-sitter ^0.20`: cannot link next to 0.27 |
| `dockerfile-parser` 0.9.0 (MIT, pest) | Last release 2024-10; reference for the instruction grammar |
| `taplo` 0.14.0 (MIT) | Rowan CST, heavier than needed; last release 2025-05 |
| `quick-xml` 0.42.0 (MIT) | Streaming with `buffer_position()`; fine, but kinds need a DOM; fallback if `roxmltree` stalls |
| `marked-yaml` 0.8.0, `yaml-rust2` 0.13.0 | Alternatives; `saphyr` was already chosen in RFC 0002 |
| `serde_yaml` | Deprecated upstream |
| `oas3` 0.22.0, `openapiv3` 2.2.0 | Serde models without spans; OpenAPI semantics run on the spanned tree instead (amends RFC 0001 §9.2) |

## 9. Crate layout and contract

### 9.1 `rueca-lang-data`

One crate, features per format (`json`, `yaml`, `toml`, `xml`, `hcl`,
`dockerfile`; all default):

```
rueca-lang-data/
  src/tree.rs        spanned data tree: Map / Seq / Scalar / Alias, comments, LineIndex
  src/keypath.rs     key-path grammar of §4.3 (format + parse, round-trip tested)
  src/formats/       json, yaml, toml, dotenv, ini, properties, xml, hcl, dockerfile → tree
  src/kinds/         detection (§5) + one module per kind, over the tree
  src/emit.rs        tree (+ kind overlay) → FileFacts
  src/lib.rs         DataPack: LanguagePack for every data Language
```

Why one crate: kinds cut across formats (OpenAPI in JSON or YAML; compose
and k8s are YAML but share the env-provider logic), every format needs the
same key-path and emit code, and the binary links all formats anyway.
Rejected: one crate per format (duplicates tree, key paths and kinds);
putting file kinds into a separate `rueca-packs` crate as RFC 0001 §16
planned (kinds need the spanned tree, which is the data crate's core).
Code-side bridge facts (§7.2) live in each code language pack, because they
are per-file syntax.

`DataPack` implements `LanguagePack` as is: `extract` → definitions, `DataRef`
references, imports; `module_name` = path; `resolve_import` = relative path
(then repository root for GitHub Actions); `is_builtin` = false;
`self_receivers` = empty.

### 9.2 Contract changes

The `LanguagePack` trait keeps its methods. Additions are **defaulted**, so
existing packs compile unchanged:

| Change | Why | Needed by |
|---|---|---|
| `resolve_import` may return a **directory** path; the resolver maps it to the `Folder` node (`IMPORTS` File → Folder) | Terraform modules, compose build contexts, Go packages, Java on-demand imports | data, Go, Java |
| `fn module_scope(&self) -> ModuleScope { ModuleScope::File }`; `Package` makes every file with the same `module_name` one module for lookups (binding environment, same-module rung, member lookup by qualified-name prefix) | Go packages, Java packages, Terraform modules: names are visible across files of a directory without imports | Go, Java, Terraform |
| `fn implicit_self(&self) -> bool { false }`; when true, a receiver-less reference inside a method is first looked up as a member of the enclosing class (new rung between 1 and 2, `type_resolved` 0.90) | C++ and Java call members without `this` | C++, Java |
| Reserved receiver texts: `$self` (pack-normalized self receiver) and `$type:<T>` (receiver whose declared type is known in the file) | Go receivers are named per method; Java, Go and C++ declare local, parameter and field types | Go, C++, Java |
| `Language::from_path` matches file names (§4.1); `.h` is C++ when the snapshot has C++ sources (indexer decision) | Dockerfile, `.env*`, C/C++ headers | data, C/C++ |
| `ProjectFiles::is_config_file` adds `go.mod`, `go.work`, `pom.xml`, `build.gradle(.kts)`, `settings.gradle(.kts)`, `compile_commands.json` (≤ 8 MiB), `compile_flags.txt` | module models of §10 | Go, Java, C/C++ |

`$type:<T>` is strategy 5 for explicitly typed languages: the resolver
resolves `T` like a `Type` reference, then looks the member up in it
(`type_resolved`, 0.85). It needs no assignment facts, only the pack's
file-local declaration scan.

*Implemented* (wave R) as listed, with these refinements, all defaulted:
`module_scope(language)` takes the language (one pack serves several:
Terraform is directory-scoped, YAML is not) and has a third variant,
`ModuleScope::Directory` (every file of one language in a directory, for
Terraform modules whose files keep their path as module name); `overloads()`
and `overrides()` opt into §10.1 items 4–5; `facts_depend_on_path()` adds
the path to the facts cache key for packs whose output depends on it (data
kinds chosen by name); `ExtractError::Skipped(reason)` reports the §4.5
skips as `skipped:<reason>`; `module_name_for(path, facts, project)` answers
open question 2 (§15). `$self` is honoured for packs that list it in
`self_receivers()` (JavaScript, Java and PHP allow `$self` as an identifier,
so it cannot be reserved for everyone). `ProjectFiles::max_config_bytes`
allows 8 MiB for `compile_commands.json`. `.csproj` is not loaded: no
planned resolver reads it.

### 9.3 Model changes

| Item | Change | Additive |
|---|---|---|
| `Language` | append `Json`, `Yaml`, `Toml`, `DotEnv`, `Ini`, `Properties`, `Xml`, `Hcl`, `Dockerfile`, then `Go`, `Cpp`, `C`, `Java` as they land | yes |
| `LanguageFamily` | add `Data` (`:`), `Go` (`.`), `C` (`::`, C and C++), `Java` (`.`) | yes (not stored in segments) |
| `Label` | append `Key`, `Document`, `Resource`, `Image` | yes |
| `RefKind` | append `EnvVar`, `ConfigKey`, `DataRef` | yes (facts) |
| `Strategy` | append `Contract` | yes |
| `EdgeKind` | none; `READS` starts being produced | yes |
| `Definition` | add `is_declaration: bool` (C/C++ prototypes and in-class method declarations) | **no**: postcard facts are positional. Lands with a `FACTS_FORMAT` constant added to the facts cache key, so the change re-extracts once instead of failing to decode |
| `Reference` | add `arity: Option<u32>` (call argument count, for §10.1 overload choice) | **no**, same facts-format bump (`FACTS_FORMAT` = 2) |
| `ResolvedGraph` | add `modules: Vec<String>` (module name per file), stored in an optional segment component `files.modules` | yes: readers ignore unknown components; older segments read as "unknown" |

*Implemented* (wave R). All `Language` variants landed at once (with
`Language::ALL`, `parse`, name-based detection and `header_language` for
`.h`); `Label::is_data_detail()` marks `Key` / `Document`; `RefKind::DataRef`
receivers are `None` (same file, then same package), an import specifier,
`$qn` (a global qualified name) or `$any` (any data file); `ConfigKey`
namespaces are `spring`, `laravel`, `dotnet`, `node-config`, `viper` and
`viper:<config name>`. The segment format stays 2: a store written before
the change opens unchanged (`rueca-segment/tests/compat.rs`), and enum
variant indexes are pinned by a test in `rueca-model`. `files.modules`
lets tools split Go and Java qualified names (import paths and packages
contain dots) into module and local name for symbol ids.

## 10. Code languages: Go, C/C++, Java

All three grammars depend only on `tree-sitter-language ^0.1` and link with
the tree-sitter 0.27 runtime (their `tree-sitter` requirement is a
dev-dependency). Locals queries come from nvim-treesitter (Apache-2.0) and
bloop (Go, Java) / scip-syntax (Go, Java), adapted as in RFC 0002 §3.4.

### 10.1 Shared resolver work (once, before Go and Java)

1. Package-scoped modules (`ModuleScope::Package`, §9.2).
2. Directory imports (`IMPORTS` File → Folder).
3. Typed receivers (`$type:<T>`) and implicit self (§9.2).
4. Overloads (Java, C++): when a scope has several definitions with the
   same name, **all** of them get a normalized parameter-type suffix in the
   local qualified name (`Parser.parse(String)`, `Parser.parse(Reader)`);
   non-overloaded names stay bare. Lookup by `qualified_name` treats the
   suffix as optional (all overloads become candidates). Call resolution
   picks overloads by argument count, else links all of them at × 0.8
   confidence.
5. `OVERRIDES` edges (in the model, not produced yet): a method in a
   subtype with the name (and arity) of a base method. `trace` callers of an
   interface method can then add `OVERRIDES` to reach implementations.

*Implemented* (wave R; `rueca-resolve` crate docs, *RFC 0003 hooks*):
package and directory scope, directory imports (a package-scoped import
binds the package whose module name is the specifier, else the largest in
the directory), `$type:` and `$self` receivers, implicit self as rung 1b,
`is_declaration` → unique body, overloads (`Reference::arity`, fan-out
× 0.8; constructors of Java and C/C++ are the members named like the
class) and `OVERRIDES` (`type_resolved` 0.90). Also generic, for the packs
that need them: C/C++ includes as transitive star imports, member imports
`C.m` (Java `import static`), the bridge of §7.3 and the data pointers of §6
(`rueca-resolve` `bridge` module), and in `rueca-query` symbol ids for every
new family, the `$env` pseudo-path, optional overload suffixes in lookups,
`readers_count` / `writers_count` and the default-label rule of §11. Each
hook has a fake-pack example in `rueca-resolve/tests/hooks.rs`.

### 10.2 Go

| Aspect | Design |
|---|---|
| Grammar | `tree-sitter-go` 0.25.0, MIT |
| Module model | package = directory. `go.mod` `module` path + relative directory = import path; nearest ancestor `go.mod` (multi-module repos), `go.work` `use`, local `replace` targets mapped to their directories; `vendor/` excluded; no `go.mod` → directory path. `_test.go` files with `package foo_test` form module `<path>_test` |
| Imports | `import "p"`, aliased, dot (`.` → star), blank (`_` → no binding). Binding name = the target package clause when the target is in the project; else last path element without a `/vN` major suffix or `go-` prefix (heuristic) |
| Qualified names | `<import path>.<local>`, separator `.` (as Go tooling prints them): `github.com/gin-gonic/gin.Engine.ServeHTTP` |
| Receivers | `func (s *Server) Handle()` → `Method` `Server.Handle`; methods may live in another file of the package, so membership is by qualified-name prefix within the package scope, not by `parent`. The pack rewrites the receiver identifier to `$self` |
| Embedding | embedded fields → `bases` → `INHERITS` (method promotion = inherited lookup) |
| Interfaces | implicit: `IMPLEMENTS` computed in a post-pass when a type's method names cover an interface's (≥ 1 method), `heuristic` 0.5, capped per interface |
| Typed receivers | `var s Server`, `s := &Server{}` / `Server{}` / `NewServer()` (constructor convention, heuristic), typed params and struct fields → `$type:` |
| Tests | `Test*`, `Benchmark*`, `Fuzz*`, `Example*` in `_test.go` |
| Heuristic | interface dispatch, method values, function-typed fields, generics (type parameters ignored) |
| Precision | scip-go 0.2.9 (Apache-2.0) |
| Bench | S1: SWE-bench Multilingual Go, 42 tasks (caddy 14, prometheus 8, gin 8, hugo 7, terraform 5; MIT); Multi-SWE-bench Go. S3: scip-go gold on gin and cli/cli (RFC 0002 corpus) |

*Implemented* (wave 3, status `preview`: gate items 1–3 and 6 met, the
benchmark entry and parity row — items 4–5 — are pending):
`rueca-lang-go` (`tree-sitter-go` 0.25.0, vendored `locals.scm` adapted
from nvim-treesitter and bloop) as designed above, registered in the
default registry. Refinements and deviations, documented in the crate
docs:

- **Binding names.** No hook lets a pack read another file's `package`
  clause, so an unaliased import binds its conventional name (last element
  without `/vN`, `.vN`, `go-` / `-go`); because Go rejects unused imports,
  an import whose conventional name is never used in the file is re-bound
  to the unexplained qualifier its path contains (`jsoniter` for
  `github.com/json-iterator/go`), else to the only unexplained qualifier.
- **External test packages** are marked by a `Package` definition in the
  facts (`package x_test`), which `module_name_for` reads.
- **Typed receivers** go beyond the table: the named result of a function
  of the same file, type assertions, type-switch cases, struct-field chains
  (`s.store.Get()`), `err` as `error` by convention, and chains through a
  package-qualified type or an import (`c.Request.Header.Set()` →
  `http.Request.Header`) are written so a third-party type resolves as
  external instead of a name guess. Types are file-local.
- **Type aliases** (`type A = B`) list `B` as a base, so `B`'s members are
  found through `A`.
- **Interfaces**: the post-pass is the Go-only `go` module of
  `rueca-resolve` (wired with one call after `OVERRIDES`); it also emits
  `OVERRIDES` (heuristic 0.5) from each covering method to the interface
  method, counts promoted methods, matches an interface that embeds one
  outside the project on its known methods when it has at least two, and
  keeps at most 64 implementers per interface (first by qualified name).
- `facts_depend_on_path` is true (`is_test` depends on `_test.go`).
- Env and config reads of §7.2 ship with the pack (`os.Getenv`,
  `os.LookupEnv`, viper `Get*` with `viper:<SetConfigName>` namespaces).
- Not done: the `go.mod` data kind (the pack reads `go.mod` / `go.work`
  only as resolver input), build tags and cgo, the benchmark suites.

Smoke repositories (`rueca-lang-go/tests/real_repo.rs`, shallow clones,
2026-10-24): gin `0f09c3a` — 99 files, 2,091 definitions, 13,840
references, 0 parse errors; urfave/cli v3 `7166c48` — 79 files, 1,801
definitions, 12,312 references, 1 file with parse errors (`new(0)`, Go
1.26 syntax the 0.25 grammar predates). Resolution (`rueca index`): gin
39.9 % resolved (type_resolved 7.7 %, heuristic 3.8 %), 49.0 % external,
4.0 % builtin, 7.2 % unresolved; cli 54.6 % resolved (type_resolved
15.3 %, heuristic 0.8 %), 38.0 % external, 4.3 % builtin, 3.1 %
unresolved.

### 10.3 C and C++

One pack crate (`rueca-lang-c`) for both languages, family `C`.

| Aspect | Design |
|---|---|
| Grammars | `tree-sitter-c` 0.24.2 and `tree-sitter-cpp` 0.23.4, MIT |
| Module model | no modules (C++20 modules out of scope). `#include "x.h"` resolves relative to the including file, then the `-I` / `-iquote` directories of `compile_commands.json` or `compile_flags.txt` when present, then conventional roots (`include/`, `src/`, repository root), then a unique path-suffix match (`IMPORTS` at `heuristic` 0.6). `<x.h>` is external unless found in a project include directory. Includes are transitive star imports (existing depth bound 10) |
| Qualified names | namespace + class path with `::`: `fmt::v11::format_to`, `redisClient::flags`; `module_name` is empty, so the global name is the local name (the resolver already omits the separator for an empty module). Anonymous namespaces and `static` functions are file-local (`exported = false`) |
| Declarations vs bodies | prototypes and in-class declarations are definitions with `is_declaration`; out-of-line bodies (`void Foo::bar() {…}`) are top-level `Method`s named `Foo::bar`. References prefer the unique body with that global name, else the declaration |
| `this` | `this->x`, `this.x` → self; implicit members via `implicit_self` |
| Overloads, templates | §10.1 suffixes; templates by name without arguments; specializations merge; operators named `operator==` |
| Macros | function-like `#define F(x)` → `Function` (signature = the define line); object-like → `Constant`. Code inside macro expansions is invisible; both branches of `#if` are parsed |
| Inheritance | base clauses → `bases`; virtual overrides → `OVERRIDES` |
| Heuristic | overload choice, ADL, calls through function pointers and virtual dispatch, macro-generated definitions |
| Precision | scip-clang 0.4.1 (Apache-2.0), needs `compile_commands.json` |
| Bench | S1: SWE-bench Multilingual C 30 (redis 12, jq 9, micropython 5, valkey 4) and C++ 12 (fmt 11, nlohmann/json 1) — small, so Multi-SWE-bench C/C++ (CC0, upstream licenses apply) supplements. S3: scip-clang gold on fmt (CMake) and jq |

*Implemented* (wave 4, status `preview`): crate **`rueca-lang-cpp`** (named
after the C++ grammar; one `CppPack` serves `Language::C` and
`Language::Cpp`), `tree-sitter-c` 0.24.2 and `tree-sitter-cpp` 0.23.4,
local scopes from vendored nvim-treesitter `c` / `cpp` `locals.scm`
(Apache-2.0, `NOTICE`). Everything in the table above ships, with these
choices and deviations (details in the crate docs and
[how-it-works](../how-it-works.md)):

- **Qualified names, declarations, overloads, templates, macros** as
  designed. Out-of-line bodies are top-level `Method`s (a `Function` when
  the qualifier is a namespace the file declares) and are not exported, so
  an include never binds a method body by its bare name. Overload suffixes
  are computed per file (open question 4 below). Anonymous namespaces add
  nothing to qualified names and make their names file-local. Tests:
  GoogleTest `TEST` / `TEST_F` / `TEST_P` / `TYPED_TEST(_P)` (`Suite::Name`)
  and Catch2 / doctest `TEST_CASE` / `TEST_CASE_METHOD` / `SCENARIO`.
- **Receivers**: `$type:T` from parameters, locals (`T x(…)`, `auto x =
  new T`, `std::make_unique<T>()`), fields of classes in the same file and
  globals; `Q::f()` gives `$type:Q` when `Q` is a type and no receiver when
  it is a namespace (namespaces are not nodes, so the name resolves through
  includes, the same file and unique names).
- **Includes** follow the module model above; the unique path-suffix match
  produces an ordinary `IMPORTS` edge at 1.0 (the generic resolver fixes
  `IMPORTS` confidence; a 0.6 edge would need a resolver change).
- **Macros**: no preprocessor, as designed, plus a lexical masking pass
  before parsing (`mask.rs`): macro tokens that expand to declaration
  syntax (`FMT_API`, `FMT_BEGIN_NAMESPACE`, `FMT_ENABLE_IF(…)` in a
  template head, `extern "C" {` split by `#ifdef __cplusplus`, a function
  header written once per `#if` branch) are blanked when their position
  allows no other reading, and `#if` conditions are replaced by `1`.
  Offsets are preserved and all text is read from the original source.
  Masking cut the files with parse errors from 34 / 45 / 53% to
  17 / 35 / 40% on hiredis / jq / fmt.
- **`OVERRIDES`** joins the in-class declarations (where the override is
  written); the resolver does not move them to bodies.
- **Gate (§3.1)**: items 1 (extraction; but see the parse-error rate), 2,
  3 and 6 ship; items 4 and 5 (bench entry, parity row vs cbm) are
  pending, so the status is `preview`. The parse-error rate is far above
  the 1% target and is reported honestly: files with at least one parse
  error after masking were 9 / 53 on hiredis (17.0%), 18 / 51 on jq
  (35.3%) and 30 / 74 on fmt (40.5%, a third of them vendored GoogleTest
  and test files), mostly one to a few local errors per file. The residue
  is grammar limits without a preprocessor (`va_arg(ap, char*)`,
  `unsigned(x)` casts, statement macros with statement arguments, `if` /
  `else` split across `#if` branches, C++26 annotations).

### 10.4 Java

| Aspect | Design |
|---|---|
| Grammar | `tree-sitter-java` 0.23.5, MIT |
| Module model | package = directory under a source root: Maven (`src/main/java`, `src/test/java`, `<sourceDirectory>`), Gradle (`src/*/java`, literal `srcDirs`), Android (`*/src/main/java`), else the directory whose path suffix matches the `package` clause. Package scope as in §10.1 |
| Imports | single-type (`import a.b.C`) binds `C`; on-demand (`a.b.*`) binds the package (folder import); `import static` binds members; same-package names visible without import; `java.lang.*` names are builtins |
| Qualified names | `com.example.Owner.getPets`, separator `.`; nested `Outer.Inner`; constructors named after the class (the constructor rung matches a member named like the class); overload suffixes per §10.1 |
| `this` / `super` | `this` → self; `super.m()` → base lookup; implicit members via `implicit_self` |
| Interfaces | explicit `implements` / `extends` → `IMPLEMENTS` / `INHERITS`; `OVERRIDES` from `@Override` and name+arity matches |
| Typed receivers | fields, parameters and locals are declared with types (`var` excepted) → `$type:`; this resolves most `repo.save()` calls to the interface method, then `OVERRIDES` reaches implementations |
| Heuristic | dependency injection, reflection, Lombok-generated members, lambdas and method references, anonymous classes (attached to the enclosing method) |
| Precision | scip-java 0.13.2 (Apache-2.0), full compile |
| Bench | S1: SWE-PolyBench Java 165 (MIT) and SWE-bench Multilingual Java 43 (lombok 17, gson 9, lucene 9, druid 5, …). S3: scip-java gold on gson and spring-petclinic |

*Implemented* (`rueca-lang-java`, status `preview`: gate items 1–3 and 6
met; the bench entry and parity row, items 4–5, are pending). As designed,
with these choices (crate docs list them all):

- **Package from facts.** The `package` clause is recorded as the file's
  first import with no names, and `module_name_for` reads it; the default
  package is the empty module. Without facts the path under a source root
  is used.
- **Source roots** are `…/src/<set>/java` at any depth plus
  `<sourceDirectory>` / `<testSourceDirectory>` of every `pom.xml` and
  literal `srcDir(s)` of every `build.gradle(.kts)`; `<modules>` and
  `settings.gradle` `include`s only break ties. Imports that miss every
  root fall back to the package-path suffix match. Single-type and static
  imports resolve to the class file, on-demand imports to the package
  directory.
- **Synthetic imports**: a fully qualified type in code (`com.x.Owner`)
  and every bare name under a static on-demand import (`import static
  a.b.C.*`) are recorded as single imports, so they resolve like the
  explicit forms (implicit `this` still wins).
- **Overload suffixes** are `(String,int)`: simple type names, generics
  removed, arrays and varargs kept, no spaces; open question 4 is left as
  stated (a second overload renames the first).
- **Constructors and implicit `this`.** With constructors as members named
  like the class, a bare type name that is an enclosing class or its
  superclass reaches the constructor through the implicit-self rung. The
  pack works around it (fully qualified `$type:` for types of the file,
  `super` for the direct superclass, no self-references); a resolver rule
  that skips constructors in type lookups would remove the workaround.
  Likewise a field and a method with one name share one member slot, so
  fields are emitted after methods.
- **Bridge**: `System.getenv`, Spring `@Value` placeholders and
  `Environment.getProperty` with file-local evidence; relaxed binding off
  (open question 8).
- Measured on gson and jackson-core (crate docs): 0 parse errors, 45.9% /
  66.6% of references resolved, 12.5% / 3.1% unresolved (mostly calls on
  call results), ~0.4 s / ~0.6 s to index.

### 10.5 Risks

| Risk | Impact | Mitigation |
|---|---|---|
| C/C++ macros and conditional compilation | missing or wrong definitions, parse errors | report parse-error rate per file; `compile_commands.json` include paths; scip-clang precision layer |
| C++ overloads, templates, ADL | wrong or fanned-out call edges | arity choice, × 0.8 confidence on fan-out, scip-clang |
| Header/body duplication | two nodes per function | `is_declaration` + body preference |
| Go implicit interfaces | missing callers through interfaces | heuristic `IMPLEMENTS` + `OVERRIDES` expansion, labelled as heuristic |
| Go package name ≠ last path element | unbound imports | read the target package clause; heuristic fallback |
| Java non-standard layouts | wrong package names | build-file source roots, `package`-clause suffix match |
| Small C++ localization set (12) | noisy parity numbers | supplement with Multi-SWE-bench; report sample sizes |
| Key flood from data files | localization regressions | §4.5 caps, `Key` out of default search labels, S1 regression check |

## 11. Tool impact

| Tool | Data files | New languages |
|---|---|---|
| `search_symbols` | `Key` and `Document` are **excluded from the default labels** unless `labels` names them or `language` is a data language; `EnvVar`, `Resource`, `Image`, `Route` are included. Key paths tokenize on `.`, `/`, `-`, `_` like code | unchanged; `language` accepts `go`, `c`, `cpp`, `java` |
| `search_code` | unchanged; `language` accepts the data languages; matches are placed in the innermost `Key` | unchanged |
| `get_symbol` | `parent` / `children` follow key nesting; `bases` shows YAML merges and compose `extends`; `EnvVar` / `Key` add `readers_count`, `writers_count` | overload candidates listed |
| `get_snippet` | key span = key + value lines | unchanged |
| `trace` | `READS` / `WRITES` for the bridge, `REFERENCES` for `$ref` and aliases, `IMPORTS` from data files (`File` → `File` / `Folder`) | `OVERRIDES` becomes useful in `edge_types` |
| `get_file_outline` | nested key tree, kind nodes on top (k8s `Resource`s, compose services, workflow jobs), bounded by the §4.5 caps | unchanged |

`docs/tools.md` changes, made with the code that needs them: `language`
enum; qualified-name conventions (data key paths, Go, C/C++, Java);
default-label rule; the `$env` pseudo-path in symbol ids; the two new
`get_symbol` counts; `READS` / `WRITES` in the `trace` examples.
`docs/how-it-works.md` gains the data-model section, the bridge, and the new
rungs.

## 12. Phasing and estimates

Estimates are focused engineer-weeks for one contributor (or agent stream),
including tests and docs, excluding review.

| Wave | Content | Depends on | Estimate |
|---|---|---|---|
| 0 | PHP, Markdown (in flight) | — | — |
| 1 | **JSON, YAML**: model additions (§9.3 except `is_declaration`), `rueca-lang-data` core (tree, key paths, emit, budgets), JSON/JSONC + YAML front ends, kinds `package.json`, tsconfig, `composer.json`, `search_symbols` default-label rule, key-recall test — *done* | 0 (model) | 2–3 |
| 2a | **Other config formats**: TOML, `.env`, INI, `.properties`, HCL (definitions), Dockerfile, XML kinds; `pyproject` — *done* | 1 | 2 |
| 2b | **Bridge and infra kinds**: `EnvVar` / `ConfigKey` facts in Python, TS/JS and PHP packs; canonical env vars, `READS` / `WRITES`, `Strategy::Contract`; compose, k8s, GitHub Actions, OpenAPI, JSON Schema, Spring/.NET config scopes — *done*; S2 bridge labels pending | 1 | 3 |
| R | **Shared resolver work** (§10.1) — *done*, with the §9.2 contract and §9.3 model | 0 | 1–2 |
| 3 | **Go** (+ `go.mod` kind, Terraform references) | R | 2–3 |
| 4 | **C++ then C** (one pack; C++ tested first, per the maintainer's order) — *implemented as `preview`* (§10.3) | R (implicit self, overloads), `is_declaration` | 3–4 |
| 5 | **Java** (+ `pom.xml` / Gradle roots) | R | 2–3 |

Parallelism: 2a and 2b run in parallel after wave 1; R runs in parallel
with waves 1–2 (it touches only the resolver); after R, Go, C/C++ and Java
are independent and can run in parallel (Java and C++ share `implicit_self`
and overloads from R). Each code wave adds its language's bridge facts
(§7.2) as part of extraction. With three parallel streams the whole plan is
roughly 7–9 calendar weeks; serial, about 15–20 engineer-weeks.

**Release point (decided 2026-10-24).** The first public release ships
after **every** wave of this RFC is `supported`, Java included: Python,
TS/JS, PHP, Markdown, every data format with the bridge, Go, C++, C and
Java. No language ships as `preview` in the first release. (The draft
recommended releasing after Go with C/C++/Java as `preview`, to get
feedback earlier and contain the C/C++ risk of §10.5; the maintainer chose
one complete release instead.)

## 13. Amendments to RFC 0001 (applied on acceptance)

1. §6.2: `Key` and `Document` join the core labels; `Resource` and `Image`
   enter the enum.
2. §6.3: env and config relationships are `READS` / `WRITES` to `EnvVar` /
   `Key` (strategy `Contract`); `USES_ENV` and `CONFIGURES` are not added.
3. §9.2: JSON via `jsonc-parser`; OpenAPI semantics over the spanned data
   tree (not `oas3`); `.env`, INI, `.properties` and Dockerfile parsers are
   built.
4. §12: the release order of this RFC; C/C++ move ahead of tier 2.
5. §16: file-kind packs live in `rueca-lang-data`, not `rueca-packs`.
6. §11.4: the final tool list moves to a later RFC (already noted there).

## 14. Alternatives considered

- **Tree-sitter grammars for data formats.** One parsing stack for
  everything, but RFC 0002 §3.3 prefers pure Rust on untrusted input,
  the YAML grammar has an open memory-safety bug, and kinds need typed
  values.
- **Do not index data; rely on `search_code`.** Text search finds
  `DATABASE_URL` but cannot say which manifest sets it, which code reads it,
  or that two spellings are the same key. cbm already offers this; not
  having it is a parity loss.
- **Index every JSON/YAML file without caps.** Floods the symbol index
  (lockfiles, fixtures, data dumps) and hurts localization ranking.
- **Flat keys (one node per leaf, no nesting).** Simpler, but loses
  `get_file_outline` structure and the `parent` of a key, and makes merge
  keys and `$ref` targets on objects impossible.
- **One `Data` language variant.** Fewer enum entries, but the `language`
  filter could not distinguish YAML from TOML.
- **A separate crate per C and C++.** They share headers, include
  resolution and most of the grammar; one pack avoids a cross-pack
  dependency.

## 15. Open questions

1. ~~**Release point**~~ — decided 2026-10-24: after Java (§12).
2. ~~Should `module_name` receive the file's facts?~~ — decided 2026-10-24
   (wave R): yes, as an additive defaulted hook,
   `module_name_for(path, facts, project)`, which the resolver calls and
   which defaults to `module_name(path, project)`. Existing packs are
   unchanged; Java reads its `package` clause, Go its `package x_test`.
3. Declaration ↔ body link for C/C++: is "references prefer the body"
   enough, or is a dedicated edge (`IMPLEMENTS` reuse vs a new `DECLARES`)
   worth adding for `get_symbol`? — *Implemented as "references prefer the
   body"* (C/C++ pack, wave 4): every reference to a declaration with a
   unique body lands on the body, and the declaration and the body share
   one qualified name, so `get_symbol` returns one and lists the other as a
   candidate. No new edge was needed for the fixtures or the smoke
   repositories; a `DECLARES` edge stays open until an agent workflow
   needs the declaration from the body.
4. Overload suffixes change a method's qualified name when a second
   overload is added. Acceptable (the symbol changed meaning), or should the
   first overload keep its bare name? — *Decided in the C/C++ pack: all
   overloads get the suffix*, the first one included (§10.1 as written).
   Lookups by `qualified_name` treat the suffix as optional, so
   `geo::scale` keeps working after an overload is added; only symbol
   ids change, which is right because the symbol did. Suffixes are
   computed per file (a declaration and its body in one file share the
   suffix; overloads split across files are not suffixed there).
5. Dockerfile `COPY` and compose/CI build contexts: worth resolving
   statically (cbm does, for image-scoped env reads), and in which wave?
6. i18n catalogs and `t('key')` reads: same bridge with a `i18n` namespace?
   Large key sets make them a localization risk.
7. JSON5 and HJSON: support (no span-preserving pure-Rust parser was found)
   or leave as `text`?
8. Spring relaxed binding and identifier-to-key matching: ship the
   heuristic rung at 0.5 by default or behind an option?
9. Should `.h` language detection be per file (try C, fall back to C++ on
   errors) rather than per snapshot? — Decided per snapshot (wave R). In
   the smoke repositories a single C++ file (hiredis's Qt example, jq's
   fuzzers) makes every header C++; C headers parse as C++ without extra
   errors once `extern "C" {` blocks are masked, so this has not been a
   problem.

## 16. Sources

- crates.io API (versions, licenses, dependency requirements), 2026-10-24:
  https://crates.io/crates/jsonc-parser , https://crates.io/crates/saphyr-parser ,
  https://crates.io/crates/saphyr , https://crates.io/crates/toml_edit ,
  https://crates.io/crates/roxmltree , https://crates.io/crates/quick-xml ,
  https://crates.io/crates/hcl-edit , https://crates.io/crates/dockerfile-parser ,
  https://crates.io/crates/tree-sitter-dockerfile , https://crates.io/crates/dotenvy ,
  https://crates.io/crates/rust-ini , https://crates.io/crates/java-properties ,
  https://crates.io/crates/taplo , https://crates.io/crates/oas3 ,
  https://crates.io/crates/openapiv3 , https://crates.io/crates/tree-sitter-go ,
  https://crates.io/crates/tree-sitter-c , https://crates.io/crates/tree-sitter-cpp ,
  https://crates.io/crates/tree-sitter-java
- Crate sources read for span and limit behaviour: `saphyr-parser` 0.2.2
  (`scanner.rs` `Marker`, `parser.rs` `SpannedEventReceiver`), `saphyr`
  0.2.2 (`loader.rs` alias handling), `jsonc-parser` 0.34.0 (`common.rs`
  `Range`, `parser.rs` depth limit), `roxmltree` 0.21.2 (`ParsingOptions`,
  entity loop detector), `toml_edit` 0.25.17 (`parser/mod.rs`
  `RecursionGuard`), `hcl-edit` 0.9.8 (`repr.rs` `Span`)
- tree-sitter-yaml OOB write: https://github.com/tree-sitter-grammars/tree-sitter-yaml/issues/48
- codebase-memory-mcp v0.12.0 README (edge types `CONFIGURES`, IaC
  indexing, language tiers): https://github.com/DeusData/codebase-memory-mcp/blob/v0.12.0/README.md
- SWE-bench Multilingual (MIT, per-language task counts):
  https://www.swebench.com/multilingual.html ,
  https://huggingface.co/datasets/SWE-bench/SWE-bench_Multilingual
- SWE-PolyBench (MIT, Java 165): https://huggingface.co/datasets/AmazonScience/SWE-PolyBench
- Multi-SWE-bench (CC0, 7 languages, 1,632 instances):
  https://huggingface.co/datasets/ByteDance-Seed/Multi-SWE-bench
- SCIP indexers (Apache-2.0): https://github.com/sourcegraph/scip-go (0.2.9),
  https://github.com/sourcegraph/scip-java (0.13.2),
  https://github.com/sourcegraph/scip-clang (0.4.1)
- Dockerfile reference: https://docs.docker.com/reference/dockerfile/
- JSON Pointer (`$ref` decoding): https://www.rfc-editor.org/rfc/rfc6901
- Go modules reference: https://go.dev/ref/mod
- Spring externalized configuration: https://docs.spring.io/spring-boot/reference/features/external-config.html
