# RFC 0009 — Code mode (experimental)

- Status: Draft
- Created: 2026-11-24
- Depends on: RFC 0001 (§11 tools, §13 security), RFC 0004 (efficiency
  metrics), RFC 0005 (envelope, pinned ids, `repo` / `ref`, tokens per
  correct answer), RFC 0006 (organization, contracts), RFC 0008 (service
  mode, child-process isolation)
- Research date: 2026-11-24. Crate versions were read from crates.io that
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
- The language is chosen in phase 0 by measurement between **JavaScript
  (QuickJS via `rquickjs`)** and **Starlark (`starlark`)**; this RFC
  recommends JavaScript (§6).
- It graduates from experimental only if the efficiency bench shows a
  clear gain on multi-step questions (§8); otherwise it is removed.

## 2. Motivation

The published benchmark (`docs/bench/alpha-results.md`) shows where Rueca
still loses to codebase-memory-mcp 0.12.0: **tokens per true edge from
`trace`** — Python 57 vs 48, TS/JS 64 vs 54, PHP 72 vs 56, C 52 vs 30,
C++ 63 vs 61; Rueca wins in Java (67 vs 76) and Rust (51 vs 106). cbm also
offers `query_graph`, a free-form graph query, which Rueca does not have.

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
| `execute` | Run a script; return its value |
| `describe_api` | Return the API reference (TypeScript declarations or Starlark docstrings), so the agent loads it only when it decides to use code mode |

`describe_api` exists so the declarations do not inflate every session's
tool list; the `execute` description stays short and points to it.

### 4.2 `execute`

| Arg | Type | Default |
|---|---|---|
| `code` | string, the script | — |
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

Phase 2 may add a few helpers that save round trips without new engine
features (e.g. `rueca.callersAcrossRepos(id)`), each justified by a bench
task. Helpers are thin compositions of the eight tools.

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
| Termination | Needs an interrupt handler (QuickJS supports one) | Guaranteed by the language: no unbounded loops or recursion | `max_operations` built in |
| Memory limit | QuickJS memory limit | to verify in phase 0 *(unverified)* | built-in size limits |
| Determinism | Remove `Date`, `Math.random` and timers | Deterministic by design | Deterministic |
| Rejected | — | — | Model fluency |

**Recommendation: JavaScript.** Code mode's value depends on the model
writing a correct script on the first try, and both published reports use
JavaScript/TypeScript. QuickJS gives the two controls that matter (memory
limit and an interrupt handler) and has no I/O unless its `std`/`os`
modules are added, which Rueca does not do. Starlark is the fallback if
phase 0 shows QuickJS's start-up, binary size or sandbox controls are a
problem; its hermeticity is attractive but its missing constructs will
cause failed scripts.

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
  would make) and a reference **script**, both written once and committed.
- Measured: tokens the agent reads (sum of tool answers vs. script source
  plus `execute` result), number of calls, correctness of the final answer
  against gold, latency. Tokens counted with the bench's tokenizer (RFC
  0005 §8).
- Compared against the same tasks answered with cbm's tools where cbm can
  express them (including `query_graph`).

**Graduation gate:** on the suite, code mode reads at least 40% fewer
tokens per correct answer than the tool-call plans, with no task becoming
incorrect, and median `execute` overhead under 20 ms above the sum of the
engine calls *(defaults)*. Single-step questions are not expected to
improve and must not regress, since the tools are unchanged.

What this does not measure: whether models write good scripts. That needs
agentic runs, which RFC 0004 defers; the reference scripts stand in for a
competent agent.

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
   phase 0 rejects both languages.
3. **Making the tools more compact.** Necessary anyway (the `trace` gap is
   formatting) and independent of this RFC.
4. **WebAssembly sandbox (wasmtime) running agent-provided modules.**
   Strongest isolation, but agents cannot write Wasm; it would host one of
   the interpreters above, at a large dependency cost.

## 10. Phases

- **Phase 0 — spike (no PR to main).** Embed QuickJS and Starlark behind
  the same `Sandbox` trait; bind two API functions; measure start-up time,
  per-call overhead, binary size, memory-limit and interrupt behaviour,
  and cargo-deny. Pick the language by §6's criteria; record it.
- **Phase 1 — `execute` and `describe_api` behind the flag.** Generated
  API from `tool_definitions()`, limits of §7, result envelope of §4.2,
  tests (limits, determinism, errors, golden transcripts unchanged when
  disabled).
- **Phase 2 — multi-step suite (§8) and helpers** justified by tasks.
- **Phase 3 — decide.** Graduate (document in tools.md as a normal tool,
  still opt-in or on by default) or remove.

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

1. JavaScript or Starlark — decided in phase 0 (§6 recommends JavaScript).
2. Should `describe_api` return TypeScript declarations for every tool, or
   a shorter hand-written reference?
3. Should enabling code mode hide the eight tools (Cloudflare's
   single-tool shape) or keep them alongside? This RFC keeps them.
4. Default `budget_tokens` and `timeout_ms`.
5. Which helpers (§4.3) earn a place, if any.
