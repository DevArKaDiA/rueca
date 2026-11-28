# RFC 0009 — Code mode (experimental)

- **Status:** Experimental — phases 0–3 implemented; off by default (behind
  `--experimental-code-mode`; Starlark behind the non-default
  `code-mode-starlark` cargo feature, §13); verdict and recommendation in
  §14; graduation pending the owner's decision
- **Created:** 2026-11-24
- **Updated:** 2026-11-24
- **Amended:** 2026-11-24 — both languages ship behind one `Sandbox` trait
  during the experiment so they can be compared; the language is an output
  of the experiment, not an up-front pick (§1, §4, §6, §8, §10, §12–§14)
- **Amended:** 2026-11-24 — phase 1: the details §4–§5 left open are settled
  (§4.4), and Starlark is a non-default cargo feature (§13)
- **Amended:** 2026-11-24 — phase 2: the multi-step suite as built (§8) and
  its results (§13.1); no helper earned a place (§4.3)
- **Amended:** 2026-11-24 — phase 3: the fluency probe and the decision
  (§14): keep code mode experimental with JavaScript only; open questions
  updated (§12)
- **Amended:** 2026-11-24 — the owner's decisions (§14.6): Starlark stays the
  optional build feature as built; the four API friction items of §14.4
  (5) are fixed before merge (§4.4)
- **Depends on:** [RFC 0001](0001-architecture.md) (§11 tools, §13 security), [RFC 0004](0004-quality-and-benchmark.md) (efficiency
  metrics), [RFC 0005](0005-agent-responses.md) (envelope, pinned ids, `repo` / `ref`, tokens per
  correct answer), [RFC 0006](0006-org-graph.md) (organization, contracts), [RFC 0008](0008-service-mode.md) (service
  mode, child-process isolation)
- **Research date:** 2026-11-24. Crate versions were read from crates.io that
  day (§11). *(estimate)* marks arithmetic on stated inputs;
  *(unverified)* a claim no primary source confirmed; *(default)* a value
  chosen here, to be tuned by measurement.

## 1. Summary

Add an **experimental** MCP tool, `execute`, that runs a short script
against a typed Rueca API inside the server and returns only what the
script returns. An agent that today chains `search_symbols` → `get_symbol`
→ `trace` → `list_contracts`, reading every intermediate answer, writes
one script that does the chain, filters, and hands back the final rows.

- Off by default: `rueca mcp --experimental-code-mode` or
  `RUECA_EXPERIMENTAL_CODE_MODE=1`. When off, nothing changes: the eight
  tools, their schemas and the golden transcripts stay as they are.
- The API is generated from the same tool definitions the MCP layer
  serves (`rueca_query::tool_definitions()`), so the script API and the
  tools cannot drift.
- Scripts run in a sandbox with no network, no filesystem, no clock and no
  randomness, under limits on time, memory, engine calls and output size.
- Two languages ship behind one `Sandbox` trait for the whole experiment,
  **JavaScript (QuickJS via `rquickjs`)** and **Starlark (`starlark`)**,
  each behind its own cargo feature; `execute` takes a `language`. Which
  to keep is an **output** of the experiment, not a choice made up front:
  §6 states the hypothesis (JavaScript) that phases 0–3 test.
- Phase 3 writes a per-language verdict from the efficiency bench (§8) and
  a fluency probe (§10), and recommends keeping both, keeping one, or
  removing code mode (§14). Graduating to on-by-default is the owner's
  decision, not this work's.

## 2. Motivation

The published benchmark (`docs/bench/alpha-results.md`) showed, at the time
of writing, where Rueca still loses to codebase-memory-mcp 0.12.0: **tokens
per true edge from `trace`** — Python 57 vs 48, TS/JS 64 vs 54, PHP 72 vs
56, C 52 vs 30, C++ 63 vs 61; Rueca wins in Java (67 vs 76) and Rust (51
vs 106). (Re-measured after §14.6, which drops code nested in tests from
`trace`: Python 55 vs 48, TS/JS 57 vs 54; the others did not move.) cbm
also offers `query_graph`, a free-form graph query, which Rueca does not
have.

The per-answer gap is formatting and can be closed in the tools themselves.
The larger cost is structural: a multi-step question costs the sum of every
intermediate answer the agent has to read and copy into the next call.
Two published reports measure that cost:

- Cloudflare, "Code Mode" (2025-09-26): models write code against a
  generated TypeScript API better than they chain tool calls, and values
  pass between calls without going through the model. Their Cloudflare API
  server measured 1,170,523 tokens as native tools against 1,069 with two
  code-mode tools.
- Anthropic, "Code execution with MCP" (2025-11-04): a Drive → Salesforce
  workflow went from about 150,000 to 2,000 tokens (98.7%) when the agent
  wrote code instead of calling tools one at a time *(figures as reported
  by secondary coverage; the primary page was not fetched)*.

Rueca's questions are exactly multi-step: "who calls this function from
another repository, and through which HTTP contract", "which functions
changed between these two refs and are reachable from this handler". Today
each step is a tool call whose full answer enters the agent's context.

## 3. Goals and non-goals

Goals:

1. Fewer tokens and fewer round trips per correct answer on multi-step
   questions, measured deterministically (§8).
2. Zero cost when disabled: no change to tools, schemas, outputs or
   latency.
3. A sandbox safe enough for local mode, and isolated enough to be
   considered for service mode later (§7.3).
4. One source of truth for the API: generated from the tool definitions.

Non-goals:

- Replacing the eight tools. They stay the primary interface; `execute`
  composes them.
- A general programming environment: no imports, packages, network, files
  or persistent state between calls.
- A graph query language (Cypher, GraphQL). Considered in §9.
- Agentic evaluation in this RFC (no budget; RFC 0004 keeps metrics
  deterministic).

## 4. Surface

### 4.1 Tools when enabled

| Tool | Purpose |
|---|---|
| `execute` | Run a script in `language`; return its value |
| `describe_api` | Return the API reference for `language` (`"js"`: TypeScript declarations; `"starlark"`: a docstring reference), so the agent loads it only when it decides to use code mode |

`describe_api` exists so the declarations do not inflate every session's
tool list; the `execute` description stays short and points to it. It
takes `language` (`"js"` | `"starlark"`, default `"js"`) like `execute`.

### 4.2 `execute`

| Arg | Type | Default |
|---|---|---|
| `code` | string, the script | — |
| `language` | `"js"` \| `"starlark"` | `"js"` while experimental |
| `repo`, `ref` | as RFC 0005: default scope for every API call in the script | the server's default |
| `budget_tokens` | int: cap on the rendered result | 2000 *(default)* |
| `timeout_ms` | int, ≤ the server maximum | 2000 *(default)*, max 10000 |

The script's last expression (JavaScript: the value of a `return` from the
top-level async function; Starlark: the value of `result`) is the answer.
Results:

```json
{
  "result": <JSON value returned by the script>,
  "logs": ["..."],
  "calls": 7,
  "snapshots": [ ... RFC 0005 envelope entries for every snapshot read ... ],
  "truncated": false
}
```

Every id inside `result` is a pinned RFC 0005 id (repo + 12-hex commit), so
an agent can follow up with the normal tools.

`partial` (the calls whose own answer was cut), `logs_truncated` and, with
`RUECA_CODE_MODE_TIMING=1`, `timing` follow when they apply (§4.4).

### 4.3 The API

Generated from `tool_definitions()`: one function per tool, same argument
names, same defaults, returning the tool's **structured JSON** (the
envelope's `results`, not the compact text). JavaScript shape:

```ts
declare const rueca: {
  searchSymbols(args: SearchSymbolsArgs): Promise<SymbolHit[]>;
  searchCode(args: SearchCodeArgs): Promise<CodeHit[]>;
  getSymbol(args: GetSymbolArgs): Promise<Symbol>;
  getSnippet(args: GetSnippetArgs): Promise<Snippet>;
  trace(args: TraceArgs): Promise<TraceResult>;
  getFileOutline(args: FileOutlineArgs): Promise<Outline>;
  listRepositories(args?: ListRepositoriesArgs): Promise<Repository[]>;
  listContracts(args: ListContractsArgs): Promise<Contract[]>;
};
```

Starlark shape: the same functions under their snake_case names, keyword
arguments only (a dict spreads with `**args`), returning the tool's JSON
as dicts and lists, synchronously:

```python
hits = rueca.search_symbols(query = "parse", limit = 5)
result = [h["name"] for h in hits]
```

Phase 2 may add a few helpers that save round trips without new engine
features (e.g. `rueca.callersAcrossRepos(id)`), each justified by a bench
task. Helpers are thin compositions of the eight tools.

### 4.4 Settled in phase 1

- **Result types.** `tool_definitions()` carries each tool's
  `output_shape` (a JSON Schema of the envelope's `results`, not sent to
  MCP clients); `describe_api` generates the TypeScript result types and
  the Starlark shapes from it, and a test checks real answers against it
  both ways (`rueca-query/tests/shapes.rs`). Arguments every tool takes
  with the same schema are written once (`CommonArgs`); `format` is left
  out of the API and refused (scripts always read the compact shape).
  `describe_api` is about 3,900 tokens for `js` and 3,800 for `starlark`.
- **Call shapes.** JavaScript: `rueca.<camelCase>(args)`, one object with
  the tools' snake_case argument names, a `Promise` of `results`; a failed
  call rejects with an `Error` whose message is `rueca.<name>: <the tool's
  error>`, catchable. Starlark: `rueca.<snake_case>(**kwargs)`, keyword
  arguments only, synchronous; a failed call stops the script with the
  same message and the line (Starlark has no exceptions).
- **Snapshots and `s`.** The calls' snapshot entries are merged (same
  entry once) and numbered in first-read order; every `s` and `in` in a
  call's `results` is renumbered to that list before the script sees it.

  A script does not need them to place a symbol: every object with an `id`
  carries `local` (the file-local name the id is built from), `qn` (the
  global one), `repo` and `ref` — one rule for every tool and every
  answer, single- or multi-snapshot, in the tools' structured JSON (not in
  their text, so single-repository answers do not grow), added before merge
  (§14.6).

  Envelope fields other than `results` are not passed to scripts; a call
  the tool cut (`truncated`) is reported in `partial` with its
  `next_cursor`.
- **Scope.** `execute`'s `repo` / `ref` are added to a call that names
  neither the argument nor an `id` (an id pins its repository and commit).
- **Per-call budget.** A call's `budget_tokens` defaults to 1,000,000 (the
  tools' maximum) inside scripts: the call's answer reaches the script,
  not the agent, so only the tool's `limit` bounds it; `execute`'s own
  `budget_tokens` caps what the agent reads.
- **Truncation.** `result` is counted as compact JSON with the tools'
  estimator (`ceil(bytes / 4)`); the largest array keeps its longest
  fitting prefix (then the next largest), then the longest string is cut
  (ending in `…`), and a result that still does not fit becomes `null`.
- **`timeout_ms`** is clamped to 1–10,000 ms, like the tools' ranges
  (not an error); the engine calls get the script's remaining wall time as
  their cooperative deadline.
- **Text content.** The tools' snapshot header lines, `result` as compact
  JSON on one line, `log:` lines, then `(truncated …)`, `(partial …)` and
  `(logs truncated)` notes. Errors: one line, `<language> <kind> error at
  line N: <message>`.
- **Where it runs.** The server side lives in `rueca-mcp` (`codemode`
  module: the engine-backed `Host`, the generated API); `rueca query
  execute` uses the same code. Service mode keeps it off explicitly.

## 5. Execution

1. Parse the script; reject it before running if it exceeds 16 KB
   *(default)*.
2. Create a fresh interpreter per call (no shared state).
3. Bind `rueca.*` to the in-process query engine (`Engine::respond` with
   the tool's arguments): no MCP round trip, no serialization to the agent.
4. Run under the limits of §7.
5. Serialize the returned value to JSON, apply `budget_tokens` (truncate
   arrays from the end, set `truncated`), attach `logs`, `calls` and the
   snapshot envelope.

Errors (syntax, runtime, limit exceeded, unknown function, bad arguments)
return `isError: true` with the interpreter's message and line, so the
agent can fix the script in one more turn.

## 6. Language

| | JavaScript (QuickJS, `rquickjs` 0.14.0) | Starlark (`starlark` 0.14.3) | Rhai (`rhai` 1.27.0) |
|---|---|---|---|
| Licence | MIT | Apache-2.0 | MIT / Apache-2.0 |
| MSRV | 1.87 (fits 1.90) | not declared | 1.66 |
| Engine | C (QuickJS) via `cc`; Rueca already builds C/C++ for llama.cpp | pure Rust (Meta, used by Buck2) | pure Rust |
| Model fluency | Highest: the language code-mode reports use | High: a Python dialect, but no `while`, recursion, classes, exceptions or imports — models will reach for them | Low: little training data |
| Termination | Needs an interrupt handler (QuickJS supports one) | No `while`, but bounded loops can still run for hours (`range(2147483647)`): needs the evaluator's cancellation hook (§13) | `max_operations` built in |
| Memory limit | QuickJS memory limit | Best-effort heap check, not on allocation (§13) | built-in size limits |
| Determinism | Remove `Date`, `Math.random` and timers | Deterministic by design | Deterministic |
| Rejected | — | — | Model fluency |

**Both ship during the experiment.** JavaScript and Starlark run behind
the same `Sandbox` trait (crate `rueca-codemode`, features `js` and
`starlark`), so every measurement in §8 and §10 is taken for both and
dropping one later is removing a feature. JavaScript is in the default
build; Starlark is the opt-in feature `code-mode-starlark` of the `rueca`
binary, because its dependencies fail the repository's `cargo deny`
policy (§13).

**Hypothesis to test: JavaScript.** Code mode's value depends on the model
writing a correct script on the first try, and both published reports use
JavaScript/TypeScript. QuickJS gives the two controls that matter (memory
limit and an interrupt handler) and has no I/O unless its `std`/`os`
modules are added, which Rueca does not do. Starlark's hermeticity is
attractive, but its missing constructs (`while`, exceptions, classes,
`**`, imports) may cause failed scripts. Phase 0 (§13) measures the
engines; phase 3's fluency probe and the suite decide (§14).

## 7. Sandbox and limits

### 7.1 Capabilities

None besides `rueca.*`, `console.log` (captured into `logs`, capped at
4 KB) and the language's pure built-ins. No network, filesystem,
environment, clock, randomness, timers, `eval` of host code, or module
loading.

### 7.2 Limits *(defaults)*

| Limit | Default | Enforcement |
|---|---|---|
| Wall time | 2 s, max 10 s | interrupt handler checks a deadline |
| Memory | 64 MB | interpreter memory limit |
| Engine calls | 200 per script | counter in the binding |
| Result size | `budget_tokens` | truncation at serialization |
| Script size | 16 KB | checked before parsing |

Each engine's enforcement of these limits, and the cases it cannot
enforce in-process, are recorded in §13.

Engine calls inherit the tools' own limits (`limit`, `depth`, token
budgets), so one call cannot do more than the tool could.

### 7.3 Local and service mode

Local mode runs the interpreter in the `rueca mcp` process: the agent and
the user are the same principal, and the limits protect the session from
runaway scripts, not from a hostile user. Service mode (RFC 0008) keeps
code mode **off**; enabling it there would run each script in a child
process with rlimits, reusing the isolation of the index worker (RFC 0008
§7.8), and would apply the caller's visibility to every engine call. That
is a separate decision after phase 3.

## 8. Measurement

A new deterministic **multi-step efficiency suite** in the bench:

- 20–30 tasks over the existing pinned corpora and the organization
  suite, each with a gold answer derived from the existing gold (S3
  callers and callees, cross-repository edges, contracts, refs). Examples:
  callers of a library function in other repositories; handlers whose HTTP
  route is consumed by another service; functions reachable from a handler
  that changed between two refs.
- For each task, a reference **tool-call plan** (the sequence an agent
  would make) and a reference **script per language** (JavaScript and
  Starlark), written once and committed.
- Measured, **per language**: tokens the agent reads (sum of tool answers
  vs. script source plus `execute` result), number of calls, correctness
  of the final answer against gold, latency. Tokens counted with the bench's tokenizer (RFC
  0005 §8).
- Compared against the same tasks answered with cbm's tools where cbm can
  express them (including `query_graph`).

**Per-language gate** (each language is judged on its own): on the suite,
code mode reads at least 40% fewer
tokens per correct answer than the tool-call plans, with no task becoming
incorrect, and median `execute` overhead under 20 ms above the sum of the
engine calls *(defaults)*. Single-step questions are not expected to
improve and must not regress, since the tools are unchanged.

**As built (phase 2).** `bench/` command `rueca-bench multistep`
(`bench/README.md`, *Code mode … and the multi-step suite*): **25 tasks**
over:

- Three S3 repositories (Flask, Requests, Hono; 16 tasks),
- The Flask cross-repository group (Flask with Click, Jinja, Werkzeug;
  4),
- The S2 `ollama` group (server and two clients; 4),
- The multi-ref scenario (1).

Kinds:

- 2-hop callers,
- Exactly-2-hop count,
- Callers ∩ callers,
- Callees ∩ callees,
- Outline → per-member caller counts,
- Top-N by fan-in in a file,
- Uncalled functions of a file,
- Callees that call nothing,
- Search → filter → details,
- The callee with most callers,
- Callers in a directory,
- Callers of a library function / classes from another repository,
- Routes called by both clients / by one only,
- OpenAPI operations a client calls,
- A route's handlers,
- The organization's dependencies,
- Functions that differ between two refs.

5 of the 25 are expected not to favour code mode (one call answers
them). Gold is derived in code from the existing gold (S3 call graphs,
the cross-repository labeler's uses, S2 pairs, `collapse_gold`),
committed with its inputs and re-derived by a test.

Arms:

- The reference tool-call plan,
- The reference JavaScript and Starlark scripts,
- cbm 0.12.0 (one `query_graph` where its Cypher subset can express the
  task: 16 of 25; the cross-repository, contract and multi-ref tasks it
  cannot).

Each task's statement is written for the phase 3 probe (`rueca-bench
multistep tasks`, `probe`, `describe-api`). Results: §13.1.

What this does not measure: whether models write good scripts. That needs
agentic runs, which RFC 0004 defers; the reference scripts stand in for a
competent agent. Phase 3 adds only a small, bounded fluency probe (§10),
not an agentic evaluation.

## 9. Alternatives considered

1. **A graph query language (`query_graph`, Cypher-like), as cbm does.**
   Expressive for graph shapes, but it exposes storage concepts, needs a
   parser and planner over Rueca's segments and organization layer, and
   models write Cypher less reliably than JavaScript. Code mode composes
   the existing, already-tuned tools instead.
2. **A declarative `batch` tool**: an array of tool calls where later
   arguments reference earlier results (`"$1.results[0].id"`). Much
   simpler and sandbox-free, and it removes round trips; it cannot filter
   or join, so intermediate answers still return. Kept as the fallback if
   the experiment rejects both languages.
3. **Making the tools more compact.** Necessary anyway (the `trace` gap is
   formatting) and independent of this RFC.
4. **WebAssembly sandbox (wasmtime) running agent-provided modules.**
   Strongest isolation, but agents cannot write Wasm; it would host one of
   the interpreters above, at a large dependency cost.

## 10. Phases

- **Phase 0 — spike (no PR to main).** Embed QuickJS and Starlark behind
  the same `Sandbox` trait (kept, and extended in phase 1); bind two API
  functions; measure start-up time, per-call overhead, binary size,
  memory-limit and interrupt behaviour, dependencies and cargo-deny, for
  each engine. Record the results in §13; no language is picked.
- **Phase 1 — `execute` and `describe_api` behind the flag, both
  languages.** Generated API from `tool_definitions()` (TypeScript
  declarations and the Starlark reference), limits of §7, result envelope
  of §4.2, tests (limits, determinism, errors, golden transcripts
  unchanged when disabled).
- **Phase 2 — multi-step suite (§8), per language, and helpers**
  justified by tasks.
- **Phase 3 — verdict.** A fluency probe: 10 tasks × 2 languages, a fresh
  model given only `describe_api`'s output for that language and the task
  statement, scored on first-try success and success after one repair
  turn (the error message of §5 as the only feedback). With the suite's
  numbers it writes a per-language verdict and a recommendation in §14:
  keep both, keep one, or remove code mode. Graduation (tools.md as a
  normal tool, on by default) is the owner's decision, not this work's.
  Done: §14.

## 11. References

- Cloudflare, "Code Mode: the better way to use MCP", 2025-09-26,
  https://blog.cloudflare.com/code-mode/
- Anthropic, "Code execution with MCP: building more efficient agents",
  2025-11-04, https://www.anthropic.com/engineering/code-execution-with-mcp
  (figures via https://www.marktechpost.com/2025/11/08/anthropic-turns-mcp-agents-into-code-first-systems-with-code-execution-with-mcp-approach/)
- `rquickjs` 0.14.0 (MIT, MSRV 1.87), `starlark` 0.14.3 (Apache-2.0),
  `rhai` 1.27.0 (MIT / Apache-2.0), `boa_engine` 0.22.0 (MSRV 1.91, above
  Rueca's 1.90): crates.io, read 2026-11-24.
- Rueca efficiency figures: `docs/bench/alpha-results.md`, "Efficiency —
  tokens per correct answer".

## 12. Open questions

1. Which language(s) to keep — **answered (phase 3)**: JavaScript only is
   recommended; Starlark stays a non-default build feature in this change
   and its removal is the owner's call (§14.5). The owner keeps it as
   built (§14.6).
2. Should `describe_api` return TypeScript declarations for every tool, or
   a shorter hand-written reference? — **Answered (phases 1, 3)**:
   generated declarations for every tool, result shapes included (§4.4),
   about 4.1k tokens (4.3k with the fields of §14.6); in the probe no script failed on the API's shape
   (§14.1), so nothing argues for a hand-written reference yet.
3. Should enabling code mode hide the eight tools (Cloudflare's
   single-tool shape) or keep them alongside? This RFC keeps them. —
   **Open**: the suite and the probe do not test it; it needs an agentic
   evaluation.
4. Default `budget_tokens` and `timeout_ms`. — **Answered for now**: 2,000
   tokens and 2 s; no task of the suite or the probe hit a limit (§13.1,
   §14.1).
5. Which helpers (§4.3) earn a place, if any. — **Answered**: none; none
   saves a round trip (§13.1).

## 13. Phase 0 results

Crate `rueca-codemode` (features `js`, `starlark`; both default-on in
phase 0, Starlark opt-in since phase 1):
`Sandbox::run(script, &mut dyn Host, &Limits) -> Result<Outcome,
ScriptError>`, two tools bound through an in-memory host.

Measured
2026-11-24 on an Apple M1 Pro (10 cores, 32 GB), macOS, Rust stable
1.99, release profile of the workspace (`lto = "thin"`,
`codegen-units = 4`, `strip`). Versions: `rquickjs` 0.14.0 (QuickJS-ng,
built from C by `rquickjs-sys` 0.14.0), `starlark` 0.14.3.

Timings come
from `cargo run --release -p rueca-codemode --example phase0`; each limit
probe runs in its own child process (peak RSS from `getrusage`), under
the default limits (2 s, 64 MiB).

| Row | JavaScript (QuickJS) | Starlark | Method |
|---|---|---|---|
| Cold start (fresh interpreter + `return 1` / `result = 1`) | p50 77–114 µs, p95 87–133 µs | p50 1.5 µs, p95 1.6–1.8 µs | 500 runs, three sessions; the sandbox (prelude / globals) is built once per server: 0.5–1.2 µs (JS), 3.7 µs (Starlark) |
| Per host call | 2.4–2.5 µs | 0.43–0.45 µs | script making 1,000 calls to a no-op host returning a 4-field object, minus the same loop without calls; median of 30 |
| Endless loop (`while (true) {}` / `for x in range(2147483647)`) | Timeout after 2000.1 ms | Timeout after 2001.5 ms | deadline 2 s; JS polls every ~10k operations, Starlark every 1,000 ticks |
| Long single built-in | Regex backtracking `/(a+)+b/` stops at 2000.1 ms (libregexp polls). `copyWithin` on a 2^40-length array-like **does not poll**: ran > 10 min, killed. Fixed by a prelude guard on every `Array.prototype` method (length ≤ 2^22); the worst case under the guard, `copyWithin` on a sparse 2^22 array, takes 127 ms | `"x" * (1 << 30)`: 3076 ms against the 2 s deadline (no check inside the operation) | probes `js-regex-backtracking`, `js-native-*`, `star-one-op-1gib` |
| Memory, gradual growth | `out of memory` (`ErrorKind::Memory`) after 140 ms, peak RSS 72 MiB | `Heap memory limit … exceeded` after 46 ms, peak RSS 65 MiB | push 10 KB strings into a list until stopped, 64 MiB limit |
| Memory, one large allocation | 512 MiB `repeat`: refused on allocation, `Memory` after 0.3 ms, RSS 3 MiB | 1 GiB repeat: allocated in full, **peak RSS 3 GiB**, then stopped (by the deadline here; `Memory` when it finishes first) | probes `js-one-op-512mib`, `star-one-op-1gib` |
| Memory, impossible allocation | refused (QuickJS checks the limit before `malloc`) | `("x" * 131072) * 2147483647` asks for 256 TiB: **the process aborts (SIGABRT)** | probe `star-one-op-256tib` |
| Memory outside the counted heap | — (every QuickJS allocation is counted) | a dict grew to **1.2–1.5 GiB RSS** in 2 s under 64 MiB; stopped only by the deadline | probe `star-dict` |
| Script catching the limit error | catching `out of memory` in a loop: stopped by the deadline at 2001 ms | (no exceptions in Starlark) | probe `js-catch-oom` |
| Binary size (stripped, release) | +1.03 MB (1,053,728 B) | +5.72 MB (5,717,136 B); both: +6.72 MB | minimal binary linking the crate and running one script, against the same binary without engines (390,240 B); the `rueca` CLI was not rebuilt (its full release build with llama.cpp is too heavy for the disk budget) |
| Dependencies | +10 crates (3 new to the workspace: `rquickjs`, `-core`, `-sys`) | +145 crates (80 new `Cargo.lock` entries, e.g. `rustyline`, `nix`, `lsp-types`, `logos`, `derive_more`) | `cargo tree -e normal,build` unique crates vs. no engine; new = added to `Cargo.lock` |
| Clean release build | 12 s (vs. 4 s without engines) | 33 s; both: 33 s (parallel) | `nice cargo build --release` of the minimal binary, empty target dir |
| C toolchain | yes: `cc` builds QuickJS-ng; bindings are pre-generated for the release targets (no `bindgen`/libclang) | no (pure Rust) | `rquickjs-sys` build script |
| `cargo deny check` (repository `deny.toml`) | passes (licences MIT; no advisories) | **fails, advisories**: RUSTSEC-2024-0388 `derivative` 2.2.0 (via `starlark`, `starlark_syntax`), RUSTSEC-2024-0436 `paste` 1.0.15 (via `starlark`), RUSTSEC-2025-0057 `fxhash` 0.2.1 (via `starlark_map`), all "unmaintained"; licences, bans and sources pass | `cargo deny check` (cargo-deny 0.21.0) |
| MSRV 1.90 | passes | passes | `cargo +1.90 check -p rueca-codemode --all-targets --locked` |
| Determinism, no ambient capabilities | same outcome twice; no `Date`, `performance`, timers, `require`, `process`, `fetch`, `eval`, `WeakRef`; `Math.random` and the `Function` constructors throw | same outcome twice; `load` rejected at parse time; no clock or randomness in the language | `crates/rueca-codemode/tests/sandbox.rs` |

What the rows mean for the experiment:

- **Start-up and call overhead are not a constraint for either engine**:
  both are far below §8's 20 ms overhead budget. Starlark is ~60× faster
  to start (no interpreter to build; JavaScript compiles its prelude on
  every run, which phase 1 can cache as bytecode) and ~5× cheaper per
  call (JavaScript crosses as JSON text and through a promise).
- **JavaScript's limits are enforceable in-process**: memory is checked
  on every allocation and the deadline is polled in the interpreter and
  the regex engine; the one non-polling built-in found
  (`Array.prototype.copyWithin` and friends on huge array-likes) is closed
  by the length guard. Residual: a built-in not probed here that loops
  without polling or allocating, bounded by the length guard or the
  memory limit.
- **Starlark's memory limit cannot be enforced in-process.** A one-line
  script can abort the `rueca mcp` process (impossible allocation), use
  gigabytes before the first check (one large operation), or grow memory
  the heap limit does not count (dictionaries) until the deadline. Running
  Starlark scripts in a child process with OS limits (the RFC 0008 §7.8
  isolation, §7.3) would contain all three, at the cost of a process per
  script or a worker pool. Its deadline is enforceable except inside one
  long operation (3.1 s observed for a 2 s limit).
- **cargo-deny**: Starlark cannot be merged to `main` as is. Options:
  - (a) set `[advisories] unmaintained = "workspace"` in `deny.toml`,
    which reports unmaintained crates only when they are direct
    dependencies of a workspace crate (verified: all checks then pass; it
    is a policy change, not an ignore, and applies to every future
    transitive crate);
  - (b) make the `starlark` feature non-default (verified: `cargo deny
    check` passes with `default = ["js"]`, since the check uses default
    features), which also takes Starlark out of CI's tests unless a job
    enables it;
  - (c) wait for upstream: `starlark-rust`'s main branch has already
    dropped `paste` and `fxhash` but still uses `derivative`;
  - (d) drop Starlark.

  No advisory is ignored and no licence was added to the allow-list.
- **Outcome (phase 1): option (b).** The owner rejected advisory
  ignores and policy changes, so Starlark is a **non-default cargo
  feature**: `rueca-codemode` has `default = ["js"]`, and the feature is
  forwarded as `code-mode-starlark` by `rueca-mcp` and the `rueca` binary.

  `cargo deny check` (CI's invocation: default features, `deny.toml`
  unchanged) passes; the default binary has no Starlark, and
  `language: "starlark"` there is an error that says to rebuild with
  `--features code-mode-starlark`. A CI job runs clippy and the tests of
  the code-mode crates with the feature on, so Starlark keeps building and
  passing; it is not in the deny job.

  For phase 3 this is evidence against
  keeping Starlark as is: it cannot be in the default build until upstream
  drops `derivative`, `paste` and `fxhash` (or the policy changes), and
  its memory limit stays best-effort in-process (`docs/tools.md`,
  "Sandbox caveats"; no cheap in-process mitigation was sound: the
  repetition operators of built-in strings and lists cannot be
  intercepted without patching the crate).
- **Starlark ergonomics** found while writing the tests: no `**` operator,
  `range` takes 32-bit integers, top-level `for`/`if` needed a dialect
  option (enabled), undefined names are compile-time errors. Unlike the
  Starlark specification, `starlark-rust` allows recursion (bounded here
  by 200 nested calls). These feed phase 3's fluency probe.

### 13.1 Phase 2 results

`rueca-bench multistep run`, 2026-11-24, Apple M4 Pro; `rueca` release
build `--no-default-features --features code-mode-starlark` (lexical only,
no embedding runtime: nothing in the suite embeds), cbm 0.12.0 (the bench's
pinned binary).

Tokens: cl100k_base. All 25 tasks and all four arms ran;
full tables in `docs/bench/multistep-results.md`, records in
`bench/results/multistep.json`.

The numbers below are re-measured after the
API fixes of §14.6 (stores re-indexed with that binary; reference scripts
and the ms25 plan without their workarounds); the first run's numbers,
before the fixes, are in the second table.

| Arm | Correct | Read / correct | … `describe_api` amortised | Written / correct | Round trips | `execute` overhead median / p95 |
|---|---:|---:|---:|---:|---:|---:|
| Tool-call plans | 18/25 | 2,743 | — | 254 | 143 | — |
| JavaScript | 18/25 | 429 | 666 | 352 | 25 | 0.63 / 1.20 ms |
| Starlark | 18/25 | 434 | 675 | 387 | 25 | 0.33 / 0.65 ms |
| cbm | 4/16 | 2,171 | — | 344 | 18 | — |

Before / after the API fixes (§14.6):

| | Plans | JavaScript | Starlark |
|---|---:|---:|---:|
| Correct | 18/25 → 18/25 | 18/25 → 18/25 | 18/25 → 18/25 |
| Read / correct | 3,639 → 2,743 | 444 → 429 | 448 → 434 |
| … `describe_api` amortised | — | 671 → 666 (4,086 → 4,260 tokens) | 680 → 675 (4,166 → 4,338 tokens) |
| Written / correct | 257 → 254 | 367 → 352 | 402 → 387 |
| Round trips | 145 → 143 | 25 → 25 | 25 → 25 |
| Overhead median / p95 | — | 0.56 / 1.13 → 0.63 / 1.20 ms | 0.32 / 0.58 → 0.33 / 0.65 ms |
| Cut vs plans (gate ≥ 40%), plain / amortised | — | 87.8% / 81.6% → 84.4% / 75.7% | 87.7% / 81.3% → 84.2% / 75.4% |

The plans got cheaper (ms25 reads one outline at both refs, 11,625 →
1,614 tokens; `trace` no longer returns code nested in tests, ms17 7,826
→ 2,502, ms19 3,795 → 3,385, ms16 1,042 → 686), so the cut shrank; the
scripts got slightly shorter (no id slicing). No task changed correctness
in any arm (cbm did not change: same binary, same answers).

- **Gate: both languages pass.** Tokens read per correct answer −84.4%
  (JavaScript) and −84.2% (Starlark) against the plans; −75.7% / −75.4%
  with `describe_api` (4,260 / 4,338 tokens) read once and amortised over
  the 25 tasks; no task the plans answer correctly is wrong in either
  language; median overhead under 1 ms (gate 20 ms).
- **Where it comes from.** The pooled ratio is dominated by the plans that
  must read whole listings:
  - The three contract tasks (6,533–7,780 tokens),
  - The classes of a library used across the organization (ms19, 3,385),
  - Callers of a library function across the organization (ms17, 2,502
    for one `trace`),
  - The ref diff (ms25, 1,614: one outline at both refs; 11,625 before
    the fixes, when it read the file at both refs).

  Without
  the five tasks that dominated the first run (ms17, ms21–ms23, ms25) the
  reduction is 76% (as before the fixes); the median per-task ratio of
  code mode to plan is 0.36 (0.26 over the 20 tasks expected to win),
  also unchanged.
- **Where code mode does not help.** A one-call question with a short
  answer: direct callers (ms04: 71 tokens as a call, 225 as a script) and
  callers outside a file (ms10: 296 vs 330) — the script source costs more
  than the answer. The other three single-step tasks still read less
  (a depth-2 trace, `list_repositories`, a route's handlers), and on the
  four tasks every arm answers correctly cbm's one Cypher query reads the
  least (446 tokens in total vs 1,307 plans, 1,116 JavaScript, 1,161
  Starlark): code mode's cost there is the script itself.
- **Correctness is the engine's, not the arm's.** The seven tasks no
  Rueca arm gets right (ms05, ms06, ms08, ms11, ms12, ms13, ms16) differ
  from the SCIP gold by one element each (a caller Rueca resolves that SCIP
  does not, a nested-function name); plans and both languages give the same
  answer on every task.

  The fixes of §14.6 moved none of them: the test
  code `trace` now leaves out was never in their answers (ms16 asks for
  callers under `src/`, and its plan only stopped reading the test-file
  callers it had to filter out).

  cbm's 12 misses are its own call graph (no
  constructor calls, class-less names for TypeScript object-literal
  methods, a missing `api.py` hop) — and its `[:CALLS*1..2]` / `[:CALLS*2]`
  return the direct callers only, so its plans spell the second hop out.
- **Limits hit**: none in any arm (plans ask for whole answers with
  `budget_tokens`; the tools' default budget would cut five of them).
- **Helpers (§4.3): none added.** Considered:
  - Depth-1 neighbours of a root in one direction as `path#Qualname` (the
    same five-line filter in 16 of 25 scripts per language — but it saves
    no round trip: one `trace` with `labels` already does the work, and a
    local-name field in the shapes would remove the boilerplate for every
    tool);
  - `callersAcrossRepos` (the three cross-repository tasks are one
    `trace` each plus an id-prefix filter);
  - A ref-diff helper (one task; the honest version needs a per-symbol
    comparison in the engine, below).

Friction from writing the 50 reference scripts (all four engine and API
items were fixed before merge, §14.6; the reference scripts no longer work
around them):

- Both languages: the result shapes' `qn` is the **global** qualified name
  (`flask.ctx.RequestContext`, `github.com/ollama/ollama/server.Server.ListHandler`)
  while the text renderings, and every statement, use the file-local one;
  the local name is only in the `id` (slice after `:` for `path#Qualname`).
  Three scripts were wrong on the first run because of it (ms05, ms24,
  ms25). The repository of a cross-repository `trace` node is likewise
  only in its id: `s` indexes `execute`'s `snapshots`, which the script
  never sees.
- Both: `changed` on multi-ref answers compares **file blobs**, so every
  symbol of a changed file is `changed` (`Session.get`, identical in both
  refs, is flagged), while the API says "not identical in every requested
  ref"; the ref-diff task has to fetch the file at both refs and compare
  line ranges. Reported, not fixed (per-symbol comparison is an engine
  change).
- Both: a `trace` with `include_tests: false` still returns functions
  nested in a `describe` block of `src/jsx/components.test.tsx`; scripts
  filter test paths themselves. Reported.
- Both: a tool argument error listed `format` among the valid arguments
  although code mode refuses it — **fixed** (`codemode/execute.rs`,
  `script_error`, with a test).
- JavaScript: nothing blocked; the root node has no `dir`, `[...new
  Set(…)]` for dedup, `await` in loops runs the calls one after another.
- Starlark: no regular expressions (test-path filter by substrings); the
  root node has no `dir`, so `n["dir"]` is a runtime error and every
  script uses `n.get("dir")`; flattening outline children needs recursion,
  which `starlark-rust` allows and the Starlark specification does not;
  `**root` with other keyword arguments, `sorted(key = lambda …)`, `set()`
  and comprehensions covered the rest.

## 14. Decision

Phase 3, 2026-11-24. The evidence is the multi-step suite (§8, §13.1),
the engine measurements of phase 0 (§13) and the fluency probe (§14.1).
This section is a verdict and a recommendation; graduating code mode is
the owner's decision.

### 14.1 Fluency probe

Protocol (`bench/multistep/probe/README.md`): 10 tasks of the suite
(ms01, ms02, ms07, ms09, ms14, ms15, ms17, ms19, ms22, ms25), chosen among
the 18 whose reference answers match gold and spanning kinds, × 2
languages.

One fresh Claude Opus subagent per (task, language) was given only that
language's `describe_api` output and the task statement — no repository
access, no reference script, no way to run anything — and wrote one
script, run unchanged with `rueca-bench multistep probe`. On a failure,
exactly one repair attempt, given the `execute` output.

n = 1 per cell and one model: an indication, not a measurement of model
fluency in general. Scripts, runner output and prompt template are
committed in `bench/multistep/probe/`; per-task table in
`docs/bench/multistep-results.md` (*Fluency probe*).

The probe ran against
the API *before* the fixes of §14.6, and its scripts and runner output are
kept as they were (they are not re-run as first tries); the reference
scripts and plans it is compared with below are the re-measured ones of
§13.1, after the fixes (before them: 339 / 338 and 4,169 tokens).

| | JavaScript | Starlark |
|---|---:|---:|
| Correct, first try | 9/10 | 9/10 |
| Correct after one repair | 9/10 | 9/10 |
| Script tokens, mean / median | 830 / 771 | 640 / 622.5 |
| Engine calls, all first tries | 166 | 167 |
| Script errors (syntax, runtime, timeout, limits) | none | none |
| Tokens read per correct answer on these 10 tasks: first-try scripts / reference scripts / plans | 1,007 / 316 / 2,594 | 796 / 318 / 2,594 |

- **No failure came from the language or the API's shape.** All 20
  first-try scripts ran without an error and no limit was hit. All 10
  Starlark scripts defined helper functions; none reached for `while` or
  exceptions (six used `set()`, which `starlark-rust` provides). The worry
  of §6 that models would trip over the dialect was not confirmed at this
  sample size.
- **The one failure is a silent wrong answer.** ms19 (classes of another
  repository) failed in both languages the same way: both scripts returned
  the gold file lists but keyed the object by the short class names
  (`HTTPException`) where the statement asks for the names "exactly as
  written" (`werkzeug.exceptions.HTTPException`) — an ambiguity in the
  statement's answer shape.

  With no error to read, the JavaScript repair
  guessed at a different cause (it widened what counts as a use) and
  returned a worse answer, still wrong. The Starlark repair was not
  obtained: the repair subagent's file write was blocked by the harness's
  permission system, a tooling failure unrelated to the language.
- **Model-written scripts are longer than the reference ones.** On these
  tasks the first-try scripts have 3.5× (JavaScript) and 2.7× (Starlark)
  the source tokens of the reference scripts (defensive checks,
  `try`/`catch` in 9 of 10 JavaScript scripts, helper functions) and make
  twice the engine calls (166 and 167 vs 81).

  Tokens read per correct answer still fall 61% (JavaScript) and 69%
  (Starlark) against the plans on the same tasks — above the 40% gate,
  but short of the 88% the reference scripts show there; on ms01, ms02
  and ms09 the JavaScript script reads as much as the plan or more
  (Starlark on ms01). Starlark scripts were about 23% shorter than
  JavaScript ones.

  (Against the plans and reference scripts before the fixes: 3.2× /
  2.5×, 83 calls, 76% / 81% vs 92%; the plans of ms17, ms19 and ms25 got
  much cheaper.)

### 14.2 Gate (§8)

| Language | Read / correct vs plans (≥ 40%) | … `describe_api` amortised | Tasks regressed (none) | Median overhead (< 20 ms) | Gate |
|---|---:|---:|---|---:|---|
| JavaScript | −84.4% (429 vs 2,743) | −75.7% (666) | none | 0.63 ms | pass |
| Starlark | −84.2% (434 vs 2,743) | −75.4% (675) | none | 0.33 ms | pass |

Re-measured after the API fixes of §14.6; before them −87.8% / −87.7%
(444 / 448 vs 3,639), −81.6% / −81.3% amortised, overhead 0.56 / 0.32 ms
(§13.1). The cut shrank because the plans got cheaper, not because the
scripts got dearer. The same 18 of 25 tasks are correct in all three Rueca
arms, before and after.

The pooled ratio is dominated by the three
contract tasks (6,533–7,780 tokens as plans) and the cross-repository
listings; without the five tasks that dominated the first run (ms17,
ms21–ms23, ms25) the cut is 76%, and the median per-task ratio of code
mode to plan is 0.36, both as before the fixes. Both numbers come from the
reference scripts; the probe's model-written scripts read more (§14.1).

### 14.3 Per-language verdict

**JavaScript: keep (experimental).** Passes the gate. Its limits are
enforceable in-process: the memory limit is checked on every allocation,
the deadline is polled by the interpreter and the regex engine, and the
one non-polling built-in found is closed by the array-length guard (§13).
It passes `cargo deny` in the default build. Cost: +1.03 MB stripped, +10
crates, a C compiler (already required by the workspace). Probe: 9/10 on
the first try, no script error; its one miss is the statement ambiguity
both languages share.

**Starlark: works, but costs what JavaScript does not.** It passes the
gate with numbers indistinguishable from JavaScript's, and starts faster
(about 1.5 µs against about 0.1 ms; irrelevant next to engine calls that
take milliseconds).

In the probe its scripts were about 23% shorter (796 vs 1,007 tokens
read per correct answer on those tasks, n = 1 per cell; the reference
scripts show no such gap: 434 vs 429 suite-wide) and it scored 9/10 on
the first try: the hypothesis that models would trip over the dialect
was not confirmed at this sample size.

Against it:

- Its dependency tree fails the repository's `cargo deny` advisory
  policy (three unmaintained crates: `derivative`, `paste`, `fxhash`),
  so it cannot be a default feature;
- Its memory limit cannot be enforced in-process (an impossible
  allocation aborts the process, dictionary tables are not counted, a
  long built-in overruns the deadline: §13);
- It adds +5.72 MB and +145 crates.

### 14.4 Where code mode falls short

1. **Single-step and near-single-step questions**: the script costs more
   than the answer (ms04: 71 tokens as a tool call, 225 as a script; ms10:
   296 vs 330).
2. **`describe_api` costs about 4.3k tokens (cl100k_base; 4,260 for
   JavaScript, 4,338 for Starlark) once per session.** The suite
   saves about 1,670 tokens per task on average (about 2,300 before the
   fixes made the plans cheaper), so it pays back after roughly three
   multi-step questions and is a net loss in a session with one small
   question.
3. **It does not improve correctness, and it can hide evidence.** The 7
   tasks the tool-call plans get wrong are wrong identically in both
   languages (engine precision against the SCIP gold, §13.1). A script
   returns only its answer: a wrong but plausible answer gives the agent
   no signal to repair, and the intermediate answers that might have let
   it notice never reach it (probe ms19).
4. **cbm's single Cypher query reads less where it can express a task and
   gets it right**: 446 vs 1,116 tokens (JavaScript) on the 4 tasks every
   arm answers correctly. But cbm was correct on only 4 of the 16 tasks it
   can express and cannot express the cross-repository, contract and ref
   tasks (9 of 25).
5. **API friction found while writing scripts** (§13.1): `qn` was the
   global name while text and ids use the file-local one; a
   cross-repository node's repository was recoverable only from its id; the
   multi-ref `changed` flag was per file, not per symbol; `include_tests:
   false` leaked functions nested in `describe` blocks of test files.
   **Fixed before merge** (§14.6); the suite's numbers above are
   re-measured after the fixes.
6. **Model-written scripts cost more than the reference ones** (§14.1):
   3.5× / 2.7× the source tokens, so the suite's −84.4% is an upper bound
   on what an agent would see.
7. **Measurement limits**: reference scripts stand in for an agent; the
   probe is n = 1 per cell with one model; a plan's wall time is inflated
   by one process per call, so latency comparisons other than the
   in-process overhead are not evidence.

### 14.5 Recommendation

- **Keep code mode as an opt-in experimental feature with JavaScript
  only.** It passes the gate, its limits hold in-process, it passes the
  dependency policy, and a fresh model wrote correct scripts from the
  reference alone in 9 of 10 tasks.
- **Remove Starlark**: its one measured edge (shorter model-written
  scripts) does not offset the advisory policy failure, the unenforceable
  memory limit and the 5.7 MB — unless the owner wants to keep it, as in
  this change, as a non-default build feature for further comparison.
  This change keeps it; removing it is deleting the `starlark` feature.
  *(The owner keeps it as built, §14.6.)*
- **Do not graduate to default-on in this change**; that is the owner's
  decision. Suggested conditions before graduating: fix the API friction
  of §14.4 (5) — *done before merge, §14.6*; give agents a cheap way to
  surface evidence (e.g. encourage returning counts or sample ids
  alongside the answer), so a wrong answer can be noticed; and run a real
  agentic evaluation.
- **No helpers (§4.3) were added**: none saved a round trip.

### 14.6 Owner decisions (2026-11-24)

- **Starlark stays as built**: the optional build feature
  `--features code-mode-starlark`, with its own CI job; `deny.toml` is
  untouched (the default build has no Starlark, so the advisories of §13
  do not reach it). JavaScript stays the default language.
- **The four API friction items of §14.4 (5) are fixed before merge**, at
  the source, for every tool and so for the script API:
  1. *Names.* Every object with an `id` carries `local`, the file-local
     qualified name its id is built from (the id's `#` part; a file's
     path), next to `qn`, the global qualified name (contract rows keep
     `symbol` for it). Scripts build `path#Qualname` from fields instead of
     slicing ids.
  2. *Origin.* The same objects carry the `repo` and `ref` of their
     snapshot, in every answer (one rule, not only when an answer spans
     several snapshots); `trace`'s root has `dir: "root"`, so every node
     has `dir`. JSON only: the text rendering (what an agent reads) is
     unchanged, so single-repository answers cost no more tokens.
  3. *`changed` per symbol.* A multi-ref item is `changed` when the
     symbol's own text (its span) differs between the requested refs, or
     it is absent from one; a symbol moved by lines added above it is not.
     `get_file_outline` marks each symbol of each version, so the ref-diff
     task (ms25) is one outline call. Versions are still collapsed by file
     blob (`in`), and the outline's own `changed` stays the file's.
  4. *Tests nested in tests.* The resolver marks a definition nested in a
     test function, method or test block as a test (a closure of a
     `describe` callback, a helper inside a Python `test_*` or a Rust
     `#[test]` function); the TypeScript pack marks definitions inside
     `beforeEach`-style hook callbacks (extractor version 7). Go `t.Run`
     closures and C++ lambdas are not definitions (their calls already
     belong to the test); Java local classes in test code were already
     tests. A test *class* does not pass the flag on (`TestClient` is also
     a library name). Existing indexes need a re-index to pick it up.

  The reference scripts and the ms25 tool-call plan drop their
  workarounds (every reference answer is unchanged; the ms25 plan reads
  one outline instead of the file at both refs). `describe_api` grows to
  4,260 (JavaScript) and 4,338 (Starlark) tokens.

  The suite numbers of §13.1 and §14 are re-measured after the fixes
  (before / after in §13.1 and `docs/bench/multistep-results.md`):

  - The gate still passes in both languages (−84.4% / −84.2%, −75.7% /
    −75.4% amortised; −87.8% / −87.7% before),
  - No task changed correctness,
  - The multi-ref scenario of the organization suite now scores: no
    unchanged symbol of the changed file is flagged `changed` (6 before
    the fixes, 0 after; `docs/bench/alpha-results.md`).
