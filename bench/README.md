# telar-bench

Benchmark harness for Telar (RFC 0001 §15, RFC 0002 §8). It compares, on the
same inputs and with the same scoring:

| Arm | What it is | How it is driven |
|---|---|---|
| `grep` | No graph. BM25 (k1=1.2, b=0.75) over file contents, function bodies and function names (tree-sitter); textual `name(` heuristic for callers/callees | in-process |
| `cbm` | codebase-memory-mcp **v0.12.0**, official release binary pinned by sha256 in `manifest.toml` (never a binary on `PATH`) | `codebase-memory-mcp cli --quiet <tool> '<json>'`, with `CBM_CACHE_DIR` / `CBM_RUNTIME_DIR` inside `bench/.cache`; one warm `daemon start` per bench process (as in an MCP session; stopped at exit), UI disabled |
| `telar` | Telar at HEAD | `$TELAR_BIN index <root> --store <dir> --json`, `$TELAR_BIN query <tool> '<json>' --root <root> --store <dir>` (alpha contract, `docs/tools.md`) |

The repository ships **manifests and code only**. Datasets, repositories and
tool binaries are fetched at runtime at pinned revisions into `bench/.cache`
(git-ignored), so nothing with an unclear licence is vendored.

## Running

```sh
# from the repository root
uv run --project bench telar-bench setup                 # tools, datasets, clones, gold labels
uv run --project bench telar-bench run --arm all --suite all
uv run --project bench telar-bench report                # bench/results/*.json + docs/bench/alpha-results.md
```

- `run --arm {grep,cbm,telar,all|a,b} --suite {s1,s3,all} [--only ID|REPO ...] [--workers N] [--force]`.
  Outputs are cached per (arm, instance) in `.cache/runs/`; re-running only
  computes what is missing (`--force` recomputes). Arm failures are recorded
  (and scored as misses), never dropped.
- `setup --select` regenerates the `[[s1.instances]]` list at the end of
  `manifest.toml` from the selection rules (deterministic).
- `clean --work` removes worktrees; `clean --runs telar` drops cached Telar outputs.
- Per-call timeouts: 120 s per query, 1800 s per index (`arms/__init__.py`).
- S1 runs repositories in parallel (`--workers`, default 2), instances of one
  repository sequentially (one partial clone per repository, one detached
  worktree per instance, removed afterwards).

### Telar arm

```sh
export PATH="$HOME/.cargo/bin:$PATH"
cargo build --release -p telar-cli
TELAR_BIN=$PWD/target/release/telar uv run --project bench telar-bench run --arm telar --suite all
uv run --project bench telar-bench report
```

Without `TELAR_BIN` the Telar column is reported as *pending*.

### Requirements

`uv`, `git` ≥ 2.40 (partial clone), `node`/`npx` (SCIP indexers, S3 setup
only), network for `setup`. `git-lfs` is not needed: worktrees are created
with the LFS filter disabled (pointers stay pointers).

**macOS note.** If `uv run` fails with `ModuleNotFoundError: telar_bench`,
macOS has marked files in `bench/.venv` hidden and Python ≥ 3.12 skips hidden
`.pth` files (editable install). Use a regular install instead:
`export UV_NO_EDITABLE=1` (the `cache-keys` in `pyproject.toml` make uv
reinstall when sources change), or `chflags -R nohidden bench/.venv`.

Disk: about 1 GB for the alpha corpus (partial clones ≈ 100 MB, one worktree
at a time per repository, datasets ≈ 120 MB, cbm binary 335 MB).

## Suites

### S1 — Localization (deterministic, no LLM)

Issue text → ranked files and functions. Method and metrics follow LocBench
(LocAgent): file Acc@1/3/5, function Acc@5/10, Recall@k, MRR, plus index time
and query latency.

**Instances.** Defined in `manifest.toml` (`[[s1.repos]]` quotas,
`[[s1.instances]]` generated list). Python: SWE-bench Verified ∪ Lite on
requests, flask, pylint, pytest, sphinx (40). TS/JS: SWE-PolyBench
(tailwindcss, code-server) and SWE-bench Multilingual (vuejs/core, preact,
axios) (41). An instance is eligible if its gold patch modifies at least one
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
`query`, `telar`: `search_symbols`), 100 results; the ranked file list is the
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
| Functions (34 other instances) | Telar tree-sitter labeler (`gold.py`) | every removed/modified old-side line → innermost enclosing function/method; a pure insertion after old line k → innermost function containing k and k+1 (Python: also the function containing k when the inserted line is indented deeper than its `def`) |
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

**Gold from PyCG** (Apache-2.0): the hand-written `callgraph.json` of every
snippet of the PyCG micro-benchmark, restricted to function → function edges
(module nodes, builtins and external names dropped). Each snippet becomes a
tiny git repository with the answer key removed; every function is a question.

**Questions.** ~50 functions per SCIP repository, stratified by (fan-in,
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
| PyCG micro-benchmark (`vitsalis/PyCG`) | commit | Apache-2.0 | S3 Python call-graph gold |
| scip-python / scip-typescript | npm version | MIT / Apache-2.0 | S3 gold |
| codebase-memory-mcp v0.12.0 | release URL + archive and binary sha256 (darwin-arm64, linux-amd64) | MIT | `cbm` arm |
| Corpus repositories | commit SHAs | their own (requests Apache-2.0, flask BSD-3, pytest MIT, pylint GPL-2.0, sphinx BSD-2, tailwindcss MIT, code-server MIT, vue MIT, preact MIT, axios MIT, hono MIT, express MIT) | cloned at runtime; nothing vendored |

Citations: Jimenez et al., *SWE-bench* (ICLR 2024); OpenAI, *SWE-bench
Verified* (2024); Rashid et al., *SWE-PolyBench* (2025); Chen et al.,
*LocAgent* (ACL 2025); EuniAI, *ContextBench* (2026); Salis et al., *PyCG*
(ICSE 2021).

Only identifiers (instance ids, commits, revisions) and Telar's own code live
in this repository. Gold labels and arm outputs stay in `bench/.cache`;
`bench/results/*.json` contains scores and per-instance metrics, which include
file paths and function names of the public corpus repositories.

## Known biases and limitations

- **Contamination.** All corpus repositories and issues are public and old;
  irrelevant for the deterministic arms, relevant for the future agent suite.
- **Process-per-call latency.** `cbm` and `telar` are driven one CLI process
  per query (`cbm` talks to its warm daemon; `telar` opens its segment on every
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
- **ContextBench TS/JS rows** carry PR descriptions instead of issue text, so
  they are not used as S1 sources; only their Python (SWE-bench Verified) gold
  contexts are used, joined by instance id and base commit.

## Deviations from the RFCs

- RFC 0001 §15 lists NDCG; the alpha report gives Acc@k, Recall@k and MRR
  (LocBench's reported metrics). NDCG can be computed from
  `results/s1.json` ranks later.
- RFC 0002 §8 suggests pyrefly for Python call edges; the alpha S3 gold uses
  scip-python (as specified for this wave) plus PyCG.
- The CGraph cross-service bench (suite 2), Harbor / Inspect (suite 4) and the
  issue-only control arm are not part of the alpha (S1 + S3 only).
- S1 uses SWE-bench Multilingual for vuejs/core, preact and axios next to
  SWE-PolyBench, because PolyBench's TS/JS repositories of small/medium size
  are few.
