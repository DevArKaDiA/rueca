# RFC 0003 — Structured data and the next languages

- Status: Draft
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
| 1 | **JSON, YAML**: model additions (§9.3 except `is_declaration`), `rueca-lang-data` core (tree, key paths, emit, budgets), JSON/JSONC + YAML front ends, kinds `package.json`, tsconfig, `composer.json`, `search_symbols` default-label rule, key-recall test | 0 (model) | 2–3 |
| 2a | **Other config formats**: TOML, `.env`, INI, `.properties`, HCL (definitions), Dockerfile, XML kinds; `pyproject` | 1 | 2 |
| 2b | **Bridge and infra kinds**: `EnvVar` / `ConfigKey` facts in Python, TS/JS and PHP packs; canonical env vars, `READS` / `WRITES`, `Strategy::Contract`; compose, k8s, GitHub Actions, OpenAPI, JSON Schema, Spring/.NET config scopes; S2 bridge labels | 1 | 3 |
| R | **Shared resolver work** (§10.1) | 0 | 1–2 |
| 3 | **Go** (+ `go.mod` kind, Terraform references) | R | 2–3 |
| 4 | **C++ then C** (one pack; C++ tested first, per the maintainer's order) | R (implicit self, overloads), `is_declaration` | 3–4 |
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
2. Should `module_name` receive the file's facts (`module_name(path, facts,
   project)`)? It would let Java read the `package` clause, Go the package
   name and PHP the namespace directly, instead of inferring from paths.
3. Declaration ↔ body link for C/C++: is "references prefer the body"
   enough, or is a dedicated edge (`IMPLEMENTS` reuse vs a new `DECLARES`)
   worth adding for `get_symbol`?
4. Overload suffixes change a method's qualified name when a second
   overload is added. Acceptable (the symbol changed meaning), or should the
   first overload keep its bare name?
5. Dockerfile `COPY` and compose/CI build contexts: worth resolving
   statically (cbm does, for image-scoped env reads), and in which wave?
6. i18n catalogs and `t('key')` reads: same bridge with a `i18n` namespace?
   Large key sets make them a localization risk.
7. JSON5 and HJSON: support (no span-preserving pure-Rust parser was found)
   or leave as `text`?
8. Spring relaxed binding and identifier-to-key matching: ship the
   heuristic rung at 0.5 by default or behind an option?
9. Should `.h` language detection be per file (try C, fall back to C++ on
   errors) rather than per snapshot?

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
