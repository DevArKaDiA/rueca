# RFC 0007 — Semantic search

- **Status:** **phases 0–1 implemented**, opt-in. Phase 0: the runtime
  (`rueca-embed`, `rueca models`, `rueca embed`, the bench's vector arm).
  Phase 1: hybrid search (`rueca index --embed` / `RUECA_EMBED`, the store
  vector cache, `vectors.*` components, `search_symbols` `mode` and
  `retrieval`, the bench's hybrid arm). The owner's decisions of
  2026-11-24 (§0) set the defaults; phases 2–3 are follow-ups (§12)
- **Created:** 2026-11-16
- **Updated:** 2026-11-24
- **Depends on:** [RFC 0001](0001-architecture.md) (§8 storage, §11 query engine), [RFC 0004](0004-quality-and-benchmark.md) (metrics,
  calibration split), [RFC 0005](0005-agent-responses.md) (envelope), [RFC 0006](0006-org-graph.md) (org index, routing)
- **Amends:** [RFC 0001](0001-architecture.md) §8.1 item 5, §11.3, §19 open question 3; [RFC 0002](0002-building-blocks.md) §2
  (excluded models), §6.2 (rows "Vectors", "Offline embeddings", "Quality
  embeddings"); [RFC 0004](0004-quality-and-benchmark.md) §6, §11, §13 open questions 1–2; [RFC 0006](0006-org-graph.md) §6, §9.2
- **Revised:** 2026-11-17 (the owner's answers, §15; organization-suite
  evidence, §2)
- **Research date:** 2026-11-16. Model cards, licenses, file sizes, crate
  versions and prices were checked that day (§17). *(estimate)* marks
  arithmetic on published figures, with its inputs; *(unverified)* marks a
  claim no primary source confirmed.

## 0. The owner's decisions (2026-11-24)

Recorded in `docs/decisions.md` (2026-11-24); they override what the
sections below proposed where they differ.

- **Model** `qwen3-0.6b-q8` (Qwen3-Embedding-0.6B `Q8_0`), canonical text
  `full`, token budget 512.
- **Fusion**: convex combination, **min-max** normalization over each
  list's top N, **α = 0.5**, **N = 100** (§7.2). These are the code's
  defaults.
- **Search**: the exhaustive int8 scan; HNSW deferred (§6.4).
- **Opt-in locally**: semantic search is off unless enabled by argument
  (`rueca index --embed[=PRESET]`) or environment (`RUECA_EMBED=1` /
  `RUECA_EMBED=<preset>`; `0`, empty, `false`, `off` disable).
  Precedence: argument > environment > configuration; `[retrieval]` alone
  never enables embedding (§8.3). An enabled run without weights is an
  error naming `rueca models pull`; indexing and queries never download.
- **Queries**: hybrid automatically when the snapshot has vectors and the
  model loads, else lexical, saying so in the envelope; `mode` picks
  `lexical`, `hybrid` or `semantic`; the envelope names the mode used and
  the model key; `rueca status` names the preset (§7.7, §8).
- **Release binaries** include the runtime; it activates only when enabled.
  Service mode turns it on later (phase 3).
- **Rejected**: Model2Vec / static embeddings, int4 as the default, tiny
  encoders as the default (none passed the bar).

**Evidence** (S1 calibration split, 118 instances, File Acc@5 / Function
Acc@10, paired against BM25). Issue text (`raw`): BM25 78.8 / 57.9, Rueca
lexical 73.7 / 61.7, hybrid with `qwen3-0.6b-q8` at α 0.5 **83.1 / 70.1**
(Function +12.1, 95% interval +2.8 … +21.5; File +4.2, −2.5 … +11.0),
hybrid with granite-47m 83.1 / 64.5. Keywords (`kw`): BM25 72.9 / 57.9,
lexical 75.4 / 53.3, hybrid 73.7 / 56.1. Semantic search helps
natural-language questions, not keyword ones. Cost: indexing Rueca itself
(9,319 symbols) took 400 s on an M1 Pro (Metal), about 23 symbols/s; a CLI
query about 0.25 s including the model load.

## 1. Summary

Rueca gets semantic search as a **stage inside `search_symbols`** and
inside the organization ranking, not as a separate tool; `search_code`
(literal and regex over content) does not change.

1. **Goal**: natural-language questions ("where is login handled?") and
   issue localization must clearly beat both BM25 and a vector arm alone,
   in one repository and at organization scope, on RFC 0004's suites.
2. **Top-tier models in presets**: the Qwen3-Embedding family (Apache-2.0,
   official GGUF files) at 0.6B / 4B / 8B, CodeRankEmbed, opt-in remote
   APIs; weights downloaded on first use, sha256-pinned, never in the
   binary. This replaces the static embeddings of RFC 0002 §6.2 and RFC
   0004 §11.
3. **Runtime assembled, not built**: llama.cpp through `llama-cpp-2`
   behind an `Embedder` / `Reranker` trait pair in a new crate
   `rueca-embed`; `fastembed` (candle, pure Rust) as fallback.
4. **Vectors are content**: one vector per symbol, keyed by a hash of the
   model and the embedded text, cached in the store like facts and packs,
   so a new branch, worktree or repository embeds only new text. Segments
   carry int8 vectors as optional components (format 2, no bump).
5. **Two stages**: BM25 ∪ dense ∪ graph-neighbour candidates fused by a
   convex combination of normalized scores times today's priors; then an
   optional cross-encoder reranker, gated by measurement.
6. **Measured before it is a default**: a vector arm, a CoIR runner, S1
   on both query variants, the organization scenario, an external
   SWE-bench Lite diagnostic. Every default is chosen by a stated rule on
   the calibration split or CoIR, never on the gated set (RFC 0004 §7.7).
   Code leaves the machine only with a `remote:` preset.

Both owner decisions were decided on 2026-11-17: the default quantization
(§3.3: `qwen3-0.6b-q8`, with int4 a preset that phase 0 promotes per
backend only by the owner's rule) and the reranker (§7.5: off by default
locally; the 4B and mxbai-rerank-large-v2 measured first, on in service
mode only past its gate). The other open questions were closed
provisionally the same day, each with a "Revisit if" note (§15).

## 2. Motivation

Rueca's symbol search is BM25 over names, signatures, docs, paths and
bodies, with a code-aware tokenizer, priors and repository routing. It
finds what an agent names, not what it describes.

- **Localization ties BM25.** Pooled over the 313 gated S1 instances
  (`kw`), File Acc@5 is 67.1 against 67.4 for the BM25 arm *(derived from
  the per-language rows of `docs/bench/alpha-results.md`)*; Rust trails on
  both gated metrics (62.8 vs 67.4, 39.0 vs 46.3). Per-language paired
  intervals span roughly ±10–17 points: gains are demonstrable only pooled.
- **Organization scope trails** the single repository: gated File Acc@5
  42.9% vs 61.9% (0.69×, floor 0.85×, RFC 0006 §9.2). In the larger
  organization scenario it cannot be told apart from BM25: File Acc@5
  56.7 vs 52.5 (+4.2, 95% interval −4.2 … +12.5; minimum detectable
  effect 12.1 points at n = 120; bench organization suite,
  [PR #60](https://github.com/DevArKaDiA/rueca/pull/60)).
- **Impact questions lose to BM25.** The organization suite's "impact"
  kind gives a changed library symbol and asks for its consumers across
  the organization: Rueca hit@10 12.5 vs BM25 37.5 (−25.0, 95% interval
  −50.0 … −6.2; same source). The consumers do not share the symbol's
  words the way BM25's file-level text does. It is a graph question —
  the consumers are the symbol's callers and importers — that §7.3's
  expansion and the dense signal should answer together.
- **Vocabulary mismatch**, met while writing this RFC with Rueca's own
  tools: "bm25 ranking" missed `top_hits`, "read symbol source text from
  content pack" missed `body_texts` (§16).
- **Published evidence.** On SWE-bench Lite a 137M code embedder,
  CodeRankEmbed, reaches File Acc@5 84.7 and Function Acc@10 58.8 against
  BM25's 61.7 / 36.9 ([LocAgent](https://arxiv.org/html/2503.09089),
  Table 4); on the nine-language SWE-bench Multilingual,
  Qwen3-Embedding-0.6B reaches Function Acc@10 38.0 and the best
  retrieve-and-rerank stack 71.4 ([SweRank+](https://arxiv.org/html/2512.20482)).
  Dense retrieval helps; multi-language localization is harder; the
  largest gains come from issue-tuned models and large rerankers.

## 3. Models

### 3.1 Presets

| Preset | Model and file | Params | Dims (MRL) | Context | License | On disk | Code quality (nDCG@10 unless noted) | Status |
|---|---|---:|---:|---:|---|---|---|---|
| `qwen3-0.6b-q8` | Qwen3-Embedding-0.6B, official `Q8_0` GGUF | 0.6B | 1024 (32–1024) | 32k | Apache-2.0 | 639 MB | MTEB-Code 75.41 ([Qwen3 report](https://arxiv.org/html/2506.05176v3)); CoIR-10 74.96 *(derived from [C2LLM](https://arxiv.org/html/2512.21332), Table 2)* | **default, provisional (§3.3)** |
| `qwen3-0.6b-q4` | same, `Q4_K_M` converted and hosted by Rueca | 0.6B | 1024 | 32k | Apache-2.0 | ≈ 380–400 MB *(estimate; a [community Q4_K_M](https://huggingface.co/PeterAM4/Qwen3-Embedding-0.6B-GGUF) is 378 MB)* | no published run; 4-bit RTN −7.5% ([arXiv 2609.24322](https://arxiv.org/html/2609.24322v1)) | optional; default on a backend only by §3.3's rule |
| `qwen3-0.6b-f16` | same, official `f16` | 0.6B | 1024 | 32k | Apache-2.0 | 1,198 MB | reference | optional; reference for rank overlap |
| `qwen3-4b-q4` / `-q8` | Qwen3-Embedding-4B, official `Q4_K_M` / `Q8_0` | 4B | 2560 | 32k | Apache-2.0 | 2,497 / 4,280 MB | MTEB-Code 80.06 (Qwen3 report) | optional; GPU |
| `qwen3-8b-q4` / `-q8` | Qwen3-Embedding-8B, official `Q4_K_M` / `Q8_0` | 8B | 4096 | 32k | Apache-2.0 | 4,677 / 8,047 MB | MTEB-Code 80.68 (same) | optional; GPU, service mode |
| `coderank-137m` | CodeRankEmbed | 137M | 768 *(unverified)* | 8192 | MIT | ≈ 275 MB at f16 *(estimate: 2 bytes per parameter)* | CoIR 60.1 ([card](https://huggingface.co/nomic-ai/CodeRankEmbed)); Lite File Acc@5 84.7, Fn Acc@10 58.8 (LocAgent) | optional; runtime path confirmed in phase 0 |
| `gte-modernbert-149m` | gte-modernbert-base | 149M | 768 | 8192 | Apache-2.0 | ≈ 300 MB at f16 *(estimate)* | CoIR 71.5 ([Granite R2](https://arxiv.org/html/2508.21085), Table 11; CoIR training data *(unverified)*) | candidate "fast" preset; runtime path *(unverified)* |
| `remote:<provider>/<model>` | API (§3.5) | — | per provider | — | provider terms | — | per provider | opt-in |

Official sizes are Hugging Face file sizes; MTEB-Code is the 12-task
`MTEB(Code, v1)`, CoIR-10 drops its two MTEB-only tasks. Permissive models
left out of v1: C2LLM (Apache-2.0, best open MTEB-Code at 7B, 80.75, but
custom PMA pooling behind `trust_remote_code`: no GGUF / llama.cpp path),
nomic-embed-code (7B, no published CoIR number), CodeSage-large-v2
(`trust_remote_code`), and [voyage-4-nano](https://huggingface.co/voyageai/voyage-4-nano)
(open weights sharing an embedding space with the voyage-4 API; no
published code numbers).

### 3.2 Excluded by license

RFC 0002 §2 forbids CC-BY-NC and "rules" licenses. On the research date
([jina-code-embeddings](https://huggingface.co/jinaai/jina-code-embeddings-1.5b),
[jina v5](https://jina.ai/news/jina-embeddings-v5-text-distilling-4b-quality-into-sub-1b-multilingual-embeddings/),
[Qodo RAIL-M](https://www.qodo.ai/open-rail-m-license/), Hugging Face API)
that excludes: **CC-BY-NC-4.0** — jina-code-embeddings-0.5b / 1.5b,
jina-embeddings-v5 (v4 treated alike), SFR-Embedding-Code ("research
purposes only"), SweRankEmbed / SweRankLLM (the issue-tuned models of
§11.3), jina-reranker-v2 / v3; **use-restriction licenses** —
Qodo-Embed-1 (Open-RAIL-M / custom) and EmbeddingGemma (Gemma terms).

### 3.3 Default embedder: quantization (decided by rule)

**Owner's decision** (2026-11-17): the default preset is
**`qwen3-0.6b-q8`**, provisional and decided in phase 0 by the owner's
rule; `qwen3-0.6b-q4` ships as a preset. **Owner's rule:** int4 wins on a
backend only if it is at least 1.2× faster there *without* a large loss,
by the thresholds below. The owner first chose int4, "the fastest
acceptable"; on the evidence below the rule yields Q8 on Metal and x86.
**Evidence:**

- **Supply.** Qwen publishes the 0.6B GGUF only as `Q8_0` (639,150,592
  bytes) and `f16` (1,197,629,632 bytes); the 4B and 8B also ship
  `Q4_K_M` ([Qwen3-Embedding-0.6B-GGUF](https://huggingface.co/Qwen/Qwen3-Embedding-0.6B-GGUF)).
  An int4 0.6B is a Rueca-made file, with weaker provenance.
- **Speed.** Embedding is prompt processing (prefill), compute-bound. In
  llama.cpp's Apple table (7B model, pp512) F16 is fastest on every M1–M4
  row (M1 Pro: F16 302, Q8_0 270, Q4_0 266 tokens/s) and Q4_0 is within
  5% of Q8_0 on every chip, slightly ahead on some and fastest on some M5
  rows measured with newer builds
  ([llama.cpp #4167](https://github.com/ggml-org/llama.cpp/discussions/4167));
  on x86 CPUs Q4_0 is 6–8% faster than Q8_0; only an ARM CPU without its
  GPU gains about 2× (M2 Max CPU 114 vs 56 tokens/s,
  [ik_llama.cpp](https://github.com/ikawrakow/ik_llama.cpp/wiki/Jan-2025:-prompt-processing-performance-comparison),
  January 2025, possibly stale).
- **Quality.** With 4-bit round-to-nearest weights (group 128),
  Qwen3-Embedding-0.6B loses 7.5% nDCG@10 on four BEIR sets and, of the
  queries whose top 1 was relevant, 22.2% lose it, the worst of the
  three sizes (8B: −3.1%) ([arXiv 2609.24322](https://arxiv.org/html/2609.24322v1)).

  A second study of the same model keeps 98.7% of nDCG@10 at group 128
  and about 100% at group 16 (asymmetric; SciFact, NFCorpus and the
  SkillRet test split), yet group 16 still replaces 13–22% of the top 10
  ([arXiv 2609.16391](https://arxiv.org/html/2609.16391v1)): the size of
  the loss is disputed, the ranking churn is not. `Q4_K` sits between
  the two group sizes *(unverified)*; no MTEB run of a Qwen3-Embedding
  `Q4_K_M` was found.

On Metal and x86 int4 gains at most 8%, below the 1.2× bar, so the rule
alone yields `Q8_0` there, and the loss is uncertain; int4 can win only
when an ARM CPU embeds without its GPU.

**Recorded:** phase 0 applies the rule per backend with these thresholds.
*Faster*: int4 at least 1.2× Q8's tokens/s on that backend. *Loss is
large*: CoIR cheap-profile mean nDCG@10 (§11.1) more than 1.0 point below
Q8, or more than one calibration instance lower (pooled File Acc@5 or
Function Acc@10), or top-10 overlap with F16 below 0.9 (aggregate nDCG
hides ranking churn, per the same paper). If int4 wins on a backend (ARM
CPU-only Linux), the default becomes per backend; the outcome is recorded
in `docs/decisions.md` with the sweep.

### 3.4 Default embedder: size and family (decided by rule)

The 0.6B is the weakest candidate on the tasks closest to localization:

| Task | Qwen3-0.6B | Qwen3-4B | Qwen3-8B | Others | Source |
|---|---:|---:|---:|---|---|
| SWE-Bench retrieval (issue → code), nDCG@10 | 76.12 | — | — | code-specialised 83.00–87.40 | [jina-code-embeddings](https://arxiv.org/html/2508.21290), Table 2 |
| CodeEditSearch, nDCG@10 | 64.42 | 76.49 | 76.97 | 80.30–84.43 | C2LLM; jina paper |
| SWE-bench Lite, Fn Acc@10 (all-gold) | 62.77 | — | 71.53 | CodeRankEmbed 58.76 | SweRank+, Table 2; LocAgent |
| SWE-bench Multilingual, Fn Acc@10 (all-gold) | 38.03 | — | 46.15 | CodeRankEmbed 35.04 | SweRank+, Table 2 |

The 4B costs about 8× the 0.6B per token (7.5 vs 0.91 GFLOP/token from
the configs *(estimate)*): 100k symbols take 77–154 min with the 0.6B and
10.6–21 h with the 4B on an M1 Pro GPU *(estimate, §5.6)*.

**Recorded (decided by rule, 2026-11-17):** phase 0 measures
`qwen3-0.6b-q8`, `qwen3-4b-q4`, `coderank-137m` and, as a fourth
candidate, `gte-modernbert-149m` on the S1 calibration split (`raw` and
`kw`) and the CoIR cheap profile.

**Rule:** the local default is the
candidate with the best macro over languages of ½ (File Acc@5 + Function
Acc@10) on the calibration split, `raw` and `kw` averaged, among those
whose cold indexing time on an M1 Pro GPU stays within **twice the
0.6B's**; ties within one calibration instance go to the cheaper model. A
candidate above the budget (the 4B, about 8× by §5.6's estimate) may
become the service-mode GPU default instead (§8.3). Every candidate must
meet the CoIR sanity row of §11.2.

### 3.5 Remote presets

Opt-in, mainly for service mode. The embed key (§5.4) uses provider,
model, version string and dimensions instead of a file hash, and Rueca
records the date of first use (a provider may change weights behind a
name).

**Recorded (provisional, 2026-11-17): privacy.** A `remote:` preset is
set per repository (`rueca.toml`) or per organization
(`rueca-org.toml`), never by default, and `rueca-org.toml` gains
`[retrieval] allow_remote` (default `false`) so an organization file can
forbid them: an organization's code leaves the machine only when that
key is `true` and a `remote:` preset is set (a repository indexed outside
any organization opts in by setting the preset alone). The envelope's
`model` (§8.2) names the preset in every answer, so a remote model is
visible wherever it ranked.
*Revisit if:* service mode needs per-caller policies (RFC 0001 §13
permissions) rather than one switch per organization.

| Preset | $ / 1M tokens | Dims | Code evidence | Source |
|---|---:|---|---|---|
| `remote:voyage/voyage-code-4` | 0.12 (first 200M free) | 2048 / 1024 / 512 / 256 | vendor: +27.54% over voyage-code-3 on 19 agentic sets | [Voyage](https://blog.voyageai.com/2026/08/13/voyage-code-4/), [pricing](https://docs.voyageai.com/docs/pricing) |
| `remote:voyage/voyage-code-3` | 0.18 | same | MTEB Code 79.84, SWE-Bench 87.02 | jina-code-embeddings |
| `remote:openai/text-embedding-3-large` | 0.13 | up to 3072 | MTEB (Code) 58.95 | [pricing](https://developers.openai.com/api/docs/pricing); Qwen3 report |
| `remote:gemini/gemini-embedding-001` | 0.15 | 128–3072 | MTEB-Code 76.00, SWE-Bench 87.40 | [pricing](https://aistudio.google.com/docs/pricing); C2LLM; jina paper |
| `remote:bedrock/titan-embed-text-v2` | 0.02 | 256 / 512 / 1024 | none published | [AWS](https://aws.amazon.com/blogs/machine-learning/get-started-with-amazon-titan-text-embeddings-v2-a-new-state-of-the-art-embeddings-model-on-amazon-bedrock/) |
| `remote:bedrock/cohere-embed-v4` | 0.12 *(unverified)* | 256–1536 | none published | [tracker](https://www.getmaxim.ai/bifrost/llm-cost-calculator/provider/bedrock/model/cohere.embed-v4-0) |
| `remote:openai-compatible/<url>` | self-hosted | per server | — | one adapter for TEI, llama-server, vLLM |

Embedding 20M tokens costs $2.40 (voyage-code-4), $2.60 (OpenAI large) or
$0.40 (Titan v2) *(estimate)*; rate limits were not researched. Rerank
APIs: Voyage rerank-3 at $0.05 per 1M tokens, Cohere Rerank 3.5 on Bedrock
at $2.00 per 1,000 queries ([Bedrock pricing](https://aws.amazon.com/bedrock/pricing/)).

## 4. Runtime

### 4.1 Crate, traits, features

A new crate, `rueca-embed`, owns presets, weights, runtimes and remote
adapters behind two traits: `Embedder` (embed key, maximum dimensions,
`embed_documents(&[&str])`, `embed_query(&str)`) and `Reranker`
(`score(query, &[&str])`). `rueca-segment` stays free of model
dependencies: the embedder is injected into `rueca-index`'s vectors pass
as `OrgLinker` is into `update_org_with`. Features in `rueca-lang-data`'s
style: `embeddings` (default on; llama.cpp, Metal on Apple targets),
`fastembed` (fallback), `cuda` (service builds), re-exported by the CLI;
`--no-default-features` builds a lexical-only binary with no C++
toolchain.

### 4.2 Runtimes compared

| Runtime | License | State on 2026-11-16 | Fit | Verdict |
|---|---|---|---|---|
| llama.cpp via [`llama-cpp-2`](https://github.com/utilityai/llama-cpp-rs) | MIT OR Apache-2.0 | 0.1.161 (2026-11-03), a release every 1–3 weeks | GGUF with the tokenizer inside; embedding and reranker examples; Metal, CUDA, Vulkan; every GGUF quantization | **Primary** |
| [`fastembed`](https://github.com/Anush008/fastembed-rs) 7.7.1, feature `qwen3-text` | Apache-2.0 | 7.7.1 (2026-11-16); Qwen3 code since 2026-10-23 | candle, pure Rust; GGUF kept quantized; Metal kernels; Qwen3-Reranker with a shared prompt prefix; ONNX Runtime optional | **Fallback**, same traits; CPU speed unmeasured |
| `ort` (ONNX Runtime) | MIT OR Apache-2.0 | 2.0.0-rc.15, no stable 2.0 | CoreML rather than Metal; [dynamic or prebuilt library](https://raw.githubusercontent.com/pykeio/ort/main/docs/content/setup/linking.mdx); the 0.6B q4 ONNX file is larger than int8 (914 vs 614 MB) | Rejected |
| [mistral.rs](https://github.com/EricLBuehler/mistral.rs) | MIT | crates.io 0.8.1 lags GitHub 0.9.5 | an LLM serving engine | Rejected: heavy |
| burn / [`embed_anything`](https://github.com/StarlightSearch/EmbedAnything) | MIT/Apache, Apache-2.0 | 0.23 pre-release / 0.7.1 | no Qwen3 embedder *(unverified)* / Python-first | Rejected |
| [TEI](https://github.com/huggingface/text-embeddings-inference), separate process | Apache-2.0 | v1.9.4 | Qwen3, ModernBERT, CUDA images | Service-mode embedder via `remote:openai-compatible` |

Phase 0 opens with a bake-off of the first two on the same GGUF files
(tokens/s at batch 32 and 128 with ~200-token inputs, binary size, cosine
to the reference model); `fastembed` replaces llama.cpp if the C++ build
breaks cargo-dist or the size budget.

### 4.3 Build and distribution costs

- **Toolchain.** `llama-cpp-sys-2` vendors llama.cpp and needs CMake and a
  C/C++ compiler, on CI and for `cargo install` with default features.
- **x86 CPU features are fixed at compile time** (`GGML_NATIVE` is off
  unless `target-cpu=native`; `build.rs` maps rustc target features to
  `GGML_AVX2` and friends).

  **Recorded (provisional, 2026-11-17):** the
  cargo-dist x86_64 builds with the `embeddings` feature target
  `x86-64-v3` (AVX2, FMA, F16C, BMI2,
  [x86-64 psABI](https://gitlab.com/x86-psABIs/x86-64-ABI)); on an older
  CPU the vectors pass and dense queries fail with a clear error naming
  the missing level, and the documented fallback is
  `cargo install --no-default-features` (lexical only).

  Rationale: one
  static binary per target, as the project promises, with no extra
  shared libraries; AVX2 has shipped since Intel Haswell (2013) and AMD
  Excavator (2015), though Celeron and Pentium models gained it only in
  2020 ([AVX2](https://en.wikipedia.org/wiki/Advanced_Vector_Extensions)).
  The alternative, llama.cpp's dynamic CPU backends, is a shared library
  per CPU level.

  *Revisit if:* users report pre-AVX2 machines, or the
  dynamic backends become loadable from inside one static binary.
- **API churn.** The crate "does not follow semver meaningfully": pinned
  exactly, upgraded in its own pull request with the tests of §9 and the
  CoIR sanity row.
- **Size.** Today's release binary is 28 MB; what the runtime adds is
  *(unverified)*; phase 0 measures it against a proposed budget of 2×.
- **Targets.** cargo-dist's four (macOS and Linux gnu, `aarch64` and
  `x86_64`): Metal on Apple silicon (on `x86_64-apple-darwin`
  *(unverified)*), Linux CPU-only, CUDA through the `cuda` feature.
- **Licenses** are within `deny.toml`; CI adds an `--all-features`
  clippy and cargo-deny job, since `[graph] all-features = false` skips
  optional dependencies today.

### 4.4 Weights

- **Never in the binary.** Downloaded on first use into
  `<store>/models/<preset>/`, or `$RUECA_MODEL_DIR` (shared by several
  stores and by the bench, whose per-instance stores are wiped).
- **Manifest compiled in:** preset → files (URL, sha256, bytes), upstream
  revision, license, pooling, prefixes, dimension range. The default's
  file is `Qwen3-Embedding-0.6B-Q8_0.gguf`, 639,150,592 bytes, sha256
  `06507c7b42688469c4e7298b0a1e16deff06caf291cf0a5b278c308249c3e439`,
  hashed once after download.
- **The hash pins metadata.** The official file declares last-token
  pooling and an appended end-of-text token; earlier uploads lacked the
  pooling key ([discussion](https://huggingface.co/Qwen/Qwen3-Embedding-0.6B-GGUF/discussions/5)),
  Qwen warned that omitting the token "can significantly hurt model
  accuracy" ([Qwen](https://x.com/Alibaba_Qwen/status/1944425668235977146)),
  and a runtime without it measured a mean cosine of 0.804 to the
  reference ([dotLLM #1398](https://github.com/jamesburton/dotLLM/issues/1398)).
  Rueca's conversions must keep these keys.
- **Commands:** `rueca models pull [PRESET] | list | rm PRESET`.
  **Offline:** an explicit request (`--embed`, `RUECA_EMBED`, `mode:
  "semantic"`) whose weights are absent is an error naming the preset, the
  URL, `rueca models pull` and `RUECA_MODEL_DIR`; a query without `mode`
  falls back to lexical search with a warning in its envelope. **As
  implemented** (§0): only `rueca models pull` downloads; `rueca index` and
  queries never do.
- **Rueca-hosted conversions.** Qwen ships no 0.6B int4 file and no
  Qwen3-Reranker GGUF, and most community reranker files return scores
  near 4.5e-23 (llama.cpp issue #16407,
  [summary](https://gist.github.com/VooDisss/42bce4eb5c76d3c325633886c5e348ee)).
  Rueca converts them with a recorded llama.cpp commit and script, pins
  their sha256 and publishes them as release assets, each with the
  upstream license file; `NOTICE` lists the license of every model Rueca
  redistributes (Apache-2.0 Qwen, MIT CodeRankEmbed).
- **Hosting. Recorded (provisional, 2026-11-17):** GitHub Releases of a
  `DevArKaDiA/rueca-models` repository: immutable tagged assets, their
  sha256 in the manifest, no Hugging Face account or token at download
  time, the same CDN as the binaries; mirrors to Hugging Face may follow.

  Each release file must be under 2 GiB, with no limit on a release's
  total size or bandwidth
  ([GitHub](https://docs.github.com/en/repositories/releasing-projects-on-github/about-releases)):
  the 0.6B int4 embedder and the 0.6B reranker fit, but a 4B reranker at
  `Q4_K_M` would be about 2.5 GB *(estimate: the official 4B embedder
  file at that quantization is 2,497 MB)*, above the limit, and ships
  split with llama.cpp's `gguf-split` (loading split files through
  `llama-cpp-2` *(unverified)*).

  Upstream official files stay on
  Hugging Face. *Revisit if:* split files prove fragile, or bandwidth
  becomes a problem.

## 5. What is embedded

### 5.1 Units

**One vector per symbol** in v1, for the labels localization asks for:
`Function`, `Method`, `Test`, `Class`, `Struct`, `Interface`, `Trait`,
`Enum` and Markdown `Section`. Not in v1: `Field`, `Variable`, `Constant`,
`TypeAlias`, configuration (`Key`, `Document`, `EnvVar`, `Resource`,
`Image`), contracts (`Route`, `RpcMethod`, `Topic`) and containers. No
file vectors: a file's dense score is its best symbol's, as SweRank and
Agent Retrieval Bench do and as S1 ranks files today.

**Recorded (provisional, 2026-11-17):** configuration keys, fields and
variables are not embedded in v1. They are reached through `search_code`
and the code ↔ config bridge today; their canonical text is short and
dominated by the name, which BM25 already matches; the data prior ranks
them low; and leaving them out cuts the vector count substantially.
Adding them later embeds only them. *Revisit if:* the S1 `raw` misses on
the calibration split show configuration files among the gold files, or
when the bridge gets its own localization gold.

### 5.2 Canonical text

```text
<language> <label> <qualified name>
path: <repository-relative path>
<signature>
<doc comment>
<body head>
```

- **Callables:** the body after signature and doc, cut to the budget.
  **Types:** their members' signatures in source order instead of the raw
  body (as [CoRet](https://arxiv.org/html/2505.24715) embeds a class), so
  a class does not re-embed when a method body changes. **Sections:**
  heading path, then the section text.
- **Path and qualified name are in the text:** a path prefix lifted
  CoRet's recall@5 from 0.42 to 0.53. Cost: a moved file re-embeds.
- **Token budget 512** (about 2,000 non-whitespace characters, inside
  [cAST](https://arxiv.org/html/2506.15655)'s best chunk size of
  2,000–2,500); 256 / 512 / 1024 swept in phase 0; cost is linear in
  tokens. Long bodies: head only; several vectors per symbol is a
  follow-up (cAST: splitting without merging loses 13–19 nDCG points).
- **No LLM-written descriptions.** An English description is closer to
  queries than raw code (cosine 0.8152 vs 0.7280,
  [Greptile](https://www.greptile.com/blog/semantic-codebase-search)) but
  needs a language model at index time: a service-mode follow-up.

### 5.3 Query side

Qwen3 queries carry an instruction, documents none:
`Instruct: {task}\nQuery:{query}`; omitting it costs "1% to 5%", and
instructions should be English ([model card](https://huggingface.co/Qwen/Qwen3-Embedding-0.6B)).

Phase 0 chooses between Qwen's code instruction ("Given a question about
coding, retrieval code or passage that can solve user's question",
[task prompts](https://media.githubusercontent.com/media/QwenLM/Qwen3-Embedding/main/evaluation/task_prompts.json))
and an issue instruction ("Given a GitHub issue or a question about a code
base, retrieve the functions, classes or sections that answer it or must
change").

The dense arm cuts the query to its own token budget (512 capped by
the model's context, whatever the documents' budget); BM25 keeps it
whole.

### 5.4 Identity: content hash

Today a node id changes with every snapshot, a symbol id pins the commit,
and there is no per-symbol content hash. Vectors get their own identity:

- **embed key** = BLAKE3(model file sha256 — or provider, model, version
  and dimensions for `remote:` ‖ pooling ‖ document prefix ‖ text builder
  version ‖ token budget); the weights' quantization is inside the hash;
- **vector key** = BLAKE3(embed key ‖ canonical text after truncation);
- **model key** = BLAKE3(embed key ‖ dimensions ‖ vector encoding): what a
  segment's vectors and a query vector must share to be compared.

The query instruction is in neither key: documents carry none, so changing
it invalidates no stored vector. Equal text has an equal key on every
branch, ref, worktree and repository; the segment maps node → row → key.

### 5.5 The store vector cache

`<store>/vectors/<embed key>/`, content-addressed like `facts/` and
`packs/`: append-only pack files and an index (key → file, offset) as in
`packs/*.rcpack` + `packs/index.rcpi`; one writer per store, temporary
file, fsync and rename. Entries are **f16 at the model's full width**, so
any Matryoshka truncation and the rescoring of §6.2 read the same entry
(100k entries at 1024 dimensions: 204.8 MB); they live while a registered
segment lists their key and are collected like pack blobs (RFC 0001 §8.4).

The cache is what makes vectors affordable: only extraction is incremental
today, and resolution, tantivy and the segment are rebuilt by every
`rueca index`, so vectors kept only in segments would be re-embedded every
time. Three checkouts of this repository hold three 14 MB segments and
share packs and facts; vectors join what is shared. `RUECA_VECTOR_DIR`
moves the cache (the bench shares it across its per-instance stores).

### 5.6 Indexing flow and cost

1. `rueca index` publishes the lexical segment as today; queries are
   lexical meanwhile.
2. The vectors pass builds canonical texts (bodies from the content packs)
   and keys, looks them up in the cache, embeds the misses in fixed-size
   batches ordered by key (§9) and appends each finished batch to the
   cache, so an interrupted run resumes.
3. The snapshot is published again with the `vectors.*` components (the
   bundle copied plus the new components, a new segment id, today's
   catalog swap); organizations holding the repository update as for any
   re-index (RFC 0006 §9.1).

When every key hits the cache (a re-index after a small change), the three
steps are one publication. `IndexReport` gains `vectors_cached`,
`vectors_embedded`, `embed_ms`; `rueca status` a `VECTORS` column.

Cold cost of 100k symbols × 200 tokens = 20M tokens *(estimate: published
7B–8B prefill rates, [Apple](https://github.com/ggml-org/llama.cpp/discussions/4167),
[CPU](https://github.com/ikawrakow/ik_llama.cpp/wiki/Jan-2025:-prompt-processing-performance-comparison),
[CUDA](https://github.com/ggml-org/llama.cpp/discussions/15013), scaled by
non-embedding parameters at 100% to 50% utilisation; no tokens/s is
published for Qwen3-Embedding-0.6B itself)*:

| Hardware | 0.6B | 4B | 8B |
|---|---:|---:|---:|
| M1 Pro GPU (Metal, F16 rate) | 77–154 min | 10.6–21 h | 20–41 h |
| M4 Max GPU (Metal, F16 rate) | 25–51 min | 3.5–7 h | 6.7–13.3 h |
| Ryzen 7950X CPU (Q4_0 rate) | 2.5–4.9 h | 20–41 h | 39–78 h |
| RTX 4090 (CUDA, Q4_0 rate) | 2–3 min | 13–26 min | 25–50 min |

A cold embed of a large repository on a laptop is a background job; the
cache, the lexical-first publication and the token budget matter more
than the quantization, and the 4B and 8B need a GPU.

## 6. Storage in the segment

### 6.1 Components

Segment format 2 allows optional components "without a version bump"
(`rueca-segment` crate docs; precedents `files.modules`,
`symbols.bodiless`). Written after `symbols.bodiless`, read like it,
validated against the node count; `SegmentMeta` unchanged.

| Component | Content |
|---|---|
| `vectors.meta` | JSON: preset, embed key, model key, model sha256, weights quantization, dimensions, encoding, pooling, text builder version, token budget, labels embedded, runtime and version, backend (metal / cpu / cuda), CPU feature level, batch and micro-batch sizes, rows |
| `vectors.nodes` | `[u32; V]` node ids with a vector, sorted |
| `vectors.keys` | `[[u8; 32]; V]` vector keys |
| `vectors.scale` | `[f32; V]` per-row int8 scale |
| `vectors.data` | `[i8; V × d]`, 8-byte aligned (`add_pod`) |
| `vectors.hnsw` | optional `usearch` index (§6.4) |

### 6.2 Encoding: int8, rescored from f16

Vectors are L2-normalized at full width, truncated to `d` (Matryoshka),
renormalized and quantized to int8 with one scale per row.

- **Not binary.** The Qwen3-VL-Embedding report finds int8 near-lossless
  and binary quantization "significantly impairs retrieval effectiveness"
  ([arXiv 2601.04720](https://arxiv.org/pdf/2601.04720), via a search
  summary). Binary suits quantization-aware models (voyage-code-3 keeps
  91.59 of 92.12 at 2048 dimensions, [Voyage](https://blog.voyageai.com/2024/12/04/voyage-code-3/));
  Qwen3 is not one.
- **Rescoring.** int8 keeps about 97% of quality alone and about 99% when
  4–5× the final candidates are rescored at full precision
  ([embedding quantization](https://huggingface.co/blog/embedding-quantization)).
  **Recorded:** int8 in the segment, f16 in the store cache; the top 200
  dense candidates (4× the reranked 50) are rescored from f16 when the
  cache has them, int8 scores stand otherwise (a service without it).
- **Size** per 100k vectors: 102.4 MB at 1024 dimensions, 51.2 MB at 512.

### 6.3 Matryoshka dimensions

Qwen3-Embedding-0.6B accepts 32 to 1024 output dimensions; Qwen publishes
no truncation table for its text models. Third-party: 1024 → 512 costs
1.4% MRR@10 on Qwen3-VL-Embedding-2B (arXiv 2601.04720, search summary);
Qwen3-Embedding-8B keeps 99% at 512 and 96% at 256
([Matryoshka vs PCA](https://dylancastillo.co/posts/matryoshka-vs-pca));
narrower native widths likely degrade faster *(inference)*. **Rule:** the
default `d` is the smallest of 1024 / 512 / 256 within 1.0 point of 1024
on the CoIR cheap-profile mean and within one calibration instance on S1.
Changing `d` re-quantizes from the cache; nothing is re-embedded.

### 6.4 Search: exhaustive scan; HNSW deferred (decided 2026-11-24)

**Owner's decision:** an exhaustive int8 scan of every selected segment
(an f32 query against int8 rows with a per-row scale), parallel across
segments; per-segment top-N lists merge by (score, segment, node), a total
order, so paging stays exact as in `top_hits`. No ANN index in phases 1–2.

**Evidence** (a benchmark outside the repository: synthetic 1,024-dimension
clustered vectors, 200 queries, top 10, M1 Pro, recall against exact f32):

| Vectors | int8 scan | `usearch` 2.26 HNSW, f16 | `hnsw_rs` 0.3 |
|---|---|---|---|
| 10k | 0.26 ms, recall 0.99 | 0.15 ms | — |
| 100k | 1.3 ms, recall 0.99 | 0.36 ms | built in 137 s |
| 1M | 8.3 ms, recall 0.99 | 0.6–0.8 ms, recall 0.61–0.81, built in 16 min | — |

The scan is exact, has no build step and suits immutable segments, and
embedding the query (about 40 ms) dominates the latency anyway. HNSW
(`usearch` f16 with exact rescoring, an optional per-segment
`vectors.hnsw`) is reserved for organizations of several million vectors
in service mode (phase 3); `usearch` i8 loses too much recall and
`hnsw_rs` builds too slowly. The data are synthetic with tight clusters, a
hard case for HNSW; real embeddings may recall better at 1M.

### 6.5 Open cost

Every component is BLAKE3-checked when a segment opens; each `rueca query`
process opens it anew, and this repository's 14 MB segment takes about
18 ms. A 100k-vector int8 component is about 7× that size. **Recorded:**
`vectors.meta` and `vectors.nodes` are verified at open, the other
`vectors.*` components lazily on the first dense use in a process
(multi-threaded BLAKE3) and once per opened segment in the MCP server.
Lexical queries never pay for vectors; a full verification command checks
every component.

## 7. Retrieval pipeline

### 7.1 Where it hooks

`Engine::search_symbols` (`crates/rueca-query/src/tools.rs`) has three
branches (organization, one view, several roots) that produce `ranked:
Vec<(view, NodeId, f64)>`; the score inside `symbols.rs::top_hits` is
bm25 × routing boost × prior (tantivy's `tweak_score`). Fusion and
reranking run after `ranked`, before truncation and
`compact::search_symbols`. tantivy and its invariant (doc id = node id)
stay as they are; `Segment` gains `priors()` and `vectors()` accessors.

### 7.2 Stage 1: candidates and convex fusion

Candidates: **BM25 top-N ∪ dense top-N ∪ graph neighbours** (§7.3), N =
100. With `bm25` the raw tantivy score and `cos` the (rescored) cosine:

```text
b̂(c) = bm25(c) / max_C bm25          (0 outside the BM25 top N)
d̂(c) = (cos(c) + 1) / (max_C cos + 1)
s(c) = (α·b̂(c) + (1 − α)·d̂(c)) · prior(c) · route(repo(c))
```

- **Convex combination, not rank fusion.** On nine datasets a convex
  combination of normalized scores beats RRF in and out of domain (MS MARCO
  NDCG@1000 0.454 vs 0.425), RRF is sensitive to `k`, and α converges with
  "just a handful of labeled queries" ([Bruch et al.](https://arxiv.org/html/2210.11934v2)).
  With `k = 60` ([Cormack et al.](http://cormack.uwaterloo.ca/cormacksigir09-rrf.pdf))
  the first rank weighs 1/61 and the tenth 1/70, only 1.15× more: the flat
  head that cost RFC 0006 §2 its 9.5% against 61.9%. RRF (60) stays a
  reference row in the bench.
- **Normalization** by the theoretical minimum (BM25 0, cosine −1), the
  most stable variant in the same paper, which suggests α of 0.6–0.8 with
  it; per-query min-max is a sweep variant. **Decided (2026-11-24, §0):
  min-max** over each list's top N, **α = 0.5**, **N = 100**: cosines of
  unrelated code sit far above −1, so the theoretical form squeezes the
  dense signal into a narrow band near 1; it stays a variant
  (`normalization = "theoretical"`). With min-max, `b̂` and `d̂` are
  `(x − min) / (max − min)` over each list's top N (0 outside it).
- Every candidate's cosine is exact (all vectors are at hand); BM25 is 0
  outside its top N. Priors and the routing prior multiply as today; ties
  break by (view, node). `semantic` mode is α = 0; `lexical` mode is
  today's path, untouched.

### 7.3 Graph expansion

Seeds are the top 5 of each list; their 1-hop neighbours over `CALLS`
(callers, callees) and `CONTAINS` (parent, children) join with s(n) =
max(s(n), 0.5 · s(seed)), at most 50 per query, their own scores computed
as for any candidate.

Evidence:

- expansion over contains edges lifts function Recall@20 by 0.05–0.12
  while import and inheritance edges add nothing, and helps only if
  admitted neighbours are more precise than what they displace
  ([SpIDER](https://arxiv.org/html/2512.16956));
- a 1-hop ego graph lifts Agentless function localization from 51.0 to
  54.0 while 2 hops fall below the baseline (26.00 vs 27.33 resolved,
  [RepoGraph](https://arxiv.org/html/2410.14684v2));
- without expansion MuLocBench file Acc@5 drops from 55.7 to 48.2
  ([LARGER](https://arxiv.org/html/2605.16352)).

Swept: decay {0, 0.25, 0.5},
edges {none, contains, contains + calls}, and SpIDER's pool filter (a
neighbour enters only if it is in the dense top 500).

The organization
suite's impact questions (§2) are the direct test of expansion: a changed
symbol's consumers are its 1-hop `CALLS` / `IMPORTS` neighbours, across
repositories through the link layer (RFC 0006 §3.3), so the edge sweep
gains a `contains + calls + imports` setting, read on §11.2's impact row.

### 7.4 Priors and centrality

Today's priors (×0.5 for data definitions, tests, bodiless declarations,
copies, vendored code) and the routing prior are unchanged. A centrality
prior — the percentile `p` of distinct in-degree over `CALLS` and
`IMPORTS` within the repository, computed at open like the other priors
(`org/route.rs::distinct_referrers` is the building block) — enters as
`((0.25 + p) / 1.25)^β`, RFC 0006's routing-prior family, with **β = 0
(off) until the calibration split shows a gain**: no paper isolates a
query-independent centrality prior as a localization gain; the evidence of
§7.3 is query-dependent, as is Aider's personalized PageRank
([repo map](https://aider.chat/docs/repomap.html)).

### 7.5 Stage 2: reranker (decided)

**Owner's decision** (2026-11-17): a cross-encoder over the top ~50, with
the embedder's preset logic, **off by default in local mode**; phase 2
measures Qwen3-Reranker-4B and mxbai-rerank-large-v2 first, then the
0.6B; on by default in service mode with a GPU only if the gate below
holds. The owner first chose Qwen3-Reranker-0.6B; the evidence below
moved the order. **Evidence:**

| Reranker | Params | License | MTEB-Code, reranking Qwen3-Emb-0.6B's top 100 | CoIR, reranking jina-v3's top 100 |
|---|---:|---|---:|---:|
| none (Qwen3-Embedding-0.6B alone) | — | — | 75.41 | — |
| Qwen3-Reranker-0.6B | 0.6B | Apache-2.0 | **73.42** | **65.18** |
| Qwen3-Reranker-4B / 8B | 4B / 8B | Apache-2.0 | 81.20 / 81.22 | 73.91 / — |
| mxbai-rerank-large-v2 / base-v2 | 1.5B / 0.5B | Apache-2.0 | — | 70.87 / 65.71 |

Sources: [Qwen3-Reranker card](https://huggingface.co/Qwen/Qwen3-Reranker-0.6B),
[jina-reranker-v3](https://arxiv.org/html/2509.25085), Table 2.

- The 0.6B scores **below** the embedder alone on code in Qwen's own
  table; the 4B and 8B gain about 5.8 points.
- Every published localization gain from reranking comes from 7B-or-larger
  **listwise** LLM rerankers: CodeRankLLM (7B, MIT) lifts Lite function
  top-10 from 59.1 to 73.7 ([CoRNStack](https://arxiv.org/html/2412.01007)),
  SweRankLLM adds 4–14 points of Function Acc@10 ([SweRank](https://arxiv.org/html/2505.07849)),
  a zero-shot 8B listwise model lifts SWE-bench Multilingual from 52.56 to
  59.83 (SweRank+). No Qwen3-Reranker result on issue localization was
  found.
- **Latency**, 50 candidates × ~500 tokens: 7–14 s per query on an M1 Pro
  GPU (8–12 s in a second estimate), 14–29 s on a Ryzen 7950X, 0.1–0.3 s
  on an RTX 4090, the 4B about 8× slower *(estimates)*; a community run
  measured 13.8 s for 50 documents on a CPU
  ([write-up](https://dev.to/m_smith_2f854964fdd6/building-a-production-rag-system-qwen3-embeddings-reranking-and-vector-database-insights-4jh3)).
  20 candidates × 200 tokens is about 1 s on an M1 Pro *(estimate)*.

**Recorded:**

- The stage ships behind `rerank: true` and the `[retrieval]`
  configuration, **off by default in local mode**, on by default in
  service mode with a GPU **only if** the gate holds.
- **Gate:** on the calibration split, reranking raises pooled Function
  Acc@10 by ≥ 3 points over the same pipeline without it and lowers pooled
  File Acc@5 by no more than one instance, read on the grown split of
  §7.6 (at 40 instances one instance is 2.5 points).
- Candidate text: the canonical text cut to 256 tokens; K = 50, swept over
  {20, 50}; the shared prompt prefix computed once per call. Final order:
  reranker order for the top K, stage-1 order below (a λ-blend is a sweep
  variant). The reranker also admits or rejects expanded neighbours
  (SpIDER used an LLM filter).
- Presets measured, in order: `qwen3-reranker-4b-q4` (a Rueca
  conversion) and `mxbai-rerank-large-v2` (runtime path *(unverified)*)
  first, then `qwen3-reranker-0.6b-q8` (a Rueca conversion); the remote
  `remote:voyage/rerank-3` and `remote:cohere/rerank-3.5` as reference
  rows.

### 7.6 Tuning protocol

Calibration split only (RFC 0004 §7.7):

1. Cache each instance's candidates with every raw score, so a sweep
   re-scores in seconds as the routing calibration does. Grid: α ∈ {0,
   0.1, …, 1}; normalization; N ∈ {50, 100, 200}; §7.3's expansion
   settings; β ∈ {0, 0.5, 1}; K ∈ {0, 20, 50}; `kw` / `raw`.
2. Objective: macro over languages of ½ (File Acc@5 + Function Acc@10),
   MRR as tie-breaker.
3. Take the centre of the plateau within one instance of the best, not the
   arg-max; leaving one language out must keep it within one instance on
   that language.
4. Freeze; evaluate the gated set once; publish every arm (§11.2).

**Recorded (provisional, 2026-11-17): the split grows first.** Phase 0
grows the calibration split from 5 to 10–15 instances per language, drawn
from SWE-rebench V2, Multi-SWE-bench and spare SWE-PolyBench repositories,
disjoint from the gated repositories (RFC 0004 §7.7), before any α or
reranker gate is read. At 40 instances one instance is 2.5 points; the +3
reranker gate (§7.5) and the α plateau need 80–120 pooled. *Revisit if:*
a language has no spare instances; that language keeps 5 and the pooled
gate carries it.

CoIR is not used for fusion (no BM25, graph or issue queries); it chooses
model, quantization, dimensions and instruction only.

### 7.7 Degradation

A snapshot without vectors for the active model key is searched lexically;
a model that fails to load makes the query lexical, with the reason in the
envelope; an explicit `mode: "semantic"` that cannot be served is an
error.

## 8. API

### 8.1 `search_symbols`

| Arg | Type | Default |
|---|---|---|
| `mode` | `"hybrid"` \| `"lexical"` \| `"semantic"` | `hybrid` when the selected snapshots have vectors for the active model key and the model loads; else `lexical` |
| `rerank` | bool | from configuration: `false` locally; service mode per §7.5 |

The description (`DESC_SEARCH_SYMBOLS`, `docs/tools.md`) says questions
and issue text are welcome. Arguments are validated strictly, so older
binaries reject `mode`; the bench drops it for them.

### 8.2 Envelope

`search_symbols` responses gain one field (RFC 0005 §4):

```json
"retrieval": {"mode": "hybrid", "model": "qwen3-0.6b-q8:3f9a1c2b",
              "rerank": false, "lexical": [1]}
```

`model` is the preset and a model-key prefix; `lexical` lists the
snapshots (`s`) served without vectors. Agents learn what ranking they
got; the bench labels its runs with it.

**As implemented (phase 1):** `retrieval` is `{"mode", "model", "alpha",
"lexical", "warnings"}`, empty fields omitted: `lexical` lists repository
ids (not snapshot indexes), `warnings` why a call fell back to lexical,
`rerank` waits for phase 2. A lexical answer carries `{"mode":
"lexical"}` and is otherwise unchanged.

### 8.3 Commands and configuration

- `rueca index [--embed[=PRESET] | --no-embed]`; `rueca index --org`
  embeds every member with the organization's preset.
- `rueca models pull | list | rm` (§4.4); `rueca embed`, a batch entry
  point (JSON lines in, vectors out) for the bench's vector arm and CoIR
  runner.
- `[retrieval]` in `rueca.toml` (new, in the store) and `rueca-org.toml`
  (RFC 0006 §7): `preset`, `dims`, `rerank`, `rerank_preset`,
  `query_instruction`, and in `rueca-org.toml` only `allow_remote`
  (§3.5). Environment: `RUECA_EMBED_PRESET`,
  `RUECA_MODEL_DIR`, `RUECA_VECTOR_DIR`, and `RUECA_SEARCH` for experiment
  knobs (α, N, K, expansion, β) as `RUECA_ORG_ROUTE` is for routing.
  Precedence: argument, environment, organization file, store file,
  built-in default.
- ~~**Embedding by default** once phase 1's acceptance holds~~ —
  **superseded by the owner's decision (2026-11-24, §0): opt-in locally.**

  `rueca index` embeds only with `--embed[=PRESET]` or `RUECA_EMBED=1` /
  `RUECA_EMBED=<preset>` (`0`, empty, `false`, `off`, `no`: disabled);
  `--no-embed` overrides the environment. Precedence: argument >
  environment > configuration; `[retrieval]` chooses `preset`, `dims`,
  `alpha`, `fusion_n` and `normalization` but never enables embedding by
  itself.

  Missing weights are an error naming `rueca models pull`; nothing
  downloads on first use. `RUECA_FUSION_ALPHA` / `_N` / `_NORM` are the
  experiment knobs (instead of one `RUECA_SEARCH` string).

  As implemented,
  `rueca index --ref` embeds too; `rueca index --org` and the MCP server's
  auto-index stay lexical until phase 3.
- **Service mode** embeds on a GPU through the CUDA build or an external
  TEI / llama-server (`remote:openai-compatible`). **Recorded
  (provisional, 2026-11-17):** its GPU defaults are the `qwen3-4b-q4`
  embedder, or whichever candidate phase 0 picks above the local budget
  (§3.4), and the reranker on only if its gate held (§7.5); `remote:`
  presets stay opt-in under §3.5's `allow_remote`. Rationale: a GPU takes
  the 4B's cold embed of 100k symbols from hours to 13–26 min (§5.6),
  and the 4B leads the 0.6B on MTEB-Code (80.06 vs 75.41, §3.1) and
  CodeEditSearch (76.49 vs 64.42, §3.4).

### 8.4 Process model and latency

The MCP server loads the model on first use and keeps it; `rueca query`
loads it per process, a cost not yet measured *(unverified)* that phase 1
reports. RFC 0004's health latency floor stays a lexical row (the bench
pins `mode: "lexical"` for it); hybrid latency is its own row on a warm
server. A keyword query is one short forward pass (30.1 / 43.0 ms per
request on an M1 Ultra, [regolo](https://regolo.ai/embeddinggemma-2-vs-qwen3-embedding-benchmarks/));
a raw issue of 4,000 characters is about 1,100 tokens, 0.25–0.5 s on an M1
Pro GPU *(estimate: 3.5 characters per token, §5.6 rates)*, reported
apart.

## 9. Determinism

Embeddings vary with batch composition (cosine 0.99985–0.99999 batched vs
alone, [llama.cpp #28656](https://github.com/ggml-org/llama.cpp/issues/28656)),
with an input split across micro-batches (silently wrong vectors,
[node-llama-cpp #674](https://github.com/withcatai/node-llama-cpp/issues/674)),
CPU feature dispatch, backend and runtime version; llama.cpp's embedding
path is not bitwise reproducible even within one build
([llama.cpp #28963](https://github.com/ggml-org/llama.cpp/issues/28963)).
**Recorded:**

- `vectors.meta` records model sha256, quantization, dimensions, runtime
  and version, backend, CPU feature level, batch and micro-batch sizes.
  Keys exclude backend and runtime (a vector embedded on Metal is reused
  on a CPU); the record is for audit.
- Every input fits one micro-batch (micro-batch ≥ budget + prefix); misses
  are embedded in fixed-size batches ordered by key. Tests: on one build,
  backend and batch order, rankings are equal where the runtime allows it
  (phase 0 measures run-to-run variance); across platforms and backends,
  top-10 overlap ≥ 0.9 on the calibration queries.
- The bench labels hybrid runs with the backend; a re-run on one machine
  must reproduce the scorecard within one instance. Quantization churn is
  reported as top-10 overlap with F16 (§3.3).

## 10. Organization scope

- **Vectors travel with the repository segment.** The organization index
  copies segments unchanged and `OrgIndex::search_with` already holds every
  selected `Segment`, so the dense scan reads each repository's own
  vectors; nothing is copied into the organization layer and the
  incremental-update floor (≤ 2×) is unaffected.
- **Comparable by construction.** BM25 needed organization-wide statistics
  (RFC 0006 §3.2); cosines from one model need nothing. An organization has
  **one model key** (`rueca-org.toml [retrieval]`). A member without
  vectors for it is scored lexically (s = b̂ · prior · route), listed in
  `retrieval.lexical` and shown with its model key by `list_repositories`
  (`OrgRepo` gains an optional `vectors` field).
- **Fusion** normalizes over the organization's candidate pool; α may be
  tuned apart on the organization calibration (§11.1).
- **Routing gains a third signal:** the cosine between the query vector and
  a per-repository **embedding centroid** (the mean of its normalized
  symbol vectors, in `profiles/<segment>.json`), min-max normalized and
  mixed with CORI and profile BM25, its weight swept through
  `RUECA_ORG_ROUTE`; several centroids per repository is a variant.
- **ANN** only above the scope threshold of §6.4.

## 11. Measuring it

### 11.1 Bench changes

1. **Arms.** A `vector` arm (`arms/vector.py`: the bench's own function
   extraction, as `grep`, embedded through `rueca embed` with the same
   preset; files ranked by their best function), independent of Rueca's
   text builder as `grep` is of its BM25. Rueca modes `lexical`, `hybrid`,
   `hybrid + rerank` (`semantic` as a diagnostic), labelled apart, on a
   warm `rueca mcp` for latency. RRF (60) of BM25 and dense as reference.
2. **Both query variants** for every arm: `kw` (the scorecard's) and `raw`
   (published dense results use the issue text; CoRNStack: "The GitHub
   issue serves as the text query").
3. **CoIR runner** (`--suite coir`) through `rueca embed`, nDCG@10,
   datasets pinned by revision and sha256 and fetched at run time (RFC
   0002 §8; Apps, CodeSearchNet, CosQA MIT "for non-commercial research
   purposes", StackOverflow QA CC-BY-SA 3.0, the rest Apache-2.0, per the
   [CoIR paper](https://arxiv.org/abs/2407.02883)). The **cheap profile**
   holds every NL → code subset and runs on a laptop in about 1.5 h
   *(estimate: 3,500 tokens/s, 3.5 characters per token)*:

   | Task | Direction | Docs | Queries | Hours, M1 Pro *(estimate)* | Published Qwen3-0.6B |
   |---|---|---:|---:|---:|---:|
   | CosQA | NL → code | 20,604 | 500 | 0.13 | 36.48 |
   | CodeSearchNetRetrieval | NL → code | 6,000 | 6,000 | 0.15 | 90.77 |
   | AppsRetrieval | NL → code | 8,765 | 3,765 | 0.26 | 75.34 |
   | SyntheticText2SQL | NL → SQL | 105,851 | 5,851 | 0.32 | 76.74 |
   | StackOverflowQA | question → answer | 19,931 | 1,994 | 0.6 | 89.99 |
   | CodeTransOceanContest | code → code | 1,008 | 221 | 0.04 | 86.05 |
   | CodeTransOceanDL | code → code | 816 | 180 | 0.03 | 31.36 |

   Sizes from [mteb](https://github.com/embeddings-benchmark/mteb)'s
   statistics; scores from C2LLM, Table 2, and (CodeSearchNetRetrieval)
   the jina paper. The large subsets cost 3.5–6.4 h each on an M1 Pro
   *(estimate)* and run on a GPU only. CoIR is a sanity check: classic
   code-search scores drop sharply on agentic and issue retrieval
   ([CORE-Bench](https://arxiv.org/abs/2606.11864)); S1 decides.
4. **External diagnostic, SWE-bench Lite (274 instances)**: `raw` query,
   LocAgent's function gold (`czlll/SWE-bench_Lite`, already in the
   bench), never tuned on, with **all-gold** and **any-gold** columns.
   LocAgent and SweRank count a hit only when *all* gold locations are in
   the top k; S1 counts *any*. Lite keeps single-file patches
   ([SWE-bench Lite](https://www.swebench.com/lite.html)), so file-level
   numbers coincide and read directly against §11.3; function-level
   all-gold is stricter. S1 gains an all-gold diagnostic column too.
5. **Tokens per correct answer** (RFC 0005 §8) for every mode;
   **throughput** (symbols/s per preset and backend, cold embed of the S1
   corpora); a **re-index test** after a one-file change; the **routing
   calibration** of RFC 0006 §9.2 gains the hybrid modes and the centroid.
6. **Organization suite**, both kinds of question, for every mode against
   BM25 and the vector arm: localization (File Acc@5, gated and the
   larger scenario of n = 120) and **impact** (hit@10: given a changed
   library symbol, its consumers across the organization), each with and
   without §7.3's expansion (bench organization suite,
   [PR #60](https://github.com/DevArKaDiA/rueca/pull/60)).

### 11.2 Acceptance gates

Paired bootstrap as RFC 0004 §6.1; pooled over the 313 gated S1 instances
unless noted.

| Metric | Today | Floor | Target |
|---|---|---|---|
| S1 File Acc@5, hybrid − BM25, `raw` | lexical Rueca − BM25, `kw`: 67.1 − 67.4 *(derived)* | whole 95% interval above 0 **and** point ≥ +5 | hybrid ≥ 80 (RFC 0004) |
| S1 File Acc@5, hybrid − vector arm, `raw` | — | interval not below 0 | interval above 0 |
| S1 Function Acc@10, hybrid − BM25, `raw` | — | point ≥ +3, interval not below 0 | hybrid ≥ 65 |
| S1 per language, both variants | met (⚠ for 7 of 8 languages on File Acc@5) | not significantly below the better of BM25 and the vector arm | — |
| S1 `kw` pooled, hybrid − BM25 | −0.3 *(derived)* | interval not below 0 | interval above 0 |
| Organization File Acc@5 over single repository, gated | 0.69× | ≥ 0.85× (RFC 0006) | ≥ 0.95× |
| Organization File Acc@5, larger scenario (PR #60, n = 120), hybrid − BM25 | lexical Rueca vs BM25: 56.7 vs 52.5 (+4.2, interval −4.2 … +12.5) | reported | — |
| Organization impact hit@10, hybrid + expansion − BM25 | 12.5 vs 37.5 (−25.0) | interval not below 0 | point ≥ +10 and interval above 0 |
| CoIR cheap profile, chosen preset: mean over tasks with a published number | — | within 1.0 point of the published mean | — |
| Hybrid latency p95, warm, keyword-length query, no reranker | — | ≤ 300 ms | ≤ 100 ms |
| Lexical latency p95 (RFC 0004 health) | met | ≤ 100 ms, unchanged | ≤ 20 ms |
| Reranker p95 (Metal, CPU, CUDA); raw-query hybrid p95; symbols/s per preset and backend; cold embed of the S1 corpora | — | reported | — |
| Re-index after a one-file change: vectors embedded | — | only that file's symbols, ≤ 1% of rows (test) | — |
| Tokens per correct answer, hybrid over lexical | — | reported | ≤ 1× |
| Top-10 overlap across backends, calibration queries | — | ≥ 0.9 | — |

`raw` gates the feature because it is the natural-language case semantic
search exists for; `kw` keeps RFC 0004's floor and must not regress. RFC
0004's localization scorecard gains hybrid and vector columns.

### 11.3 Calibrating RFC 0004's targets (its open question 1)

| System | Lite File Acc@5 / Fn Acc@10 | LocBench File Acc@5 / Fn Acc@10 | Source |
|---|---:|---:|---|
| BM25 | 61.7 / 36.9 | 49.3 / — | LocAgent; [LARGER](https://arxiv.org/html/2605.16352) (all-gold) |
| CodeRankEmbed (137M, MIT) | 84.7 / 58.8 | 74.3 / 43.4–47.9 | LocAgent; SweRank (the papers disagree) |
| Qwen3-Embedding-0.6B · 8B | — / 62.8 · — / 71.5 | — / 47.3 · — / 55.0 | SweRank+ |
| GTE-Qwen2-7B-Instruct | 89.8 / 70.4 | 75.5 / 51.8 | SweRank |
| SweRankEmbed-Small (137M, CC-BY-NC, issue-tuned) | 90.9 / 74.5 | 80.4 / 58.6 | SweRank |
| SweRankEmbed-Large + SweRankLLM-Large (7B + 32B) | 96.0 / 88.7 | 86.6 / 71.3 | SweRank |

Published numbers are all-gold and Python-only; on SWE-bench Multilingual
retrieval-only Function Acc@10 spans 35.0–52.6 and the best stack reaches
71.4 (SweRank+). **Answer:** keep File Acc@5 **80** (competitive
retrieval-only on Python, ambitious for C, C++, Go and Rust, where no
file-level published numbers exist) and Function Acc@10 **65**, labelled
"needs a reranker or an issue-tuned embedder": it is the state of the art
with a 7B listwise reranker, not a 0.6B stack. RFC 0004's open question 2
is answered by §11.2's per-language row: hybrid must not be significantly
below the better of BM25 and the vector arm.

## 12. Phasing

| Phase | Delivers | Acceptance | Status (2026-11-24) |
|---|---|---|---|
| 0. Measurement | Calibration split grown (§7.6); runtime bake-off (§4.2); minimal `rueca-embed` and `rueca embed`; `vector` arm; CoIR cheap runner; sweeps of model (§3.4), quantization (§3.3), dimensions (§6.3), token budget (§5.2) and instruction (§5.3) | Each choice made by its rule and recorded in `docs/decisions.md` with the sweep; vector-arm rows on S1 (`kw`, `raw`) and the Lite diagnostic; CoIR sanity row met; binary size within budget | **Implemented**: runtime, `rueca models`, `rueca embed`, vector arm, split of 118, `compare`; defaults decided by the owner (§0). The CoIR runner and the Lite diagnostic stay on `feat/semantic-search-next` |
| 1. Hybrid search | `rueca-embed`, weights and `rueca models`; store vector cache; `vectors.*` components; lexical-first publication; exhaustive scan, f16 rescoring; convex fusion in `search_symbols` tuned on calibration; `mode`, envelope, configuration | §11.2 File Acc@5 rows (`raw` gate, `kw` and per-language floors); latency rows; re-index test; backend overlap row; Function Acc@10 reported | **Implemented**, opt-in (§0, §8.3); the bench's `rueca-hybrid` arm, fusion sweep and §11.2 rows; calibration evidence in §0 |
| 2. Graph and reranker | Expansion and centrality prior, each kept only if the sweep keeps it; reranker stage and presets, default per §7.5; `raw` results published | §11.2 Function Acc@10 gate; reranker gate; organization impact hit@10 floor; reranker latency reported | Follow-up: graph expansion, centrality prior and reranker on `feat/semantic-search-next` |
| 3. Organization and service | One model key per organization; centroid routing; ANN threshold measured at organization scale; remote presets; service-mode GPU defaults | Organization File Acc@5 ≥ 0.85× (gated); organization impact hit@10 target; incremental-update floor unchanged; ANN recall against the exact scan reported | Follow-up: organization scope, centroid routing, remote presets and `rueca query --batch` on `feat/semantic-search-next`; small-encoder presets, text profiles and the units filter on `feat/semantic-static`; service mode turns embedding on (RFC 0008 phase 3); HNSW only past several million vectors (§6.4) |

Follow-ups, not phases:

- **An issue-tuned embedder.** Every strong localization number comes from
  issue-tuned retrievers: SweRankEmbed-Small beats Qwen3-Embedding-0.6B by
  11.7 points of Lite Function Acc@10 (74.5 vs 62.8), and a 0.6B tuned from
  Qwen3 reaches 52.6 on SWE-bench Multilingual against 38.0 (SweRank+).
  Those weights are CC-BY-NC or unreleased; the follow-up fine-tunes
  Qwen3-Embedding-0.6B on an Apache-compatible issue → function set mined
  as in RFC 0002 §8.
- Several vectors per long symbol (§5.2); CUDA release builds;
  LLM-written symbol descriptions in service mode.

## 13. Amendments

- **RFC 0001 §8.1 item 5** — vectors: int8 per-row-scaled components
  keyed by content hash, with f16 in a content-addressed store cache,
  instead of an f16 matrix only; `usearch` per segment above a scope
  threshold (§6).
- **RFC 0001 §11.3** — semantic search is a mode of symbol search fused by
  convex combination, not reciprocal rank fusion (§7.2).
- **RFC 0001 §19 open question 3** — answered: a local model by default,
  downloaded on first use; remote endpoints opt-in (§3–§4).
- **RFC 0002 §2** — the excluded list gains jina-embeddings-v5,
  Qodo-Embed-1, EmbeddingGemma, SweRankEmbed / SweRankLLM and the jina
  rerankers (§3.2).
- **RFC 0002 §6.2** — "Offline embeddings" (`model2vec-rs`): removed.
  "Quality embeddings" (`fastembed` + `ort`): `llama-cpp-2` primary,
  `fastembed` (candle) fallback, `ort` avoided (§4). "Vectors": kept, int8
  with rescoring (§6).
- **RFC 0004 §6** — targets stay 80 / 65; 65 is labelled "needs a reranker
  or an issue-tuned embedder" (§11.3). **§11** — the vector arm and hybrid
  retrieval are this RFC; static embeddings give way to the presets of
  §3. **§13** — open question 1 answered (§11.3); open question 2
  answered: hybrid's per-language floor is "not significantly below the
  better of BM25 and the vector arm".
- **RFC 0006 §6** — "rank fusion where scores come from different engines"
  becomes convex fusion of normalized scores (§7.2). **§9.2** — routing
  gains an embedding-centroid signal (§10).

## 14. Alternatives considered

- **Static embeddings** (`model2vec-rs`): the owner requires top-tier
  quality; the static model was chosen for being small and offline.
- **File-level vectors**: a function buried in a file dilutes its signal
  (cosine 0.768 alone vs 0.739 inside a file, Greptile); a file scores as
  its best symbol. **Fixed-window chunks**: not something the tools
  return, and syntax-blind splitting loses quality (cAST).
- **RRF** as the default fusion (§7.2): kept as a reference row. **ANN
  everywhere**: the exact scan is milliseconds at repository scale (§6.4).
  **ONNX Runtime**: a release candidate, CoreML instead of Metal, no size
  win at 4 bits (§4.2). **Binary vectors**: Qwen3 is not trained for them
  (§6.2); a measured binary pre-filter remains possible.
- **Weights in the binary**: the default file alone is 639 MB against a
  28 MB binary; presets change without a release.
- **Remote-only**: the default must work offline (RFC 0001 §19) and code
  must not leave the machine by default; Sourcegraph dropped its
  embeddings partly because code had to be sent to a provider
  ([Sourcegraph](https://sourcegraph.com/blog/how-cody-understands-your-codebase)).
- **A separate `semantic_search` tool**: agents would pick between two
  rankings and pay tokens for both; `mode` covers explicit use. **LLM
  query rewriting**: out of scope; the calling agent can rephrase.
- **Agentic grep only**: several agent CLIs index nothing. Claude Code's
  early versions used RAG with a local vector database, dropped because
  "agentic search generally works better" and avoids security, privacy
  and staleness issues; no benchmark was published
  ([Boris Cherny](https://x.com/bcherny/status/2017824286489383315)).

  Cline explains the same choice
  ([Cline](https://cline.bot/blog/why-cline-doesnt-index-your-codebase-and-why-thats-a-good-thing)),
  Codex CLI's instructions prefer `rg` and its semantic-indexing request
  is still open ([codex #5181](https://github.com/openai/codex/issues/5181)),
  and Sourcegraph replaced Cody's embeddings with its keyword search (same
  post).

  On the other side:

  - semantic search beat deep agentic search (a planner and a search
    sub-agent) on SWE-QA, 65.2% vs 46.2%, at less than half the cost per
    correct answer
    ([Deep Agentic Search](https://arxiv.org/abs/2608.01507));
  - Cursor reports 12.5% higher answer accuracy with a custom embedder
    next to grep ([Cursor](https://cursor.com/blog/semsearch));
  - GitHub reports 2% less time at unchanged quality
    ([GitHub](https://github.blog/changelog/2026-03-17-copilot-coding-agent-works-faster-with-semantic-code-search/)).

  Claude Context (Zilliz: AST chunks, hybrid BM25 + dense search) keeps
  F1 at 0.40 against grep alone with 39% fewer tokens on 30 SWE-bench
  Verified instances
  ([evaluation](https://github.com/zilliztech/claude-context/tree/master/evaluation)).

  Augment's Context Engine, served over MCP, reports 30–80% quality gains
  in a vendor-run benchmark of 300 Elasticsearch PRs without disclosing
  its method ([Augment](https://www.augmentcode.com/blog/context-engine-mcp-now-live)).

  Rueca keeps both — `search_code` for grep-shaped work, hybrid
  `search_symbols` for descriptions — and measures tokens per correct
  answer next to localization.

## 15. Open questions — all resolved provisionally on 2026-11-17

The phase 1 decisions of 2026-11-24 (fusion defaults, exhaustive scan,
opt-in and its precedence) are the owner's, in §0.

1. ~~**Default quantization:** int4, as the owner chose, or Q8, as the
   owner's rule yields on Metal and x86?~~ Decided (§3.3):
   `qwen3-0.6b-q8`, provisional; int4 wins on a backend in phase 0 only if
   ≥ 1.2× faster there without §3.3's loss.
2. ~~**Reranker default:** Qwen3-Reranker-0.6B, as the owner chose, or off
   locally and gated?~~ Decided (§7.5): off by default locally; the 4B
   and mxbai-rerank-large-v2 measured first, then the 0.6B; on in service
   mode with a GPU only past the gate.
3. ~~**Default embedder after phase 0:** 0.6B, 4B (GPU only) or
   CodeRankEmbed?~~ Decided by rule (§3.4): best calibration score within
   twice the 0.6B's indexing time; above it, a service-mode candidate.
4. ~~**Hosting conversions:** a `rueca-models` GitHub repository or a
   Hugging Face repository?~~ Decided (§4.4): GitHub Releases of
   `DevArKaDiA/rueca-models`, sha256 in the manifest; Hugging Face mirrors
   may follow.
5. ~~**Calibration split size:** grow from 5 to 10–15 instances per
   language?~~ Decided (§7.6): grown in phase 0, disjoint from the gated
   repositories, before any α or reranker gate is read.
6. ~~**Configuration keys, fields and variables** embedded in v1, or
   later?~~ Decided (§5.1): not in v1; revisited on calibration misses in
   configuration files.
7. ~~**x86 releases:** `x86-64-v3` or llama.cpp's dynamic CPU
   backends?~~ Decided (§4.3): `x86-64-v3` with a clear error on older
   CPUs and a documented lexical-only `cargo install`.
8. ~~**Service mode and privacy:** GPU defaults, and can an organization
   file forbid `remote:` presets?~~ Decided (§3.5, §8.3): the 4B (or phase
   0's above-budget pick) and the reranker only past its gate;
   `[retrieval] allow_remote = false` by default in `rueca-org.toml`.

## 16. Dogfood notes: where Rueca failed while writing this RFC

The integration points of this RFC were located with Rueca's own tools on
this repository (10–20 ms per query). Free-text search was the weakest
tool whenever the words were not the identifiers — the case semantic
search is for:

1. "bm25 ranking" (Rust) returned `RouteMode`, `RouteParams`, `OrgScope`
   and missed `scoring_query`, `build_query`, `BOOSTS` and `top_hits`;
2. "read symbol source text from content pack" missed `body_texts`,
   `Segment::content` and `Engine::get_snippet`;
3. "multiply score by prior inside top-k" ranked `top_hits` fifth and
   missed `priors::priors`;
4. "routing prior repository relevance boost" missed `OrgIndex::route` and
   `OrgIndex::search_with`.

Unrelated follow-ups:

5. `trace` misses `views[0].seg.search_symbols(…)`, whose receiver type
   comes from a struct field (`View.seg: Arc<Segment>`) declared in another
   file, and `Engine.search_symbols → OrgIndex.search_with` is only a
   heuristic 0.5 edge;
6. `get_symbol {"qualified_name": "search_symbols"}` picks a `pub(crate)`
   helper, and `pub(crate)` items are reported as exported;
7. `get_file_outline` of a large file is cut at the 2,000-token budget, the
   CLI has no `--budget-tokens` flag, and struct outlines cannot hide
   fields;
8. `list_repositories --root <worktree>` prints the main checkout's branch
   in the header;
9. `search_code` lists matches in path order and stops at the limit, with
   no relevance order or per-file cap;
10. `rueca-segment`'s documented store layout omits `facts/`, and RFC 0001
    §8.1 describes `SegmentReader` / `SegmentWriter` traits the code lacks.

## 17. Sources

All consulted on 2026-11-16, except the revision's, consulted on
2026-11-17.

- **Qwen3:**
  - https://huggingface.co/Qwen/Qwen3-Embedding-0.6B
  - https://huggingface.co/Qwen/Qwen3-Embedding-0.6B-GGUF (and the 4B / 8B GGUF repositories via the Hugging Face API)
  - https://huggingface.co/Qwen/Qwen3-Embedding-0.6B-GGUF/discussions/5
  - https://x.com/Alibaba_Qwen/status/1944425668235977146
  - https://arxiv.org/html/2506.05176v3
  - https://media.githubusercontent.com/media/QwenLM/Qwen3-Embedding/main/evaluation/task_prompts.json
  - https://huggingface.co/Qwen/Qwen3-Reranker-0.6B
  - https://github.com/QwenLM/Qwen3-Embedding
  - https://gist.github.com/VooDisss/42bce4eb5c76d3c325633886c5e348ee
  - https://github.com/jamesburton/dotLLM/issues/1398
- **Other models and licenses:**
  - https://arxiv.org/html/2512.21332
  - https://huggingface.co/codefuse-ai/C2LLM-0.5B
  - https://arxiv.org/html/2508.21290
  - https://huggingface.co/jinaai/jina-code-embeddings-1.5b
  - https://jina.ai/news/jina-embeddings-v5-text-distilling-4b-quality-into-sub-1b-multilingual-embeddings/
  - https://arxiv.org/html/2509.25085
  - https://huggingface.co/mixedbread-ai/mxbai-rerank-base-v2
  - https://huggingface.co/nomic-ai/CodeRankEmbed
  - https://huggingface.co/nomic-ai/nomic-embed-code
  - https://huggingface.co/Alibaba-NLP/gte-modernbert-base
  - https://arxiv.org/html/2508.21085
  - https://huggingface.co/voyageai/voyage-4-nano
  - https://www.qodo.ai/open-rail-m-license/
  - https://huggingface.co/PeterAM4/Qwen3-Embedding-0.6B-GGUF
- **Quantization and storage:**
  - https://arxiv.org/html/2609.24322v1
  - https://arxiv.org/html/2609.16391v1
  - https://huggingface.co/blog/embedding-quantization
  - https://blog.voyageai.com/2024/12/04/voyage-code-3/
  - https://arxiv.org/pdf/2601.04720 (via a search summary)
  - https://dylancastillo.co/posts/matryoshka-vs-pca
  - https://crates.io/crates/usearch
- **Runtimes and throughput:**
  - https://github.com/utilityai/llama-cpp-rs
  - https://github.com/Anush008/fastembed-rs
  - https://github.com/huggingface/candle
  - https://raw.githubusercontent.com/pykeio/ort/main/docs/content/setup/linking.mdx
  - https://github.com/EricLBuehler/mistral.rs
  - https://github.com/StarlightSearch/EmbedAnything
  - https://github.com/huggingface/text-embeddings-inference
  - crates.io API for `llama-cpp-2`, `fastembed`, `ort`, `candle-core`,
    `burn`, `mistralrs`, `embed_anything`
  - https://github.com/ggml-org/llama.cpp/discussions/4167
  - https://github.com/ggml-org/llama.cpp/discussions/15013
  - https://github.com/ikawrakow/ik_llama.cpp/wiki/Jan-2025:-prompt-processing-performance-comparison
  - https://github.com/ggml-org/llama.cpp/issues/28656
  - https://github.com/ggml-org/llama.cpp/issues/28963
  - https://github.com/withcatai/node-llama-cpp/issues/674
  - https://regolo.ai/embeddinggemma-2-vs-qwen3-embedding-benchmarks/
  - https://dev.to/m_smith_2f854964fdd6/building-a-production-rag-system-qwen3-embeddings-reranking-and-vector-database-insights-4jh3
- **Remote providers:**
  - https://blog.voyageai.com/2026/08/13/voyage-code-4/
  - https://docs.voyageai.com/docs/pricing
  - https://docs.voyageai.com/docs/embeddings
  - https://developers.openai.com/api/docs/pricing
  - https://developers.openai.com/api/reference/resources/embeddings/methods/create
  - https://aistudio.google.com/docs/pricing
  - https://aistudio.google.com/docs/embeddings
  - https://aws.amazon.com/bedrock/pricing/
  - https://aws.amazon.com/blogs/machine-learning/get-started-with-amazon-titan-text-embeddings-v2-a-new-state-of-the-art-embeddings-model-on-amazon-bedrock/
  - https://www.getmaxim.ai/bifrost/llm-cost-calculator/provider/bedrock/model/cohere.embed-v4-0
- **Localization, fusion, chunking:**
  - https://arxiv.org/html/2503.09089
  - https://arxiv.org/html/2505.07849
  - https://arxiv.org/html/2512.20482
  - https://arxiv.org/html/2412.01007
  - https://arxiv.org/html/2605.16352
  - https://www.swebench.com/lite.html
  - https://arxiv.org/html/2210.11934v2
  - http://cormack.uwaterloo.ca/cormacksigir09-rrf.pdf
  - https://arxiv.org/html/2512.16956
  - https://arxiv.org/html/2410.14684v2
  - https://aider.chat/docs/repomap.html
  - https://arxiv.org/html/2505.24715
  - https://arxiv.org/html/2506.15655
  - https://www.greptile.com/blog/semantic-codebase-search
  - https://arxiv.org/html/2607.24882
- **Benchmarks:**
  - https://arxiv.org/abs/2407.02883
  - https://github.com/CoIR-team/coir
  - https://github.com/embeddings-benchmark/mteb
  - https://arxiv.org/abs/2606.11864
- **Practice:**
  - https://cursor.com/blog/semsearch
  - https://github.blog/changelog/2026-03-17-copilot-coding-agent-works-faster-with-semantic-code-search/
  - https://sourcegraph.com/blog/how-cody-understands-your-codebase
  - https://cline.bot/blog/why-cline-doesnt-index-your-codebase-and-why-thats-a-good-thing
  - https://github.com/openai/codex/issues/5181
  - https://arxiv.org/abs/2608.01507
  - https://x.com/bcherny/status/2017824286489383315
  - https://github.com/zilliztech/claude-context (and its `evaluation/`)
  - https://www.augmentcode.com/blog/context-engine-mcp-now-live
- **Revision of 2026-11-17:**
  - https://github.com/DevArKaDiA/rueca/pull/60 (bench organization suite)
  - https://docs.github.com/en/repositories/releasing-projects-on-github/about-releases
  - https://gitlab.com/x86-psABIs/x86-64-ABI
  - https://en.wikipedia.org/wiki/Advanced_Vector_Extensions
