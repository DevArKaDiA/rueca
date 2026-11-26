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
| `vector` | No graph, no BM25: the `grep` arm's units (same files, same tree-sitter functions) embedded as a canonical text and ranked by exact cosine (RFC 0007 §11.1); files by their best function. Localization only (S1); never part of `--arm all` | `$RUECA_BIN embed --preset <p> ... --format f32le` through temporary files, vectors cached by content hash (see *Vector arm*) |

Suites, by name (the S-numbers stay as short identifiers):

| Suite | Question | Gated in the scorecard |
|---|---|---|
| **Localization** (S1) | "Given this issue, where must the code change?" | File Acc@5, Function Acc@10 (not significantly below BM25) |
| **Structure** (S3) | "Who calls this function, and what does it call?" | callers / callees precision and recall, definition found |
| **Micro-benchmarks** (part of S3) | the same on hand-labelled snippets, one language feature each | callee F1 |
| **Health** (S5) | "Does it parse, and is it fast?" | files with parse errors, query latency p95 |
| **Efficiency** | "How many tokens does a correct answer cost?" (RFC 0005 §8) | reported only |
| **Org scenarios** | "Does it still find the code in a store of many repositories, and of one repository at two commits?" (RFC 0005 §5–§6) | reported; paired vs BM25 with MDE (not a language gate) |
| **Configuration** (RFC 0003 §3.3) | "Which keys exist, who reads this env var, who sets it?" | key recall, span accuracy, bridge precision / recall |
| **Service** (RFC 0008 §8.2–§8.3) | "Does `rueca serve` answer the organization suite over HTTP exactly as `rueca query` does, and how fast?" | reported (differential n/n, time to ready, p50 / p95) |
| **Multi-step** (RFC 0009 §8, experimental code mode) | "How many tokens does a multi-step question cost as tool calls, as one code-mode script, and with cbm?" | reported; RFC 0009's per-language graduation gate |

The repository ships **manifests and code only** (plus Rueca's own labels:
the micro-benchmarks in `micro/{python,ecma,php,go,java,c,cpp,rust,shell,csharp,kotlin,ruby}/` and the
env-bridge labels in `labels/bridge/`). Datasets, repositories and tool
binaries are fetched at runtime at pinned revisions into `bench/.cache`
(git-ignored), so nothing with an unclear licence is vendored.

`rueca-bench report` writes `docs/bench/alpha-results.md`, which opens with
the **scorecard** (RFC 0004 §9): one table per language, *Metric | Rueca
(95% CI) | Floor | Target | Status | BM25 | cbm*, followed by the language's
status (`stable` / `preview`) and its failing floors, then a configuration
card, then the detailed tables of every suite. `bench/results/*.json` holds
every number (`scorecard.json` is what the CI regression check compares;
`org.json` is the organization suite, one row per question and arm, compact
like `s3.json`).

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
  `php`, `go`, `java`, `c`, `cpp`, `rust`, `shell`, `csharp`, `kotlin`, `ruby`) and/or `data` (the data-format suite);
  `report` always scores everything cached.
- `run --arm {grep,cbm,rueca,vector,rueca-hybrid,rueca-semantic,all|a,b} --suite {s1,s3,data,health,org,all|a,b} [--only ID|REPO|SET ...] [--split gated|calibration] [--workers N] [--force] [--prune]`.
  `--split` restricts S1 to one split; the `vector`, `rueca-hybrid` and
  `rueca-semantic` arms are never included by `all`.
  Outputs are cached per (arm, instance) in `.cache/runs/`; re-running only
  computes what is missing (`--force` recomputes). Arm failures are recorded
  (and scored as misses), never dropped. `--workers` runs S1 repositories
  and S3 sets in parallel; `--prune` deletes each repository clone once its
  runs are done (bounded disk; re-created from the pinned commits on next
  use). `health` runs the Rueca arm only; `org` (the multi-repository,
  cross-repository and multi-ref scenarios) is self-contained (it does not
  read S1 outputs) and is skipped with `--lang`; for `org`, `--only` takes
  `gated` / `calibration` (the multi-repository splits to run) and / or
  repositories (a smoke run of the multi-repository scenario restricted to
  those members, e.g. `--only psf/requests pallets/flask tokio-rs/axum
  pallets/click`; its outputs record the filter and a full run recomputes
  them); with `--only` the cross-repository and multi-ref scenarios are not
  run.
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
- `compare` (decision tables over cached outputs) and `sweep` (the RFC 0007
  §7.6 fusion sweep): below, after the vector arm.
- `uv run --project bench pytest bench/tests` — unit tests of the harness
  pieces (efficiency counting, contract normalizers, health rules, rank
  fusion, the organization file and RFC 0006 metrics, the multi-ref scorer,
  S3 gold derivation and scoring, the vector arm, the `rueca embed`
  protocol against a fake binary, all-gold metrics, `compare`, the Rueca
  hybrid arm and the sweep against the fake binary, the plateau choice, the
  RFC 0007 §11.2 gate rows); CI runs them in the `bench harness` job.
- Per-call timeouts: 120 s per query, 1800 s per index (`arms/__init__.py`;
  7200 s when the Rueca arm indexes with `--embed`).
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

### Service suite

```sh
export PATH="$HOME/.cargo/bin:$PATH"
cargo build --release -p rueca                       # the `service` feature is on by default
RUECA_BIN=$PWD/target/release/rueca uv run --project bench rueca-bench service --small   # ≈ 10 s, no setup
RUECA_BIN=$PWD/target/release/rueca uv run --project bench rueca-bench service           # the gated organization (needs `setup`)
```

`service [--small] [--split gated|calibration] [--only REPO ...] [--limit N]
[--sample N] [--repeat N] [--keep] [--out FILE] [--bin PATH]` (also `run
--suite service`, which is not part of `all`). Writes
`bench/results/service-<UTC timestamp>.json`; the method is under *Suites →
Service*.

```sh
cargo build -p rueca                                  # a debug build: test-only flags
RUECA_BIN=$PWD/target/debug/rueca uv run --project bench rueca-bench service-index --small --indexers 1 2   # a few minutes
RUECA_BIN=$PWD/target/debug/rueca uv run --project bench rueca-bench service-index                          # the gated organization, 1 and 4 indexers
```

`service-index [--small] [--split gated|calibration] [--only REPO ...]
[--indexers N ...] [--pushes N] [--no-faults] [--keep] [--out FILE] [--bin
PATH]` writes `bench/results/service-index-<UTC timestamp>.json`; the
method is under *Suites → Service indexing*.

### Vector arm (RFC 0007 phase 0)

```sh
cargo build --release -p rueca                # a binary with `rueca embed`
export UV_NO_EDITABLE=1 RUECA_BIN=$PWD/target/release/rueca
$RUECA_BIN models pull qwen3-0.6b-q8          # the weights, once (639 MB, sha256-verified); nothing downloads them implicitly
export RUECA_BENCH_SHARED_CACHE=/path/to/other/bench/.cache   # optional: pinned datasets already downloaded
uv run --project bench rueca-bench run --arm grep,vector --suite s1 --split calibration --workers 4 --prune
RUECA_BENCH_VECTOR_DIMS=256 uv run --project bench rueca-bench run --arm vector --suite s1 --split calibration   # one sweep setting
uv run --project bench rueca-bench report
```

`arms/vector.py` uses exactly the `grep` arm's units (`arms/grep.py:
code_files` and the tree-sitter functions), so `grep` and `vector` differ
only in the scorer. Each function is embedded once, as a **canonical text**
(RFC 0007 §5.2; text builder version 1, the `x1` in the run tag):

```text
<language> <label> <qualified name>
path: <repository-relative path>
<signature>
<doc comment>
<body head>
```

- `language` is the instance language (`python`, `typescript`, `cpp`, …),
  `label` the labeler's `Function` / `Method`, `qualified name` its local
  qualified name (`Class.method`, `outer.inner`, `ns::f`).
- `signature`: from the declaration's first line (after decorators) to the
  end of its header, lines stripped and joined with one space — up to and
  including the `:` ending a Python header, up to (not including) the first
  `{` outside parentheses and brackets elsewhere; at most 12 lines, else the
  first line only.
- `doc comment`: Python's docstring (the body's first statement when it is
  a string literal); otherwise the comment lines directly above the
  declaration and its decorators, no blank line between (Rust attributes
  skipped), stripped, comment markers kept (`#` counts in Python only).
- `body head`: the rest of the body (after the docstring), without the
  closing `}`, dedented, trailing spaces removed, blank lines dropped.
- Empty parts are left out; parts are joined with `\n`; the whole text is
  cut to `max_tokens` × 4 characters (512 tokens → 2,048 characters), then
  right-stripped; `rueca embed --max-tokens` cuts again at real tokens.
- `RUECA_BENCH_VECTOR_FILE_TEXT=1` (sweep variant, off by default) also
  embeds one text per file, `<language> File <path>\npath: <path>\n<file
  head>`, and a file scores the best of its functions and its own text;
  off, a file scores its best function (RFC 0007 §5.1) and files without
  functions are not ranked.

Queries are the same `kw` / `raw` strings every arm gets, embedded with the
query flag (the binary adds the preset's instruction, or
`RUECA_BENCH_VECTOR_INSTRUCTION`) in calls of their own whose
`--max-tokens` is the query budget — `query_token_budget` of `rueca embed
--info`, 512 capped by the preset's context (512 for a binary that does not
report it) — whatever the document budget; scores are exact cosines over
L2-normalized rows, ties broken by unit order. Sweep knobs, all part of the
run directory `runs/s1/vector/<preset>-d<dims>-t<tokens>-i<instruction
hash>-f<file text>-x<text version>/` (so two settings never share outputs;
`report` reads the setting the environment names, default
`qwen3-0.6b-q8-dfull-t512-idefault-f0-x1`, and labels the column with it):

| Variable | Default | Meaning |
|---|---|---|
| `RUECA_BENCH_VECTOR_PRESET` | `qwen3-0.6b-q8` | `rueca embed --preset` |
| `RUECA_BENCH_VECTOR_DIMS` | the model's | Matryoshka dimensions: the first N components, re-normalized (in the bench) |
| `RUECA_BENCH_VECTOR_MAX_TOKENS` | 512 | document budget (× 4 characters) and the documents' `--max-tokens`; queries keep their own budget (below) |
| `RUECA_BENCH_VECTOR_INSTRUCTION` | the preset's | `--instruction` for queries; a binary without the flag stops the run before any instance |
| `RUECA_BENCH_VECTOR_FILE_TEXT` | `0` | file texts (above) |
| `RUECA_BENCH_VECTOR_DIR` | `bench/.cache/vectors` | the vector cache (share it across checkouts) |

**Vector cache** (`embed.py`). Rows are stored at the model's full width as
float16, append-only, per namespace:
`vectors/<model key>/t<max tokens>/doc/` and
`vectors/<model key>/t<max tokens>/query-<instruction hash | default>[-q<query budget>]/`
(the `-q` suffix only when the query budget differs from the document
budget, so `t512` caches keep their names),
each holding `keys.txt` (sha256 of the text, line i = row i), `rows.f16`
and `meta.json` (width); writers take an exclusive `flock`, rows are written
before their keys. Dimensions are applied after the cache, so a sweep over
dimensions, or over query instructions, embeds no document again, and the
instances of one repository share almost every vector. The model key comes
from `rueca embed --info`. Size: 2 bytes × width per distinct text (1024
dimensions: 2 KB per function).

**Raw scores** (RFC 0007 §7.6). The `grep` arm (BM25) and the `vector` arm
(cosine) store, per query, their top 200 files and functions with the raw
score, post-filtered like the rankings, in the run record
(`queries.<variant>.scores = {"files": [[path, score]], "functions":
[[path, name, start line, score]]}`), so fusion sweeps re-score offline;
`grep` outputs cached before this change have none (`run --arm grep
--force`).

**Report.** The S1 tables and the per-repository / calibration / cost
tables gain a `vector` column once the arm has results (its `overall`
column only when it covers every pooled language); the scorecard's
localization rows gain a `vector` column (RFC 0007 §11.2). Every arm's S1
table also shows the **all-gold** diagnostic: File Acc@5 counting an
instance only when every gold file is in the top 5, Function Acc@10 only
when every gold function is in the top 10 (the LocAgent / SweRank
convention; S1's gated Acc@k counts any gold item).

### Rueca hybrid and semantic modes (RFC 0007 phase 1)

```sh
rueca models pull qwen3-0.6b-q8               # weights into ~/.cache/rueca/models (or $RUECA_MODEL_DIR)
RUECA_BENCH_RUECA_MODE=hybrid uv run --project bench rueca-bench run --arm rueca --suite s1 --split calibration --prune
RUECA_FUSION_ALPHA=0.7 uv run --project bench rueca-bench run --arm rueca-hybrid --suite s1 --split calibration   # another setting
```

`RUECA_BENCH_RUECA_MODE` = `lexical` (default; the Rueca arm exactly as
before, so existing outputs stay valid), `hybrid` or `semantic`. A
non-lexical mode is its own localization-only arm, `rueca-hybrid` /
`rueca-semantic` (`--arm rueca` under the variable is renamed to it, and
the S3, data, health and organization suites keep running lexically
without it):

- **Index**: `rueca index <worktree> --store <instance store> --json
  --embed=<preset>`, the preset from `RUECA_BENCH_VECTOR_PRESET` (default
  `qwen3-0.6b-q8`, the vector arm's knob). The run record's `index_info`
  keeps the binary's `vectors` report (units, cached, embedded, tokens,
  time). The preflight stops the run before any instance when `rueca index`
  has no `--embed` or the preset's weights are missing (an index failure
  would be recorded and never retried).
- **Store sharing**: the store stays one per instance (deleted after it, as
  for the lexical arm), but every call gets `RUECA_VECTOR_DIR` =
  `$RUECA_BENCH_RUECA_VECTOR_DIR` (default `.cache/rueca-vectors`), the Rust
  store vector cache (RFC 0007 §5.5: content-addressed by embed key, shard
  locks, safe for parallel indexers), so a later commit of a repository —
  or a re-index in a sweep — embeds only symbols whose canonical text
  changed. `RUECA_MODEL_DIR` points at `$RUECA_MODEL_DIR`, else the default
  store's `models/` (`$RUECA_HOME`, else `~/.cache/rueca`): a fresh
  per-instance store has no weights of its own. This cache is the binary's
  format, not the vector arm's `.cache/vectors`.
- **Query**: `search_symbols` with `{"mode": "hybrid"}` (or `semantic`), in
  the verbose scoring shape; an explicit mode errors instead of silently
  ranking lexically. Every call gets `RUECA_FUSION_ALPHA`, `RUECA_FUSION_N`
  and `RUECA_FUSION_NORM` from the environment, with the bench's explicit
  defaults 0.5 / 100 / `minmax` (the binary's own), so the run directory
  names exactly what ran: `runs/s1/rueca-<mode>/<preset>-a<α>-n<N>-<norm>/`
  (`report` reads the setting the environment names, like the vector arm).
  The fused score of each result is kept as `scores` (as the vector and
  `grep` arms keep theirs).
- **Report**: `rueca-hybrid` (and `rueca-semantic`) get S1 columns next to
  `rueca`, `grep` and `vector` once they have results, a `hybrid` column in
  the scorecard's localization rows, and a *Semantic search gates* section
  (`semgates.py`, also `scorecard.json` `semantic_gates`) with RFC 0007
  §11.2's rows, paired per instance: File Acc@5 hybrid − BM25 `raw` pooled
  (whole interval above 0 and ≥ +5), hybrid − vector `raw` pooled (interval
  not below 0), Function Acc@10 hybrid − BM25 `raw` (≥ +3, not below 0),
  `kw` pooled hybrid − BM25 (not below 0), and per language and variant
  hybrid − the better of BM25 and the vector arm there (not below 0); for
  the gated split (the gate) and the calibration split (a diagnostic).
  Published rows of an arm with sweep settings are carried only into a
  report of the same setting (`s1.json` `run_labels`).

### Fusion sweep (RFC 0007 §7.6)

```sh
export UV_NO_EDITABLE=1 RUECA_BIN=$PWD/target/release/rueca
uv run --project bench rueca-bench run --arm grep --suite s1 --split calibration --prune      # the base, if not cached
uv run --project bench rueca-bench sweep --suite s1 --split calibration --arm rueca --mode hybrid \
    --alpha 0,0.1,...,1 --norm minmax,theoretical --n 100 --prune --json /tmp/sweep.json
uv run --project bench rueca-bench sweep --mode hybrid --alpha 0,0.1,...,1 --norm minmax,theoretical --report-only
```

`rueca-bench sweep` (`sweep.py`) runs every setting (α × normalization ×
N, one preset; `semantic` mode ignores α and runs α = 0) on the
calibration split only. Per instance it indexes **once** with `--embed`
and re-runs only the queries for each pending setting, each written to its
own run directory (an interrupted sweep resumes; `--force` recomputes); the
efficiency search is skipped. Then it prints the `compare` tables (base
`grep`) and the plateau choice:

- objective: the macro over languages of ½ (File Acc@5 + Function Acc@10),
  `--variant both` (default) the mean of `kw` and `raw`;
- one instance = 1 / n of the objective, n the pooled instances (one
  instance flipping both metrics in a language of average size);
- the plateau: a maximal run of consecutive α values on one (normalization,
  N) curve, all within one instance of the best setting; the widest run
  wins (then the higher mean objective, then the order given) and its
  **centre** is the choice, not the arg-max (an even run: the better middle
  point, the lower α on a tie);
- leave one language out: the choice redone without each language; stable
  on that language when that choice scores there within one instance (1 /
  n of the language) of the full choice.

Cost: one `rueca query` process per call loads the model (about 0.2 s), so
22 settings × 2 variants × 118 instances are about 5,200 calls; indexing
pays the lexical index once per instance, embeddings only on the first
commit of each repository (the shared cache).

### Compare: decision tables over cached runs (RFC 0007)

```sh
uv run --project bench rueca-bench compare --split calibration --base grep --arms vector --variant both
uv run --project bench rueca-bench compare --arms vector:qwen3-0.6b-q8-dfull-t512-idefault-f0-x1 vector:qwen3-0.6b-q8-d256-t512-idefault-f0-x1 \
    --cache /path/to/other/bench/.cache --json /tmp/compare.json
```

`rueca-bench compare --arms ARM[:TAG] ... [--base grep] [--split
calibration|gated|all] [--variant kw|raw|both] [--cache DIR] [--json OUT]`
(`compare.py`) turns the cached S1 outputs of one split into the tables the
RFC 0007 rules read, without running anything and without writing to the
cache (`--cache` may be another checkout's, even while a run is filling
it). A column is an arm (`grep`, `rueca`) or an arm and one sweep setting:
`vector` expands to every tag under `runs/s1/vector/`, `vector:<tag>`
selects one. Scoring is the report's (`metrics.py`; a failed query is a
miss), on the instances of the split with gold. Markdown goes to stdout,
the whole result (both variants) to `--json`:

- **Pooled** and **per language**, per variant: n, any-gold and all-gold
  File Acc@5 and Function Acc@10, file and function MRR.
- **Paired bootstrap** of column − base per instance (`stats.py`, the
  manifest's `bootstrap_resamples` = 10,000 and `bootstrap_seed`, 95%),
  pooled and per language, for the four Acc@k metrics; a column is paired
  only on the instances both have, so a sweep that is still running shows
  a smaller n instead of a biased number.
- **Objective** (RFC 0007 §7.6, also §3.4's model rule): the macro over
  languages of ½ (File Acc@5 + Function Acc@10), per variant and averaged
  over `kw` and `raw`, with its difference to the base; the number of
  languages it covers is shown (compare only columns with the same
  coverage). One pooled instance is worth 100 / n points (the rules'
  "within one calibration instance").
- **Top-10 overlap** for every pair of non-base columns: the mean over
  common instances of the Jaccard index of the two top-10 file lists and of
  the two top-10 function lists (both empty: 1). RFC 0007 §3.3 (int4 vs
  F16, floor 0.9) and §6.3 (dimensions) read it.

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
pinned `linux-amd64` scip-clang binary), a Rust toolchain with `cargo`,
`rustc` and the `rust-src` component on `PATH` (rust-analyzer's `scip`
command runs `cargo metadata`, and `cargo check` for build scripts and proc
macros; the pinned rust-analyzer is a release binary, not the toolchain's
own), Docker for the C# gold (scip-dotnet's pinned `linux/amd64` image,
≈ 2.4 GB in Docker's storage, with the .NET SDK inside; `dotnet restore`
needs network to nuget.org; nothing .NET is installed on the host), the
pinned scip-ruby release binary for the Ruby gold (Sorbet, static: no
Ruby, Bundler or gem installation on the host), network for `setup` (and for the
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
7 MB (+ 36 MB for the calibration files, ponyc's 26 MB), scip-go 13 MB,
scip-clang 71 MB, scip-java 90 MB, Go module downloads for scip-go ≈ 0.5 GB (deletable after `setup`), plus the
`eclipse-temurin:21-jdk` image (≈ 500 MB, in Docker's storage). The RFC 0004
gold repositories (jinja, click, zod, goawk, javapoet, yaml-cpp, Lua) and
the first calibration repositories add ≈ 0.3 GB of clones; the 16
repositories added when the split grew (RFC 0007 §7.6) add an estimated
≈ 0.5 GB more (uncompressed trees at the pinned commits, from the GitHub
API: zeek ≈ 0.1 GB per commit, mostly test baselines and traces, and
jackson-core's oldest pinned commit 0.16 GB of javadoc; the other 14 are
under 10 MB each). Rust adds ≈ 0.4 GB:
shallow per-commit clones of the 8 S1 repositories ≈ 165 MB (nushell,
coreutils and ruff are the large ones) and the 3 S3 repositories ≈ 10 MB,
rust-analyzer 37 MB (+ 14 MB archive), and what rust-analyzer's cargo runs
leave in the cache: the crates.io registry (`.cache/registry`, ≈ 70 MB) and
build-script / proc-macro outputs (`.cache/cargo-target`, ≈ 120 MB), both
deletable after `setup`. C# adds ≈ 1.5 GB: shallow per-commit clones of the
11 S1 repositories and the 3 S3 repositories (≈ 0.2 GB), the scip-dotnet
image (≈ 2.4 GB in Docker's storage, not counted), and the NuGet packages
`dotnet restore` downloads for the S3 gold (`.cache/nuget`, ≈ 1 GB,
deletable after `setup`). With `--prune` a full
run peaks at about 2 GB of cache (clones are deleted as soon as their runs
are done; gold, datasets and tools stay). The vector arm adds its cache
(`.cache/vectors`, 2 KB per distinct function text at 1024 dimensions, per
model and token budget) and the model weights `rueca models pull`
downloads (`RUECA_MODEL_DIR`).

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
   declarators, `rust` for impl / trait / `mod` scopes and test code,
   `shell` for file-level shell functions and the parser repairs the shell
   pack applies, `ruby` for constant paths and `#` / `.` method names).
   `calls` / `members` describe call sites for SCIP edge
   derivation (`treesitter.py: _is_callee`); `token_calls` names a
   grammar's unparsed macro-argument node (Rust `token_tree`) whose
   `name(` / `name!` tokens are call sites too. The labeler gives S1 function
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
   release archives — a `.tar.gz` with a `member`, a raw binary, or one
   gzip-compressed binary (`.gz`, rust-analyzer) — with `{out}` / `{root}` /
   `{compdb}` in `args`;
   scip-clang gets a synthesized `compdb` whose `files` may be globs,
   scip-java a `javac` source list), `include` / `exclude` path prefixes,
   `license`, optional `split` (`calibration`, `diagnostic`), or another
   gold method (`phpstan` for PHP, `shell` for the code-backed shell
   labeler); scip-dotnet takes `dotnet = { projects
   = [...], framework = "net8.0", remove = ["global.json"] }`: a scratch
   copy of the tree gets a generated solution of the listed projects, each
   made single-target, and runs in the pinned image; and a `[[s3.micro]]` micro-benchmark
   (`format = "rueca"` with snippets under `bench/micro/<lang>/`,
   `crosscheck = "<SCIP indexer>"`, `crosscheck_args` for npm indexers;
   a snippet carries whatever its indexer needs to see a project: Go a
   `go.mod`, Rust a `Cargo.toml` with an empty `[workspace]`).
5. `setup --lang <family>`, `run --arm all --lang <family>`, `report`;
   document sources, licences and biases here.

A new S3 gold method (another external tool) is the only part that needs new
harness code; it must return the same graph shape as `scip.derive_call_graph`
(callables, callers, callees, stats).

### Additive results

`report` scores what is in the local cache and **carries forward** the
published entries of `bench/results/*.json` for everything else: S1
per-instance rows (and gold summaries), S3 per-question rows (compact
counts per question, `s3.json` schema 4, carried only when the set's gold
commit is unchanged), data-suite entries per (arm, repository) and health
rows per repository (same commit). So a
checkout that only adds a language, or the CI regression check that runs
only the Rueca arm, reproduces every other number without re-running it,
and the report lists what was carried, from which run (date, machine).
Local outputs always win over carried ones. Tokens per correct answer
(efficiency) pairs Rueca and cbm on the same queries and is recomputed from
the rows at hand, local or carried: carried S1 rows keep their efficiency
record (`e`) and carried S3 rows the tokens an agent reads (`ta`, `lta`),
so re-running one arm of one suite (say, only the Rueca arm of S3) rescores
it against the published rows of the other arm. A part of a language's
entry (`search` from S1, `trace` / `lookup` / `raw` from the gated S3 sets)
keeps its published value only when some carried row behind it lacks those
counts (a publication older than them); the entry then lists it under
`carried` and the report says so. Delete stale local runs of languages you
did not mean to re-measure (`rueca-bench clean --runs <arm>`, or the files
under `bench/.cache/runs/{s1,s3}/<arm>/`) before `report`.

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
is only evaluated. In the manifest, `split = "calibration"`. The S1 split
started at 5 instances per language and was grown to 15 per language (13
for C++) on three repositories each before any RFC 0007 fusion or reranker
gate is read (RFC 0007 §7.6): 118 instances on 24 repositories. Sources:
SWE-rebench V2 (rows its own LLM screen rated `A`, a column of the pinned
parquet, so the screen is reproduced without an LLM), Multi-SWE-bench
(human-validated) and a spare SWE-PolyBench repository. Every calibration
repository is disjoint from every gated S1, S3 and data-format repository
(GitHub renames checked), and the Python ones also from SWE-bench Lite's
repositories, so an external SWE-bench Lite diagnostic (RFC 0007 §11.1)
stays untuned:

| Language | S1 (instances; dataset) | S3 |
|---|---|---|
| python (15) | pallets/click (5), Python-Markdown/markdown (5); SWE-rebench V2 · yt-dlp/yt-dlp (5; SWE-PolyBench) | pallets/click 8.1.8 (scip-python) |
| ts/js (15) | yargs/yargs (5, JS), sindresorhus/got (5, TS); SWE-rebench V2 · iamkun/dayjs (5, JS; Multi-SWE-bench) | — |
| php (15) | JsonMapper/JsonMapper (5), CuyZ/Valinor (5), phpactor/phpactor (5); SWE-rebench V2 | — |
| go (15) | benhoyt/goawk (5; SWE-rebench V2) · grpc/grpc-go (5), zeromicro/go-zero (5; Multi-SWE-bench) | benhoyt/goawk v1.29.1 (scip-go) |
| java (15) | decorators-squad/eo-yaml (5; SWE-rebench V2) · fasterxml/jackson-core (5), mockito/mockito (5; Multi-SWE-bench) | — |
| c (15) | nanomsg/nng (5; SWE-rebench V2) · facebook/zstd (5), ponylang/ponyc (5; Multi-SWE-bench) | — |
| cpp (13) | randombit/botan (5), zeek/zeek (5), ccache/ccache (3); SWE-rebench V2 | — |
| rust (15) | tokio-rs/bytes (5), tokio-rs/tracing (5), sharkdp/fd (5); Multi-SWE-bench | tokio-rs/bytes v1.12.1 (rust-analyzer) |
| shell (0) | none (no dataset has shell instances) | bats-core/bats-core v1.13.0 (shell labeler) |
| csharp (11) | btcpayserver/btcpayserver (5), ant-design-blazor/ant-design-blazor (3), spectreconsole/spectre.console (3); SWE-rebench V2 | aaubry/YamlDotNet v18.1.0 (scip-dotnet) |
| kotlin (15) | evant/kotlin-inject (5), square/anvil (5), ajalt/mordant (5); SWE-rebench V2 | square/kotlinpoet 2.4.0 (scip-java) |
| ruby (4) | faker-ruby/faker (2), jordansissel/fpm (2); SWE-bench Multilingual | rack/rack v3.2.9, Shopify/packwerk v3.3.1 (scip-ruby) |

Ruby stops at 4: SWE-bench Multilingual is the only dataset with Ruby
instances (SWE-rebench V2, Multi-SWE-bench and SWE-PolyBench have none),
and its 44 are needed in the gated split to reach 30 instances that
change a function (RFC 0004 §7.2); no Ruby ranking weight was tuned.

C++ stops at 13: outside the gated repositories, Multi-SWE-bench has only
cpp-httplib (one eligible instance) and SWE-rebench V2 only zeek (70
eligible), ccache (3) and repositories with one or two. PHP has no
Multi-SWE-bench, SWE-PolyBench or SWE-bench Lite source, so its split is
SWE-rebench V2 only.

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
`raw` variant, ContextBench context-file recall, and the all-gold File
Acc@5 / Function Acc@10 (every gold file / function in the top k; RFC 0007
§11.1).

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
instances (mostly C++ and Java). The calibration split adds 15 instances
per language (13 for C++) on 24 repositories of its own (above; 45 of the
78 added when it grew carry assembled gold: SWE-PolyBench
`modified_nodes` or ContextBench context); they are run by every arm and
reported apart.

C# (39): SWE-bench Multilingual and Multi-SWE-bench have no C#
instances; SWE-rebench V2 has 173 C# rows, 134 of them rated `A` by its
own LLM screen (the PHP filter). Gated: microsoft/kiota (25 of its 105
`A` rows, by sha256 of the instance id), devlooped/moq (all 7), and every
eligible row of the other small repositories (dapr/dotnet-sdk 2,
radzen-blazor, grpc-dotnet, yarp, dotnet-script, mathnet-numerics 1
each); every patch touches a non-test `.cs` file. Calibration: the three
other repositories with at least three `A` rows (btcpayserver 5,
ant-design-blazor 3, spectre.console 3). Function gold comes from the
tree-sitter labeler (C# test projects, `*.Tests` / `*.UnitTests` / …
directories, are test paths).

Kotlin (46): SWE-bench Multilingual has no Kotlin instances;
Multi-SWE-bench (the revision already pinned for C++, which includes its
Kotlin release) has 105, all touching Kotlin files. Gated: pinterest/ktlint
16, detekt/detekt 12, oss-review-toolkit/ort 6, TeXiFy-IDEA 5, Kotlin
DataFrame 4, square/okhttp 2, GradleUp/shadow 1 (by sha256 of the instance
id; Anki-Android left out: a GPL-3.0 Android application, a large clone);
every patch touches a non-test `.kt` / `.kts` file. Calibration: three
SWE-rebench V2 repositories rated `A` by its LLM screen (kotlin-inject,
anvil, mordant; five each), disjoint from the gated and S3 repositories.
Function gold comes from the tree-sitter labeler.

Rust (43): every SWE-bench Multilingual Rust instance (tokio 9, bat 8,
axum 7, ruff 7, coreutils 5, nushell 5, ripgrep 2; every patch touches a
non-test `.rs` file). Its calibration split is Multi-SWE-bench's
tokio-rs/bytes (all five instances), tokio-rs/tracing and sharkdp/fd (five
each; SWE-rebench V2 is not needed: Multi-SWE-bench is already pinned for
C++, at the same revision). Function gold comes from
the tree-sitter labeler (ContextBench has context gold for one nushell and
three bytes instances). Two gated instances have empty function gold
(axum-1730: additions outside existing functions; tokio-4384: code added
inside a `feature! { … }` block, which tree-sitter keeps as unparsed
tokens), as do four of the five bytes instances (new functions, struct
fields and `static` vtable initializers). ruff and nushell are the large repositories
(≈ 40,000 and ≈ 20,000 cbm nodes); cbm indexes each in seconds, so no
instance is left out for size.

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
| Functions (34 other Python/TS instances, all 40 PHP, Go, C, C++, Rust and 8 Java) | Rueca tree-sitter labeler (`gold.py`) | every removed/modified old-side line → innermost enclosing function/method; a pure insertion after old line k → innermost function containing k and k+1 (Python: also the function containing k when the inserted line is indented deeper than its `def`). PHP methods are qualified `Class::method` (classes, interfaces, traits, enums); PHP functions by bare name. Go methods `Type.Method` (receiver type); Java `Outer.Inner.method`, constructors `Class.Class`; C/C++ every `function_definition` with a body, `ns::Class::method` (out-of-line definitions keep their declarator scope); Rust `Type.method` (last path segment of the impl's self type), `Trait.method`, `mod.fn` for inline modules, `outer.inner` for nested functions, `macro_rules!` definitions by name, test code (`#[test]`, `#[cfg(test)]`) left out |
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
definition (path + name + def line ±2; the nearest when several arm symbols
are within ±2), then one depth-1 neighbours answer
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

**Scoring rules** (`report.py: score_answers`, `match.py`). Predictions are
restricted to the gold's scope before scoring: in-scope, non-test files, a
callable label (Function / Method / Test / Constructor), **and a callable of
the gold's universe** (a definition the bench labeler sees as a callable:
functions and methods with a body), so module, file, class or variable
pseudo-neighbours, C/C++ macros and prototypes are neither rewarded nor
penalised (the report counts what this dropped per arm).

- **One hit, one callable.** Each hit is assigned to at most one gold
  callable: same path and bare name, a name line, declaration start or
  decorator start within ±2 lines of the hit's start line; when the arm
  gives a qualified name (Rueca), the callables whose qualified name agrees
  with it come first (trailing segments equal, template arguments as
  written, then without them; parameter lists ignored); then the nearest
  line. Predictions and gold neighbours are compared as sets of callables,
  so a hit never matches two neighbouring overloads (or a constructor two
  lines from another), and two hits naming one callable are one prediction.
- **Recursion is an edge.** A recursive callable is its own caller and
  callee in the gold and in every arm: Rueca's `trace` reports the self-edge
  in `edges` (never in `nodes`), the grep heuristic sees `name(` in the
  body, cbm reports what `trace_path` returns.
- **Equivalence classes** (class-aware scoring, the gated numbers): when the
  gold cannot tell callables apart (below), a predicted callee that is any
  member of a class matches a gold edge to the class, and a class counts
  once per question (two members predicted are one prediction). A callers
  question on a class member is asked of the whole class: the gold callers
  of every member, against the union of the arm's callers for every member
  (the run asks the arm about each member). The same scoring without
  classes is reported as the **strict** diagnostic row.

`typing.overload` stubs are not callables; scip-python emits the definition
only on the first stub, which is aliased to the implementation.

**Equivalence classes in the gold** (`classes` in each gold file, counted
per set in the report's gold table). SCIP gives references to symbols, and
some indexers give one symbol to several definitions: scip-clang to
same-signature overloads (fmt's three SFINAE `copy(InputIt, InputIt,
OutputIt)`), function template specializations (`num_bits<int128_opt>` and
the primary template) and members of class template specializations, and
two C `static` functions of one name in different files; scip-python to
same-named functions nested in the methods of one class. The gold binds
the symbol's references to one definition (the first bound), so an arm that
names another is no worse in fact but would count as a false positive and a
miss. Every definition bound to one symbol is therefore one class; so is a
callable no SCIP definition binds to whose qualified name is that of one
class in the same file (Python: conditional `def`s, overload stubs written
`@t.overload`) or directory (C, C++, Go: the `#if` branch or build-tagged
file the indexer did not compile, e.g. Lua's three `lsys_load`) — not in
Java, TS/JS or PHP, where an unbound callable is another function (an
anonymous class). For PHPStan gold, a call target that maps to several
callables (same-named functions in one file, same-named methods within the
line tolerance) is a class too; the two PHP repositories have none.

**Populations** (gated sets, questions callers / callees): Python 508 / 570
(requests 105 / 88, flask 133 / 125, jinja 270 / 357), TS/JS 440 / 562 (hono
365 / 372, zod 75 / 190), PHP 710 / 811 (monolog, commonmark), Go 351 / 521
(gin, urfave/cli), Java 667 / 884 (gson, javapoet), C 978 / 1035 (cJSON,
Lua), C++ 878 / 1085 (fmt, yaml-cpp), Rust 1417 / 1296 (serde_json
342 / 513, ripgrep 1075 / 783), shell 1738 / 2170 (nvm 119 / 125, acme.sh
1238 / 1630, bash-it 381 / 415), Ruby 1233 / 1205 (jekyll 585 / 513,
ruby-lsp 648 / 692) — every language above the ≥ 300 per
direction of §7.2. Calibration: click 210 / 191, goawk 245 / 186, bytes
140 / 300, bats-core 33 / 21, rack 231 / 191, packwerk 240 / 170; diagnostic: express 8 / 19.

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
headers on Linux; nothing is compiled or linked); serde_json v1.0.154
(`src/`), ripgrep 15.2.0 (the Cargo workspace's `crates/`, minus the
`examples/` of three crates) and bytes v1.12.1 (calibration, `src/`)
(rust-analyzer 2026-11-09 release binary, `rust-analyzer scip <root>`:
`cargo metadata` over the repository's own manifest and lockfile when it
ships one, default features, build scripts and proc macros run by `cargo
check` with `CARGO_TARGET_DIR` and `CARGO_HOME` inside the bench cache;
std and dependencies resolve through the toolchain's `rust-src` and the
downloaded crates, and are out of scope); moshi 2.0.0-alpha.1
(`moshi/src/main/java/`, Kotlin sources), clikt 5.2.1
(`clikt/src/{commonMain,jvmMain}/`) and kotlinpoet 2.4.0 (calibration,
`kotlinpoet/src/{commonMain,jvmMain}/`) (scip-java 0.14.2's Kotlin indexer,
built on the Kotlin Analysis API 2.4.0 (K2): a `scip-java.json` build
(manifest `kotlin = { sources, classpath }`) listing the source
directories and sha256-pinned classpath jars — kotlin-stdlib and
kotlin-reflect 2.3.21, okio-jvm 3.16.4, jsr305 — run by the pinned
launcher in the same `eclipse-temurin:21-jdk` image as the Java gold with
`--network=none` and read-only sources; nothing is compiled; about 5 s
per repository, no file skipped); jekyll v4.4.1 (untyped,
`lib/`), ruby-lsp v0.26.11 (Sorbet `# typed: strict` with RBS inline
comments, `lib/`), rack v3.2.9 and packwerk v3.3.1 (calibration, `lib/`)
(`scip-ruby` 0.5.1 release binary — Sourcegraph's indexer on Sorbet — run
in the worktree with `--no-config` and the repository's own Sorbet options
on the command line: `--parser=prism --enable-experimental-rbs-comments
--enable-experimental-requires-ancestor` for the two Shopify repositories,
whose `sorbet/config` also sets a cache directory inside the tree; no
`bundle install`: gem constants are unresolved and out of scope). SCIP gives references,
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
   arguments, C++ `new T`, and C++ constructions by declaration — the name
   a declaration in a function body declares, `Token token(a, b);`,
   `Parser parser(input);` (the most vexing parse), `T x{a};`, `T x;`,
   where scip-clang puts the constructor reference; manifest `constructions`;
   Rust `call_expression.function` through `.` method calls, `::` paths
   (`Type::new()`, `Self::f()`, `<T as Tr>::f()`) and turbofish
   (`f::<T>()`), `macro_invocation.macro` (an invocation of a
   `macro_rules!` the repository defines), and calls written in a macro's
   arguments, which tree-sitter keeps as a flat token tree
   (`println!("{}", f(x))`, `tri!(self.parse())`: a name followed by a
   `(…)` token tree or by `!`; manifest `token_calls`) — rust-analyzer
   resolves those tokens through the expansion. Operators (`a + b` with a
   user `impl Add`, on which rust-analyzer puts a reference to `add`) and
   `?` (a reference to `Try::branch`) are not calls; a function named as a
   value (`.map(double)`, `let f = double;`) is a reference, not a call;
   Ruby: the `method` of a `call` node (`x.m`, `m(a)`, `Foo.new`, on
   which scip-ruby puts its reference to `initialize`) and a bare
   identifier, also as a receiver (`helper`, `items.map`): Sorbet gives
   local variables `local` symbols, so a bare name bound to a method is a
   call; operators, index calls and symbols are not);
3. the caller is the innermost callable containing the occurrence
   (module-level calls produce no edge).

The SCIP reader decodes the protobuf wire format directly and reads **both**
the deprecated packed `range` / `enclosing_range` fields and the typed
`single_line_*` / `multi_line_*` oneofs.

**Gold from the shell labeler (shell).** No compiler-grade call graph
exists for shell (no SCIP indexer, no type checker), so the bench derives
it from the code with its own labeler (`shellgold.py`, tree-sitter-bash
0.25.1), written from bash's run-time name lookup and independent of the
pack's path evaluation (the pack evaluates `source` paths from
`$(dirname "$0")`, `${BASH_SOURCE%/*}` and directory variables and resolves
names through import edges; the labeler matches sourced paths by their
literal tail and looks names up over files), at pinned tags: nvm v0.41.0
(`nvm.sh`, `install.sh`: one large library), acme.sh v3.1.7 (`acme.sh` and
its `dnsapi/`, `deploy/`, `notify/` hooks, which call the main script's
helpers it sources at run time), bash-it v3.2.0 (`bash_it.sh`, `lib/`,
`plugins/`, `completion/`, `aliases/`, `themes/`, `scripts/`, `hooks/`:
components sourced through `${BASH_IT}/…`); bats-core v1.13.0 (`lib/`,
`install.sh`, `contrib/`) is the calibration set. Rules:

1. callables = every `function_definition` of an in-scope, non-test shell
   file (extension-less scripts are outside the bench's file set), named
   by its name (shell functions are global; nesting does not qualify
   them); several definitions of one name in one file (conditional
   definitions) are one equivalence class;
2. sources: `source P` / `. P` (`\. P`) whose end is a literal path (the
   tail after the last expansion, starting at a `/`; `.` and `..`
   dropped), resolved to the only file whose path ends with that tail;
   dynamic, home and absolute paths source nothing; transitive (depth ≤ 10);
3. calls: every command whose word is a literal name, in any position
   (pipelines, lists, subshells, `$(…)`, `<(…)`, conditions); not the
   operands of other commands (`command f`, `exec f`, `xargs f`, `sudo f`,
   a `trap` handler — a callback —, `eval "f"`, `time f`); the caller is
   the innermost callable containing the command; file-level code has no
   edge;
4. target: the name's definitions in the caller's file; else in the files
   it sources, when one file holds them (several: no edge, the run order
   decides); else in the only non-test file of the repository that
   defines the name (a library calling a helper its entry script sources);
   else none.

The labeler and the pack read one grammar with the same same-length
repairs (`${s:$i:1}`, `&` / `<` / `>` in an expansion's pattern, a
line-start `\.`, zsh parameter flags).

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

**Python, TS/JS, Go, Java, C, C++, Rust, shell and Kotlin micro-benchmarks**
(`micro/{python,ecma,go,java,c,cpp,rust,shell,kotlin}/`, Rueca's own labels, Apache-2.0), same
format and spirit as the PHP one; every snippet compiles (`py_compile`,
`tsc --strict` 5.6 / `node --check`, `go vet`, `javac` 21 in Docker,
`clang -fsyntax-only` / `clang++ -std=c++17 -fsyntax-only`, `cargo check
--all-targets` with Rust 1.99 — every Rust snippet is a crate: `Cargo.toml`
with an empty `[workspace]`, `src/main.rs`, other modules). Answer keys name callables by the bench
labeler's qualified names; overloads (Java, C++) are told apart as
`path:Qualified@<name line>`.

| Set | Snippets | Functions | Edges | Covers |
|---|---:|---:|---:|---|
| Python | 58 | 273 | 181 | functions, imports (`import m`, `from m import f`, aliases, packages, star imports with `__all__`), relative imports (`.`, `..`), `__init__` re-exports (one and two levels), classes / `self` / `cls`, constructors (explicit, inherited, chained `__init__`), inheritance (`super()`, multilevel, multiple inheritance and the MRO, diamonds, abstract bases), decorators (`functools.wraps`, factories, decorated methods, class decorators), closures and nested functions, local classes, properties, `@staticmethod` / `@classmethod` / alternative constructors, typed parameters / attributes / return types / generic element types / Protocols, dataclasses, name traps, dynamic calls |
| TS/JS | 60 | 242 | 165 | ESM (named, default, namespace, aliases, dynamic `import()`, `.mjs`), CommonJS (`require` with and without destructuring, `module.exports = …`, `exports.f`), barrels and re-exports (`export *`, `export { a as b } from`, chained barrels, `export * as ns`), default exports, classes / `this` / statics / accessors / arrow fields / `super.m()`, constructors and `super(...)`, tsconfig `paths` / `baseUrl`, decorators, generics, typed receivers (annotations, return types, fluent chains, `instanceof` narrowing), closures and callbacks, name traps, no-edge cases (untyped JS receivers, interface / abstract receivers, computed members) |
| Go | 60 | 240 | 156 | functions, packages (same package across files, sub-packages, aliases, dot / blank imports, package name ≠ directory), methods (value / pointer receivers, `&T{}` / `T{}` / `var x T` / constructor functions, typed params, fields, range and map / slice elements, chained returns, method values vs method expressions), embedding (promotion, pointer embedding, shadowing, multilevel), interfaces, type assertions and switches, closures, `go` / `defer`, generics, `init` / `main`, name traps |
| Java | 65 | 195 | 109 | packages (same package, single-type / on-demand / static imports, fully qualified calls), methods (`this`, implicit this, typed receivers, `var`, chains, builders), overloads (arity, type, varargs, widening, constructors), constructors (`new`, `this(...)`, `super(...)`), inheritance and overrides, interfaces (default, static, abstract, `I.super.m()`), nested / inner / anonymous / local classes, lambdas and method references, statics, enums and records, generics |
| C | 26 | 82 | 50 | prototypes in headers vs definitions, `static` same-name functions in two files, `static inline` in headers, includes from sub-directories, function pointers and dispatch tables, function-like macros, opaque handles and callbacks |
| Shell | 43 | 109 | 54 | functions (both forms, recursion, nested definitions, wrappers named like utilities, a function passed as an argument), call positions (`$(…)`, backticks, pipelines, `&&` / `||`, subshells, `&`, conditions and loops, `<(…)`, `!`, `time`, here-documents, arithmetic), runners (`command`, `exec`, `xargs`, `sudo`, `env`, `nohup`, `eval` literals, `trap` handlers), sourcing (`$(dirname "$0")`, `.` literal paths, `${BASH_SOURCE%/*}`, directory variables, transitive sources, a library calling a sibling library, the own definition winning, one name in two sourced files, sourcing loops, unknown prefixes, `\.` after a word), script runs (by path, through `bash` / `sh -c` / `zsh`, a script named like a function), env syntax around calls (`VAR=1 cmd`, `${X:-$(f)}`, `export -f`, a variable named like a function), zsh (common syntax, parameter flags, `${0:A:h}`), name traps (dynamic command words, strings and comments, one name in unrelated scripts) |
| C# | 72 | 164 | 76 | methods (`this`, typed parameters / fields / properties, `var`, conditional access `?.`, `out var`, patterns, casts and `as`, `foreach`, chains through return types, same name in two types), statics (`using static`, inherited statics, factory chains), inheritance (inherited and implicit-`this` calls, `base.M()`, overrides through base- and sub-typed receivers, multilevel), interfaces (interface and concrete receivers, default interface methods, explicit implementations), constructors (`new`, `: base(…)`, `: this(…)`, target-typed `new()`, object initializers, exception constructors), overloads (arity, argument type, optional parameters, `params`), namespaces (same namespace in two files, `using`, enclosing namespace, fully qualified calls, aliases, a type name in two namespaces, `global using`, nested blocks), partial classes over two files, extension methods (receiver and static form), nested types, local functions, generics (generic classes and methods, constrained type parameters), lambdas, LINQ, method groups, `async` / `await`, records, structs, enums and switch expressions, expression-bodied properties, traps (a local delegate shadowing a method, `nameof`, strings and comments, a library method named like a project one), recursion, top-level statements |
| Rust | 58 | 190 | 133 | functions (nested `fn`, items local to a function, turbofish, recursion), modules (inline `mod`, `mod` file and `mod.rs`, same-name functions in two modules, `use` / glob / alias imports, `super::` / `crate::`, `pub use` re-export chains, library + binary crate), methods (`self`, `Self::` / `Type::new()`, builder chains, field receivers, `&T` / `Box<T>` / `Option<T>` / slice receivers, two impl blocks in two files, returned receivers), traits (default methods, `impl Tr for T` on a concrete receiver, `T: Tr` bounds and `where`, `dyn Tr`, `impl Tr` parameters and returns, supertraits, `T::create()` / `<T as Tr>::f()`, blanket impls, operator traits, `Default`, `From` / `Into`), closures and iterator adapters, `macro_rules!` (invoked, in another module with `#[macro_use]`, `crate::m!`, inside `println!`), calls in `println!` / `format!` / `vec!` / `assert_eq!` arguments, derives, struct / tuple-struct / enum-variant literals, generics, traps (strings and comments, a closure shadowing a function, a method and a function of one name, `?`, test functions) |
| C++ | 46 | 148 | 82 | namespaces (nested, `using`, aliases, anonymous), classes (inline and out-of-line methods across `.h` / `.cpp`, implicit `this`, static methods, nested classes), overloads, constructors, inheritance and virtuals, templates, operators, lambdas, name traps |
| Ruby | 61 | 152 | 76 | functions (top-level `def`, recursion, calls without parentheses, a local shadowing a method, a call before the assignment that makes the name a local, calls in blocks and lambdas, predicate / bang names, endless methods), classes (`self`, `def self.m`, `class << self`, `new` → `initialize` own and inherited, `new` inside the class, `self.class.m`, private methods, accessors, constants), inheritance (inherited and overridden methods, `super` with and without arguments, multilevel, singleton methods inherited, namespaced superclasses), mixins (`include`, `extend`, `prepend`, `module_function`, `extend self`, a mixin in another file, module singleton methods), namespaces (nested modules, compact `class A::B`, lexical lookup, `::Top`, one name in two namespaces, a class reopened in two files, a constant through the superclass, a compact body that does not see its outer module), typing (`x = Foo.new`, `@x = Foo.new` in `initialize`, Sorbet `sig` parameters and returns, RBS comment parameters and returns, `T.let`, two classes assigned to one variable, `Foo.new.m`), `require` / `require_relative`, `Struct.new` with a block, traps (untyped parameters, call results without a declared type, an unknown method on `self`, `&:sym`, strings and comments, an alias) |

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
**Kotlin**: callables are named functions (top-level, member, extension,
abstract interface members, local) and constructors named like their class
(`Foo.Foo`): the primary one, written or implicit (every class that is
not an interface and does not declare only secondary constructors), on the
class name's line with the class header as its extent, so that the
supertype constructor call and default arguments written there are its
calls; secondary ones on their first line. Property accessors, `init`
blocks, property initializers and lambdas are not callables (calls in them
have no caller, like Java field initializers). A call on a typed receiver
points to the member of the static type (interfaces to the declaration),
`Foo(…)` to the constructor, a companion member through its class to the
companion's member, an extension call to the extension function; callable
references (`::f`) and infix calls are not call sites.

**Rust**: callables are `fn` items with a body, trait method signatures and
`macro_rules!` definitions (named as in the pack: `Type.method`,
`Trait.method`, `mod.fn`, `outer.inner`); a method call on a concrete
receiver (a typed binding, `self`, a field, a returned value, a `Box`
or `&`) points to the impl method of that type, also when the impl
implements a trait (`impl Shape for Square` → `Square.area`), and so does a
path call with a concrete self type (`Type::new()`, `Self::f()`,
`Config::default()`, `Default::default()` with a known type, `<W as
Factory>::create()`); a call on a generic `T: Tr` / `impl Tr` parameter or
a `dyn Tr` receiver points to the trait method (no expansion to
implementations), while one on a value of an opaque `impl Tr` return type
points to the impl the function returns (rust-analyzer sees through it); a
blanket `impl<T: Display> Shout for T` method is `T.shout`; an invocation
of a repository `macro_rules!` is an edge to the macro, and calls written
inside the macro's body give none (rust-analyzer resolves no tokens of a
`macro_rules!` body; unlike scip-clang, it does not attribute them to the
expansion site); calls written in any macro's arguments are edges of the
enclosing function; closures belong to the enclosing function and calling
one gives no edge; a function named as a value (`map(double)`, `let g =
double; g(1)`) gives none; operators, `?`, `.into()` (the standard blanket
impl), derived impls and struct / tuple-struct / enum-variant
constructions give none; test code (`#[test]`, `#[cfg(test)] mod tests`)
has no callables. **Shell**: callables are `function_definition`s named by
their name; an edge exists for every command word that bash would look up
as a function defined in the snippet when the script runs as written (the
caller's file, what it sources in the order written — the last of two
sourced definitions wins —, what its entry script sources); `command f`,
`exec f`, `xargs f`, `sudo f`, `env f`, `nohup f`, `sh -c 'f'` and a
`trap` handler give none (a handler is a callback); `eval 'f'` and `time
f` do; running a script by path gives no edge to its functions;
file-level code has no edge.
**Ruby**: callables are `def`s (named as in the pack: `A::B#m`,
`A::B.m`, top-level `m`); accessors, aliases and `define_method` blocks
are not; an edge exists for a call whose target follows from constants
(Ruby's lexical lookup, then ancestors), `self` and the ancestors
(`prepend`, the class, `include`, the superclass; singleton methods
through `extend` and the superclass chain), `Foo.new` (to the
`initialize` the ancestors give, none when the project defines none),
`super`, a local or instance variable assigned `Foo.new`, a Sorbet `sig`,
an RBS comment or `T.let`, and a call result with a declared return type;
a call on an untyped parameter or on the result of a method without a
declared type gives none, and so do `&:sym` blocks; a variable assigned two
classes calls both; calls in blocks belong to the enclosing `def`; a call
of an alias points to the aliased method.

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
Java vs scip-java 109 / 109 (65 identical), C vs scip-clang 51 / 51 key
edges (26 identical; two `static helper` functions in different files
share one SCIP symbol, compared as one equivalence class like the
repository scoring — strictly 50 / 51 with one extra edge; the macro-body
call of `macros/macro_body_calls` follows scip-clang's convention), C++ 79 / 82 with no extra edges (scip-clang misses a
`using`-declared function and two explicit `operator` calls), Rust vs
rust-analyzer 133 / 133 (58 snippets identical; one key was aligned with
rust-analyzer after the check: `make().render()` on an opaque `impl Render`
return points to `Page.render`, the impl `make` returns, not to the trait
method), shell vs the shell labeler 51 / 54 with no extra edge (40 of 43
snippets identical: the labeler, like the S3 gold, follows neither `time
f` nor `eval 'f'`, and gives no edge for a name two sourced files define,
where the key follows the order the script sources them), C# vs scip-dotnet 72 / 76 with no extra edge (68 of 72 snippets
identical: scip-dotnet records no occurrence for `: base(…)` / `: this(…)`
initializers, target-typed `new()` and explicit generic calls `M<T>()`), Kotlin vs scip-java 85 / 89 with no extra edge (62 of 65
snippets identical: scip-java's Kotlin indexer records no occurrence for
constructor delegation, `: this(…)` / `: super(…)`; the key keeps those
four edges), Ruby vs scip-ruby 70 / 76 key edges with 1 extra (56 of 61
snippets identical: scip-ruby records no occurrence for `super` — 2 edges
—, does not dispatch through `prepend` — the extra edge is `talk → say`
on the class instead of the prepended module's `say`, and the module's
`super` is unresolved —, does not type an instance variable from `@x =
Foo.new` in `initialize` without a `T.let`, and leaves `new.run` inside a
singleton method untyped).
**C#**: callables are methods (with or without a body), constructors,
destructors, operators, conversion operators and local functions (named as
in the pack: `Type.member`, constructors `Type.Type`, explicit interface
implementations `Type.IFace.member`, local functions `Type.method.local`);
a call on a typed receiver points to the member of the static type
(interface and abstract receivers to the declaration, no expansion to
implementations); `new T(…)`, target-typed `new(…)`, `: base(…)` and
`: this(…)` point to the constructor the arguments choose (none when the
type has no explicit constructor); an extension method called on a value
points to the extension method; property and indexer accessors, operators
used as operators, delegate invocations, method groups (`return Tick;`)
and `nameof` give none; lambdas and LINQ belong to the enclosing method.

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

### Code mode (RFC 0009, experimental) and the multi-step suite (`multistep/`)

Code mode is off unless asked for, so the arms above are unaffected.
`execute` runs through the same interface as the other tools:

```sh
RUECA_EXPERIMENTAL_CODE_MODE=1 $RUECA_BIN query execute '{"code": "<script>"}' --root <root> --store <dir> [--text]
RUECA_EXPERIMENTAL_CODE_MODE=1 $RUECA_BIN query execute '{"language": "starlark"}' --code-file task.star --root <root> --store <dir>
```

(`--code-file FILE|-` reads `code` from a file or stdin; `--text` prints
what a model reads.) Starlark needs a binary built with `--features
code-mode-starlark`; the default build answers `language: "starlark"`
with an error. With `RUECA_CODE_MODE_TIMING=1` the JSON result gains
`timing: {"total_us", "engine_us"}` (the whole `execute` and the sum of its
engine calls, both measured inside the process). Without the variable the
answer is deterministic. `docs/tools.md` ("Experimental: code mode") has
the response and the limits.

**Multi-step efficiency** (RFC 0009 §8, `rueca_bench/multistep/`). 25
multi-step questions over pinned inputs the bench already has, each
answered four ways: its reference **tool-call plan** (`plans.py`: the
calls a competent agent makes, later calls built from what the earlier
compact text answers said, with sensible `limit`s, `labels` and one depth-2
`trace` where that answers the question), its reference **JavaScript** and
**Starlark** scripts run once through `execute` (`multistep/scripts/<id>.js`,
`.star`), and a reference **cbm** plan (`cbm_plans.py`: one `query_graph`
Cypher query when cbm's subset can express the join) where cbm can express
the task at all.

- **Corpora** (`corpora.py`): the S3 repositories `flask`, `requests` and
  `hono` at their `[[s3.repos]]` commits; the Flask cross-repository group
  of `crossrepo.GROUPS` (Flask with Click, Jinja and Werkzeug) as one
  organization; the S2 group `ollama` of `contracts.GROUPS` (the Go server
  and its Python and JavaScript clients) as one organization; the
  multi-ref scenario (`[multiref] instance`: base commit plus the fix
  commit as a ref). Checkouts go to `.cache/work/ms-*`, stores to
  `.cache/multistep/<corpus>/store` (re-indexed only when the members or
  the binary change).
- **Gold** is derived mechanically (`derive.py`) from existing gold, copied
  into `multistep/inputs.json` by `rueca-bench multistep gold --extract`:
  the S3 call graphs, the cross-repository labeler's *uses*
  (`crossrepo.label_python`), the S2 gold pairs and the multi-ref
  `collapse_gold` classes. `multistep/gold.json` holds every answer;
  `tests/test_multistep.py` re-derives it from `inputs.json` (no corpora
  needed) and, when the bench cache holds the S3 gold, re-extracts the S3
  inputs and compares.
- **Tasks** (`tasks.py`): 2-hop callers and an exactly-2-hop count,
  callers ∩ callers and callees ∩ callees, file outline → per-symbol caller
  counts (all methods of a class, the three most called functions of a
  file, the uncalled ones), callees that call nothing, search → filter by
  name → caller counts, the callee with the most callers, callers in a
  directory, callers of a library function / class from another
  repository, routes called by both clients, by one and not the other, the
  OpenAPI operations a client calls, the functions that differ between two
  refs; and five where code mode is not expected to win (one call answers
  them: direct callers, callers outside a file, a depth-2 trace, the
  organization's dependencies, a route's handlers). Each has an
  agent-facing statement (repository ids, files, refs, the exact answer
  shape, no solution steps) that the fluency probe reuses. Answers are
  compared exactly (sets, ordered lists, integers, objects).
- **Measured per arm**: tokens **read** (cl100k_base) — plans and cbm: every
  tool answer's text as the agent receives it; code mode: the script source
  plus `execute`'s text result (or its error line) — with `describe_api`'s
  text (read once per session) reported apart and amortised over the suite;
  tokens **written** (tool-call arguments as JSON; `execute`'s arguments with
  the script); round trips (code mode: one `execute`; its engine calls
  apart); correctness; wall time (each `rueca query` process opens the
  store, so a plan pays that per call: the wall column favours code mode);
  `execute` overhead (`total_us − engine_us`, median and p95); limits hit
  (truncated, partial, timeouts, errors). Headline: **tokens read per
  correct answer**, per arm, by expectation, by task kind and per task, plus
  the tasks every arm answered correctly (paired).
- **Gate** (RFC 0009 §8, per language): at least 40% fewer tokens read per
  correct answer than the tool-call plans, no task correct under the plans
  incorrect in that language, median `execute` overhead under 20 ms (also
  shown with `describe_api` amortised).

```sh
cargo build --release -p rueca --no-default-features --features code-mode-starlark   # lexical only (no llama.cpp); enough for this suite
export RUECA_BIN=$CARGO_TARGET_DIR/release/rueca           # or target/release/rueca
uv run --project bench rueca-bench multistep run [--arm plan,js,starlark,cbm] [--only ms01 ...]
uv run --project bench rueca-bench multistep report         # docs/bench/multistep-results.md from bench/results/multistep.json
uv run --project bench rueca-bench multistep gold [--extract]   # print (and with --extract re-derive) the gold answers
```

`run` needs the S3 gold of `flask`, `requests` and `hono` (`rueca-bench
setup`) only for `gold --extract`; the run itself reads the committed
`inputs.json`. It clones the ollama group on first use (network, three
small shallow clones). Results: `bench/results/multistep.json` (one record
per task and arm) and `docs/bench/multistep-results.md`.

**Probe runner** (for RFC 0009 phase 3's fluency probe):

```sh
uv run --project bench rueca-bench multistep tasks                       # JSON: id, kind, corpus, expect, answer shape, statement
uv run --project bench rueca-bench multistep describe-api --language js --out api-js.txt
uv run --project bench rueca-bench multistep probe --task ms17 --language starlark --script attempt.star
```

`probe` runs the script unchanged (`execute` with its defaults; `language`
only for Starlark) against the task's corpus and prints one JSON line:
`{task, language, correct, error (the execute error line, else null),
result_preview, script_tokens, result_tokens, calls (engine calls),
truncated}`.

The phase 3 probe itself (10 tasks × 2 languages, one fresh model per
cell given only the statement and `describe_api`) is committed in
`multistep/probe/`: its protocol (`README.md`), the prompt template, the
model-written scripts unchanged and the runner's output lines.
`multistep report` adds them to `docs/bench/multistep-results.md`
(*Fluency probe*); the reading is in RFC 0009 §14.

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

**Multi-repository organization.** Members are the manifest's
`[[org.repos]]`, each pinned to one commit by a deterministic rule (every
gated S1 repository at the base commit of its first gated
`[[s1.instances]]` entry, then every calibration S1 repository at the base
commit of its first calibration entry; `org.derive_pins`, checked by a unit
test; the repositories of the languages listed in `[org]
pending_languages` — added after the last full run of the scenario, C# and Kotlin
today — are left out until the next full run re-measures every arm, so
the published organization numbers keep describing the organization they
were measured on). The **gated organization** is the `split = "gated"` members (42); the
**calibration organization** is the gated one plus the `split =
"calibration"` members (24 since the calibration split grew from 8; RFC 0007
phase 0), the gated ones acting as distractors. A member
without S1 instances (a sibling library) is indexed like the others.
Instances, per member of a split, in manifest order: the S1 instances whose
gold is present at the pin — every gold file exists there and, when the
instance has function gold, at least one gold function exists there by
qualified name — and whose fix is not already at the pin (`patch_at_pin`
!= `reverse`, below) — the first `[org] max_instances_per_repo` (3;
`calibration_max_instances_per_repo`, 5, for the calibration split, which is
only ever tuned on). Their gold is
the S1 gold carried to the pin (`gold_at_pin`, cached in
`.cache/gold/org/<id>.json`): the same files, the functions re-resolved at
the pin (their lines at the pin), plus `files_same_blob`, the gold files
whose content at the pin equals the base commit's (a drift signal: when the
pin is later than an instance's base its fix may already be in). The query
is the S1 keyword query of the issue (kind **issue**).

**Impact instances** (`impact.py`; kind **impact**): a library's issue
asked of the organization and scored on the consumers that use what its
fix changes. A *library member* is a member that a consumer member of the
same split depends on (`crossrepo.GROUPS` edges between members: tokio for
axum; carbon, monolog and commonmark for laravel/framework; gson for dubbo
and apollo; none in the calibration split). Every S1 instance of a library
member whose gold is at the pin and whose fix is not there (the issue rule
without the cap) is a candidate; its **library gold symbols** are every
segment of each gold function's qualified name at the pin (the callable the
patch touches and the classes / impl types / traits containing it) and,
for a gold file without function gold, every symbol defined in that file.
The **consumer side** is every non-test source file of a consumer member
whose *uses* — the independent cross-repository labeler run over the whole
consumer with the consumer and its libraries at their organization pins —
reach one of those symbols under the edge scorer's acceptance rule (the
symbol, the class owning it, or a library ancestor: base classes, PHP traits
and interfaces, Rust traits it implements). Candidates with an empty
consumer side are dropped (today 16 gated: carbon 6, monolog 4, gson 3,
tokio 3; commonmark's fixes touch parser classes laravel does not use). Uses are
cached per consumer in `.cache/gold/org/impact/uses/`, records in
`.cache/gold/org/impact/<id>.json`. The run asks `org_questions(split)`:
the issue instances plus the impact instances that are not issue instances
(an id of both kinds is asked once; both kinds are scored from the same
answer). An impact row adds, to the own-side File Acc@5 / Function
Acc@10, **consumer hit@10** and **@20** (a consumer gold file of a consumer
repository among the first 10 / 20 distinct candidate files of the answer)
and **consumer recall@20** (consumer gold files found in the first 20 /
min(their number, 20)).

Arms, every member indexed one at a time from a worktree at its pin (removed
afterwards, so disk stays bounded):

- **Rueca**: **one organization** (RFC 0006), built one repository at a time
  — the bench appends each worktree to an organization file
  (`.cache/org/rueca-org.toml`) and runs `rueca index --org <file> <root>`;
  **native** = one `search_symbols` served with `--org` and no `repo` (one
  ranking over the organization symbol index); **per repository** = one
  `repo:`-scoped search per member in the same store, merged by reciprocal
  rank fusion (k = 60; one item per repository at each rank, ties broken by
  the raw per-repository score) — what an agent without an org query must
  do; **single** = the own member's list of that sweep. The calibration
  organization is a copy of the gated store (copy-on-write on APFS) with
  the calibration members added the same way.
- **grep (BM25)**: **native** = one BM25 ranking over the union of the
  members (`bm25.union_search`: document count, lengths and term
  frequencies of the union; file documents rank files, function-body
  documents rank functions, as in S1); **per repository** = each member's
  own BM25 ranking merged by RRF; **single** = the own member's ranking.
  Each member's index is restricted to the terms of the run's queries and
  dropped, so one full index is in memory at a time. No tokens (grep has no
  rendered answer, as in S1).
- **cbm**: one project per member; **per repository** = one `search_graph`
  per project merged by RRF; **single** = the own project's list (cbm has
  no cross-project search).

Scored like S1 (File Acc@5, Function Acc@10) with repository-qualified
paths and the candidate filter of every language, against the same arm's
single answer in the same run (paired per instance); tokens of the top-10
answer (per repository: the sum of its calls) and the share of the top 10
taken by other repositories. Raw outputs
`.cache/runs/org/multirepo-{gated,calibration}-<arm>.json` (shape in the
`org.py` docstring).

RFC 0006 §8 metrics (Rueca, report table "organization suite"): File Acc@5
at organization scope over the single answer (floor ≥ 0.85×, target ≥
0.95×); **tokens per correct answer** at organization scope — Σ tokens of
the top-10 answers / number of queries whose top 10 holds a gold file of
the instance's own repository — against the same ratio of the single
answers (target ≤ 1.2×); **incremental update** — the organization-layer
time `org_ms` that every `index --org` step reports (adding one repository)
and, after the gated members, of re-indexing the repository with the most
symbol documents (its part replaced), each over that repository's own index
time (floor ≤ 2×, target ≤ 1×; the per-step row is judged on its maximum).
Since RFC 0006 phase 2 the native answer is weighed by repository routing
and the organization-layer times include routing profiles, package
registries and link sets.

**Statistics** (`orgstats.py`, per split). Every 0/1 rate carries its
Wilson 95% interval and n; the organization / single ratio of File Acc@5
and the tokens-per-correct ratio carry percentile-bootstrap intervals
(instances resampled). Paired comparisons, A − B over the instances both
arms answered (paired bootstrap, `[scorecard]` resamples and seed,
`seeded(base, "org", split, kind, metric, arm_a, mode_a, arm_b, mode_b)`):
Rueca native − BM25 native, Rueca native − cbm per repository, Rueca native
− Rueca per repository and BM25 native − BM25 per repository, on File Acc@5
and Function Acc@10 (instances with function gold) for issue instances and
on consumer hit@10 and own File Acc@5 for impact instances. Status as RFC
0004 §6.1 reads a relative floor (✅ the 95% interval ≥ 0, ⚠ it crosses 0,
❌ it is below 0). Each comparison reports the **discordance** d (share of
instances where one arm hits and the other misses), the observed
half-width, and the **minimum detectable effect** at 80% power and
two-sided α = 0.05 for a paired binary outcome, **MDE ≈ (1.96 + 0.84) ·
√(d / n)**, next to the MDE the 21-instance scenario would have had with
the same d (what the larger population bought). The scorecard's `org` block
(`orgstats.org_block`, gated split only) holds these per arm and mode, the
comparisons, the impact kind, the RFC 0006 §8 rows with floors, targets and
statuses, the cross-repository pools (all pairs, without Rust, Rust alone),
the instance counts per language, kind and `patch_at_pin` stratum, and the
settings (caps, members, sha256 of the `[[org.repos]]` list); it is not a
language gate (`rueca-bench check` reads `languages` and `config` only).

**Carrying.** Published multi-repository scores are carried only for the
same population (`org.population`: sha256 of the members, their pins and
the instance ids with their kinds): the whole split when nothing was run,
an arm when that arm was not run. The 21-instance scenario is another
population and is never carried; an arm without output is *pending*.

**Plugging a new arm into the organization scenario.** An arm (e.g. the
semantic-search arm of RFC 0007) is added to `org.ORG_ARMS` with a
`_multirepo_<arm>(arm, pending, insts, gated, extra)` that returns, per
split in `pending`, `_record(members, insts[split], index, answers, ...)`.
What it must provide: **index the members one at a time** from
`add_worktree(repo, pin)` (remove the worktree after indexing; the
calibration organization is the gated one plus `extra`); for each question
(`inst["query"]`, `inst["repo"]`) an answer `{native?, per_repo, single}`
where every list is repository-qualified items `[repo, path, name,
start_line, label]` (`_items`; top 50) — **native** = one organization
query returning hits that name their repository (leave it out when the arm
has none), **per_repo** = one query per member merged with `rrf_merge`,
**single** = the own member's list; an arm with a separate file ranking
adds `files: [[repo, path]]`. Tokens come from what an agent reads: `eff`
= `_eff(arm.search_eff(...))` of the native top-10 answer and of the
single answer, and `tokens` = the sum over per-member top-10 answers
(`null` when the arm has no rendered answer, as BM25). Scoring, statistics
and the report pick the arm up from its raw output
(`.cache/runs/org/multirepo-<split>-<arm>.json`); add it to
`orgstats.ARMS` and to `orgstats.COMPARISONS` for its paired rows.

**Routing calibration** (RFC 0004 §7.7; `uv run --project bench python -m
rueca_bench.org calibrate [ROUTE ...] [--force]`, not part of `run`).
Repository routing is tuned on the calibration split only: the calibration
instances of the multi-repository scenario (`org_instances("calibration")`)
are asked of its calibration organization (the gated members as distractors
— their documents only, none of their instances is asked — plus the
calibration members, each at its pin) under each routing setting
(`RUECA_ORG_ROUTE`: `off`, `prior,alpha=…,mu=…,eps=…`, `top,top=…,ratio=…`,
CORI constants, profile field weights, `stats=local`), and of each
instance's repository alone (`repo`, no `--org`) in the same store. Scored
like the multi-repository scenario (File Acc@5 against the gold at the pin,
other-repository share of the top 10, tokens per correct answer,
organization over single). The store is kept between runs
(`.cache/org/calib-store`), so a sweep costs ≈ 30 s per setting; raw output
`.cache/runs/org/calibration-rueca.json`. The chosen default (prior, α 2,
μ 0.5, ε 0.25, CORI 50 / 150) and the sweep that chose it (35 instances of
the 7 calibration repositories of the time, 21 gated distractors) are in
[RFC 0006 §9.2](../docs/rfcs/0006-org-graph.md).

**Cross-repository edges** (RFC 0006 §8, `crossrepo.py`, part of `--suite
org`). Consumers pinned with the libraries they depend on (`crossrepo.GROUPS`,
12 pairs): Flask 3.1.3 with Click 8.1.8, Jinja 3.1.6 and Werkzeug 3.1.5;
Laravel (the S1 organization commit) with Carbon (its S1 commit), Monolog
3.9.0 and league/commonmark 2.8.3; Apache Dubbo and Apollo (S1 commits)
with Gson 2.15.0; Axum (its organization pin) with Tokio (its organization
pin), Hyper 0.14.27 and Tower 0.4.13; Tokio with Mio 1.0.1 (Rust: the Cargo
labeler of the `crossrepo.py` docstring). Each group is
indexed as one organization (`rueca index --org`) and its link layer listed
with `rueca links --org --json`. Gold, labelled from the consumer's source
by an independent labeler (Python `ast`; PHP and Java regular expressions
over `use` / `import` statements, the library's PSR-4 map or package
directories): **imports** — (consumer file, library file) of every import
of a library module or class (Python: the module's file, plus `m.x` when it
is a submodule; PHP: top-level `use` of a library class; Java: single-type
and static imports; on-demand imports are directories and left out);
**uses** — (consumer file, library symbol) for every library class or
function imported by name and used outside the import lines, Python
attribute chains through an imported library module (following the
library's re-exports), and PHP fully qualified library class names. A
non-`IMPORTS` edge observed in the consumer is correct when its target is a
used symbol, a member of it or of one of its library ancestors (bases,
PHP traits and interfaces), or a library ancestor of a consumer class the
file names (`Illuminate\Support\Carbon extends Carbon\Carbon`); a gold
use is found when one of the file's edges hits it that way. Precision =
correct / predicted, recall = found / gold, pooled; precision is a lower
bound (a method of an object a library call returned is invisible to the
labeler). RFC 0006 §8: floor ≥ 90 / 75, target ≥ 97 / 90, judged on the
pooled counts with Wilson intervals; the report pools all pairs, the pairs
without Rust and the Rust pairs alone. Raw output
`.cache/runs/org/crossrepo-rueca.json`.

**Multi-ref.** `[multiref] instance` (psf__requests-2317): the repository at
the base commit and at a fix commit (base + the instance's gold patch,
committed by the bench with a fixed identity and date, so its id is
deterministic). The base worktree is indexed, then the fix commit as a ref
snapshot of the same repository (`rueca index <base> --ref <fix commit>`,
RFC 0006 §9.3: exported from the object database, facts shared by blob;
the report keeps each step's files extracted and facts-cache hits). Gold
(`collapse_gold`): every callable of the changed files classified `changed`
(its text differs), `added`, `removed` or `same_in_changed_file`, and every
symbol of an unchanged file `identical` (same blob: one item listing both
snapshots). Queries: every changed / added / removed function name and a
fixed sample of unchanged ones, each a `search_symbols` with `ref: [base,
fix]` against the two single-ref answers (`ref: base`, `ref: fix`: the
uncollapsed baseline, in tokens); `score_collapse` counts collapsed
identical symbols, `in` lists and `changed` flags, matching items to gold
by path and file-local name (`local`). Rueca compared versions by file
blob until RFC 0009 §14.6, so an unchanged callable of a changed file came
back flagged; it is counted apart (`changed_same_file`, 0 since `changed`
compares each symbol's own text), not as a false positive. Trace collapse
needs per-commit call-graph gold and is not scored yet.

### Cross-service edges (S2, RFC 0004 §4, `contracts.py`)

Contract edges of the organization link layer (RFC 0006 phase 4): HTTP
routes, OpenAPI operations and protobuf / gRPC services matched across
repositories. `--suite s2` (Rueca only) indexes each group of
`contracts.GROUPS` as one organization (`rueca index --org`), reads its
link layer (`rueca links --org --json`, edges with `strategy: contract`)
and its dangling consumers (`rueca contracts --side dangling`). Groups,
pinned to commits: **ollama** (ollama/ollama — gin server and
`docs/openapi.yaml` — with ollama-python and ollama-js: *http-code*,
*http-spec*), **qdrant** (qdrant v1.20.2 — actix routes, tonic services,
`.proto` — with qdrant-client: *http-code*, *rpc-spec-client*,
*rpc-code*), **dapr** (dapr/dapr — `.proto`, Go runtime — with python-sdk
and go-sdk: *rpc-spec-client*, *rpc-spec-server*, *rpc-code*),
**weaviate** (weaviate — Swagger 2 spec, `.proto`, Go gRPC server — with
weaviate-python-client: *http-spec*, *rpc-spec-client*, *rpc-code*);
**openai** (openai-openapi with openai-python v3.52.0, *http-spec*) is a
diagnostic group, not gated: its 4–5 MB specs exceed the 2 MiB extraction
budget.

**Gold** (`contracts_gold/`), computed on the fly from the pinned checkouts
by labelers independent of Rueca: Python `ast` (HTTP calls with a literal,
f-string, concatenated or `path_template(...)` path and a literal or
`method=client.get` method; gRPC stub calls through names bound to
`<Service>Stub(...)` or annotated `-> <Service>Stub`; grpcio servicers),
tree-sitter for TypeScript calls (verb wrappers, `fetch`, one hop through a
function that interpolates its parameter into the URL), regular
expressions for Go (gin / echo routes with group prefixes and their
handlers; `net/http` ServeMux registrations with a literal pattern,
Go 1.22 `"GET /path"` method patterns included; gRPC clients bound to
`<Service>Client`; server methods by rpc name and signature, on the type
embedding `Unimplemented<Service>Server` when there is one, request /
response types from the service's `go_package`), Rust (actix route
attributes; tonic `impl <Service> for <Type>`, generic wrapper types
`impl<T: S> S for W<T>` included), `.proto` files and OpenAPI 3 /
Swagger 2 documents. Python verb calls read the path from a `url=` /
`path=` keyword when there is no positional argument
(`self._connection.get(path="/nodes")`), and a stub or servicer module
that re-exports a generated one under another name
(`from dapr.proto import appcallback_service_v1`) takes the package of
the repository's generated modules that declare the service when they
agree. Keys use the
canonical form of `rueca_model::contract` (`contracts_gold/keys.py`); a
consumer is paired with the providers of its best match quality (exact >
template > method wildcard; the proto package when the client's generated
module or Go package tells it). Gold pairs are cross-repository only, at
(source file → target file + symbol): *http-code* (client → handler),
*http-spec* (client → `METHOD /path` operation), *rpc-spec-client* (stub
call → `.proto` rpc), *rpc-spec-server* (server method → `.proto` rpc,
IMPLEMENTS, the method must match too), *rpc-code* (stub call → server
method). Tests and generated stubs (`_pb2*.py`, `*.pb.go`, Go files marked
`Code generated`) are out of scope for the gold and for scoring.
`python -m rueca_bench.contracts gold` labels every group without Rueca
(`.cache/runs/org/contracts-gold.json`, about a minute with cold clones);
`… audit` prints a fixed sample of 15 pairs per kind with their evidence.

**Scoring.** A contract edge is classified by its target (`RpcMethod` /
`.proto`, `Route` / spec file, else a handler or implementation — HTTP or
RPC by the provider's contract kind) and is correct when it hits a gold
pair of that kind (same repositories and files, an accepted target name:
handler or method name, `Service.Method`, the operation's canonical key).
Precision = correct / predicted, recall = found / gold, pooled over the
gated groups per kind and overall, percentile bootstrap over source files.
Edges are scored **at every confidence** (the gated row: ambiguous edges,
confidence 0.5, count like the others) and again **at confidence ≥ 0.7**
(`summary_high`, not gated: what an agent that drops ambiguous edges
keeps). Floors ≥ 90 / 75, targets ≥ 97 / 90 (the scorecard family
*Cross-service edges*, gated per kind; the pooled *all* row is shown, not
gated). Edges of a kind a group does not label (qdrant's versioned specs,
`http-spec` of the dapr runtime) are counted as unscored. Raw output
`.cache/runs/org/contracts-rueca.json`, results `bench/results/s2.json`.

**Measuring and publishing.** `RUECA_BIN=<release binary> uv run
rueca-bench run --arm rueca --suite s2 --force` (about two minutes for the
five groups with warm clones), then `uv run rueca-bench report --s2-only`:
it writes `bench/results/s2.json`, sets the `contracts` family of
`bench/results/scorecard.json` and replaces the S2 row of the scorecard
summary and the *Cross-service edges* section of
`docs/bench/alpha-results.md`, leaving every other published number as it
is (a full `report` re-scores S1 / S3 from the local cache, carrying what
it does not hold). `python -m rueca_bench.contracts score` prints the
pooled summary of the last run.

**Results** (2026-11-17, feat/org-contracts, pooled over the gated groups,
every confidence): http-code P 98.3 (116/118) / R 98.0 (97/99); http-spec
100.0 (131/131) / 100.0 (110/110); rpc-spec-client 96.1 (219/228) / 100.0
(212/212); rpc-spec-server 100.0 (18/18) / 90.0 (18/20); rpc-code 95.5
(277/290) / 95.4 (270/283); all 96.9 / 97.7. Every gated floor is met
(rpc-spec-client and rpc-code precision ⚠: the interval crosses 90). What
is left: **ollama** `POST /api/create` registered in `routes.go` with its
handler in `create.go` (a handler in another file: the row is owned by the
registering function, 2 wrong + 2 missed); **dapr** go-sdk clients that
only know the method (`c.protoClient.X` with the field typed in another
file, `*/X`) also reach the `Scheduler` and `Operator` protos (4 wrong,
0.5); the durabletask `TaskHubSidecarService`, whose `.proto` is outside
the group, has no gold provider (9 edges: 6 to `fallbackAwareServer`,
which embeds the service's server interface, 3 to the actors backend's
same-shaped methods); `BulkPublishEvent` / `ConverseAlpha1/2` take
requests not named after the method (`BulkPublishRequest`,
`ConversationRequest`: the name-only server shape misses them, 12 pairs);
a Python servicer with two `…Servicer` bases is read as the first service,
so its `AppCallbackAlpha` methods are missed (3 pairs); **weaviate** stub
calls `Weaviate/Search`, `Weaviate/BatchObjects` match both the `v0` and
`v1` services (no package in the client code: 8 wrong, 0.5); **qdrant**
the server's own Rust gRPC client (not labeled) reaches the client
repository's copy of `qdrant.proto` (1 wrong, 0.5). Rueca's dangling
consumers are mostly test calls and calls to third-party services (`rueca
contracts --side dangling`); the labelers' count is lower because they
skip tests.

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

### Service (RFC 0008 §8.2–§8.3, `service.py`)

The organization served by `rueca serve` (phase 1, the query role) and
asked over MCP-over-HTTP, against `rueca query` on the same snapshots.
Self-contained: everything lives in `.cache/service/` (removed after the
run unless `--keep`).

1. **Organization.** `--small`: the two repositories of
   `bench/service/small.toml` (this checkout's `crates/rueca-storage` and
   `bench/src/rueca_bench`, cut with `git archive HEAD:<path>` and committed
   with a fixed identity and date) and its twelve keyword queries, indexed
   with one `rueca index --org`. Otherwise the multi-repository scenario's
   members of `--split` (default `gated`; `--only` restricts the members),
   indexed one at a time from worktrees at the pins as the org suite does,
   and its questions' keyword queries (read from
   `.cache/runs/org/multirepo-<split>-rueca.json` when it exists for the
   same filter, else selected again).
2. **Store.** `rueca publish --repo <id> --org bench` per member into a
   `file://` service store, then `rueca pull --generation` into a separate
   mirror, which builds the organization and publishes generation 1 (the
   hidden phase 0 commands; phase 2's updater will do this).
3. **Serve.** `rueca serve --role query` with a generated
   `rueca-service.toml`: free local ports, its own cache directory,
   `ready_when = "index"`, one static token (`sha256:` of a random
   `rueca_…`, `repos = ["**"]`), rate limits set out of reach. **Time to
   ready** = process start → first `200` from `/readyz` (`/healthz` is
   recorded too). One request without a token records the `401` and its
   `WWW-Authenticate`.
4. **Calls.** A minimal MCP client (`http.client`, one keep-alive
   connection, `Authorization: Bearer`, `MCP-Protocol-Version: 2025-06-18`,
   `Mcp-Method` / `Mcp-Name`): `initialize` and `notifications/initialized`,
   then one `search_symbols` `{"query", "limit": 10}` per query, then
   `--sample` (20) `get_symbol` calls and up to as many `trace` calls
   (`direction: both`, `depth: 2`, callable symbols only) on every k-th
   distinct result id of the search answers. `--repeat` (3) replays the
   list; only the first pass is checked.
5. **Differential** (RFC 0008 §8.3). Every first-pass answer: the HTTP
   `structuredContent` must equal `rueca query <tool> '<args>' --org bench
   --store <mirror>` (parsed JSON), and the text content must equal the
   same call with `--text`. The report gives JSON and text pass counts, per
   tool, and the first difference (JSON path, both values).
6. **Latency.** Client: per tool p50 / p95 / max over every pass
   (`client`) and over the first, cold pass (`client_first_pass`),
   nearest rank. Server: `/metrics` after the run, `rueca_mcp_request_seconds`
   summed over scopes per tool (and per tool and scope), p50 / p95
   interpolated inside the buckets as Prometheus' `histogram_quantile`
   (bucket bounds 1, 2.5, 5, 10, 25, 50, 75, 100, 150, 250 ms …, so a p95
   is only as precise as its bucket), plus `rueca_mcp_requests_total` by
   outcome. The RFC 0008 §8.1 floor (lexical `search_symbols` p95 ≤ 100 ms,
   server side, warm) reads `server.tools.search_symbols.p95_ms`.
7. **Sizes.** Binary, service store, harness mirror, the server's cache
   directory and the indexing store (bytes); server RSS after the calls;
   the shutdown (SIGTERM → exit code, seconds).

Not yet (RFC 0008 §8.2, later phases): an S3-compatible store, concurrent
clients. Latency is measured with one client and one connection. Indexers,
webhooks and faults are the next suite's.

### Service indexing (RFC 0008 §8.2–§8.3 phase 2, `service_index.py`)

The indexer role end to end: the organization indexed from URLs by 1 and N
indexers, push → queryable through signed webhooks, and fault injection.
Self-contained in `.cache/service-index/` (removed unless `--keep`). The
binary must be a **debug** build (`cargo build -p rueca`): the local git
host is reached through the test-only `[indexing].allow_file_urls`, and
the looping-parser fixture (`RUECA_TEST_WORKER_FAULT`) exists only in
debug builds.

1. **Members.** `--small`: the two repositories of
   `bench/service/small.toml`, one commit each. Otherwise the split's
   members (`--only` restricts), each cut at its pin into a one-commit
   repository with `git archive` (needs `setup`).
2. **Local baseline.** `rueca index <member>` once per member, timed one
   at a time; the sum is the reference for the cold index.
3. **Git host.** Bare clones (`uploadpack.allowFilter = true`) served by
   `git daemon --export-all --base-path` on an ephemeral loopback port:
   `git://127.0.0.1:<port>/<name>.git`, protocol v2, blob filters and
   commit wants work as on a hosted server. Chosen over `git http-backend`
   behind a CGI shim: one process, no Python server, the same on macOS and
   Linux; the cost is that `git://` is accepted only by the test flag.
4. **Runs** (`--indexers`, default `1 4`; fresh host and `file://` store
   each): N = 1 runs one `rueca serve --role all`; N > 1 one `--role
   query` replica and N `--role indexer` processes, `org_updater` on the
   first only. Discovery, the updater tick and the replica's poll are 1 s;
   polling of the host waits at least 5 min, so pushes travel by webhook;
   `worker.wall_time` is 15 s (or 4 × the slowest local index).
   - **Cold index**: process start → every member's commit served by the
     replica (`/status`), next to the local sum; `rueca status
     --store-url` catalogs and the suite's `search_symbols` answers are
     compared with the first run's (`equal_to_first`).
   - **Push → queryable** (`--pushes`, 10): a commit adding a function
     `rueca_push_marker_<run>_<i>`, pushed to the host, then a signed GitHub
     `push` delivery (`X-Hub-Signature-256`, a fresh `X-GitHub-Delivery`)
     to one indexer, round robin; from the delivery to the first
     `search_symbols` answer naming the function (polled every 100 ms).
     p50 / p95 (nearest rank) and `rueca_publish_seconds` per stage
     (`fetch`, `export`, `index`, `upload`, `generation`) over the pushes
     alone: bucket deltas of every indexer's `/metrics`, quantiles as
     `histogram_quantile`, as precise as the buckets.
5. **Faults** (the last run; `--no-faults` skips), each `ok` with its
   evidence: `looping_parser` (an on-demand ref whose worker spins is
   killed at the wall-clock limit and fails alone), `worker_killed` (a
   worker SIGKILLed from outside: the ref fails alone, the indexer lives),
   `lease_taken` (the job object rewritten mid-job with a forged, expired
   lease of epoch + 1: the running indexer loses its finish, another lease
   re-runs the job, the served default commit never regresses),
   `webhooks_duplicate_reordered` (newest delivery first, then the older,
   then the newest again: `accepted`, `accepted`, `duplicate`; the newest
   commit is served and stays), `storage_unwritable` (`chmod -R a-w` on the
   store for 8 s around a delivery: its confirmation fails and is
   forgotten, the redelivery with the same id after the window is accepted
   and the commit becomes queryable). Not injected: a bucket answering 409
   or 503, which needs an S3-compatible store.

Output: `bench/results/service-index-<UTC timestamp>.json`: `local`,
`runs[]` (`cold`, `catalog`, `answers`, `push`, `faults`, `equal_to_first`)
and the summary printed at the end.

## Datasets, tools and licences

| Asset | Pinned | Licence | Use |
|---|---|---|---|
| SWE-bench Verified (`princeton-nlp/SWE-bench_Verified`) | HF revision + sha256 | none stated on the card (SWE-bench code: MIT) | S1 issues, patches |
| SWE-bench Lite (`SWE-bench/SWE-bench_Lite`) | HF revision + sha256 | none stated on the card | S1 issues, patches |
| SWE-PolyBench (`AmazonScience/SWE-PolyBench`) | HF revision + sha256 | MIT | S1 TS and Java issues, patches, `modified_nodes` gold; the Python calibration repository yt-dlp |
| SWE-bench Multilingual (`SWE-bench/SWE-bench_Multilingual`) | HF revision + sha256 | MIT | S1 TS/JS issues, patches |
| LocAgent / Loc-Bench labels (`czlll/SWE-bench_Lite`) | HF revision + sha256 | none stated on the card (LocAgent code: Apache-2.0) | S1 function gold |
| ContextBench (`Contextbench/ContextBench`) | HF revision + sha256 | none stated on the card (ContextBench code: Apache-2.0) | S1 context-file gold |
| SWE-rebench V2 (`nebius/SWE-rebench-V2`) | HF revision + sha256 | CC-BY-4.0 (attribution: Nebius, SWE-rebench V2) | S1 PHP issues, patches (Twig, monolog, commonmark); S1 C# issues, patches (kiota, moq and six smaller repositories); calibration repositories of every language but Rust |
| Multi-SWE-bench (`ByteDance-Seed/Multi-SWE-bench`), four C++, three Rust, two C, two Go, two Java and one JS JSONL file | HF revision + per-file sha256 | CC0 (dataset README; upstream repository licences apply) | S1 C++ issues, patches (fmt, nlohmann/json, Catch2, simdjson); calibration repositories: Rust (bytes, tracing, fd), C (zstd, ponyc), Go (grpc-go, go-zero), Java (jackson-core, mockito), JS (dayjs) |
| PyCG micro-benchmark (`vitsalis/PyCG`) | commit | Apache-2.0 | S3 Python call-graph gold (diagnostic) |
| PHP micro-benchmark (`bench/micro/php`) | in repository | Apache-2.0 (Rueca's own labels) | S3 PHP call-graph gold |
| Python / TS/JS / Go / Java / C / C++ / Rust / shell / C# / Kotlin micro-benchmarks (`bench/micro/{python,ecma,go,java,c,cpp,rust,shell,csharp,kotlin}`) | in repository | Apache-2.0 (Rueca's own labels) | S3 call-graph gold |
| Env bridge labels (`bench/labels/bridge`) | in repository | Apache-2.0 (Rueca's own labels) | data-format suite gold |
| PHPStan (`phpstan/phpstan`) release PHAR | version + sha256 | MIT | S3 PHP gold (run in Docker, never vendored) |
| `php:8.3-cli` Docker official image | digest | PHP-3.01 (separate process) | runtime for PHPStan |
| tree-sitter-php, -go, -java, -c, -cpp, -rust, -bash, -c-sharp (PyPI) | `uv.lock` | MIT | bench labeler for PHP, Go, Java, C, C++, Rust, shell (and the shell S3 gold), C# |
| fwcd/tree-sitter-kotlin (ast-grep's release, git commit `1a6f9b1`, built from source by uv) | `uv.lock` | MIT | bench labeler for Kotlin |
| PyYAML, python-hcl2 (+ lark, regex) (PyPI) | `uv.lock` | MIT / MIT (+ MIT, Apache-2.0) | data-format reference parsers |
| numpy (PyPI) | `uv.lock` | BSD-3-Clause | bootstrap intervals |
| tiktoken (PyPI) + the `cl100k_base` encoding file | `uv.lock`; the encoding by tiktoken's own sha256 | MIT | efficiency (tokens per correct answer) |
| scip-python / scip-typescript | npm version | MIT / Apache-2.0 | S3 gold |
| scip-go 0.2.9 | release archive + binary sha256 (darwin-arm64, linux-amd64) | Apache-2.0 | S3 Go gold, Go micro cross-check |
| scip-clang 0.4.1 | release binary sha256 (darwin-arm64, linux-amd64) | Apache-2.0 | S3 C / C++ gold, micro cross-checks |
| scip-java 0.13.2 | release launcher sha256 (JVM, any platform) | Apache-2.0 | S3 Java gold, Java micro cross-check |
| scip-java 0.14.2 (Kotlin Analysis API indexer) | release launcher sha256 (JVM, any platform) | Apache-2.0 | S3 Kotlin gold, Kotlin micro cross-check |
| kotlin-stdlib / kotlin-reflect 2.3.21, okio-jvm 3.16.4, jsr305 3.0.2 (Maven Central) | sha256 | Apache-2.0 | classpath of the Kotlin gold |
| rust-analyzer 2026-11-09 (`0.3.3113-standalone`, e5e7830640) | release archive (`.gz`) + binary sha256 (darwin-arm64, linux-amd64) | MIT OR Apache-2.0 | S3 Rust gold (`rust-analyzer scip`), Rust micro cross-check; runs the host's `cargo` (crates.io dependencies of the indexed repositories are downloaded into the cache, never vendored) |
| scip-dotnet 0.2.16 (`sourcegraph/scip-dotnet`, `linux/amd64` image with the .NET SDK 10.0.402) | Docker image digest | Apache-2.0 | S3 C# gold, C# micro cross-check; restores the indexed projects' NuGet packages from nuget.org into the cache (never vendored) |
| `eclipse-temurin:21-jdk` Docker official image | digest | GPL-2.0 with Classpath Exception (separate process) | runtime for javac + scip-java |
| `error_prone_annotations` 2.50.0 (Maven Central) | sha256 | Apache-2.0 | compile-only classpath for the gson gold |
| codebase-memory-mcp v0.12.0 | release URL + archive and binary sha256 (darwin-arm64, linux-amd64) | MIT | `cbm` arm |
| Corpus repositories | commit SHAs | their own (requests Apache-2.0, flask BSD-3, jinja BSD-3, click BSD-3, pytest MIT, pylint GPL-2.0, sphinx BSD-2, tailwindcss MIT, code-server MIT, vue MIT, preact MIT, axios MIT, hono MIT, zod MIT, express MIT, yargs MIT, carbon MIT, php-cs-fixer MIT, laravel/framework MIT, phpspreadsheet MIT, Twig BSD-3, monolog MIT, commonmark BSD-3, JsonMapper MIT; caddy Apache-2.0, gin MIT, prometheus Apache-2.0, hugo Apache-2.0, terraform BUSL-1.1 (source-available), urfave/cli MIT, goawk MIT; gson Apache-2.0, javapoet Apache-2.0, eo-yaml BSD-3, lombok MIT, dubbo Apache-2.0, rocketmq Apache-2.0, apollo Apache-2.0; redis BSD-3 before 7.4, RSALv2 / SSPLv1 (and AGPLv3 from 8.0) after (source-available at most pinned commits), jq MIT, micropython MIT, valkey BSD-3, cJSON MIT, Lua MIT (text in `lua.h`), nng MIT; fmt MIT, yaml-cpp MIT, Botan BSD-2, nlohmann/json MIT, Catch2 BSL-1.0, simdjson Apache-2.0; tokio MIT, bat MIT OR Apache-2.0, axum MIT, ruff MIT, coreutils MIT, nushell MIT, ripgrep Unlicense OR MIT, bytes MIT, serde_json MIT OR Apache-2.0; nvm MIT, acme.sh GPL-3.0, bash-it MIT, bats-core MIT; microservices-demo Apache-2.0, awesome-compose CC0-1.0, terraform-aws-vpc Apache-2.0) | cloned at runtime as benchmark input only; nothing vendored or copied (RFC 0002 §2 governs what enters the repository) |

Citations: Jimenez et al., *SWE-bench* (ICLR 2024); OpenAI, *SWE-bench
Verified* (2024); Rashid et al., *SWE-PolyBench* (2025); Chen et al.,
*LocAgent* (ACL 2025); EuniAI, *ContextBench* (2026); Salis et al., *PyCG*
(ICSE 2021); Badertdinov et al., *SWE-rebench* (Nebius, 2025) and SWE-rebench
V2 (CC-BY-4.0); SWE-bench team, *SWE-bench Multilingual* (2025); Ondřej
Mirtes et al., *PHPStan*; Zan et al., *Multi-SWE-bench* (2025); Sourcegraph /
SCIP steering committee, *scip-go*, *scip-java*, *scip-clang*; the
rust-analyzer team, *rust-analyzer* (its `scip` command).

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
- **Calibration split.** 15 instances per language (13 for C++) on three
  repositories each, from SWE-rebench V2 (LLM screened, train split),
  Multi-SWE-bench and SWE-PolyBench: 118 pooled, still wide intervals per
  language, meant for tuning, not for claims. PHP and C++ draw on
  SWE-rebench V2 only.
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
- **Equivalence classes are lenient by construction.** Members of a class
  are interchangeable for scoring, so an arm that picks the wrong
  same-signature overload, specialization or `#if` alternative is not
  penalised (the gold cannot tell which is right), and a callers question
  on a class pools the callers of all its members. The strict row shows the
  numbers without classes; the gold table counts classes, their callables
  and the gold edges that point to a class per set. Classes come only from
  the indexer (one symbol, several definitions) or from definitions it did
  not compile; same-named callables the indexer tells apart are never
  merged.
- **SCIP gold for Go, Java, C/C++** has the usual SCIP blind spots: interface
  dispatch (scip-go, scip-java resolve to the interface method only, so
  implementations get no caller edges), function pointers, macros
  (scip-clang attributes a call written in a macro body to the expansion
  site, which tree-sitter tools cannot see), `static` functions with the same
  name in different C files (one SCIP symbol, scored as one equivalence
  class). The fmt and yaml-cpp golds
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
- **scip-dotnet gold for C#** (serilog, eShopOnWeb, YamlDotNet, micro):
  - the index is Roslyn's for one configuration: multi-targeting projects
    are indexed for one framework (`net8.0`, manifest `dotnet.framework`),
    so the other targets' `#if` branches have no definitions;
  - scip-dotnet records `new T(…)` as a reference to the type, not to the
    constructor: the gold turns it into a call of `T`'s constructors, one
    equivalence class when `T` has several (the overload is not recorded);
  - no occurrence at all for constructor initializers (`: base(…)`,
    `: this(…)`), target-typed `new(…)`, and calls or constructions with
    explicit generic arguments (`M<T>()`, `new Box<T>()`), so those true
    calls are not gold edges (an arm reporting them loses precision);
  - a call of a constructed generic method is recorded with the symbol of
    the first overload of its name (`Write().` for `Write<T>(…)`); the gold
    sends such a call to the generic overloads that take its argument
    count (an equivalence class when several);
  - local functions are `local` symbols, bound per document.
- **scip-java gold for Kotlin** (moshi, clikt, kotlinpoet, micro):
  - the Kotlin indexer analyses the listed sources as one JVM module:
    Kotlin Multiplatform `expect` / `actual` pairs (kotlinpoet's
    `commonMain` / `jvmMain`) do not resolve, which is why kotlinpoet is the
    calibration repository;
  - constructor delegation (`: this(…)`, `: super(…)`) has no occurrence,
    and a supertype constructor call (`: Base(x)`) references the class,
    not its constructor: neither is a gold edge, while Rueca reports both
    as calls of the constructor;
  - an annotation written with arguments (`@Delicate(message = …)`) is a
    call of the annotation class's constructor in the gold (a call site
    under the labeler's `constructor_invocation` rule);
  - an extension function's symbol is owned by its receiver's simple name
    in the file's package (`a/b/String#ext().`), so extensions of two
    receivers with one simple name are one equivalence class (six groups
    in kotlinpoet, none in the gated repositories);
  - a primary constructor written with `constructor` on a line of its own
    is defined on that keyword: the gold binds it to the callable whose
    extent (the class header) holds that line.
- **rust-analyzer gold for Rust** (serde_json, ripgrep, bytes, micro):
  - calls on a generic `T: Trait`, an `impl Trait` parameter or a `dyn
    Trait` receiver resolve to the trait method only, so trait impls get no
    caller edges from generic code (monomorphization is not followed);
    calls on a concrete receiver resolve to the impl method;
  - code generated by macros is invisible in the source: calls written in a
    `macro_rules!` body give no edge (rust-analyzer resolves no tokens of a
    macro body and, unlike scip-clang, does not attribute them to the
    expansion site), items a macro defines (`deserialize_any!`,
    `delegate_iterator!`, tokio's `cfg_rt! { … }` blocks, which tree-sitter
    keeps as unparsed tokens) are not callables, and derived impls
    (`#[derive(Clone)]`, serde derives) have no definition in the source —
    rust-analyzer logs these as `Bug: definition … should have been in an
    SCIP document`, which is noise, as is `Encountered enclosing definition
    with no name` (the crate root module);
  - calls written in macro arguments (`println!`, `format!`, `assert!`,
    serde_json's `tri!`) are gold edges: rust-analyzer resolves them through
    the expansion. They are 172 of serde_json's 1045 gold edges, 86 of
    ripgrep's 2276 and 7 of bytes' 467; an arm that does not look inside
    macro arguments loses that recall;
  - only the default cargo features and the host platform are compiled:
    `#[cfg(...)]`'d-out items (serde_json's `arbitrary_precision`,
    `raw_value`, `preserve_order`; `#[cfg(windows)]` code on macOS / Linux)
    have no SCIP definition. A cfg'd-out item with the qualified name of a
    compiled one in the same file joins its equivalence class (serde_json 9,
    ripgrep 21, bytes 3); the others stay callables without edges, and a
    file compiled only for another platform has no SCIP document and is out
    of scope;
  - one SCIP symbol can name several definitions (two `impl X for Error`
    blocks for traits named alike give both `custom` methods one symbol in
    serde_json; nested functions are named without their enclosing
    function): one equivalence class, as for scip-clang;
  - test code inside source files (`#[cfg(test)] mod tests`, `#[test]`
    functions) is not a callable, like test files elsewhere: its calls are
    module-level (ripgrep: 2515 of 5331 call sites);
  - repositories without a `Cargo.lock` (serde_json, bytes) resolve their
    dependencies to the newest compatible versions at `setup` time; only
    types of dependencies can change with that, never the in-scope edges'
    endpoints, so the gold is stable up to the inference of method
    receivers whose type comes from a dependency.
- **Shell structural gold is code-backed.** No compiler-grade oracle
  exists for shell; the gold (`shellgold.py`) encodes bash's run-time
  name lookup, which the pack implements too, so high agreement shows the
  pack follows those rules on real code, not that an independent tool
  confirms the edges. The two differ where they read the code differently
  (`source` paths by tail vs by evaluation, `time f` and `eval 'f'`,
  builtin-named functions outside the file). Dynamic behaviour neither
  sees: functions defined by `eval` or `autoload`, aliases, variable
  command words, sourcing loops (`for f in lib/*.sh; do . "$f"; done`:
  both fall back to a unique name), and calls a library makes to
  functions its caller defines when the name is not unique. nvm's library
  is one file and acme.sh computes its hook paths at run time, so their
  gold edges come from the file itself (nvm; acme.sh 4,857 call sites) and
  from unique names (acme.sh's hooks calling its helpers: 13,242 call
  sites); sourced paths are exercised by bash-it (33 of 57 `source` paths
  resolve, 24 call sites through them), the calibration set and the
  micro-benchmark. No localization dataset has shell instances, so shell
  has no S1 row and cannot be `stable`.
- **scip-ruby gold for Ruby** (jekyll, ruby-lsp, rack, packwerk, micro) is
  Sorbet's view of the code:
  - untyped code is inferred (as `# typed: true`, also in `# typed: false`
    files): constants, `self` calls through the ancestors and mixins,
    `Foo.new` (to `initialize`) and locals assigned `Foo.new` resolve; a
    method without a `sig` or RBS comment returns `T.untyped`, so calls on
    its result and on untyped parameters have no gold edge. Rueca's by-name
    rung (`heuristic` 0.5) binds some of those calls to the only method of
    their name: right or wrong in fact, they count as false positives
    unless Sorbet types the receiver (they are the confidence ≥ 0.6 row's
    difference);
  - `super` has no occurrence in scip-ruby's index, so `super` calls are
    not gold edges (Rueca's are false positives); `prepend` does not
    change dispatch; an instance variable assigned `Foo.new` in
    `initialize` is untyped without `T.let`; `new` called inside a
    singleton method is untyped; methods defined after `module_function`
    inside an `if` and calls after code Sorbet considers unreachable have
    no occurrence;
  - an alias resolves to the aliased method (`alias_method :n, :o`: a call
    of `n` is a call of `o`), which the micro key and Rueca follow;
  - the bench labeler's tree-sitter-ruby 0.23.1 does not parse an endless
    method with a command-call body (`def m = raise E, "x"`; the Rueca pack
    repairs it before parsing, the labeler does not): the rest of such a
    file is one ERROR node, and its callables there get the qualified name
    of the top level (packwerk's `validator.rb`); lines and names still
    match.
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

## Changelog of scoring rules

Rule changes that move published numbers, newest first (the results files
carry the S3 schema version; rows scored under an older schema are never
carried forward).

- **2026-11-25 — C# gold and test paths.** scip.py gains three grammar
  flags used by C# only (`local_callables`, `type_constructions`,
  `generic_overloads`, *Known biases*), and the shared test-path heuristic
  gains `test_dir_suffixes` (C# `Foo.Tests/` projects); no other language
  declares either, so no published number moves.
- **2026-11-17 — S2, first published run.** Gold labelers: `net/http`
  ServeMux routes, generic tonic wrapper impls, Python verb calls with a
  `path=` / `url=` keyword only, stub / servicer modules re-exporting a
  generated module (package from the agreeing generated modules). Scoring:
  every confidence (gated) and confidence ≥ 0.7 (`summary_high`, shown).
- **2026-11-16 — Organization scenario, larger and self-contained.** The
  multi-repository scenario moves from 21 instances (one per repository, 21
  repositories) to pinned `[[org.repos]]` members (42 gated, 8
  calibration) and up to 3 instances per member whose S1 gold is present at
  the pin, scored against that gold carried to the pin; the *single* answer
  is the same arm's own-repository answer in the same run (no longer the S1
  run at the instance's base commit); grep (BM25 over the union of the
  members, and per member + RRF) is a new arm; a calibration split is
  scored separately. Numbers are not comparable with the 21-instance ones,
  which are no longer carried or published (another population). Impact
  instances (a library's issue scored on the consumer files that use what
  its fix changes) are a second kind; every rate carries a Wilson interval,
  paired comparisons a bootstrap interval, discordance and minimum
  detectable effect; the scorecard gains an `org` block.
- **2026-10-26 — S3 schema 4.** (1) Equivalence classes: callables one SCIP
  symbol binds to (and unbound alternatives of one class) are one
  neighbour; class-aware scores are the gated numbers, the old scoring is
  the *strict* diagnostic row. (2) Recursion: self-edges are gold edges and
  every arm reports them (Rueca's arm reads `trace` `edges`, where the
  self-edge is, instead of only `nodes`; the grep heuristic stops excluding
  its root). (3) Matching: one hit → at most one callable (qualified name
  when the arm gives one, then the nearest line) instead of "any callable
  within ±2 lines", and arms resolve a question's definition to the nearest
  symbol within ±2. (4) C++ call sites: constructions by declaration
  (`Token token(...);`, `Parser parser(input);`, `T x{...};`, `T x;`),
  which scip-clang records on the variable name, are call edges.
- **2026-10-25 — RFC 0004** (S3 schema 3): every eligible callable is a
  question, callable universe filter, confidence ≥ 0.6 row.

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
  sets exist for Python (click), Go (goawk) and Rust (bytes) only so far.
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
