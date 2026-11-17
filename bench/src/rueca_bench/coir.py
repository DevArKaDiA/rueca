"""CoIR runner (RFC 0007 §11.1 item 3): nDCG@10 of a `rueca embed` preset on
code-retrieval tasks, through mteb (optional dependency group `coir`).

    uv run --project bench --group coir rueca-bench coir --profile cheap [--preset P]
        [--dims N] [--max-tokens N] [--instruction task|preset|TEXT] [--tasks A,B]
        [--limit-corpus N] [--batch N] [--cpu] [--threads N]

mteb fetches each task's dataset at its own pinned Hugging Face revision
(recorded per task in the results; nothing is vendored) into
`bench/.cache/hf` unless `HF_DATASETS_CACHE` / `HF_HUB_CACHE` say otherwise,
and scores with its own retrieval evaluator (pytrec_eval). The model is
`RuecaMtebModel`: queries are embedded with the query flag (the binary adds
the instruction), documents without; vectors go through the bench's vector
cache (`embed.CachedEmbedder`), so re-running with other dimensions is free
and an interrupted run resumes. Profiles:

- `cheap` — every NL → code subset (RFC 0007 §11.1 table): CosQA,
  CodeSearchNetRetrieval (MTEB's 6,000 / 6,000, not CoIR's 1M-document
  `COIRCodeSearchNetRetrieval`), AppsRetrieval, SyntheticText2SQL,
  StackOverflowQA, CodeTransOceanContest, CodeTransOceanDL; about 1.5–1.8 h
  on an M-series laptop *(RFC estimate)*.
- `full` — mteb's `CoIR` benchmark (10 tasks; the large ones cost 3.5–6.4 h
  each on a laptop: GPU only).

Instructions (`--instruction`): `task` (default) sends each task's query
instruction from Qwen3-Embedding's own evaluation prompts
(`QWEN3_TASK_PROMPTS`, the prompts behind the published numbers) for Qwen3
presets and the preset's default otherwise; `preset` keeps the binary's
default instruction; any other text is sent to every task. Results:
`bench/results/coir/<preset>-<dims|native>-<tokens>[-full][-tasks<hash>][-i<instruction>][-smoke<N>].json`,
with the published number per task for the preset's model when known
(`PUBLISHED`) and the delta; the RFC 0007 §11.2 sanity row compares the mean
over tasks that have a published number.
"""

from __future__ import annotations

import datetime as dt
import os
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Iterable

import numpy as np

from .common import CACHE, RESULTS_DIR, log, write_json
from .embed import DEFAULT_MAX_TOKENS, DEFAULT_PRESET, CachedEmbedder, Embedder, RuecaEmbedder, short_hash, truncate_normalize

# Datasets land in the bench cache, not the user's Hugging Face cache (set
# before mteb / datasets read their configuration at import).
os.environ.setdefault("HF_DATASETS_CACHE", str(CACHE / "hf" / "datasets"))
os.environ.setdefault("HF_HUB_CACHE", str(CACHE / "hf" / "hub"))

try:  # optional dependency group `coir`
    import mteb
    from mteb.models.abs_encoder import AbsEncoder
    from mteb.models.model_meta import ModelMeta
    from mteb.types import PromptType
except ImportError:  # pragma: no cover - exercised only without the group
    mteb = None
    AbsEncoder = object
    ModelMeta = None
    PromptType = None

PROFILES: dict[str, tuple[str, ...]] = {
    "cheap": (
        "CosQA",
        "CodeSearchNetRetrieval",
        "AppsRetrieval",
        "SyntheticText2SQL",
        "StackOverflowQA",
        "CodeTransOceanContest",
        "CodeTransOceanDL",
    ),
    "full": (
        "AppsRetrieval",
        "CodeFeedbackMT",
        "CodeFeedbackST",
        "CodeSearchNetCCRetrieval",
        "CodeTransOceanContest",
        "CodeTransOceanDL",
        "CosQA",
        "COIRCodeSearchNetRetrieval",
        "StackOverflowQA",
        "SyntheticText2SQL",
    ),
}

# Published nDCG@10 (percent) per model, from C2LLM, Table 2 (MTEB-Code
# leaderboard as of 2025-12-25), https://arxiv.org/html/2512.21332. RFC 0007
# §11.1 quotes CodeSearchNetRetrieval for Qwen3-0.6B from the jina paper
# (90.77, https://arxiv.org/html/2508.21290); the C2LLM table says 91.01 and
# is used here so every number comes from one table.
PUBLISHED_SOURCE = "C2LLM, Table 2 — https://arxiv.org/html/2512.21332"
PUBLISHED: dict[str, dict[str, float]] = {
    "qwen3-0.6b": {
        "AppsRetrieval": 75.34,
        "CodeSearchNetRetrieval": 91.01,
        "CodeSearchNetCCRetrieval": 91.72,
        "COIRCodeSearchNetRetrieval": 84.69,
        "CodeEditSearchRetrieval": 64.42,
        "CodeFeedbackMT": 90.82,
        "CodeFeedbackST": 86.39,
        "CodeTransOceanContest": 86.05,
        "CodeTransOceanDL": 31.36,
        "CosQA": 36.48,
        "StackOverflowQA": 89.99,
        "SyntheticText2SQL": 76.74,
    },
    "qwen3-4b": {
        "AppsRetrieval": 89.18,
        "CodeSearchNetRetrieval": 92.34,
        "CodeSearchNetCCRetrieval": 95.59,
        "COIRCodeSearchNetRetrieval": 87.93,
        "CodeEditSearchRetrieval": 76.49,
        "CodeFeedbackMT": 93.21,
        "CodeFeedbackST": 89.51,
        "CodeTransOceanContest": 90.99,
        "CodeTransOceanDL": 35.04,
        "CosQA": 37.98,
        "StackOverflowQA": 94.32,
        "SyntheticText2SQL": 78.21,
    },
    "qwen3-8b": {
        "AppsRetrieval": 91.07,
        "CodeSearchNetRetrieval": 92.66,
        "CodeSearchNetCCRetrieval": 96.35,
        "COIRCodeSearchNetRetrieval": 89.51,
        "CodeEditSearchRetrieval": 76.97,
        "CodeFeedbackMT": 93.70,
        "CodeFeedbackST": 89.93,
        "CodeTransOceanContest": 93.73,
        "CodeTransOceanDL": 32.81,
        "CosQA": 38.04,
        "StackOverflowQA": 94.75,
        "SyntheticText2SQL": 78.75,
    },
}

# Qwen3-Embedding's evaluation instructions for these tasks,
# https://media.githubusercontent.com/media/QwenLM/Qwen3-Embedding/main/evaluation/task_prompts.json
QWEN3_TASK_PROMPTS: dict[str, str] = {
    "CosQA": "Given a question about coding, retrieval code or passage that can solve user's question",
    "CodeSearchNetRetrieval": "Given a code snippet, retrieve the comment corresponding to that code.",
    "AppsRetrieval": "Given a question about code problem, retrieval code that can solve user's problem",
    "SyntheticText2SQL": "Given a user's question, retrieve SQL queries that are appropriate responses to the question",
    "StackOverflowQA": "Given a question about coding, retrieval code or passage that can solve user's question",
    "CodeTransOceanContest": "Given a piece for code, retrieval semantically similar code",
    "CodeTransOceanDL": "Given a piece for code, retrieval semantically similar code",
    "COIRCodeSearchNetRetrieval": "Given a code snippet, retrieve the comment corresponding to that code.",
    "CodeSearchNetCCRetrieval": "Given a code comment, retrieve the code snippet corresponding to that comment.",
    "CodeFeedbackMT": "Given a question about coding, retrieval code or passage that can solve user's question",
    "CodeFeedbackST": "Given a question about coding, retrieval code or passage that can solve user's question",
}

SANITY_TOLERANCE = 1.0  # RFC 0007 §11.2: within 1.0 point of the published mean


def model_family(preset: str) -> str | None:
    """`qwen3-0.6b-q8` -> `qwen3-0.6b` (the published numbers are per model)."""
    for fam in PUBLISHED:
        if preset.startswith(fam + "-") or preset == fam:
            return fam
    return None


def require_mteb() -> None:
    if mteb is None:
        raise SystemExit("rueca-bench coir needs the optional `coir` group: uv run --project bench --group coir rueca-bench coir ...")


@dataclass
class InstructionPolicy:
    """`task`: per-task Qwen3 prompts for Qwen3 presets; `preset`: the
    binary's default; anything else: that text for every task."""

    mode: str = "task"
    preset: str = DEFAULT_PRESET

    def for_task(self, task: str) -> str | None:
        if self.mode == "preset":
            return None
        if self.mode == "task":
            return QWEN3_TASK_PROMPTS.get(task) if self.preset.startswith("qwen3") else None
        return self.mode

    def tag(self) -> str:
        return "" if self.mode == "task" else ("-ipreset" if self.mode == "preset" else f"-i{short_hash(self.mode)}")


def _model_meta(name: str, revision: str, dims: int | None, max_tokens: int) -> Any:
    return ModelMeta(
        loader=None,
        name=name,
        revision=revision,
        release_date=None,
        languages=None,
        n_parameters=None,
        memory_usage_mb=None,
        max_tokens=max_tokens,
        embed_dim=dims,
        license=None,
        open_weights=True,
        public_training_code=None,
        public_training_data=None,
        framework=[],
        similarity_fn_name="cosine",
        use_instructions=True,
        training_datasets=None,
    )


class RuecaMtebModel(AbsEncoder):
    """mteb encoder over an `Embedder` (cached): `prompt_type` query -> the
    query flag, with the task's instruction set on the embedder first."""

    def __init__(self, embedder: CachedEmbedder, dims: int | None, policy: InstructionPolicy, max_tokens: int = DEFAULT_MAX_TOKENS):
        self.embedder = embedder
        self.dims = dims
        self.policy = policy
        self.calls: list[tuple[str, bool, int]] = []
        name = f"rueca/{policy.preset}"
        self.mteb_model_meta = _model_meta(name, embedder.model_key(), dims, max_tokens) if ModelMeta is not None else None

    def _set_instruction(self, task: str) -> None:
        inner = self.embedder.inner
        if hasattr(inner, "instruction"):
            inner.instruction = self.policy.for_task(task)

    def encode(self, inputs: Iterable[dict], *, task_metadata: Any, hf_split: str, hf_subset: str, prompt_type: Any = None, **kwargs: Any) -> np.ndarray:
        texts = [t for batch in inputs for t in batch["text"]]
        query = PromptType is not None and prompt_type == PromptType.query
        if query:
            self._set_instruction(task_metadata.name)
        self.calls.append((task_metadata.name, query, len(texts)))
        return truncate_normalize(self.embedder.embed(texts, query), self.dims)


# --- smoke runs: a smaller corpus ------------------------------------------------------------


def _positive(docs: dict) -> list[str]:
    return [d for d, v in docs.items() if v and v > 0]


def limit_split(split: dict, n: int) -> None:
    """Keep at most `n` documents of one (subset, split): the gold documents
    of the first queries (sorted ids) up to half of `n`, then other
    documents in corpus order; queries whose gold was not kept are dropped."""
    rel = split["relevant_docs"]
    keep_q: list[str] = []
    gold: list[str] = []
    for q in sorted(rel):
        docs = _positive(rel[q])
        if not docs or len(gold) + len(docs) > max(1, n // 2):
            continue
        keep_q.append(q)
        gold.extend(d for d in docs if d not in gold)
    keep = set(gold)
    for d in split["corpus"]["id"]:
        if len(keep) >= n:
            break
        keep.add(d)
    corpus_ids = split["corpus"]["id"]
    split["corpus"] = split["corpus"].select([i for i, d in enumerate(corpus_ids) if d in keep])
    qset = set(keep_q)
    split["queries"] = split["queries"].select([i for i, q in enumerate(split["queries"]["id"]) if q in qset])
    split["relevant_docs"] = {q: rel[q] for q in keep_q}
    if split.get("top_ranked"):
        split["top_ranked"] = {q: [d for d in split["top_ranked"][q] if d in keep] for q in keep_q if q in split["top_ranked"]}


def limit_task(task: Any, n: int) -> None:
    for subset in task.dataset.values():
        for split in subset.values():
            limit_split(split, n)


# --- running ------------------------------------------------------------------------------------


def _sizes(task: Any) -> tuple[int, int]:
    docs = sum(len(sp["corpus"]) for sub in task.dataset.values() for sp in sub.values())
    queries = sum(len(sp["queries"]) for sub in task.dataset.values() for sp in sub.values())
    return docs, queries


def evaluate_task(model: RuecaMtebModel, task: Any, limit_corpus: int | None = None) -> dict:
    """nDCG@10 (percent) of one mteb task, with its dataset revision and size."""
    t0 = time.perf_counter()
    task.load_data()
    if limit_corpus:
        limit_task(task, limit_corpus)
    docs, queries = _sizes(task)
    res = mteb.evaluate(model, task, cache=None, overwrite_strategy="always", show_progress_bar=False)
    score = res.task_results[0].get_score(getter=lambda s: s["ndcg_at_10"])
    return {
        "ndcg@10": round(100 * float(score), 2),
        "docs": docs,
        "queries": queries,
        "dataset": task.metadata.dataset.get("path"),
        "revision": task.metadata.dataset.get("revision"),
        "seconds": round(time.perf_counter() - t0, 1),
    }


def compare(scores: dict[str, float], published: dict[str, float]) -> dict:
    """Mean over all tasks, and over the tasks with a published number
    against the published mean (RFC 0007 §11.2 sanity row)."""
    common = [t for t in scores if t in published]
    out: dict = {"mean": round(float(np.mean(list(scores.values()))), 2) if scores else None, "tasks_with_published": common}
    if common:
        ours = float(np.mean([scores[t] for t in common]))
        ref = float(np.mean([published[t] for t in common]))
        out.update(mean_published_tasks=round(ours, 2), published_mean=round(ref, 2), delta=round(ours - ref, 2), within_tolerance=abs(ours - ref) <= SANITY_TOLERANCE)
    return out


@dataclass
class CoirRun:
    preset: str = DEFAULT_PRESET
    dims: int | None = None
    max_tokens: int = DEFAULT_MAX_TOKENS
    instruction: str = "task"
    profile: str = "cheap"
    tasks: list[str] | None = None
    limit_corpus: int | None = None
    out_dir: Path = field(default_factory=lambda: RESULTS_DIR / "coir")

    def task_names(self) -> list[str]:
        return list(self.tasks) if self.tasks else list(PROFILES[self.profile])

    def out_path(self) -> Path:
        prof = "" if self.profile == "cheap" or self.tasks else f"-{self.profile}"
        smoke = f"-smoke{self.limit_corpus}" if self.limit_corpus else ""
        tasks = f"-tasks{short_hash(','.join(self.tasks))[:8]}" if self.tasks else ""
        name = f"{self.preset}-{self.dims or 'native'}-{self.max_tokens}{prof}{tasks}{InstructionPolicy(self.instruction, self.preset).tag()}{smoke}.json"
        return self.out_dir / name

    def run(self, embedder: Embedder, task_objects: list[Any] | None = None) -> dict:
        """Evaluate every task (mteb's own task objects unless given) and
        write the results file."""
        require_mteb()
        cached = embedder if isinstance(embedder, CachedEmbedder) else CachedEmbedder(embedder, progress=True)
        model = RuecaMtebModel(cached, self.dims, InstructionPolicy(self.instruction, self.preset), self.max_tokens)
        tasks = task_objects if task_objects is not None else [mteb.get_task(n) for n in self.task_names()]
        published = PUBLISHED.get(model_family(self.preset) or "", {})
        per: dict[str, dict] = {}
        for task in tasks:
            name = task.metadata.name
            log(f"coir {name}")
            row = evaluate_task(model, task, self.limit_corpus)
            row["published"] = published.get(name)
            row["delta"] = round(row["ndcg@10"] - row["published"], 2) if row["published"] is not None else None
            row["instruction"] = model.policy.for_task(name)
            per[name] = row
        result = self._result(cached, per, published)
        write_json(self.out_path(), result)
        log(f"coir: wrote {self.out_path()}")
        return result

    def _result(self, cached: CachedEmbedder, per: dict[str, dict], published: dict[str, float]) -> dict:
        return {
            "date": dt.datetime.now(dt.timezone.utc).strftime("%Y-%m-%d %H:%M UTC"),
            "preset": self.preset,
            "dims": self.dims,
            "max_tokens": self.max_tokens,
            "instruction": self.instruction,
            "profile": None if self.tasks else self.profile,
            "limit_corpus": self.limit_corpus,
            "model": cached.info(),
            "mteb_version": getattr(mteb, "__version__", None),
            "published_source": PUBLISHED_SOURCE if published else None,
            "tasks": per,
            "summary": compare({k: v["ndcg@10"] for k, v in per.items()}, published),
            "cache": {"hits": cached.hits, "misses": cached.misses},
        }


def make_embedder(binary: str | None, preset: str, max_tokens: int, batch: int | None, cpu: bool, threads: int | None) -> RuecaEmbedder:
    if not binary:
        raise SystemExit("rueca-bench coir: set RUECA_BIN to a rueca binary with `rueca embed`")
    return RuecaEmbedder(binary, preset, max_tokens, None, batch, cpu, threads)


def summary_lines(result: dict) -> list[str]:
    lines = ["| Task | nDCG@10 | published | Δ |", "|---|---:|---:|---:|"]
    for name, r in result["tasks"].items():
        pub = "—" if r["published"] is None else f"{r['published']:.2f}"
        delta = "—" if r["delta"] is None else f"{r['delta']:+.2f}"
        lines.append(f"| {name} | {r['ndcg@10']:.2f} | {pub} | {delta} |")
    s = result["summary"]
    if s.get("published_mean") is not None:
        ok = "within" if s["within_tolerance"] else "OUTSIDE"
        lines.append(f"\nmean over {len(s['tasks_with_published'])} tasks with a published number: {s['mean_published_tasks']:.2f} vs {s['published_mean']:.2f} (Δ {s['delta']:+.2f}, {ok} ±{SANITY_TOLERANCE})")
    return lines
