# RFC 0004 — Quality bar and benchmark

- Status: Draft
- Created: 2026-10-25
- Depends on: RFC 0001 (§15 benchmark), RFC 0003 (§3 release gate)
- Amends: RFC 0001 §15 (adoption gate), RFC 0003 §3 (statuses, parity row)

## 1. Summary

Rueca's quality is defined by **deterministic metrics with absolute
thresholds**, not by beating another tool. Every metric has two numbers:

- a **floor** — the minimum for a language to be called `stable`; the first
  public release requires every language at `stable`;
- a **target** — where the project is heading; published next to every
  result so each change can be judged by how far it moves us.

Other tools (codebase-memory-mcp v0.12.0) and simple baselines (BM25) are
measured on the same inputs and **published as reference**, never as the
gate. The benchmark is fixed where its numbers are not yet trustworthy:
structural questions over every function instead of a 50-function sample,
two or more gold repositories per language, own micro-benchmarks for Python
and TS/JS, confidence intervals everywhere, and a regression check in CI.

Agent end-to-end runs, a vector retrieval arm and hybrid retrieval are on
the roadmap (§11), not in this RFC.

## 2. Motivation

The alpha benchmark (`bench/`, `docs/bench/alpha-results.md`) answers three
questions for each language, with no LLM involved:

| Suite | Question | How it is answered | Main metrics |
|---|---|---|---|
| Localization (S1) | "Given this issue, where must the code change?" | One symbol search with keywords from the issue; compare the ranked files and functions with those the fixing patch touched | File Acc@5, function Acc@10, MRR |
| Structure (S3) | "Who calls this function, and what does it call?" | For sampled functions, compare depth-1 callers / callees with a compiler-grade oracle (SCIP, PHPStan) or hand-labeled snippets | precision, recall, F1, definition found |
| Configuration | "Which keys exist, who reads this env var, who sets it?" | Compare nodes with a reference parser, and bridge edges with hand-checked labels | key recall, span accuracy, bridge precision / recall |

The gate was "Rueca ≥ cbm on every one of 13 metrics, per language". Running
it on nine languages showed four problems:

1. **It measures noise.** With 30–40 instances per language one instance is
   2.5–3.3 points, and S3 used 50 questions on one or two repositories. A
   13-way strict comparison flips on single instances (PHP once "failed" by
   one instance out of 38).
2. **It chases another tool instead of a quality bar.** Java passed 13/13
   against cbm while its call-edge precision is 86–88%: cbm is weak on Java,
   which made Rueca look good, not correct. Configuration key recall of
   1.4% (cbm, YAML) is not a bar for anything.
3. **Some gold is unreliable.** PyCG, restricted to function → function
   edges, scores every tool at 28–33%: it measures a mismatch of the gold,
   not the tools. express has 19 gold edges. Java, C and C++ have one
   repository of structural gold each.
4. **Precision and recall are folded into F1.** For a coding agent a wrong
   edge is worse than a missing one — it sends the agent to read the wrong
   code — so the two need separate bars.

## 3. Principles

1. **Deterministic first.** The north-star metrics are 100% deterministic:
   same binary, same corpus, same numbers. Agent runs are valuable but need a
   budget and a sample size that make them representative; until then they
   are roadmap (§11).
2. **Absolute bars.** A language is good when it is correct, not when it is
   better than something else. Floors and targets are absolute numbers.
3. **Floors are a promise, targets are a direction.** Floors block a
   release; targets are aspirational on purpose and are part of the
   project's philosophy. A target that is met is raised.
4. **Precision over recall.** Every edge Rueca returns is a claim an agent
   will act on. Precision floors are higher than recall floors.
5. **Baselines keep us honest.** Retrieval must beat what a plain text
   search gets for free; otherwise the graph adds nothing. cbm is the
   reference we publish against, never the bar we aim at.
6. **Publish everything.** Every number, every loss, every known bias of
   the gold, with confidence intervals.
7. **Don't fit the benchmark.** Ranking weights and heuristics are not tuned
   on the gated instances (§7.7).

## 4. Suites

| Suite | Name in reports | Status in this RFC |
|---|---|---|
| S1 | **Localization** | kept, fixed (§7) |
| S2 | Cross-service edges | roadmap (§11) |
| S3 | **Structure** | kept, rebuilt (§7) |
| S4 | Agent end-to-end | roadmap (§11) |
| S5 | **Health** (operational) | promoted to a gated suite |
| — | **Configuration** | kept (RFC 0003 §3.3) |
| — | **Efficiency** | new, reported only |

Reports use the names; the S-numbers stay as short identifiers.

## 5. Metric definitions

All percentages are over the stated population; every metric is reported
with a 95% confidence interval (§7.6).

**Localization (S1)**, per issue, population = instances of the language:

- **File Acc@5** — 1 if at least one gold file is in the top 5 ranked files
  of the tool's answer, else 0; averaged. Gold files = non-test source files
  of the language modified by the fixing patch that exist at the base commit.
- **Function Acc@10** — the same over the top 10 ranked functions; a
  function matches when path and bare name are equal and the definition line
  is within ±2 of the gold's. Instances whose patch changes no function are
  excluded from this metric only.
- Diagnostic (not gated): File Acc@1/3, Recall@10, MRR, the `raw` query
  variant, context-file recall (ContextBench).

**Structure (S3)**, population = every in-scope callable with at least one
gold edge in that direction (§7.1):

- **Precision** (callers, callees separately) — of the neighbours Rueca
  returns at depth 1, the fraction that are gold edges; macro-averaged over
  functions.
- **Recall** — of the gold neighbours, the fraction Rueca returns.
- **Definition found** — the fraction of questioned functions Rueca resolves
  to the right definition (path, name, line ±2).
- Diagnostic: F1, micro precision / recall, lookup Acc@1/@10.

Neighbours are restricted to callables in scope (no File / Class / Variable
pseudo-neighbours), and are scored **at every confidence** (default `trace`)
and **at confidence ≥ 0.6** (heuristic edges dropped); the floor applies to
the default, the second row shows what an agent filtering by confidence
gets.
Callables the gold cannot tell apart — several definitions one SCIP symbol
binds to (same-signature overloads, template specializations, `#if`
alternatives) — form an equivalence class: a neighbour in a class matches a
gold edge to the class, once per question; the same scores without classes
are published as the *strict* row (diagnostic). A recursive callable is its
own caller and callee.

**Micro-benchmarks**, population = every callable of the snippet set: F1 of
callees (precision and recall shown).

**Configuration** (RFC 0003 §3.3): key recall and span-line accuracy against
a reference parser; env-var bridge precision / recall against the labeled
set.

**Health (S5)**: parse-error rate (files with ERROR / MISSING nodes over
extracted files of the language, across the smoke and corpus repositories);
query latency p95 (warm segment, every S1/S3 query); index throughput.

**Efficiency**: tokens returned per tool answer (tiktoken `cl100k_base` over
the JSON result), per tool, Rueca and cbm on the same queries. Reported
only; the cheapest signal of agent cost that needs no agent.

## 6. Floors, targets and statuses

| Family | Metric | Floor (`stable`) | Target |
|---|---|---|---|
| Localization | File Acc@5 | not significantly below BM25 (§6.1) | ≥ 80% |
| Localization | Function Acc@10 | not significantly below BM25 (§6.1) | ≥ 65% |
| Structure | Precision, callers and callees | ≥ 90% | ≥ 97% |
| Structure | Recall, callers and callees | ≥ 75% | ≥ 90% |
| Structure | Definition found | ≥ 95% | 100% |
| Micro | Callee F1 | ≥ 90% | ≥ 98% |
| Configuration | Key recall, span accuracy | ≥ 95% | 100% |
| Configuration | Bridge precision / recall | ≥ 95% / ≥ 75% | ≥ 99% / ≥ 90% |
| Health | Files with parse errors | ≤ 2% | ≤ 0.5% |
| Health | Query latency p95 | ≤ 100 ms | ≤ 20 ms |
| Efficiency | Tokens per answer | — (reported) | ≤ 50% of cbm |

The localization targets (80 / 65) are deliberately above every result
measured so far (best today: 67 / 52 overall) and above the plain-text
baseline; they will be calibrated against published deterministic
localization results before the first release (open question 1). Absolute
localization floors are not set yet because the ceiling depends on how much
an issue says about the code; beating free text search is the minimum.

### 6.1 How a floor is checked

- **Absolute floors** (structure, micro, configuration, health): the point
  estimate must meet the floor, and the population must reach the minimum
  size of §7.2. When the 95% interval crosses the floor the report marks the
  cell ⚠ (met, but not with confidence).
- **Relative-to-baseline floors** (localization): a paired bootstrap over
  instances (10,000 resamples, fixed seed) of `Rueca − BM25`; the floor is
  met unless the whole 95% interval is below zero.

### 6.2 Statuses

Replaces the statuses of RFC 0003 §3.1:

| Status | Meaning |
|---|---|
| `stable` | Every floor of its family met, every item of the RFC 0003 gate (extraction, resolution, tests, benchmark entry, docs) shipped. Required for the first public release. |
| `preview` | Extraction ships and is documented; a floor or a gate item is missing. Shown as such in the README and `docs/tools.md`. |
| `text` | No pack: `File` node and content search only. |

RFC 0003's "supported" becomes `stable`; its "parity row" becomes the
**scorecard row** of §9.

## 7. Benchmark changes

1. **Structure over every function.** Questions are every in-scope callable
   with at least one gold edge (minus dunders and one-line bodies), not a
   stratified sample of 50. The suite is deterministic and cheap, so the
   population is the whole repository: thousands of questions instead of 50,
   which removes most of the sampling noise. Lookup Acc keeps a stratified
   sample (it needs one search per question).
2. **Two or more gold repositories per language**, of different shapes
   (library and application), and a minimum population per language:
   ≥ 300 structural questions per direction, ≥ 30 localization instances
   (≥ 40 where datasets allow). Today Java, C and C++ have one structural
   repository each; candidates are chosen by the bench (SCIP-indexable
   without a full dependency build).
3. **Replace PyCG with an own Python micro-benchmark**, same format and
   conventions as the PHP / Go / Java / C / C++ sets and cross-checked
   against scip-python. PyCG stays as a diagnostic row.
4. **A TS/JS micro-benchmark** (ESM / CommonJS, barrels and re-exports,
   classes and `this`, default exports, tsconfig paths, decorators),
   cross-checked against scip-typescript.
5. **More TS/JS structural gold.** express (19 edges) stays as a diagnostic;
   one or two typed TypeScript repositories with substantial call graphs are
   added.
6. **Confidence intervals in every table**: Wilson intervals for
   proportions, bootstrap for macro averages and paired differences; the
   scorecard flags ⚠ when an interval crosses a floor.
7. **No fitting on gated instances.** A small calibration split per
   language (instances and repositories disjoint from the gated ones) is
   where ranking weights and heuristics are tuned; the gated set is only
   evaluated. Changes that were tuned are documented with the split they used.
8. **Regression check in CI.** A scheduled workflow builds `main`, runs the
   Rueca arm over cached corpora and gold (baseline arms are re-run only when
   their versions change), and fails when any floor metric drops by more
   than its interval half-width against the last published results. PRs that
   touch a language pack or the resolver can trigger it manually.
9. **Gold bias disclosed per row.** The scorecard links each structural row
   to its gold source (SCIP indexer and version, PHPStan, hand labels) and the
   README of the bench keeps the list of known blind spots (interface
   dispatch, function pointers, macros, untyped JavaScript), and the report
   counts, per gold set, the equivalence classes (§5) — definitions the gold
   cannot tell apart — and the gold edges that point to one.

## 8. What does not change

- Corpora at pinned commits, datasets by revision and sha256, tools by
  version and sha256, Docker images by digest; nothing proprietary.
- Same inputs for every arm; identical post-filtering (language, non-test).
- The matching rules of S1 and S3 (path + bare name + line ±2). S3 refines
  them (amended 2026-10-26): each hit is assigned to one callable — the
  arm's qualified name first, then the nearest line within ±2 — and
  equivalence classes of the gold (§5) count as one neighbour
  (`bench/README.md`, *Changelog of scoring rules*).
- Results are generated (`rueca-bench report`), never edited by hand.

## 9. Reporting: the scorecard

`docs/bench/<release>-results.md` opens with one scorecard per language:

| Metric | Rueca (95% CI) | Floor | Target | Status | BM25 | cbm |
|---|---|---|---|---|---|---|
| Callers precision | 98.1 (97.4–98.7) | 90 | 97 | ✅ | — | 96.6 |
| … | | | | | | |

followed by the language's status (`stable` / `preview` and the failing
floors), then the detailed suite tables as today. The root README shows one
line per language: status and the floors that are not met.

## 10. Amendments

- **RFC 0001 §15** — the adoption gate "matches or beats the baseline on
  suites 1–3" is replaced by the floors of §6; baselines are reference.
  Suite 4 (agent) moves to the roadmap.
- **RFC 0003 §3.1** — "supported" → `stable` (§6.2); item 5 "parity row" →
  scorecard row (§9); "at parity" is no longer a gate.
- **README** — the benchmark section shows the scorecard summary; a
  high-level roadmap is added.

## 11. Roadmap (out of scope here)

- **Vector baseline arm** in the bench: pure embedding retrieval over
  symbols, as a second retrieval floor next to BM25.
- **Hybrid retrieval in Rueca**: BM25 over code + static embeddings
  (`model2vec-rs`, RFC 0002 §6.2; small MIT-licensed models, vectors in the
  segment, exact scan) + graph signals, fused on the calibration split. Its
  goal is the localization targets of §6.
- **Agent end-to-end (S4)** when there is a budget for a representative
  sample: same agent and model with and without Rueca; answer quality,
  tokens, tool calls.
- **Cross-service edges (S2)** with the org mode.

## 12. Alternatives considered

- **Keep "match or beat cbm" as the gate.** Rejected: it gates on another
  tool's weaknesses and on sampling noise (§2).
- **One threshold for every metric (e.g. 75%).** Rejected: metrics have
  different ceilings. Correctness against an oracle can approach 100%;
  localization from issue text is bounded by what the issue says, and no
  tool reaches 75% function Acc@10 today. 75% is kept where it fits: the
  recall floor.
- **Agent runs as the north star now.** Rejected for now: a small run is not
  representative and a representative one is not affordable yet (§11).
- **F1 as the structural gate.** Rejected: hides the precision / recall
  trade-off that matters for agents (§3.4).

## 13. Open questions

1. Calibrating the localization targets (80 / 65) against published
   deterministic results (LocBench / LocAgent, SweRank, CoRNStack) — keep,
   raise or lower before the first release?
2. Should localization get an absolute floor once the vector arm exists
   (e.g. ≥ the better of BM25 and vector)?
3. Efficiency: which tokenizer is the reference, and should a floor be set
   after one release of data?
4. Regression check cadence: nightly, or on every PR touching packs and the
   resolver (cost: ~20 min of CI per run with cached corpora)?
