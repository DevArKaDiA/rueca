# rueca-bench

Benchmark harness for Rueca (RFC 0001 §15, RFC 0002 §8). It compares, on the
same inputs and with the same scoring:

| Arm | What it is | How it is driven |
|---|---|---|
| `grep` | No graph. BM25 (k1=1.2, b=0.75) over file contents, function bodies and function names (tree-sitter); textual `name(` heuristic for callers/callees | in-process |
| `cbm` | codebase-memory-mcp **v0.12.0**, official release binary pinned by sha256 in `manifest.toml` (never a binary on `PATH`) | `codebase-memory-mcp cli --quiet <tool> '<json>'`, with `CBM_CACHE_DIR` / `CBM_RUNTIME_DIR` inside `bench/.cache`; one warm `daemon start` per bench process (as in an MCP session; stopped at exit), UI disabled |
| `rueca` | Rueca at HEAD | `$RUECA_BIN index <root> --store <dir> --json`, `$RUECA_BIN query <tool> '<json>' --root <root> --store <dir>` (alpha contract, `docs/tools.md`) |

The repository ships **manifests and code only** (plus Rueca's own labels:
the PHP micro-benchmark in `micro/php/`). Datasets, repositories and tool
binaries are fetched at runtime at pinned revisions into `bench/.cache`
(git-ignored), so nothing with an unclear licence is vendored.

Results are reported **per language family** (python, ts/js, php, …; the
manifest `[languages]` table) plus an `overall` column, and a per-language
parity table against cbm. A language counts as supported only with a bench
entry and a published parity row (RFC 0001 §12, §15).

## Running

```sh
# from the repository root
uv run --project bench rueca-bench setup                 # tools, datasets, clones, gold labels
uv run --project bench rueca-bench run --arm all --suite all
uv run --project bench rueca-bench report                # bench/results/*.json + docs/bench/alpha-results.md
```

- `setup [--lang FAMILY ...]` and `run ... [--lang FAMILY ...]` restrict work
  to language families (manifest `[languages]` keys: `python`, `ecma`,
  `php`); `report` always scores everything cached.
- `run --arm {grep,cbm,rueca,all|a,b} --suite {s1,s3,all} [--only ID|REPO ...] [--workers N] [--force]`.
  Outputs are cached per (arm, instance) in `.cache/runs/`; re-running only
  computes what is missing (`--force` recomputes). Arm failures are recorded
  (and scored as misses), never dropped.
- `setup --select` regenerates the `[[s1.instances]]` list at the end of
  `manifest.toml` from the selection rules (deterministic).
- `clean --work` removes worktrees; `clean --runs rueca` drops cached Rueca outputs.
- Per-call timeouts: 120 s per query, 1800 s per index (`arms/__init__.py`).
- S1 runs repositories in parallel (`--workers`, default 2), instances of one
  repository sequentially (one partial clone per repository, one detached
  worktree per instance, removed afterwards).

### Rueca arm

```sh
export PATH="$HOME/.cargo/bin:$PATH"
cargo build --release -p rueca
RUECA_BIN=$PWD/target/release/rueca uv run --project bench rueca-bench run --arm rueca --suite all
uv run --project bench rueca-bench report
```

Without `RUECA_BIN` the Rueca column is reported as *pending*; a language
whose Rueca outputs are missing shows *pending* in its own column and is left
out of `overall` (which pools only languages every arm has results for), so
the overall numbers stay comparable.

### Sharing a cache between checkouts

`RUECA_BENCH_CACHE` moves the whole cache. `RUECA_BENCH_SHARED_CACHE` points at
*another* bench cache from which pinned, sha256-verified assets (the cbm
binary, the PHPStan PHAR, datasets) are read instead of downloaded again;
only files whose sha256 matches the manifest are used and nothing is written
there. Example (a second worktree reusing the main checkout's downloads while
keeping its clones, gold and runs separate):

```sh
export RUECA_BENCH_SHARED_CACHE=/path/to/rueca/bench/.cache
uv run --project bench rueca-bench setup --lang php
uv run --project bench rueca-bench run --arm grep,cbm --suite all --lang php
```

cbm's daemon socket lives in `CBM_RUNTIME_DIR`; when the cache path is too
deep for a unix socket (macOS `sun_path` limit) it falls back to
`/tmp/rueca-bench-cbm-<hash of the cache path>`.

### Requirements

`uv`, `git` ≥ 2.40 (partial clone), `node`/`npx` (SCIP indexers, S3 setup
only), Docker (PHP S3 gold only: PHPStan runs in the pinned `php:8.3-cli`
image; nothing PHP is installed on the host — without Docker the PHP
repository gold is reported as missing and the micro-benchmark is still
built), network for `setup`. `git-lfs` is not needed: worktrees are created
with the LFS filter disabled (pointers stay pointers).

**macOS note.** If `uv run` fails with `ModuleNotFoundError: rueca_bench`,
macOS has marked files in `bench/.venv` hidden and Python ≥ 3.12 skips hidden
`.pth` files (editable install). Use a regular install instead:
`export UV_NO_EDITABLE=1` (the `cache-keys` in `pyproject.toml` make uv
reinstall when sources change), or `chflags -R nohidden bench/.venv`.

Disk: about 1 GB for the alpha corpus (partial clones ≈ 100 MB, one worktree
at a time per repository, datasets ≈ 120 MB, cbm binary 335 MB). PHP adds
≈ 0.6 GB: partial clones ≈ 100 MB, SWE-rebench V2 parquet 429 MB (read with a
repository filter), PHPStan PHAR 31 MB, plus the `php:8.3-cli` image
(≈ 530 MB, in Docker's storage).

## Adding a language

Adding a language is data plus, at most, one labeler entry:

1. **Language family** — `[languages.<family>]` in `manifest.toml`: report
   `label`, member instance languages, source `extensions`,
   `exclude_suffixes`, `test_files` patterns (pooled into the shared
   test-path heuristic), and `s1_sources` / `s3_gold` / `notes` shown in the
   report. Report columns, the per-language summary and the parity row
   appear automatically.
2. **Grammar** — `[grammars.<name>]`: the Python binding
   (`"<package>:<function>"`, pinned in `pyproject.toml`, one import in
   `treesitter.py: _BINDINGS`), its extensions, and for the data-driven
   labeler the callable node types (→ `Function` / `Method`), container node
   types and the qualified-name separator. Grammars whose function shapes
   need code (JS/TS) name a code-backed `labeler`. The labeler gives S1
   function gold, the grep arm's function documents and S3 callables.
3. **S1** — `[[s1.repos]]` entries (repository, `language`, `quota`,
   `sources` = dataset keys) and, if needed, a `[datasets.<key>]` pin
   (HF revision, file, sha256, licence, optional `row_filter` /
   `filter_repos`). `rueca-bench setup --select` appends the instances
   deterministically; existing instances do not move.
4. **S3** — `[[s3.repos]]` with `gold = "scip"` (an `indexer` from `[tools]`)
   or another gold method (`phpstan` for PHP); and/or a `[[s3.micro]]`
   micro-benchmark (`format = "pycg"`, or `format = "rueca"` with snippets
   under `bench/micro/<lang>/`).
5. `setup --lang <family>`, `run --arm grep,cbm --lang <family>`, `report`;
   document sources, licences and biases here.

A new S3 gold method (another external tool) is the only part that needs new
harness code; it must return the same graph shape as `scip.derive_call_graph`
(callables, callers, callees, stats).

## Suites

### S1 — Localization (deterministic, no LLM)

Issue text → ranked files and functions. Method and metrics follow LocBench
(LocAgent): file Acc@1/3/5, function Acc@5/10, Recall@k, MRR, plus index time
and query latency.

**Instances.** Defined in `manifest.toml` (`[[s1.repos]]` quotas,
`[[s1.instances]]` generated list). Python: SWE-bench Verified ∪ Lite on
requests, flask, pylint, pytest, sphinx (40). TS/JS: SWE-PolyBench
(tailwindcss, code-server) and SWE-bench Multilingual (vuejs/core, preact,
axios) (41). PHP (40): SWE-bench Multilingual — the only human-validated PHP
localization source — on all four of its PHP repositories (carbon 8,
php-cs-fixer 8, laravel/framework 6, phpspreadsheet 6), plus SWE-rebench V2 on
three small libraries for repository diversity (Twig, monolog, commonmark,
4 each), restricted to rows its own LLM screen rated well specified
(`meta.llm_metadata.code = "A"`, manifest `row_filter`). Multi-SWE-bench,
SWE-PolyBench and ContextBench have no PHP; no assembled PHP function labels
exist, so PHP function gold comes from the tree-sitter labeler. An instance is eligible if its gold patch modifies at least one
existing, non-test source file of the language. Inside each repository quota,
instances with more assembled gold (below) come first, then
`sha256(instance_id)` ascending.

**Queries.** Same strings for every arm (`queries.py`):

- `kw` — up to 12 tokens: identifiers from inline/fenced code first, then
  CamelCase / camelCase / snake_case tokens from the prose, then other
  non-stopword words (≥ 3 chars), each class ordered by frequency then first
  occurrence.
- `raw` — the issue text, whitespace-collapsed, truncated to 4000 chars.

Graph arms answer with their symbol search (`cbm`: `search_graph` BM25
`query`, `rueca`: `search_symbols`), 100 results; the ranked file list is the
order of first appearance of each file among the symbol hits, the ranked
function list is the callable hits. `grep` ranks files by BM25 over file
contents and functions by BM25 over function bodies (name-only BM25 reported
as a secondary number).

**Gold** (assembled first, RFC 0002 §9):

| Granularity | Source | Notes |
|---|---|---|
| Files | gold patch | modified/deleted files existing at the base commit, source extension of the instance language, not a test path. New files are excluded (cannot be localized). |
| Functions (27 Python instances) | **LocAgent** `edit_functions` (`czlll/SWE-bench_Lite`) | `path:Class.func` labels, mapped to the tree-sitter definition at the base commit to obtain def lines |
| Functions (20 TS instances, 13 non-empty) | **SWE-PolyBench** `modified_nodes` | CST node paths (`path->program->kind:name…`); class/interface-only nodes skipped |
| Functions (34 other Python/TS instances, all 40 PHP) | Rueca tree-sitter labeler (`gold.py`) | every removed/modified old-side line → innermost enclosing function/method; a pure insertion after old line k → innermost function containing k and k+1 (Python: also the function containing k when the inserted line is indented deeper than its `def`). PHP methods are qualified `Class::method` (classes, interfaces, traits, enums); PHP functions by bare name |
| Context files (22 instances) | **ContextBench** `gold_context` | human-annotated context spans; scored as context-file recall@5/@10 of the ranked files |

Where both an assembled label and the tree-sitter labeler exist, their
agreement (Jaccard over `(path, qualified name)`) is reported; it validates
the labeler used for the remaining instances. Instances with empty function
gold (change outside any function) are excluded from function metrics only.

**Matching.** A file hit matches by repository-relative path. A function hit
matches a gold function when path and bare name are equal and the hit's start
line is within ±2 lines of the gold definition's name line, declaration start
or first decorator line (`arms/base.py: same_def`). Every arm's output is
post-filtered identically: source extension of the instance language and not
a test path (`common.py: is_test_path`).

### S3 — Structural questions

**Gold from SCIP** (RFC 0002 §4.2) at pinned tags: requests v2.34.2, flask
3.1.3 (`scip-python` 0.6.8); hono v4.13.13 (`scip-typescript` 0.5.2); express
v5.3.0 (`scip-typescript --infer-tsconfig`). SCIP gives references, not
calls, so (`scip.py`):

1. callables = tree-sitter functions/methods in scope (`include` prefixes, no
   tests); a SCIP definition whose (line, name) equals a callable's name
   identifier binds the SCIP symbol to it (its `enclosing_range`, when present,
   replaces the tree-sitter extent);
2. each non-definition occurrence of a bound symbol is kept only if
   tree-sitter says it is a call site (callee of a call / `new`, directly or as
   the property of a member access);
3. the caller is the innermost callable containing the occurrence
   (module-level calls produce no edge).

The SCIP reader decodes the protobuf wire format directly and reads **both**
the deprecated packed `range` / `enclosing_range` fields and the typed
`single_line_*` / `multi_line_*` oneofs.

**Gold from PHPStan (PHP).** PHP has no usable SCIP indexer (scip-php: no
enclosing ranges, no relationships; RFC 0002 §4.2), so the bench ships a
small PHPStan extension (`bench/phpstan/`, Apache-2.0): a collector that
records, for every call expression, the target PHPStan's type inference
resolves, and a rule that dumps them as JSON. PHPStan 2.3.2 (release PHAR,
sha256-pinned) runs in the official `php:8.3-cli` image (pinned by digest,
`--network=none`, read-only mounts) at pinned tags: monolog 3.9.0
(`src/Monolog/`), commonmark 2.8.3 (`src/`). Resolved: `$this->m()`,
`$obj->m()` / `$obj?->m()` on any inferred receiver type (typed parameters
and properties, promoted constructor properties, return types, `new`,
phpdoc `@var` / `@param` / `@return`), `self::` / `static::` / `parent::` /
`Class::m()`, `new C` → `C::__construct` (inherited constructors included),
named functions (namespace fallback included), `$obj()` → `__invoke`, constant
string callables. Edges are derived like the SCIP gold (`phpstan.py`): the
callee is the tree-sitter callable at the reflected file/line and name, the
caller the innermost tree-sitter callable containing the call line, both in
scope. No `composer install` is run (no adopter code or dependency is
executed): edges into vendor code are lost (out of scope anyway), and so are
receiver types that only vendor return types would give.

**PHP micro-benchmark** (`micro/php/`, Rueca's own labels, Apache-2.0): 60
hand-written snippets (162 functions, 95 call edges) in 12 categories —
functions, namespaces (`use function`, fully qualified and relative names,
global fallback, group use, aliases), methods (`$this`, typed parameters /
properties / promoted properties, return-type chains, fluent builders,
nullsafe, phpdoc `@var`, same-name methods on other classes, untyped
receivers), static calls and late static binding, constructors, inheritance,
interfaces, traits (`insteadof` / `as` aliases, trait → host calls),
closures, enums, and dynamic calls. Each snippet directory holds the sources
and `callgraph.json`, `{"path:Qualified": ["path:Qualified", ...]}`, with
every callable as a key (functions without calls map to `[]`, so spurious
edges count against micro precision). Answer-key conventions: an edge exists
for every call whose target is statically determinable from declared types
(including phpdoc) and literals in the snippet; it points to the declaring
method found by walking up classes and traits; interface- and
abstract-typed receivers point to the interface / abstract declaration (no
expansion to implementations); `static::` resolves like `self::`; calls in
closures and arrow functions belong to the enclosing named function;
builtins and top-level code produce no edge. Setup cross-checks the answer
keys against the PHPStan extractor and the report prints the agreement
(91 of 95 edges, no extra PHPStan edges; the 4 differences are
`call_user_func` callables, a conditionally declared function and a trait
`as` alias, which PHPStan does not resolve and the key does).

**Gold from PyCG** (Apache-2.0): the hand-written `callgraph.json` of every
snippet of the PyCG micro-benchmark, restricted to function → function edges
(module nodes, builtins and external names dropped). Each snippet becomes a
tiny git repository with the answer key removed; every function is a question.

**Questions.** ~50 functions per SCIP / PHPStan repository, stratified by (fan-in,
fan-out) bucket with a fixed seed, skipping dunders, functions shorter than 3
lines and functions without edges. For each: resolve the definition
(path + name + def line ±2), then depth-1 callers and callees, plus a symbol
lookup by bare name (is the definition in the top 1 / top 10 of the arm's
symbol search?). Predictions (neighbours and lookup hits) are restricted to
the same scope as the gold before scoring: in-scope, non-test files and a
callable label (Function / Method / Test / Constructor), so module, file,
class or variable pseudo-neighbours are neither rewarded nor penalised.
`typing.overload` stubs are not callables; scip-python emits the definition
only on the first stub, which is aliased to the implementation. Metrics: definition-found rate; per-direction precision / recall / F1
macro-averaged over questions with non-empty gold for that direction; micro
precision / recall pooled over all edges; lookup Acc@1/@10; query latency.

## Datasets, tools and licences

| Asset | Pinned | Licence | Use |
|---|---|---|---|
| SWE-bench Verified (`princeton-nlp/SWE-bench_Verified`) | HF revision + sha256 | none stated on the card (SWE-bench code: MIT) | S1 issues, patches |
| SWE-bench Lite (`SWE-bench/SWE-bench_Lite`) | HF revision + sha256 | none stated on the card | S1 issues, patches |
| SWE-PolyBench (`AmazonScience/SWE-PolyBench`) | HF revision + sha256 | MIT | S1 TS issues, patches, `modified_nodes` gold |
| SWE-bench Multilingual (`SWE-bench/SWE-bench_Multilingual`) | HF revision + sha256 | MIT | S1 TS/JS issues, patches |
| LocAgent / Loc-Bench labels (`czlll/SWE-bench_Lite`) | HF revision + sha256 | none stated on the card (LocAgent code: Apache-2.0) | S1 function gold |
| ContextBench (`Contextbench/ContextBench`) | HF revision + sha256 | none stated on the card (ContextBench code: Apache-2.0) | S1 context-file gold |
| SWE-rebench V2 (`nebius/SWE-rebench-V2`) | HF revision + sha256 | CC-BY-4.0 (attribution: Nebius, SWE-rebench V2) | S1 PHP issues, patches (Twig, monolog, commonmark) |
| PyCG micro-benchmark (`vitsalis/PyCG`) | commit | Apache-2.0 | S3 Python call-graph gold |
| PHP micro-benchmark (`bench/micro/php`) | in repository | Apache-2.0 (Rueca's own labels) | S3 PHP call-graph gold |
| PHPStan (`phpstan/phpstan`) release PHAR | version + sha256 | MIT | S3 PHP gold (run in Docker, never vendored) |
| `php:8.3-cli` Docker official image | digest | PHP-3.01 (separate process) | runtime for PHPStan |
| tree-sitter-php (PyPI) | `uv.lock` | MIT | bench labeler for PHP |
| scip-python / scip-typescript | npm version | MIT / Apache-2.0 | S3 gold |
| codebase-memory-mcp v0.12.0 | release URL + archive and binary sha256 (darwin-arm64, linux-amd64) | MIT | `cbm` arm |
| Corpus repositories | commit SHAs | their own (requests Apache-2.0, flask BSD-3, pytest MIT, pylint GPL-2.0, sphinx BSD-2, tailwindcss MIT, code-server MIT, vue MIT, preact MIT, axios MIT, hono MIT, express MIT, carbon MIT, php-cs-fixer MIT, laravel/framework MIT, phpspreadsheet MIT, Twig BSD-3, monolog MIT, commonmark BSD-3) | cloned at runtime; nothing vendored |

Citations: Jimenez et al., *SWE-bench* (ICLR 2024); OpenAI, *SWE-bench
Verified* (2024); Rashid et al., *SWE-PolyBench* (2025); Chen et al.,
*LocAgent* (ACL 2025); EuniAI, *ContextBench* (2026); Salis et al., *PyCG*
(ICSE 2021); Badertdinov et al., *SWE-rebench* (Nebius, 2025) and SWE-rebench
V2 (CC-BY-4.0); SWE-bench team, *SWE-bench Multilingual* (2025); Ondřej
Mirtes et al., *PHPStan*.

Only identifiers (instance ids, commits, revisions) and Rueca's own code live
in this repository. Gold labels and arm outputs stay in `bench/.cache`;
`bench/results/*.json` contains scores and per-instance metrics, which include
file paths and function names of the public corpus repositories.

## Known biases and limitations

- **Contamination.** All corpus repositories and issues are public and old;
  irrelevant for the deterministic arms, relevant for the future agent suite.
- **Process-per-call latency.** `cbm` and `rueca` are driven one CLI process
  per query (`cbm` talks to its warm daemon; `rueca` opens its segment on every
  call); `grep` runs in process, so its query latency is ~0. Index times were
  measured with up to 3 repositories processed in parallel.
- **grep shares the gold's extractor.** The `grep` arm and the gold callables
  both come from the bench's tree-sitter extractor, so `grep` resolves 100%
  of definitions by construction and its textual `name(` heuristic is close
  to how SCIP call sites are classified; its high S3 recall is partly an
  artefact of that. Its S3 precision is the informative number.
- **Index time** includes everything each tool does by default (`cbm` runs
  `index_repository` mode `full`, i.e. with semantic passes, which a
  localization query does not use).
- **Symbol search only.** Graph arms localize with their symbol search, not
  with their text search or multi-step tool use; this measures the first tool
  an agent would call, not an agent.
- **SCIP gold is a precision oracle, not complete.** Dynamic dispatch on
  untyped JavaScript (`this.handle()`, `res.send` in express) is mostly
  invisible to scip-typescript, so express has few gold edges (questions are
  restricted to functions with at least one edge). Calls through callbacks,
  decorators and `getattr` are missed everywhere. Recall numbers are relative
  to this oracle.
- **PyCG** contains mostly module-level calls; restricted to function →
  function edges it is small (see the gold table in the report).
- **Label agreement.** Assembled labels and the tree-sitter labeler disagree
  on some instances (mostly multi-function patches and code outside
  functions); the report shows the mean Jaccard.
- **Test heuristic.** One path heuristic decides "test file" for gold and
  arms alike (`common.py`); tests are excluded from gold and predictions.
- **PHP S3 gold is type-driven.** The PHPStan gold resolves what declared
  and phpdoc types say, which tree-sitter tools cannot see (phpdoc `@var`,
  return types of other classes, promoted properties); it misses
  `call_user_func` / `array_map` callables, `__call` magic, variable method
  names and everything that depends on vendor types (no dependencies are
  installed). Interface-typed receivers resolve to the interface method only,
  so a tool that expands dynamic dispatch to implementations is penalised in
  precision. Recall numbers are relative to this oracle.
- **PHP micro-benchmark** is written by the bench authors to cover PHP
  name-resolution features one at a time; it is not a sample of real code,
  and its answer-key conventions (above) are choices. The PHPStan
  cross-check guards against labelling mistakes, not against convention bias.
- **PHP S1 sources.** SWE-bench Multilingual has 43 PHP instances on 4
  repositories (laravel/framework and phpspreadsheet are medium-large);
  SWE-rebench V2 rows are mined automatically and only LLM-screened, and they
  come from its train split (irrelevant for the deterministic arms). PHP
  function gold has no assembled labels to validate the tree-sitter labeler
  against; the labeler is validated on Python/TS instead (agreement above).
- **PHP test heuristic** adds `*Test.php` to the shared test-file patterns;
  `tests/` directories were already excluded.
- **ContextBench TS/JS rows** carry PR descriptions instead of issue text, so
  they are not used as S1 sources; only their Python (SWE-bench Verified) gold
  contexts are used, joined by instance id and base commit.

## Deviations from the RFCs

- RFC 0001 §15 lists NDCG; the alpha report gives Acc@k, Recall@k and MRR
  (LocBench's reported metrics). NDCG can be computed from
  `results/s1.json` ranks later.
- RFC 0002 §8 suggests pyrefly for Python call edges; the alpha S3 gold uses
  scip-python (as specified for this wave) plus PyCG.
- RFC 0001 §9.3 / RFC 0002 §4.2 plan a PHPStan *extension emitting Rueca
  facts* as Rueca's PHP precision layer. The bench's collector
  (`bench/phpstan/`) is a gold extractor only — it emits call sites and
  targets for scoring, not Rueca facts — but it is a working prototype of the
  collector half of that extension.
- The CGraph cross-service bench (suite 2), Harbor / Inspect (suite 4) and the
  issue-only control arm are not part of the alpha (S1 + S3 only).
- S1 uses SWE-bench Multilingual for vuejs/core, preact and axios next to
  SWE-PolyBench, because PolyBench's TS/JS repositories of small/medium size
  are few.
