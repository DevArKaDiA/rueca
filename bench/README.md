# rueca-bench

Benchmark harness for Rueca (RFC 0001 §15, RFC 0002 §8, **RFC 0004**). Rueca's
quality is defined by deterministic metrics with absolute **floors** (the bar
a language must meet to be `stable`) and aspirational **targets**; other
tools are measured on the same inputs and published as reference, never as
the gate. It compares, on the same inputs and with the same scoring:

| Arm | What it is | How it is driven |
|---|---|---|
| `grep` | No graph. BM25 (k1=1.2, b=0.75) over file contents, function bodies and function names (tree-sitter); textual `name(` heuristic for callers/callees | in-process |
| `cbm` | codebase-memory-mcp **v0.12.0**, official release binary pinned by sha256 in `manifest.toml` (never a binary on `PATH`) | `codebase-memory-mcp cli --quiet <tool> '<json>'`, with `CBM_CACHE_DIR` / `CBM_RUNTIME_DIR` inside `bench/.cache`; one warm `daemon start` per bench process (as in an MCP session; stopped at exit), UI disabled |
| `rueca` | Rueca at HEAD | `$RUECA_BIN index <root> --store <dir> --json`, `$RUECA_BIN query <tool> '<json>' --root <root> --store <dir> [--text]` (`docs/tools.md`; both the alpha contract and the RFC 0005 contract are understood, see *Rueca arm*) |

Suites, by name (the S-numbers stay as short identifiers):

| Suite | Question | Gated in the scorecard |
|---|---|---|
| **Localization** (S1) | "Given this issue, where must the code change?" | File Acc@5, Function Acc@10 (not significantly below BM25) |
| **Structure** (S3) | "Who calls this function, and what does it call?" | callers / callees precision and recall, definition found |
| **Micro-benchmarks** (part of S3) | the same on hand-labelled snippets, one language feature each | callee F1 |
| **Health** (S5) | "Does it parse, and is it fast?" | files with parse errors, query latency p95 |
| **Efficiency** | "How many tokens does a correct answer cost?" (RFC 0005 §8) | reported only |
| **Org scenarios** | "Does it still find the code in a store of many repositories, and of one repository at two commits?" (RFC 0005 §5–§6) | reported only |
| **Configuration** (RFC 0003 §3.3) | "Which keys exist, who reads this env var, who sets it?" | key recall, span accuracy, bridge precision / recall |

The repository ships **manifests and code only** (plus Rueca's own labels:
the micro-benchmarks in `micro/{python,ecma,php,go,java,c,cpp}/` and the
env-bridge labels in `labels/bridge/`). Datasets, repositories and tool
binaries are fetched at runtime at pinned revisions into `bench/.cache`
(git-ignored), so nothing with an unclear licence is vendored.

`rueca-bench report` writes `docs/bench/alpha-results.md`, which opens with
the **scorecard** (RFC 0004 §9): one table per language, *Metric | Rueca
(95% CI) | Floor | Target | Status | BM25 | cbm*, followed by the language's
status (`stable` / `preview`) and its failing floors, then a configuration
card, then the detailed tables of every suite. `bench/results/*.json` holds
every number (`scorecard.json` is what the CI regression check compares).

## Running

```sh
# from the repository root
uv run --project bench rueca-bench setup                 # tools, datasets, clones, gold labels
uv run --project bench rueca-bench run --arm all --suite all --workers 4
uv run --project bench rueca-bench report                # bench/results/*.json + docs/bench/alpha-results.md
uv run --project bench rueca-bench check                 # regression check vs the committed scorecard
```

- `setup [--lang FAMILY ...]` and `run ... [--lang FAMILY ...]` restrict work
  to language families (manifest `[languages]` keys: `python`, `ecma`,
  `php`, `go`, `java`, `c`, `cpp`) and/or `data` (the data-format suite);
  `report` always scores everything cached.
- `run --arm {grep,cbm,rueca,all|a,b} --suite {s1,s3,data,health,org,all|a,b} [--only ID|REPO|SET ...] [--workers N] [--force] [--prune]`.
  Outputs are cached per (arm, instance) in `.cache/runs/`; re-running only
  computes what is missing (`--force` recomputes). Arm failures are recorded
  (and scored as misses), never dropped. `--workers` runs S1 repositories
  and S3 sets in parallel; `--prune` deletes each repository clone once its
  runs are done (bounded disk; re-created from the pinned commits on next
  use). `health` runs the Rueca arm only; `org` (the multi-repository and
  multi-ref scenarios) runs the Rueca and cbm arms after S1 and is skipped
  with `--lang`.
- `check [--baseline FILE] [--current FILE]` — the regression check
  (below): compares `bench/results/scorecard.json` with the one committed
  at `HEAD`; exit status 1 when a floor metric dropped by more than its
  committed interval half-width.
- `setup --no-cbm` skips the cbm binary (Rueca-only runs).
- `setup --select` regenerates the `[[s1.instances]]` list at the end of
  `manifest.toml` from the selection rules (deterministic). With `--lang`,
  only those families are re-selected; every other instance keeps its entry
  and position.
- `clean --work` removes worktrees; `clean --runs rueca` drops cached Rueca outputs.
- `uv run --project bench pytest bench/tests` — unit tests of the harness
  pieces (efficiency counting, contract normalizers, health rules, rank
  fusion, the organization file and RFC 0006 metrics, and the multi-ref
  scorer).
- Per-call timeouts: 120 s per query, 1800 s per index (`arms/__init__.py`).
- S1 runs repositories in parallel (`--workers`, default 2), instances of one
  repository sequentially (one partial clone per repository, one detached
  worktree per instance, removed afterwards). S3 runs sets in parallel, the
  arms of one set sequentially on one worktree. Only one bench process may
  drive cbm at a time (one daemon per cache).

### Rueca arm

```sh
export PATH="$HOME/.cargo/bin:$PATH"
cargo build --release -p rueca
RUECA_BIN=$PWD/target/release/rueca uv run --project bench rueca-bench run --arm rueca --suite all
uv run --project bench rueca-bench report
```

A language pack still on a branch is benchmarked the same way: build that
branch (e.g. in a temporary worktree with its own `CARGO_TARGET_DIR`) and run
`--arm rueca --lang <family>` with `RUECA_BIN` pointing at it and
`RUECA_BIN_LABEL=<branch>@<sha>` (recorded in every output's arm version).
The report lists the Rueca binary per language, and `manifest.toml`
(`[tools.rueca]`) records which binary produced the published numbers.

**Contracts.** The arm detects the binary's response contract: `rueca query
--help` lists `--text` → RFC 0005 (envelope, grouped results, `trace`
edges by node index), else alpha. Under RFC 0005 the structural scoring
calls ask for `"format": "verbose"` (the alpha shapes, no token budget), the
tokens an agent reads are counted on the same request rendered with
`--text` (the MCP text content) and the JSON diagnostic on its compact JSON;
the efficiency search is the agent's own call (compact, default budget),
parsed from its compact JSON so items line up with the text. Normalizers
(`arms/rueca.py`: `flat_results`, `trace_neighbours`, `trace_graph`)
accept both shapes, so the same harness scores either binary and the
report says which contract produced the numbers.

Without `RUECA_BIN` the Rueca column is reported as *pending* (and every
scorecard floor of that language is *pending*, so it cannot be `stable`); a
language whose Rueca outputs are missing is left out of the detailed tables'
`overall` column (which pools only languages every arm has results for).

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

`uv`, `git` ≥ 2.40 (partial clone), `node`/`npx` (scip-python /
scip-typescript, S3 setup and the Python / TS/JS micro cross-checks), `go` (scip-go runs the Go toolchain; module
downloads go to `bench/.cache/go`, `GOTOOLCHAIN=local`), Docker (PHP S3 gold:
PHPStan in the pinned `php:8.3-cli` image; Java S3 gold: javac in the pinned
`eclipse-temurin:21-jdk` image — nothing PHP or Java is installed on the
host; without Docker those repository golds are reported as missing),
Xcode command-line tools on macOS (scip-clang reads the SDK from
`xcrun --show-sdk-path`; on Linux the system headers are used and the
pinned `linux-amd64` scip-clang binary), network for `setup` (and for the
first token count: tiktoken fetches `cl100k_base` once into
`bench/.cache/tiktoken`, verified by its own hash). `git-lfs` is not needed:
worktrees are created with the LFS filter disabled (pointers stay pointers).

**macOS note.** If `uv run` fails with `ModuleNotFoundError: rueca_bench`,
macOS has marked files in `bench/.venv` hidden and Python ≥ 3.12 skips hidden
`.pth` files (editable install). Use a regular install instead:
`export UV_NO_EDITABLE=1` (the `cache-keys` in `pyproject.toml` make uv
reinstall when sources change), or `chflags -R nohidden bench/.venv`.

Disk: about 1 GB for the alpha corpus (partial clones ≈ 100 MB, one worktree
at a time per repository, datasets ≈ 120 MB, cbm binary 335 MB). PHP adds
≈ 0.6 GB: partial clones ≈ 100 MB, SWE-rebench V2 parquet 429 MB (read with a
repository filter), PHPStan PHAR 31 MB, plus the `php:8.3-cli` image
(≈ 530 MB, in Docker's storage). Go, Java, C, C++ and the data corpus add
≈ 1.3 GB: shallow per-commit clones of 26 repositories ≈ 0.8 GB (hugo,
terraform, prometheus, redis are the large ones; `[corpus] shallow` keeps one
tree per pinned commit instead of whole histories), Multi-SWE-bench JSONL
7 MB, scip-go 13 MB, scip-clang 71 MB, scip-java 90 MB, Go module downloads
for scip-go ≈ 0.5 GB (deletable after `setup`), plus the
`eclipse-temurin:21-jdk` image (≈ 500 MB, in Docker's storage). The RFC 0004
gold repositories (jinja, click, zod, goawk, javapoet, yaml-cpp, Lua) and
the calibration repositories add ≈ 0.3 GB of clones. With `--prune` a full
run peaks at about 2 GB of cache (clones are deleted as soon as their runs
are done; gold, datasets and tools stay).

## Adding a language

Adding a language is data plus, at most, one labeler entry:

1. **Language family** — `[languages.<family>]` in `manifest.toml`: report
   `label`, member instance languages, source `extensions`,
   `exclude_suffixes`, `test_files` patterns and `test_dirs` (both pooled
   into the shared test-path heuristic, e.g. Go `testdata`), and
   `s1_sources` / `s3_gold` / `notes`. Report columns and the language's
   scorecard (with its computed status) appear automatically.
2. **Grammar** — `[grammars.<name>]`: the Python binding
   (`"<package>:<function>"`, pinned in `pyproject.toml`, one import in
   `treesitter.py: _BINDINGS`), its extensions (grammars sharing one, like
   C and C++ `.h`, are told apart by `languages` = the instance languages
   they parse), and for the data-driven labeler the callable node types
   (→ `Function` / `Method`), container node types and the qualified-name
   separator. Grammars whose function shapes need code name a code-backed
   `labeler` (`ecma` for JS/TS, `go` for receiver methods, `c` for C/C++
   declarators). `calls` / `members` describe call sites for SCIP edge
   derivation (`treesitter.py: _is_callee`). The labeler gives S1 function
   gold, the grep arm's function documents and S3 callables.
3. **S1** — `[[s1.repos]]` entries (repository, `language`, `quota`,
   `sources` = dataset keys, optional `source_priority` to prefer earlier
   sources, `split = "calibration"` for the calibration repository) and, if needed, a `[datasets.<key>]` pin (HF revision, `file` +
   `sha256` or `files = { path = sha256 }`, `format` for non-SWE-bench
   layouts such as `multi_swe_bench`, licence, optional `row_filter` /
   `filter_repos`). `rueca-bench setup --select --lang <family>` selects that
   family's instances deterministically; other instances do not move.
4. **S3** — at least two `[[s3.repos]]` (RFC 0004 §7.2: ≥ 300 questions
   per direction once pooled) with `gold = "scip"` (an `indexer` from
   `[tools]`: npm indexers run through `npx`, binary indexers from pinned
   release archives with `{out}` / `{root}` / `{compdb}` in `args`;
   scip-clang gets a synthesized `compdb` whose `files` may be globs,
   scip-java a `javac` source list), `include` / `exclude` path prefixes,
   `license`, optional `split` (`calibration`, `diagnostic`), or another
   gold method (`phpstan` for PHP); and a `[[s3.micro]]` micro-benchmark
   (`format = "rueca"` with snippets under `bench/micro/<lang>/`,
   `crosscheck = "<SCIP indexer>"`, `crosscheck_args` for npm indexers).
5. `setup --lang <family>`, `run --arm all --lang <family>`, `report`;
   document sources, licences and biases here.

A new S3 gold method (another external tool) is the only part that needs new
harness code; it must return the same graph shape as `scip.derive_call_graph`
(callables, callers, callees, stats).

### Additive results

`report` scores what is in the local cache and **carries forward** the
published entries of `bench/results/*.json` for everything else: S1
per-instance rows (and gold summaries), S3 per-question rows (compact
counts per question, `s3.json` schema 3, carried only when the set's gold
commit is unchanged), and data-suite entries per (arm, repository). So a
checkout that only adds a language, or the CI regression check that runs
only the Rueca arm, reproduces every other number without re-running it,
and the report lists what was carried, from which run (date, machine).
Local outputs always win over carried ones.

## Scorecard (RFC 0004 §5–§9)

Floors, targets, minimum populations and the bootstrap settings are data in
`manifest.toml` (`[scorecard]`, `[[scorecard.metrics]]`,
`[[scorecard.config_metrics]]`); `report.py` only evaluates them. Percent
unless noted:

| Family | Metric | Floor (`stable`) | Target |
|---|---|---|---|
| Localization | File Acc@5, Function Acc@10 (keyword query) | not significantly below BM25 (paired bootstrap) | ≥ 80 / ≥ 65 |
| Structure | Precision, callers and callees | ≥ 90 | ≥ 97 |
| Structure | Recall, callers and callees | ≥ 75 | ≥ 90 |
| Structure | Definition found | ≥ 95 | 100 |
| Micro | Callee F1 | ≥ 90 | ≥ 98 |
| Configuration | Key recall, span accuracy (per format) | ≥ 95 | 100 |
| Configuration | Env bridge precision / recall | ≥ 95 / ≥ 75 | ≥ 99 / ≥ 90 |
| Health | Files with parse errors (gated: vendored, copied and misdetected files out, see *Health*) | ≤ 2 | ≤ 0.5 |
| Health | Query latency p95 | ≤ 100 ms | ≤ 20 ms |
| Health | Files with parse errors, raw (vendored and misdetected files included) | shown, not gated | — |
| Efficiency | Tokens to first gold hit and per gold hit (`search_symbols`, top 10); tokens per true edge (`trace`) | — (reported) | ≤ 50% of cbm |

**How a floor is checked** (§6.1). Absolute floors: the point estimate must
meet the floor and the population must reach its minimum (≥ 300 structural
questions per direction, ≥ 30 localization instances; definition found uses
the union of both directions); ✅ when the whole 95% interval is on the
right side of the floor, ⚠ when it crosses it, ❌ when the estimate misses
or the population is too small. Localization: a paired bootstrap over
instances of Rueca − BM25 (`grep` arm), ❌ when the whole interval is below
zero, ⚠ when it contains zero, ✅ otherwise. A language is **`stable`** when
every gated floor is ✅ or ⚠, else **`preview`** (with the failing floors);
missing results count as failing. The structural precision / recall rows at
confidence ≥ 0.6 (`min_confidence`) are shown but not gated.

**Intervals** (§7.6), all 95%: Wilson for proportions (Acc@k, definition
found, lookup Acc, key recall, bridge P / R, files with parse errors);
percentile bootstrap with a fixed seed and 10,000 resamples for macro
averages (P / R over questions, recall / MRR over instances), pooled ratios
(micro-benchmark F1 over callables) and paired differences; binomial order
statistics for quantiles (latency p95); percentile bootstrap over queries
for the pooled tokens-per-correct-answer ratios. Seeds are derived per
metric from `bootstrap_seed`, so intervals are reproducible and independent
of evaluation order (`stats.py`).

## Calibration split (RFC 0004 §7.7)

Ranking weights and heuristics are tuned only on a calibration split whose
instances and repositories are disjoint from the gated ones; the gated set
is only evaluated. In the manifest, `split = "calibration"`:

| Language | S1 (SWE-rebench V2, LLM screen A) | S3 |
|---|---|---|
| python | pallets/click (5) | pallets/click 8.1.8 (scip-python) |
| ts/js | yargs/yargs (5) | — |
| php | JsonMapper/JsonMapper (5) | — |
| go | benhoyt/goawk (5) | benhoyt/goawk v1.29.1 (scip-go) |
| java | decorators-squad/eo-yaml (5) | — |
| c | nanomsg/nng (5) | — |
| cpp | randombit/botan (5) | — |

Every arm runs on the split and the report prints it in its own section;
it never enters the scorecard. A change tuned on it says so in its PR (and
in `docs/decisions.md`). `split = "diagnostic"` marks sets that are
reported but never gated: express (19 gold edges, untyped JavaScript) and
PyCG (function → function edges of a call-graph benchmark built mostly
around module-level calls).

## Regression check in CI (RFC 0004 §7.8)

`.github/workflows/bench.yml` (weekly schedule and `workflow_dispatch`, with
an optional `lang` input to restrict the run to some language families)
builds the checked-out commit in release mode, restores the pinned inputs
and gold from `actions/cache` (key = hash of the manifest, the hand-written
labels and the harness sources), runs `setup --no-cbm`, then the Rueca arm of
every suite (`run --arm rueca --suite health,s1,s3,data --prune`),
`report`, and `check`. It never needs cbm: the baseline arms are carried
from the committed `bench/results/*.json`. `check` fails when any gated floor
metric of any language (or of the configuration card) moved the wrong way by
more than the half-width of its committed 95% interval, or disappeared.
Query latency is not compared (CI runners are not the machine that produced
the published numbers) and neither are tokens (no floor). The regenerated
results are uploaded as an artifact. Runtime on `ubuntu-latest`: about
60–90 min with a cold cache (datasets, every gold set, the SCIP indexers),
about 40–60 min warm (estimates from local timings; the first scheduled run
will tell) — locally (Apple M4 Pro, 4 workers) setup takes about 6 min and
the Rueca arm of all suites about 16 min (health and S1 10, S3 5, data 1),
while cbm alone needs about 25 min for S3 (one 0.45 s process per call). PRs that touch a language pack or
the resolver can dispatch it on their branch.

## Suites

### Localization (S1, deterministic, no LLM)

Issue text → ranked files and functions. Method and metrics follow LocBench
(LocAgent): file Acc@1/3/5, function Acc@5/10, Recall@k, MRR, plus index time
and query latency. Gated (RFC 0004 §5): **File Acc@5** — 1 if a gold file is
in the top 5 ranked files, averaged over the language's instances;
**Function Acc@10** — the same over the top 10 functions, over instances
whose patch changes a function. Diagnostic: File Acc@1/3, Recall@k, MRR, the
`raw` variant, ContextBench context-file recall.

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

Go (41): every SWE-bench Multilingual Go instance with an eligible patch
(caddy 14, gin 8, prometheus 8, hugo 6, terraform 5; one hugo instance only
touches non-Go files). Java (40): SWE-PolyBench Java (assembled
`modified_nodes` gold) on gson 10 (with SWE-bench Multilingual), dubbo 8,
rocketmq 8, apollo 6, plus SWE-bench Multilingual lombok 8; lucene, druid
and trino are left out for size. C (30): every SWE-bench Multilingual C
instance (redis 12, jq 9, micropython 5, valkey 4; the 30 RFC 0004 §7.2
asks for, which is why C's calibration repository comes from SWE-rebench V2
rather than out of the gated set). C++ (38): SWE-bench
Multilingual has only 12 (fmt 11, nlohmann/json 1), so Multi-SWE-bench C++
(CC0) supplements it on the same two repositories plus Catch2 and simdjson
(fmt 14, nlohmann/json 10, Catch2 8, simdjson 6; SWE-bench Multilingual
first via `source_priority`, after instances with ContextBench gold).
Multi-SWE-bench rows give the issue text as title + body of each resolved
issue (as SWE-bench does) and `fix_patch` as the gold patch. Function gold
for these languages comes from the tree-sitter labeler, except SWE-PolyBench
Java (`modified_nodes`; its `ReturnType_name` node names are mapped back to
the method name). ContextBench gold context exists for 32 of the new
instances (mostly C++ and Java). The calibration split adds 5 SWE-rebench V2
instances per language on seven more repositories (above); they are run
by every arm and reported apart.

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
| Functions (32 Java instances) | **SWE-PolyBench** `modified_nodes` | as above; `method_declaration:String_buildKey` style names (return type prefix) are mapped to `buildKey` |
| Functions (34 other Python/TS instances, all 40 PHP, Go, C, C++ and 8 Java) | Rueca tree-sitter labeler (`gold.py`) | every removed/modified old-side line → innermost enclosing function/method; a pure insertion after old line k → innermost function containing k and k+1 (Python: also the function containing k when the inserted line is indented deeper than its `def`). PHP methods are qualified `Class::method` (classes, interfaces, traits, enums); PHP functions by bare name. Go methods `Type.Method` (receiver type); Java `Outer.Inner.method`, constructors `Class.Class`; C/C++ every `function_definition` with a body, `ns::Class::method` (out-of-line definitions keep their declarator scope) |
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

### Structure (S3)

**Questions** (RFC 0004 §7.1): every in-scope callable with at least one gold
edge in that direction — minus dunders and one-line bodies (extent shorter
than `[s3] min_lines` = 3 lines) — not a sample: callers questions and
callees questions are scored separately. For each question: resolve the
definition (path + name + def line ±2), then one depth-1 neighbours answer
(Rueca `trace` with `direction: "both"`, cbm `trace_path` with `direction:
"both"`, grep its textual heuristic). The symbol lookup (is the definition
in the top 1 / top 10 of the arm's symbol search for its bare name?) needs
one search per question, so it keeps a stratified sample of 50 per set
(fan-in × fan-out buckets, fixed seed). Micro-benchmarks: every callable of
every snippet is a question.

**Metrics** (RFC 0004 §5), per direction: **precision** — of the neighbours
the arm returns, the fraction that are gold edges, macro-averaged over the
direction's questions with at least one prediction; **recall** — of the gold
neighbours, the fraction returned, macro-averaged over the direction's
questions; **definition found** — questions whose definition the arm
resolves. Diagnostic: F1, micro (pooled) P / R, lookup Acc@1/@10. Rueca
returns a confidence per edge: each neighbour carries the best confidence of
the edges linking it to the root in that direction, and every structural
number is also computed with neighbours below `min_confidence` (0.6) dropped
— the row an agent that filters by confidence gets (cbm and grep return no
confidences: same numbers in both rows). Micro-benchmarks gate the
**callee F1 pooled over every callable** (precision and recall pooled over
the snippet set's edges, so spurious edges on callables without gold count).

**Scoring rules.** Predictions are restricted to the gold's scope before
scoring: in-scope, non-test files, a callable label (Function / Method /
Test / Constructor), **and a callable of the gold's universe** (a definition
the bench labeler sees as a callable: functions and methods with a body), so
module, file, class or variable pseudo-neighbours, C/C++ macros and
prototypes are neither rewarded nor penalised (the report counts what this
dropped per arm). Self-edges (recursion) are not neighbours: trace tools
never list their root. `typing.overload` stubs are not callables;
scip-python emits the definition only on the first stub, which is aliased to
the implementation.

**Populations** (gated sets, questions callers / callees): Python 508 / 565
(requests 105 / 88, flask 133 / 124, jinja 270 / 353), TS/JS 438 / 556 (hono
364 / 367, zod 74 / 189), PHP 709 / 810 (monolog, commonmark), Go 348 / 516
(gin, urfave/cli), Java 665 / 878 (gson, javapoet), C 976 / 1034 (cJSON,
Lua), C++ 863 / 1073 (fmt, yaml-cpp) — every language above the ≥ 300 per
direction of §7.2. Calibration: click 210 / 191, goawk 245 / 186;
diagnostic: express 8 / 19.

**Gold from SCIP** (RFC 0002 §4.2) at pinned tags: requests v2.34.2, flask
3.1.3, jinja 3.1.6 (`src/jinja2/`), click 8.1.8 (calibration) (`scip-python`
0.6.8); hono v4.13.13, zod v3.23.8 (`src/` minus `__tests__` and
`benchmarks`; `scip-typescript` 0.5.2, the repository's tsconfig, no
`npm install`); express v5.3.0 (`scip-typescript --infer-tsconfig`,
diagnostic); gin v1.11.0, urfave/cli v3.9.1 and goawk v1.29.1 (calibration)
(`scip-go` 0.2.9, release binary; every package of the module); gson 2.15.0
and javapoet 1.13.0 (`scip-java` 0.13.2: the javac plugin embedded in the
pinned launcher, run by `javac` in the pinned `eclipse-temurin:21-jdk` image
with `--network=none` over `gson/src/main/java` + `java-templates` and the
sha256-pinned `error_prone_annotations` jar, resp. javapoet's
`src/main/java` with no classpath, then `scip-java aggregate` — no Maven
run); fmt 12.1.0, yaml-cpp 0.8.0, cJSON v1.7.19 and Lua 5.4.7 (`scip-clang`
0.4.1 release binary over a `compile_commands.json` the bench writes: the
listed translation units — fmt `src/format.cc`, `src/os.cc`; yaml-cpp
`src/*.cpp`, `src/contrib/*.cpp`; cJSON `cJSON.c`, `cJSON_Utils.c`; Lua every
`*.c` but the amalgamation `onelua.c` and the internal test library
`ltests.c` — plus, for fmt and yaml-cpp, one bench-owned TU outside the
worktree that includes every public header so that headers no source file
includes are indexed too; the host SDK from `xcrun` on macOS, the system
headers on Linux; nothing is compiled or linked). SCIP gives references,
not calls, so (`scip.py`):

1. callables = tree-sitter functions/methods in scope (`include` prefixes, no
   tests); a SCIP definition whose (line, name) equals a callable's name
   identifier binds the SCIP symbol to it (its `enclosing_range`, when present,
   replaces the tree-sitter extent);
2. each non-definition occurrence of a bound symbol is kept only if
   tree-sitter says it is a call site (callee of a call / `new`, directly or as
   the property of a member access; for Go, Java and C/C++ the manifest's
   `calls` / `members` per grammar: Go `call_expression.function` through
   selectors and generic instantiations, Java `method_invocation.name`,
   `object_creation_expression.type` and `this(...)` / `super(...)`, C/C++
   `call_expression.function` through `.`/`->`, `::` and template
   arguments, C++ `new T`);
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

**Python, TS/JS, Go, Java, C and C++ micro-benchmarks**
(`micro/{python,ecma,go,java,c,cpp}/`, Rueca's own labels, Apache-2.0), same
format and spirit as the PHP one; every snippet compiles (`py_compile`,
`tsc --strict` 5.6 / `node --check`, `go vet`, `javac` 21 in Docker,
`clang -fsyntax-only` / `clang++ -std=c++17 -fsyntax-only`). Answer keys name callables by the bench
labeler's qualified names; overloads (Java, C++) are told apart as
`path:Qualified@<name line>`.

| Set | Snippets | Functions | Edges | Covers |
|---|---:|---:|---:|---|
| Python | 58 | 273 | 181 | functions, imports (`import m`, `from m import f`, aliases, packages, star imports with `__all__`), relative imports (`.`, `..`), `__init__` re-exports (one and two levels), classes / `self` / `cls`, constructors (explicit, inherited, chained `__init__`), inheritance (`super()`, multilevel, multiple inheritance and the MRO, diamonds, abstract bases), decorators (`functools.wraps`, factories, decorated methods, class decorators), closures and nested functions, local classes, properties, `@staticmethod` / `@classmethod` / alternative constructors, typed parameters / attributes / return types / generic element types / Protocols, dataclasses, name traps, dynamic calls |
| TS/JS | 60 | 242 | 165 | ESM (named, default, namespace, aliases, dynamic `import()`, `.mjs`), CommonJS (`require` with and without destructuring, `module.exports = …`, `exports.f`), barrels and re-exports (`export *`, `export { a as b } from`, chained barrels, `export * as ns`), default exports, classes / `this` / statics / accessors / arrow fields / `super.m()`, constructors and `super(...)`, tsconfig `paths` / `baseUrl`, decorators, generics, typed receivers (annotations, return types, fluent chains, `instanceof` narrowing), closures and callbacks, name traps, no-edge cases (untyped JS receivers, interface / abstract receivers, computed members) |
| Go | 60 | 240 | 156 | functions, packages (same package across files, sub-packages, aliases, dot / blank imports, package name ≠ directory), methods (value / pointer receivers, `&T{}` / `T{}` / `var x T` / constructor functions, typed params, fields, range and map / slice elements, chained returns, method values vs method expressions), embedding (promotion, pointer embedding, shadowing, multilevel), interfaces, type assertions and switches, closures, `go` / `defer`, generics, `init` / `main`, name traps |
| Java | 65 | 195 | 109 | packages (same package, single-type / on-demand / static imports, fully qualified calls), methods (`this`, implicit this, typed receivers, `var`, chains, builders), overloads (arity, type, varargs, widening, constructors), constructors (`new`, `this(...)`, `super(...)`), inheritance and overrides, interfaces (default, static, abstract, `I.super.m()`), nested / inner / anonymous / local classes, lambdas and method references, statics, enums and records, generics |
| C | 26 | 82 | 50 | prototypes in headers vs definitions, `static` same-name functions in two files, `static inline` in headers, includes from sub-directories, function pointers and dispatch tables, function-like macros, opaque handles and callbacks |
| C++ | 46 | 148 | 82 | namespaces (nested, `using`, aliases, anonymous), classes (inline and out-of-line methods across `.h` / `.cpp`, implicit `this`, static methods, nested classes), overloads, constructors, inheritance and virtuals, templates, operators, lambdas, name traps |

Answer-key conventions (beyond the PHP ones): an edge exists for every
explicit call whose target is statically determinable from types declared
or inferred in the snippet, pointing to the declaring definition.
**Python**: imports, annotations (parameters, variables, attributes, return
types, generic element types, Protocols, `Optional`) and literal
constructors bind receivers; `self` / `cls` / `super()` follow the MRO of
the enclosing class (no expansion to overrides); `C(...)` points to the
`__init__` the MRO finds, none when no class declares one (dataclasses'
generated `__init__`); calls in lambdas and comprehensions belong to the
enclosing function, nested `def`s and local classes own theirs; a call to a
decorated function points to its definition; property access, decorator
application, implicit dunders (`with`, `for`, operators, `__post_init__`),
`getattr`, calls through function-valued variables and callbacks give no
edge. **TS/JS**: imports, re-exports (to the declaring definition), CommonJS
`require` / `exports`, types and literals declared in the snippet,
`instanceof` narrowing and `this` / `super` with the class hierarchy bind
targets; `new C()` / `super(...)` point to the nearest explicit
`constructor`; interface-typed receivers and abstract signatures give no
edge (no callable); calls in anonymous callbacks belong to the enclosing
named callable; builtins, packages, getters / setters, decorator
application and `f.call` / `f.apply` give none; `module.exports = function
f(){}` is the callable `exports` (the labeler's assignment rule). **Go**:
calls through interface values, function-typed values (including method
values `f := s.M; f()`) and on type parameters give no edge (interface
method elements are not callables); method expressions `T.M(x)` and type
assertions `x.(T).M()` do; promoted methods point to the embedded type that
declares them; calls in function literals, `go` and `defer` belong to the
enclosing function; package-level `var` initializers give none. **Java**:
interface- / abstract-typed receivers point to the interface or abstract
declaration; overloads follow Java's static resolution; `new C(...)`,
`this(...)`, `super(...)` point to the explicit constructor (none when the
class has no explicit constructor; implicit `super()` gives none); method
references `C::m` are references, not calls; lambdas belong to the enclosing
method, anonymous- and local-class methods are their own callables
(`Outer.method.m`); field and static initializers give none. **C / C++**:
callables are function definitions with a body (prototypes, in-class
declarations, pure virtuals and macros are not); edges point to the body
wherever it lives; virtual calls point to the static type's definition;
calls through function pointers and implicit calls (destructors,
conversions, operators used as operators) give no edge; a call written in
a macro body belongs to the macro's expansion site — the function that uses
the macro calls the callee, as scip-clang records it (`quiet` → `log_msg`
through `LOG(m)`) — and so do calls written in a macro's arguments and
explicit `a.operator+(b)`; `new T(...)` points to `T::T` when it has a body.

Setup cross-checks every answer key against a SCIP indexer, deriving edges
exactly like the repository gold (`crosscheck` in the manifest; scratch
copies, nothing is written into the trees the arms index) and the report
prints the agreement: Python vs scip-python 164 / 181 key edges with 1
extra SCIP edge (44 of 58 snippets identical: scip-python records `C(...)`
as a reference to the class, not to `__init__` — 15 edges — and does not
resolve names from `from m import *`; the extra edge is a decorator
application, which the key excludes by convention), TS/JS vs scip-typescript
149 / 165 with 3 extra (48 of 60 identical: scip-typescript does not follow
CommonJS `exports.x` / `module.exports = function`, nested callables and
members of returned object literals are SCIP `local` symbols, a
default-exported object literal, `super(id)` and a constructor inherited two
levels up are not resolved; the 3 extra edges are decorator applications
inside a method's range), Go vs scip-go 156 / 156 edges (60 snippets identical),
Java vs scip-java 109 / 109 (65 identical), C vs scip-clang 50 / 51 key
edges with 1 extra SCIP edge (25 of 26 identical: two `static helper`
functions in different files share one SCIP symbol; the macro-body call
of `macros/macro_body_calls` now follows scip-clang's convention), C++ 79 / 82 with no extra edges (scip-clang misses a
`using`-declared function and two explicit `operator` calls).

**Gold from PyCG** (Apache-2.0, **diagnostic**): the hand-written
`callgraph.json` of every snippet of the PyCG micro-benchmark, restricted to
function → function edges (module nodes, builtins and external names
dropped). Each snippet becomes a tiny git repository with the answer key
removed; every function is a question. Its edges are mostly module-level
calls, so restricted to functions it is small and scores every tool at
28–33% (RFC 0004 §2): it is reported, the Python micro-benchmark is gated.

### Health (S5)

**Files with parse errors**: Rueca's own `IndexReport` (`files_partial /
files_extracted`) per language, over every corpus repository — each S3 gold
repository at its tag and each S1 repository at the base commit of its
first instance — indexed once per (repository, language) on a hard-linked
scratch tree that holds only that language's files (tests included, the
walk's excludes as in the repository); gated repositories pooled, Wilson
interval over files (`health.py`). Two rates:

- **gated** (the scorecard floor): every file of the language except
  - vendored third-party code, by the same directory names Rueca's ranking
    demotes (`rueca-segment` priors: `third_party`, `thirdparty`,
    `third-party`, `3rdparty`, `vendored`, `deps`, `dependencies`) plus
    `vendor` and `node_modules` (never indexed) — e.g. redis' and valkey's
    `deps/` (jemalloc, Lua, hiredis);
  - GoogleTest / GoogleMock copies (`googletest`, `googlemock`, `gtest`,
    `gmock` directories; `gtest-all.cc`, `gmock-all.cc`) — fmt's
    `test/gtest`, yaml-cpp's `test/gtest-1.11.0`;
  - amalgamations (`single_include/`, `singleheader/`, names containing
    `amalgamat`) — nlohmann/json, simdjson, Catch2;
  - per-repository third-party trees and copies without a conventional
    name (manifest `[[health.exclude]]`, each with its evidence):
    MicroPython's `lib/` (its external dependencies: CMSIS, littlefs, libm,
    oofatfs, …), the TI CC3200 driverlib and SimpleLink, FreeRTOS 9.0.0 and
    ST's USB host library under `ports/`, Catch2's `include/external`
    (Clara), nlohmann/json's frozen v3.10.5 single header for the ABI tests;
  - files that carry the language's extension but are not the language
    (manifest `[[health.not_language]]`): MicroPython's `qstrdefs*.h`
    (`Q(...)` / `QCFG(...)` lists read by `py/makeqstrdata.py`) and
    Objective-C / Objective-C++ headers in C and C++ repositories (a line
    starting with `@interface`, `@implementation`, `@protocol` or
    `#import`; Catch2's `catch_objc*.hpp` and its Xcode sample);
- **raw**: every file of the language, shown next to the gated rate (not
  gated). The report lists, per repository, how many files each rule
  excluded.

Generated code (jq's bison `parser.c`), test files and code fragments
(MicroPython's `*_globals.h` initializer lists) stay in: they are the
project's own C. **Query latency p95**: every Rueca query
of the language's gated S1 instances and S3 repository sets (`search_symbols`,
`get_symbol`, `trace`), one `rueca query` process per call on a warm segment
(the process opens the memory-mapped segment each time; process start-up is
included, so this overstates an MCP session). **Index throughput**: files
per second of the health index, reported.

### Efficiency — tokens per correct answer (RFC 0005 §8)

Measured with tiktoken `cl100k_base` on **what an agent reads**: Rueca's
MCP text content (`rueca query ... --text`) when the binary has the RFC 0005
contract, else the JSON it prints (alpha); cbm's MCP text content, which is
its default `tree` rendering (the bench parses cbm's `format: json` answer
and counts the tree rendering of the same request). Rueca and cbm answer the
same queries; a query enters a language's numbers only when both answered.

- **`search_symbols`** (localization): the S1 keyword query of every gated
  instance, issued as an agent would — top 10 (`limit` 10, every other
  argument at its default). A *gold hit* is an item in a gold file (and,
  as a diagnostic, a gold function). **Tokens to first gold hit** = Σ
  tokens read until the first gold hit, the whole answer when there is
  none / queries with a gold hit; **tokens per gold hit** = Σ answer tokens
  / Σ gold hits in the top 10. The cost of a prefix of an answer is the
  tokens of the text up to the end of that item's line (both text
  renderings put one item per indented line; headers before it included);
  for a JSON answer, the serialized items in order with the rest of the
  document counted up front (`efficiency.py`).
- **`trace`** (structure): one depth-1 `trace` (direction `both`) per gated
  structural question (cbm `trace_path`), the same request as the
  structural scoring; **tokens per true edge** = Σ answer tokens / Σ gold
  edges returned (callers + callees, after the universe filter of S3).
- Diagnostics: tokens per definition found by the name lookup (S3 lookup
  sample, top 10); the alpha metric — median tokens per answer, on the JSON
  (cbm `format: json`) and on what an agent reads.

Ratios are pooled over queries with percentile-bootstrap 95% intervals;
reported only, target (RFC 0004 §6 as amended by RFC 0005 §8): Rueca ≤ 50%
of cbm on each of the three. A query without a correct answer costs its
tokens and adds nothing, so a cheaper but worse answer does not look
better.

### Org scenarios (RFC 0005 §5–§6, `org.py`)

**Multi-repository store.** The first gated S1 instance of the first
`[org] repos_per_family` (3) gated S1 repositories of every language family
— 21 repositories, each at that instance's base commit — are indexed
together. Rueca: **one organization** (RFC 0006), built one repository at a
time — as each worktree is added, the bench appends it to an organization
file (`.cache/org/rueca-org.toml`) and runs `rueca index --org <file>
<root>`, which indexes that repository and adds it to the organization
layer; cbm: one project per repository. Each instance's keyword query is
asked of the whole organization: Rueca **native**, one `search_symbols`
served with `--org` and no `repo` (one ranking over the organization symbol
index), and, for Rueca and cbm, **per repository**: one search per
repository, merged by the bench with reciprocal rank fusion (k = 60; one
item per repository at each rank, ties broken by the raw per-repository
score) — what an agent without an org query must do; cbm has no
cross-project search. Scored like S1 (File Acc@5, Function Acc@10) with
repository-qualified paths and the candidate filter of every language,
against the same arm's single-repository S1 answer for the same instance;
tokens of the top-10 answer (per repository: the sum of its calls) and the
share of the top 10 taken by other repositories. Worktrees are removed after
indexing (the stores keep what queries need), so disk stays bounded; the S1
gold of each instance is computed (or read from the cache) on the way.

RFC 0006 §8 metrics (Rueca, report table "organization suite"): File Acc@5
at organization scope over the single-repository S1 value (floor ≥ 0.85×,
target ≥ 0.95×); **tokens per correct answer** at organization scope — Σ
tokens of the top-10 answers / number of queries whose top 10 holds a gold
file of the instance's own repository — against the same ratio of the S1
single-repository answers (target ≤ 1.2×); **incremental update** — the
organization-layer time `org_ms` that every `index --org` step reports
(adding one repository) and, after the 21, of re-indexing the repository
with the most symbol documents (its part replaced), each over that
repository's own index time (floor ≤ 2×, target ≤ 1×). An arm not run keeps
its published numbers (marked carried).

**Multi-ref.** `[multiref] instance` (psf__requests-2317): the repository at
the base commit and at a fix commit (base + the instance's gold patch,
committed by the bench with a fixed identity and date, so its id is
deterministic), both indexed into one store. Gold (`collapse_gold`): every
callable of the changed files classified `changed` (its text differs),
`added`, `removed` or `same_in_changed_file`, and every symbol of an
unchanged file `identical` (same blob: one item listing both snapshots).
Queries: every changed / added / removed function name and a fixed sample of
unchanged ones, each a `search_symbols` over both refs; `score_collapse`
counts collapsed identical symbols, `in` lists and `changed` flags. The two
single-ref answers per query are measured today (the uncollapsed baseline);
the multi-ref answer is *pending* while the local catalog holds one snapshot
per root (RFC 0005 §9). Trace collapse needs per-commit call-graph gold and
is not scored yet.

### Configuration — data formats (RFC 0003 §3.3)

`data.py`, manifest `[[data.repos]]`, pinned: GoogleCloudPlatform/
microservices-demo v0.10.7 (Online Boutique: Kubernetes manifests, Helm,
kustomize, GitHub Actions, Terraform, eleven services in five languages),
a 16-stack subset of docker/awesome-compose (compose files and small apps),
and terraform-aws-modules/terraform-aws-vpc v6.7.3 (HCL).

- **Key recall vs a reference parser.** Every data file in scope (JSON /
  JSONC, YAML, TOML, Terraform `.tf`) is flattened by a reference parser
  into leaf keys — keys whose value is not a mapping; arrays of mappings are
  entered (≤ 64 elements, depth ≤ 32). JSON uses a small position-keeping
  scanner of the JSON grammar (plus JSONC comments and trailing commas),
  YAML PyYAML's composer (aliases are leaves, never expanded; merge keys
  skipped; multi-document streams), TOML `tomllib` (lines from a header /
  key scan), HCL python-hcl2's syntax tree (attributes and labelled blocks).
  Files cut by RFC 0003 §4.5 are not in the reference: lockfiles, fixtures /
  testdata, > 256 KiB, generated markers, and YAML that is a Go / Helm
  template (`{{ }}` other than GitHub Actions `${{ }}`). An arm **covers** a
  reference key when it has a node in the same file named like the key (last
  key segment, or a qualified name ending in it after `.` / `/` / `:`) whose
  start line lies inside the key's span — the arms
  qualify keys differently (Rueca key paths, cbm flattened names), so paths
  are not compared. **Span line accuracy** = covered keys whose node starts
  exactly on the key's line. Rueca's nodes come from `get_file_outline`,
  cbm's from `query_graph` over every node of the file.
- **Kinds covered**: files of each detected kind (compose, Kubernetes,
  GitHub Actions, Terraform, `package.json`, …) on which the arm emitted a
  kind node (`Resource`, `Image`, `EnvVar`, `Route`).
- **Env bridge precision / recall** against hand-checked labels
  (`labels/bridge/*.json`, Rueca's own, Apache-2.0): reads are
  `(file, function, variable)` — the innermost named function (or
  `<module>`) that reads the variable with a string-literal name through a
  standard env API (`os.Getenv` / `LookupEnv`, `os.environ[...]` / `.get` /
  `in`, `os.getenv`, `process.env.X` / `['X']`, `System.getenv(...)` and
  `System.getenv().get*(...)`); writes are `(manifest, variable, line ±1)` —
  Kubernetes container `env` entries and compose `environment` items. Names
  built at run time (Online Boutique's `mustMapEnv(&x, "NAME")` wrapper),
  C# code and Helm templates are not labelled. Online Boutique: 66 reads, 36
  writes; awesome-compose: 21 reads, 46 writes. cbm's `CONFIGURES` edges are
  mapped by direction (`env_access` → reads, `env_set` → writes at the
  entry's line); Rueca's `READS` / `WRITES` come from `trace` callers of
  `$env:NAME` for every candidate name (gold names plus every UPPER_CASE
  literal / env-looking token in scope, so spurious edges are seen). The
  `grep` arm is a regex baseline: the env APIs above in code (reader = the
  innermost tree-sitter function) and `- name: X` / `- X=…` / `X: …` lines
  inside `env:` / `environment:` blocks of YAML manifests.

## Datasets, tools and licences

| Asset | Pinned | Licence | Use |
|---|---|---|---|
| SWE-bench Verified (`princeton-nlp/SWE-bench_Verified`) | HF revision + sha256 | none stated on the card (SWE-bench code: MIT) | S1 issues, patches |
| SWE-bench Lite (`SWE-bench/SWE-bench_Lite`) | HF revision + sha256 | none stated on the card | S1 issues, patches |
| SWE-PolyBench (`AmazonScience/SWE-PolyBench`) | HF revision + sha256 | MIT | S1 TS issues, patches, `modified_nodes` gold |
| SWE-bench Multilingual (`SWE-bench/SWE-bench_Multilingual`) | HF revision + sha256 | MIT | S1 TS/JS issues, patches |
| LocAgent / Loc-Bench labels (`czlll/SWE-bench_Lite`) | HF revision + sha256 | none stated on the card (LocAgent code: Apache-2.0) | S1 function gold |
| ContextBench (`Contextbench/ContextBench`) | HF revision + sha256 | none stated on the card (ContextBench code: Apache-2.0) | S1 context-file gold |
| SWE-rebench V2 (`nebius/SWE-rebench-V2`) | HF revision + sha256 | CC-BY-4.0 (attribution: Nebius, SWE-rebench V2) | S1 PHP issues, patches (Twig, monolog, commonmark); the calibration split of every language |
| Multi-SWE-bench (`ByteDance-Seed/Multi-SWE-bench`), four C++ JSONL files | HF revision + per-file sha256 | CC0 (dataset README; upstream repository licences apply) | S1 C++ issues, patches (fmt, nlohmann/json, Catch2, simdjson) |
| PyCG micro-benchmark (`vitsalis/PyCG`) | commit | Apache-2.0 | S3 Python call-graph gold (diagnostic) |
| PHP micro-benchmark (`bench/micro/php`) | in repository | Apache-2.0 (Rueca's own labels) | S3 PHP call-graph gold |
| Python / TS/JS / Go / Java / C / C++ micro-benchmarks (`bench/micro/{python,ecma,go,java,c,cpp}`) | in repository | Apache-2.0 (Rueca's own labels) | S3 call-graph gold |
| Env bridge labels (`bench/labels/bridge`) | in repository | Apache-2.0 (Rueca's own labels) | data-format suite gold |
| PHPStan (`phpstan/phpstan`) release PHAR | version + sha256 | MIT | S3 PHP gold (run in Docker, never vendored) |
| `php:8.3-cli` Docker official image | digest | PHP-3.01 (separate process) | runtime for PHPStan |
| tree-sitter-php, -go, -java, -c, -cpp (PyPI) | `uv.lock` | MIT | bench labeler for PHP, Go, Java, C, C++ |
| PyYAML, python-hcl2 (+ lark, regex) (PyPI) | `uv.lock` | MIT / MIT (+ MIT, Apache-2.0) | data-format reference parsers |
| numpy (PyPI) | `uv.lock` | BSD-3-Clause | bootstrap intervals |
| tiktoken (PyPI) + the `cl100k_base` encoding file | `uv.lock`; the encoding by tiktoken's own sha256 | MIT | efficiency (tokens per correct answer) |
| scip-python / scip-typescript | npm version | MIT / Apache-2.0 | S3 gold |
| scip-go 0.2.9 | release archive + binary sha256 (darwin-arm64, linux-amd64) | Apache-2.0 | S3 Go gold, Go micro cross-check |
| scip-clang 0.4.1 | release binary sha256 (darwin-arm64, linux-amd64) | Apache-2.0 | S3 C / C++ gold, micro cross-checks |
| scip-java 0.13.2 | release launcher sha256 (JVM, any platform) | Apache-2.0 | S3 Java gold, Java micro cross-check |
| `eclipse-temurin:21-jdk` Docker official image | digest | GPL-2.0 with Classpath Exception (separate process) | runtime for javac + scip-java |
| `error_prone_annotations` 2.50.0 (Maven Central) | sha256 | Apache-2.0 | compile-only classpath for the gson gold |
| codebase-memory-mcp v0.12.0 | release URL + archive and binary sha256 (darwin-arm64, linux-amd64) | MIT | `cbm` arm |
| Corpus repositories | commit SHAs | their own (requests Apache-2.0, flask BSD-3, jinja BSD-3, click BSD-3, pytest MIT, pylint GPL-2.0, sphinx BSD-2, tailwindcss MIT, code-server MIT, vue MIT, preact MIT, axios MIT, hono MIT, zod MIT, express MIT, yargs MIT, carbon MIT, php-cs-fixer MIT, laravel/framework MIT, phpspreadsheet MIT, Twig BSD-3, monolog MIT, commonmark BSD-3, JsonMapper MIT; caddy Apache-2.0, gin MIT, prometheus Apache-2.0, hugo Apache-2.0, terraform BUSL-1.1 (source-available), urfave/cli MIT, goawk MIT; gson Apache-2.0, javapoet Apache-2.0, eo-yaml BSD-3, lombok MIT, dubbo Apache-2.0, rocketmq Apache-2.0, apollo Apache-2.0; redis BSD-3 before 7.4, RSALv2 / SSPLv1 (and AGPLv3 from 8.0) after (source-available at most pinned commits), jq MIT, micropython MIT, valkey BSD-3, cJSON MIT, Lua MIT (text in `lua.h`), nng MIT; fmt MIT, yaml-cpp MIT, Botan BSD-2, nlohmann/json MIT, Catch2 BSL-1.0, simdjson Apache-2.0; microservices-demo Apache-2.0, awesome-compose CC0-1.0, terraform-aws-vpc Apache-2.0) | cloned at runtime as benchmark input only; nothing vendored or copied (RFC 0002 §2 governs what enters the repository) |

Citations: Jimenez et al., *SWE-bench* (ICLR 2024); OpenAI, *SWE-bench
Verified* (2024); Rashid et al., *SWE-PolyBench* (2025); Chen et al.,
*LocAgent* (ACL 2025); EuniAI, *ContextBench* (2026); Salis et al., *PyCG*
(ICSE 2021); Badertdinov et al., *SWE-rebench* (Nebius, 2025) and SWE-rebench
V2 (CC-BY-4.0); SWE-bench team, *SWE-bench Multilingual* (2025); Ondřej
Mirtes et al., *PHPStan*; Zan et al., *Multi-SWE-bench* (2025); Sourcegraph /
SCIP steering committee, *scip-go*, *scip-java*, *scip-clang*.

Only identifiers (instance ids, commits, revisions) and Rueca's own code live
in this repository. Gold labels and arm outputs stay in `bench/.cache`;
`bench/results/*.json` contains scores and per-instance metrics, which include
file paths and function names of the public corpus repositories.

## Known biases and limitations

- **Contamination.** All corpus repositories and issues are public and old;
  irrelevant for the deterministic arms, relevant for the future agent suite.
- **Process-per-call latency.** `cbm` and `rueca` are driven one CLI process
  per query (`cbm` talks to its warm daemon; `rueca` opens its segment on every
  call); `grep` runs in process, so its query latency is ~0. The latency
  floor is therefore checked on a pessimistic number (process start-up
  included). Index times were measured with up to 4 repositories processed
  in parallel.
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
  invisible to scip-typescript, so express has few gold edges (diagnostic
  only). Calls through callbacks, decorators and `getattr` are missed
  everywhere; scip-python records `C(...)` as a reference to the class, so
  Python repository gold has no constructor → `__init__` edges (the Python
  micro-benchmark has them). Recall numbers are relative to this oracle.
- **Every function, not a sample.** Since RFC 0004 the structural questions
  are every eligible callable of the gold repositories, so the population
  is dominated by the largest repositories (Lua is 91% of C's callers
  questions, jinja 53% of Python's, hono 83% of TS/JS's callers); per-set
  rows in the report show each repository alone. Numbers are not comparable
  with the alpha's 50-question samples.
- **The callable universe** rule (predictions that are not gold callables are
  dropped) is lenient on precision where Rueca links to a node the bench
  labeler does not see as a callable — a C/C++ prototype instead of the
  body, a macro — and strict on recall (the body edge is then missing). The
  report counts the dropped predictions per arm and set.
- **PyCG** contains mostly module-level calls; restricted to function →
  function edges it is small (see the gold table in the report); diagnostic.
- **Calibration split.** Five SWE-rebench V2 instances per language (LLM
  screened, train split) on one repository each: wide intervals, meant for
  tuning, not for claims.
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
  contexts are used, joined by instance id and base commit. The same join
  gives context-file gold to 32 C++ / Java / C instances.
- **Go, C, C++ S1 function gold** has no assembled labels; it comes from the
  tree-sitter labeler, validated on Python / TS / Java (mean Jaccard in the
  report). C/C++ callables are definitions with a body: a patch that only
  edits a prototype or a macro has no function gold. `.h` files follow the
  instance language (C grammar for C repositories, C++ for C++).
- **Small and skewed samples.** C++ S1 leans on fmt and nlohmann/json
  (24 of 38); nlohmann/json and simdjson ship amalgamated single headers
  (`single_include/`) that duplicate the sources, so file rankings there can
  hit either copy (only the patched one is gold). Java S1 has no lucene /
  druid / trino (size). C S1 has exactly the 30 instances of SWE-bench
  Multilingual.
- **SCIP gold for Go, Java, C/C++** has the usual SCIP blind spots: interface
  dispatch (scip-go, scip-java resolve to the interface method only, so
  implementations get no caller edges), function pointers, macros
  (scip-clang attributes a call written in a macro body to the expansion
  site, which tree-sitter tools cannot see), `static` functions with the same
  name in different C files (one SCIP symbol). The fmt and yaml-cpp golds
  index the library translation units plus one TU including every public
  header, so header-only templates are covered without a CMake configure
  (template code only instantiated in user code keeps dependent calls
  unresolved by clang, hence no gold edge); cJSON's gold covers its two
  sources; Lua's every source but the amalgamation and the test library —
  Lua is macro-heavy (`luaM_*`, `setobj*`), and a call through a macro is
  attributed by scip-clang to the macro's expansion site, which the gold
  counts as a call of the expanded function from that line. gson's and
  javapoet's golds compile only `src/main/java` (tests and other modules are
  out of scope anyway). C/C++ golds built on Linux (CI) use the system
  headers instead of the macOS SDK; the in-project edges are the same up to
  `#if` branches on the platform.
- **Micro-benchmarks** are written by the bench authors; their conventions
  are choices (above). The SCIP cross-checks guard against labelling
  mistakes, not against convention bias.
- **Data formats.** The reference flattens leaf keys only and matches arms
  by file + name + line span, not by key path, because Rueca and cbm qualify
  keys differently; a tool that emits a node per leaf at the right line
  scores 100 % regardless of its naming. The bridge labels cover literal env
  reads through standard APIs and Kubernetes / compose providers only: no
  config keys (Spring, viper, Laravel), no Dockerfile `ENV`, no `.env`
  files, no wrapper functions — those are later extensions. Rueca's bridge
  edges are found by asking `trace` about candidate names (it has no
  enumeration tool); a spurious edge to a name that never appears as a
  literal in scope would be missed.
- **Source-available corpus repositories.** terraform (BUSL-1.1) and redis
  (RSALv2 / SSPLv1 / AGPLv3 at recent commits) come with SWE-bench
  Multilingual (RFC 0003 §10.2, §10.3 list them). They are cloned at runtime
  as benchmark input; no code is vendored or copied, and results only name
  their files and functions.

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
  issue-only control arm are not part of the bench (RFC 0004 §11, roadmap).
- RFC 0004 §5 defines latency on a warm segment; the arm measures one
  `rueca query` process per call (segment re-opened each time, page cache
  warm), which overstates it.
- RFC 0005 §8 replaces RFC 0004's "tokens per answer" with tokens per
  correct answer; the alpha medians stay in the report as a diagnostic.
  `trace` collapse across refs (§6) is not scored (no per-commit call-graph
  gold); the multi-repository scenario fuses per-repository rankings for
  cbm and for Rueca's per-repository mode in the bench (cbm has no
  cross-project query). The organization's incremental update time is
  measured on one re-index after the facts cache is warm (the repository's
  own index time is then lower than its first index, so the ratio is
  conservative); both times are reported. RFC 0004 §5 counts health over "extracted files";
  the gated rate now leaves out vendored, copied and misdetected files (the
  raw rate is shown next to it).
- RFC 0004 §7.7 asks for a calibration split per language; S3 calibration
  sets exist for Python (click) and Go (goawk) only so far.
- S1 uses SWE-bench Multilingual for vuejs/core, preact and axios next to
  SWE-PolyBench, because PolyBench's TS/JS repositories of small/medium size
  are few.
- RFC 0003 §10.2 names gin and cli/cli for Go S3 gold; the bench uses gin and
  urfave/cli (smaller, no generated code). §10.3 names fmt and jq for C/C++;
  jq needs an autotools configure for `config.h`, so cJSON and Lua stand in for C (and yaml-cpp joins fmt for C++).
  §10.4 names gson and spring-petclinic for Java; petclinic needs Spring on
  the classpath (a Maven resolution), so javapoet (no dependencies) is the second Java repository.
- RFC 0003 §3.3 puts bridge precision / recall in suite 2 (contracts) on
  OpenTelemetry demo, Online Boutique, spring-petclinic, laravel/laravel and
  two compose services; this first cut labels Online Boutique and an
  awesome-compose subset, env vars only. The key-recall measurement is a
  bench metric here (the RFC also wants it as a CI test in the data pack).
