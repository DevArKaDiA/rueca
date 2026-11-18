//! The presets manifest, compiled into the binary (RFC 0007 §3.1, §4.4).
//!
//! Every local preset names one GGUF file by URL, sha256 and size. Official
//! files are pinned to a Hugging Face revision (the URL holds the commit),
//! and their sha256 is the Git LFS object id published for that revision.
//! Weights are never in the binary: [`crate::ModelCache`] downloads them on
//! request and checks them against this table.

use serde::Serialize;

use crate::error::{Error, Result};

/// Default query instruction for code retrieval: the issue-shaped one, chosen
/// by the phase 0 sweep on the S1 calibration split (RFC 0007 §5.3; mean
/// objective 68.5 against 66.7 for [`QUESTION_INSTRUCTION`] and 68.0 for a
/// code-search instruction; recorded in `docs/decisions.md`). Documents carry
/// no instruction, so changing it invalidates no stored vector (§5.4).
pub const DEFAULT_CODE_INSTRUCTION: &str =
    "Given a GitHub issue, retrieve the source code functions that must change to fix it";

/// The question-shaped instruction the phase 0 sweep started from.
pub const QUESTION_INSTRUCTION: &str =
    "Given a natural-language question about a codebase, retrieve the code symbols that answer it";

/// Qwen's own code-retrieval instruction (Qwen3-Embedding `task_prompts.json`),
/// a phase 0 sweep candidate.
pub const QWEN_CODE_INSTRUCTION: &str =
    "Given a question about coding, retrieval code or passage that can solve user's question";

/// The issue-localization instruction of RFC 0007 §5.3, a phase 0 sweep
/// candidate.
pub const ISSUE_INSTRUCTION: &str = "Given a GitHub issue or a question about a code base, retrieve the functions, \
     classes or sections that answer it or must change";

/// Whether a preset can be used, and when it could be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PresetStatus {
    /// Official weights, downloadable now.
    Available,
    /// A Rueca-hosted conversion that is not published yet; a local file
    /// with the pinned sha256 still loads.
    Unavailable,
    /// Listed for the phase 0 model sweep; no GGUF file pinned yet.
    Planned,
    /// A remote API preset (RFC 0007 §3.5): no weights, the provider's
    /// API key in the environment, opt-in.
    Remote,
    /// Listed, not implemented yet (Bedrock).
    Later,
}

impl PresetStatus {
    /// The manifest spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Unavailable => "unavailable",
            Self::Planned => "planned",
            Self::Remote => "remote",
            Self::Later => "later",
        }
    }
}

/// How token states become one vector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Pooling {
    /// The last token's state (decoder embedders, Qwen3).
    Last,
    /// The first token's state (BERT-family encoders).
    Cls,
    /// The mean over tokens.
    Mean,
    /// Whatever a remote provider does (not known, not keyed).
    Provider,
}

impl Pooling {
    /// Stable name, hashed into the embed key.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Last => "last",
            Self::Cls => "cls",
            Self::Mean => "mean",
            Self::Provider => "provider",
        }
    }
}

/// How a query is turned into model input. Documents are embedded as they
/// are (no prefix) for every preset in this manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "style", rename_all = "snake_case")]
pub enum QueryStyle {
    /// Qwen3: `Instruct: {instruction}\nQuery:{query}` (model card format,
    /// no space after `Query:`).
    Instruct {
        /// The default instruction; [`crate::EmbedOptions::query_instruction`]
        /// overrides it.
        instruction: &'static str,
    },
    /// A fixed prefix before the query (CodeRankEmbed).
    Prefix {
        /// The prefix.
        prefix: &'static str,
    },
}

/// Where a preset's vectors come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PresetKind {
    /// A GGUF file run in-process.
    Local,
    /// A provider API (`remote:` presets).
    Remote,
}

/// What a preset's model does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Turns texts into vectors ([`crate::Embedder`]).
    Embedder,
    /// Scores (query, document) pairs ([`crate::Reranker`], RFC 0007 §7.5).
    Reranker,
}

impl Role {
    /// The manifest spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Embedder => "embedder",
            Self::Reranker => "reranker",
        }
    }
}

/// One weights file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct WeightsFile {
    /// File name inside `<models>/<preset>/`.
    pub file_name: &'static str,
    /// Download URL (pinned revision).
    pub url: &'static str,
    /// Lower-case hex sha256 of the file.
    pub sha256: &'static str,
    /// Size in bytes.
    pub bytes: u64,
}

/// One manifest entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Preset {
    /// Preset name (`qwen3-0.6b-q8`).
    pub name: &'static str,
    /// Upstream model.
    pub model: &'static str,
    /// Upstream repository revision the weights come from.
    pub revision: &'static str,
    /// Model license (SPDX).
    pub license: &'static str,
    /// Local or remote.
    pub kind: PresetKind,
    /// Embedder or reranker.
    pub role: Role,
    /// Availability.
    pub status: PresetStatus,
    /// Weights quantization (`Q8_0`, `F16`, `Q4_K_M`).
    pub quantization: &'static str,
    /// Full output width.
    pub dims: usize,
    /// Smallest Matryoshka width the model was trained for.
    pub min_dims: usize,
    /// Model context length in tokens.
    pub context: usize,
    /// Pooling.
    pub pooling: Pooling,
    /// Query formatting; for a reranker, the instruction of its prompt.
    pub query: QueryStyle,
    /// Prefix of every document (empty for every preset today).
    pub document_prefix: &'static str,
    /// The weights, for local presets with a pinned file.
    pub weights: Option<WeightsFile>,
    /// One line for `rueca models list`.
    pub note: &'static str,
}

impl Preset {
    /// The weights, or the reason this preset has none to load.
    ///
    /// # Errors
    ///
    /// [`Error::Unavailable`] for planned and remote presets.
    pub fn weights(&self) -> Result<&WeightsFile> {
        self.weights.as_ref().ok_or_else(|| Error::Unavailable {
            preset: self.name.to_string(),
            status: self.status.as_str().to_string(),
            reason: self.note.to_string(),
        })
    }
}

const QWEN_INSTRUCT: QueryStyle = QueryStyle::Instruct { instruction: DEFAULT_CODE_INSTRUCTION };

/// Download URL of a file of an official Qwen GGUF repository, pinned to the
/// revision recorded in [`qwen`].
macro_rules! qwen_url {
    (B06, $file:literal) => {
        concat!(
            "https://huggingface.co/Qwen/Qwen3-Embedding-0.6B-GGUF/resolve/370f27d7550e0def9b39c1f16d3fbaa13aa67728/",
            $file
        )
    };
    (B4, $file:literal) => {
        concat!(
            "https://huggingface.co/Qwen/Qwen3-Embedding-4B-GGUF/resolve/f4602530db1d980e16da9d7d3a70294cf5c190be/",
            $file
        )
    };
    (B8, $file:literal) => {
        concat!(
            "https://huggingface.co/Qwen/Qwen3-Embedding-8B-GGUF/resolve/69d0e58a13e463cd99a9b83e3f5fee7c10265fab/",
            $file
        )
    };
}

impl Preset {
    /// The weights, if `rueca models pull` can download them now.
    ///
    /// # Errors
    ///
    /// [`Error::Unavailable`] for planned and remote presets and for
    /// Rueca-hosted files not published yet.
    pub fn pullable(&self) -> Result<&WeightsFile> {
        match (self.status, self.weights.as_ref()) {
            (PresetStatus::Available, Some(w)) => Ok(w),
            _ => Err(Error::Unavailable {
                preset: self.name.to_string(),
                status: self.status.as_str().to_string(),
                reason: self.note.to_string(),
            }),
        }
    }
}

/// A Qwen3-Embedding preset with an official or Rueca-hosted GGUF file.
const fn qwen(
    name: &'static str,
    size: Qwen,
    quantization: &'static str,
    status: PresetStatus,
    weights: WeightsFile,
    note: &'static str,
) -> Preset {
    let (model, revision, dims) = match size {
        Qwen::B06 => ("Qwen/Qwen3-Embedding-0.6B-GGUF", "370f27d7550e0def9b39c1f16d3fbaa13aa67728", 1024),
        Qwen::B4 => ("Qwen/Qwen3-Embedding-4B-GGUF", "f4602530db1d980e16da9d7d3a70294cf5c190be", 2560),
        Qwen::B8 => ("Qwen/Qwen3-Embedding-8B-GGUF", "69d0e58a13e463cd99a9b83e3f5fee7c10265fab", 4096),
    };
    Preset {
        name,
        model,
        revision,
        license: "Apache-2.0",
        kind: PresetKind::Local,
        role: Role::Embedder,
        status,
        quantization,
        dims,
        min_dims: 32,
        context: 32_768,
        pooling: Pooling::Last,
        query: QWEN_INSTRUCT,
        document_prefix: "",
        weights: Some(weights),
        note,
    }
}

#[derive(Clone, Copy)]
enum Qwen {
    B06,
    B4,
    B8,
}

/// An upstream encoder model (BERT family): repository, revision, license,
/// pooling and query style.
#[derive(Clone, Copy)]
struct Encoder {
    model: &'static str,
    revision: &'static str,
    license: &'static str,
    pooling: Pooling,
    query: QueryStyle,
}

/// CodeRankEmbed (NomicBERT, 137M, 768 dims, 8,192 tokens, CLS pooling;
/// queries get the model card's prefix, documents none).
const CODERANK: Encoder = Encoder {
    model: "nomic-ai/CodeRankEmbed",
    revision: "3c4b60807d71f79b43f3c4363786d9493691f8b1",
    license: "MIT",
    pooling: Pooling::Cls,
    query: QueryStyle::Prefix { prefix: CODERANK_QUERY_PREFIX },
};

/// CodeRankEmbed's query prefix (`config_sentence_transformers.json`,
/// prompt `query`).
pub const CODERANK_QUERY_PREFIX: &str = "Represent this query for searching relevant code: ";

/// gte-modernbert-base (ModernBERT, 149M, 768 dims, 8,192 tokens, CLS
/// pooling, no prefix).
const GTE_MODERNBERT: Encoder = Encoder {
    model: "Alibaba-NLP/gte-modernbert-base",
    revision: "e7f32e3c00f91d699e8c43b53106206bcc72bb22",
    license: "Apache-2.0",
    pooling: Pooling::Cls,
    query: QueryStyle::Prefix { prefix: "" },
};

/// An encoder preset: 768 dims (no Matryoshka range), 8,192 tokens of
/// context, documents without a prefix.
const fn encoder(
    name: &'static str,
    upstream: Encoder,
    quantization: &'static str,
    status: PresetStatus,
    weights: Option<WeightsFile>,
    note: &'static str,
) -> Preset {
    Preset {
        name,
        model: upstream.model,
        revision: upstream.revision,
        license: upstream.license,
        kind: PresetKind::Local,
        role: Role::Embedder,
        status,
        quantization,
        dims: 768,
        min_dims: 768,
        context: 8192,
        pooling: upstream.pooling,
        query: upstream.query,
        document_prefix: "",
        weights,
        note,
    }
}

/// A `remote:` embedder preset (RFC 0007 §3.5): widths `min_dims..=dims`
/// (the provider's default when none is asked, `crate::remote`).
const fn remote(name: &'static str, (min_dims, dims): (usize, usize), note: &'static str) -> Preset {
    remote_as(name, (min_dims, dims), Role::Embedder, PresetStatus::Remote, note)
}

/// A `remote:` preset with a role and status.
const fn remote_as(
    name: &'static str,
    (min_dims, dims): (usize, usize),
    role: Role,
    status: PresetStatus,
    note: &'static str,
) -> Preset {
    Preset {
        name,
        model: name,
        revision: "",
        license: "provider terms",
        kind: PresetKind::Remote,
        role,
        status,
        quantization: "",
        dims,
        min_dims,
        context: 0,
        pooling: Pooling::Provider,
        query: QWEN_INSTRUCT,
        document_prefix: "",
        weights: None,
        note,
    }
}

/// The note of an implemented remote preset.
const REMOTE_NOTE: &str = "remote API, opt-in (an organization needs `[retrieval] allow_remote = true`); \
     any model of the provider works as `remote:<provider>/<model>` (RFC 0007 §3.5)";
/// The note of the Bedrock presets.
const BEDROCK_NOTE: &str = "later: needs AWS SigV4 signing and credential resolution (RFC 0007 §3.5)";

/// The instruction of the reranker prompt for code (Qwen3-Reranker's
/// `<Instruct>:` line). Pinned separately from the embedder's: the phase 0
/// instruction sweep measured embedders only, so the reranker keeps the
/// question-shaped text until its own sweep (RFC 0007 §7.5).
pub const RERANK_INSTRUCTION: &str = QUESTION_INSTRUCTION;

/// A Qwen3-Reranker preset (RFC 0007 §7.5): a causal LM converted with
/// llama.cpp's `convert_hf_to_gguf.py`, which turns the `yes` / `no` rows
/// of the LM head into a two-way classifier (`cls.output`, rank pooling).
const fn qwen_reranker(
    name: &'static str,
    (model, revision): (&'static str, &'static str),
    quantization: &'static str,
    status: PresetStatus,
    dims: usize,
    weights: Option<WeightsFile>,
    note: &'static str,
) -> Preset {
    Preset {
        name,
        model,
        revision,
        license: "Apache-2.0",
        kind: PresetKind::Local,
        role: Role::Reranker,
        status,
        quantization,
        dims,
        min_dims: dims,
        context: 32_768,
        pooling: Pooling::Last,
        query: QueryStyle::Instruct { instruction: RERANK_INSTRUCTION },
        document_prefix: "",
        weights,
        note,
    }
}

/// The default reranker preset (off by default, RFC 0007 §7.5).
pub const DEFAULT_RERANK_PRESET: &str = "qwen3-reranker-0.6b-q8";

/// The default preset (RFC 0007 §3.3, provisional).
pub const DEFAULT_PRESET: &str = "qwen3-0.6b-q8";

/// Every preset, default first.
pub static PRESETS: &[Preset] = &[
    qwen(
        "qwen3-0.6b-q8",
        Qwen::B06,
        "Q8_0",
        PresetStatus::Available,
        WeightsFile {
            file_name: "Qwen3-Embedding-0.6B-Q8_0.gguf",
            url: qwen_url!(B06, "Qwen3-Embedding-0.6B-Q8_0.gguf"),
            sha256: "06507c7b42688469c4e7298b0a1e16deff06caf291cf0a5b278c308249c3e439",
            bytes: 639_150_592,
        },
        "default (provisional, RFC 0007 §3.3); official Q8_0",
    ),
    qwen(
        "qwen3-0.6b-f16",
        Qwen::B06,
        "F16",
        PresetStatus::Available,
        WeightsFile {
            file_name: "Qwen3-Embedding-0.6B-f16.gguf",
            url: qwen_url!(B06, "Qwen3-Embedding-0.6B-f16.gguf"),
            sha256: "421a27e58d165478cc7acb984a688c2aa41404968b0203e7cd743ece44c54340",
            bytes: 1_197_629_632,
        },
        "reference for rank overlap; official f16",
    ),
    qwen(
        "qwen3-0.6b-q4",
        Qwen::B06,
        "Q4_K_M",
        PresetStatus::Unavailable,
        WeightsFile {
            file_name: "Qwen3-Embedding-0.6B-Q4_K_M.gguf",
            url: "https://github.com/DevArKaDiA/rueca-models/releases/download/qwen3-embedding-0.6b-q4_k_m-v1/Qwen3-Embedding-0.6B-Q4_K_M.gguf",
            sha256: Q4_SHA256,
            bytes: Q4_BYTES,
        },
        "Rueca-made Q4_K_M (llama-quantize of the official f16, llama.cpp d81235049384), not published yet; \
         a local file with the pinned sha256 loads (docs/how-it-works.md, Embeddings)",
    ),
    qwen(
        "qwen3-4b-q4",
        Qwen::B4,
        "Q4_K_M",
        PresetStatus::Available,
        WeightsFile {
            file_name: "Qwen3-Embedding-4B-Q4_K_M.gguf",
            url: qwen_url!(B4, "Qwen3-Embedding-4B-Q4_K_M.gguf"),
            sha256: "2b0cf8f17b4c723c27303015383c27ec4bf2d8314bb677d05e920dd70bb0f16b",
            bytes: 2_496_703_776,
        },
        "official Q4_K_M; GPU",
    ),
    qwen(
        "qwen3-4b-q8",
        Qwen::B4,
        "Q8_0",
        PresetStatus::Available,
        WeightsFile {
            file_name: "Qwen3-Embedding-4B-Q8_0.gguf",
            url: qwen_url!(B4, "Qwen3-Embedding-4B-Q8_0.gguf"),
            sha256: "b60ae5ce2dd6a0b77f82cadf21def1f310a3e10cde380ad0081b07a9d416949d",
            bytes: 4_279_660_224,
        },
        "official Q8_0; GPU",
    ),
    qwen(
        "qwen3-8b-q4",
        Qwen::B8,
        "Q4_K_M",
        PresetStatus::Available,
        WeightsFile {
            file_name: "Qwen3-Embedding-8B-Q4_K_M.gguf",
            url: qwen_url!(B8, "Qwen3-Embedding-8B-Q4_K_M.gguf"),
            sha256: "3fcd3febec8b3fd64435204db75bf0dd73b91e8d0661e0331acfe7e7c3120b85",
            bytes: 4_676_804_928,
        },
        "official Q4_K_M; GPU, service mode",
    ),
    qwen(
        "qwen3-8b-q8",
        Qwen::B8,
        "Q8_0",
        PresetStatus::Available,
        WeightsFile {
            file_name: "Qwen3-Embedding-8B-Q8_0.gguf",
            url: qwen_url!(B8, "Qwen3-Embedding-8B-Q8_0.gguf"),
            sha256: "d20ddc71e8a5c4344f2343481e242233a997dc5eaff442427a945836c97b4deb",
            bytes: 8_047_105_824,
        },
        "official Q8_0; GPU, service mode",
    ),
    encoder(
        "coderank-137m",
        CODERANK,
        "F16",
        PresetStatus::Unavailable,
        Some(WeightsFile {
            file_name: "CodeRankEmbed-F16.gguf",
            url: "https://github.com/DevArKaDiA/rueca-models/releases/download/coderankembed-f16-v1/CodeRankEmbed-F16.gguf",
            sha256: CODERANK_F16_SHA256,
            bytes: CODERANK_F16_BYTES,
        }),
        "Rueca conversion (convert_hf_to_gguf.py --outtype f16, llama.cpp d81235049384; NomicBERT, CLS pooling), \
         not published yet: convert locally (docs/how-it-works.md §10, Encoder presets) and register the file \
         with `rueca models import coderank-137m <file>`, or pull once published",
    ),
    encoder(
        "coderank-137m-q8",
        CODERANK,
        "Q8_0",
        PresetStatus::Unavailable,
        Some(WeightsFile {
            file_name: "CodeRankEmbed-Q8_0.gguf",
            url: "https://github.com/DevArKaDiA/rueca-models/releases/download/coderankembed-q8_0-v1/CodeRankEmbed-Q8_0.gguf",
            sha256: CODERANK_Q8_SHA256,
            bytes: CODERANK_Q8_BYTES,
        }),
        "Rueca conversion (convert_hf_to_gguf.py --outtype q8_0, llama.cpp d81235049384), not published yet: \
         convert locally (docs/how-it-works.md §10, Encoder presets) and register the file with \
         `rueca models import coderank-137m-q8 <file>`, or pull once published",
    ),
    encoder(
        "gte-modernbert-149m",
        GTE_MODERNBERT,
        "F16",
        PresetStatus::Unavailable,
        Some(WeightsFile {
            file_name: "gte-modernbert-base-F16.gguf",
            url: "https://github.com/DevArKaDiA/rueca-models/releases/download/gte-modernbert-base-f16-v1/gte-modernbert-base-F16.gguf",
            sha256: GTE_MODERNBERT_F16_SHA256,
            bytes: GTE_MODERNBERT_F16_BYTES,
        }),
        "Rueca conversion (convert_hf_to_gguf.py --outtype f16, llama.cpp d81235049384; ModernBERT, CLS pooling, \
         no prefix), not published yet: convert locally (docs/how-it-works.md §10, Encoder presets) and register \
         the file with `rueca models import gte-modernbert-149m <file>`, or pull once published",
    ),
    qwen_reranker(
        "qwen3-reranker-0.6b-q8",
        ("Qwen/Qwen3-Reranker-0.6B", "e61197ed45024b0ed8a2d74b80b4d909f1255473"),
        "Q8_0",
        PresetStatus::Unavailable,
        1024,
        Some(WeightsFile {
            file_name: "Qwen3-Reranker-0.6B-Q8_0.gguf",
            url: "https://github.com/DevArKaDiA/rueca-models/releases/download/qwen3-reranker-0.6b-q8_0-v1/Qwen3-Reranker-0.6B-Q8_0.gguf",
            sha256: RERANK_06_SHA256,
            bytes: RERANK_06_BYTES,
        }),
        "Rueca conversion (convert_hf_to_gguf.py --outtype q8_0, llama.cpp d81235049384), not published yet: \
         convert locally (docs/how-it-works.md §10, Reranker) and register the file with \
         `rueca models import qwen3-reranker-0.6b-q8 <file>`, or pull once published",
    ),
    qwen_reranker(
        "qwen3-reranker-4b-q4",
        ("Qwen/Qwen3-Reranker-4B", ""),
        "Q4_K_M",
        PresetStatus::Planned,
        2560,
        None,
        "Rueca conversion to come (≈ 2.5 GB, published split under the 2 GiB release-asset limit); \
         measured first in the phase 2 sweep (RFC 0007 §7.5)",
    ),
    Preset {
        name: "mxbai-rerank-large-v2",
        model: "mixedbread-ai/mxbai-rerank-large-v2",
        revision: "",
        license: "Apache-2.0",
        kind: PresetKind::Local,
        role: Role::Reranker,
        status: PresetStatus::Planned,
        quantization: "F16",
        dims: 1536,
        min_dims: 1536,
        context: 8192,
        pooling: Pooling::Last,
        query: QWEN_INSTRUCT,
        document_prefix: "",
        weights: None,
        note: "Qwen2-based reranker with its own prompt; llama.cpp runtime path unverified (RFC 0007 §7.5)",
    },
    remote_as("remote:voyage/rerank-2.5", (0, 0), Role::Reranker, PresetStatus::Remote, REMOTE_NOTE),
    remote_as("remote:cohere/rerank-v3.5", (0, 0), Role::Reranker, PresetStatus::Remote, REMOTE_NOTE),
    remote("remote:voyage/voyage-code-4", (256, 2048), REMOTE_NOTE),
    remote("remote:voyage/voyage-code-3", (256, 2048), REMOTE_NOTE),
    remote("remote:openai/text-embedding-3-large", (1, 3072), REMOTE_NOTE),
    remote("remote:gemini/gemini-embedding-001", (128, 3072), REMOTE_NOTE),
    remote_as("remote:bedrock/titan-embed-text-v2", (256, 1024), Role::Embedder, PresetStatus::Later, BEDROCK_NOTE),
    remote_as("remote:bedrock/cohere-embed-v4", (256, 1536), Role::Embedder, PresetStatus::Later, BEDROCK_NOTE),
];

/// sha256 of the phase 0 local conversion (`llama-quantize` of the official
/// f16 to `Q4_K_M`); the published release asset must match it.
const Q4_SHA256: &str = "44f67b5875def0e90ae67247b141a5a861538efb8d8b0f783eef3d94211d9dcd";
/// Size of that conversion.
const Q4_BYTES: u64 = 396_474_560;

/// sha256 of the Rueca conversion of Qwen3-Reranker-0.6B to `Q8_0`
/// (reproducible: two runs give the same file); the release asset must
/// match it.
const RERANK_06_SHA256: &str = "def2f55d8729597c9456a441f9cc7781bbbbc1ac5ce8592a404a7575e30dd81e";
/// Size of that conversion.
const RERANK_06_BYTES: u64 = 639_153_376;

/// sha256 of the Rueca conversion of CodeRankEmbed to `F16` (reproducible:
/// two runs give the same file); the release asset must match it.
const CODERANK_F16_SHA256: &str = "8017b5bca09c111b5264cd686274b361aa54213a10cdf307eced3a3a0940444c";
/// Size of that conversion.
const CODERANK_F16_BYTES: u64 = 274_181_632;
/// sha256 of the Rueca conversion of CodeRankEmbed to `Q8_0` (reproducible).
const CODERANK_Q8_SHA256: &str = "829cc983d0c54aec5705ee6a3cdf12f6f87db145300d88ec2145bc235d94be9a";
/// Size of that conversion.
const CODERANK_Q8_BYTES: u64 = 146_033_152;

/// sha256 of the Rueca conversion of gte-modernbert-base to `F16` (the
/// upstream weights are `float16`; reproducible).
const GTE_MODERNBERT_F16_SHA256: &str = "a17fb60a5a2d8b6f171f846c834876fdaac3626e78a0b199c3e335adfff9eafe";
/// Size of that conversion.
const GTE_MODERNBERT_F16_BYTES: u64 = 299_877_088;

/// Whether `name` is a `remote:` preset (RFC 0007 §3.5), listed or not.
#[must_use]
pub fn is_remote_preset(name: &str) -> bool {
    name.starts_with("remote:")
}

/// Finds a preset by name.
///
/// # Errors
///
/// [`Error::UnknownPreset`] when no preset has that name.
pub fn preset(name: &str) -> Result<&'static Preset> {
    PRESETS.iter().find(|p| p.name == name).ok_or_else(|| Error::UnknownPreset(name.to_string()))
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn manifest_integrity() {
        let mut names = HashSet::new();
        let mut files = HashSet::new();
        for p in PRESETS {
            assert!(names.insert(p.name), "duplicate preset {}", p.name);
            assert!(p.min_dims <= p.dims, "{}", p.name);
            if let Some(w) = p.weights {
                assert_eq!(w.sha256.len(), 64, "{}", p.name);
                assert!(w.sha256.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)), "{}", p.name);
                assert!(w.bytes > 0, "{}", p.name);
                assert!(w.url.starts_with("https://") && w.url.ends_with(w.file_name), "{}", p.name);
                assert!(files.insert(w.sha256), "duplicate file in {}", p.name);
            }
            match p.kind {
                PresetKind::Remote => assert!(p.name.starts_with("remote:") && p.weights.is_none()),
                PresetKind::Local => assert!(!p.name.contains(':') && !p.name.contains('/'), "{}", p.name),
            }
            assert_eq!(p.status == PresetStatus::Available, p.pullable().is_ok(), "{}", p.name);
        }
        assert_eq!(preset(DEFAULT_RERANK_PRESET).unwrap().role, Role::Reranker);
        assert_eq!(PRESETS[0].name, DEFAULT_PRESET);
        assert!(preset("nope").is_err());
    }

    #[test]
    fn default_preset_is_the_official_q8() {
        let p = preset(DEFAULT_PRESET).unwrap();
        let w = p.weights.unwrap();
        assert_eq!(w.sha256, "06507c7b42688469c4e7298b0a1e16deff06caf291cf0a5b278c308249c3e439");
        assert_eq!(w.bytes, 639_150_592);
        assert_eq!((p.dims, p.pooling, p.status), (1024, Pooling::Last, PresetStatus::Available));
    }

    #[test]
    fn coderank_is_a_cls_encoder_with_a_query_prefix() {
        for name in ["coderank-137m", "coderank-137m-q8"] {
            let p = preset(name).unwrap();
            assert_eq!((p.dims, p.min_dims, p.context, p.pooling), (768, 768, 8192, Pooling::Cls), "{name}");
            assert_eq!(p.query, QueryStyle::Prefix { prefix: CODERANK_QUERY_PREFIX }, "{name}");
            assert_eq!((p.document_prefix, p.status), ("", PresetStatus::Unavailable), "{name}");
            assert!(p.weights.is_some() && p.pullable().is_err(), "{name}");
        }
        assert_eq!(preset("coderank-137m").unwrap().weights.unwrap().sha256, CODERANK_F16_SHA256);
        let gte = preset("gte-modernbert-149m").unwrap();
        assert_eq!((gte.pooling, gte.query, gte.dims), (Pooling::Cls, QueryStyle::Prefix { prefix: "" }, 768));
        assert_eq!(gte.status, PresetStatus::Unavailable);
    }
}
