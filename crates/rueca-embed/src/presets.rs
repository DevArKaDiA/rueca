//! The presets manifest, compiled into the binary (RFC 0007 §3.1, §4.4).
//!
//! Every preset names one GGUF file by URL, sha256 and size. Official files
//! are pinned to a Hugging Face revision (the URL holds the commit), and
//! their sha256 is the Git LFS object id published for that revision.
//! Weights are never in the binary: [`crate::ModelCache`] downloads them only
//! on request (`rueca models pull`) and checks them against this table.
//!
//! The table holds the Qwen3-Embedding family (RFC 0007 §3.1; the default is
//! the owner's decision recorded in `docs/decisions.md`). Another model is
//! one more [`Preset`] entry: weights file, pooling, query style and widths.

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
    /// A file whose download is not published yet; a local file with the
    /// pinned sha256 still loads.
    Unavailable,
}

impl PresetStatus {
    /// The manifest spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Unavailable => "unavailable",
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
}

impl Pooling {
    /// Stable name, hashed into the embed key.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Last => "last",
            Self::Cls => "cls",
            Self::Mean => "mean",
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
    /// A fixed prefix before the query (encoder models).
    Prefix {
        /// The prefix.
        prefix: &'static str,
    },
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
    /// Query formatting.
    pub query: QueryStyle,
    /// Prefix of every document (empty for every preset today).
    pub document_prefix: &'static str,
    /// The weights file.
    pub weights: WeightsFile,
    /// One line for `rueca models list`.
    pub note: &'static str,
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
    /// [`Error::Unavailable`] for a file whose download is not published.
    pub fn pullable(&self) -> Result<&WeightsFile> {
        match self.status {
            PresetStatus::Available => Ok(&self.weights),
            PresetStatus::Unavailable => Err(Error::Unavailable {
                preset: self.name.to_string(),
                status: self.status.as_str().to_string(),
                reason: self.note.to_string(),
            }),
        }
    }
}

/// A Qwen3-Embedding preset with an official GGUF file.
const fn qwen(
    name: &'static str,
    size: Qwen,
    quantization: &'static str,
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
        status: PresetStatus::Available,
        quantization,
        dims,
        min_dims: 32,
        context: 32_768,
        pooling: Pooling::Last,
        query: QWEN_INSTRUCT,
        document_prefix: "",
        weights,
        note,
    }
}

#[derive(Clone, Copy)]
enum Qwen {
    B06,
    B4,
    B8,
}

/// The default preset: the owner's decision (RFC 0007 §3.3, `docs/decisions.md`).
pub const DEFAULT_PRESET: &str = "qwen3-0.6b-q8";

/// Every preset, default first.
pub static PRESETS: &[Preset] = &[
    qwen(
        "qwen3-0.6b-q8",
        Qwen::B06,
        "Q8_0",
        WeightsFile {
            file_name: "Qwen3-Embedding-0.6B-Q8_0.gguf",
            url: qwen_url!(B06, "Qwen3-Embedding-0.6B-Q8_0.gguf"),
            sha256: "06507c7b42688469c4e7298b0a1e16deff06caf291cf0a5b278c308249c3e439",
            bytes: 639_150_592,
        },
        "default (docs/decisions.md); official Q8_0",
    ),
    qwen(
        "qwen3-0.6b-f16",
        Qwen::B06,
        "F16",
        WeightsFile {
            file_name: "Qwen3-Embedding-0.6B-f16.gguf",
            url: qwen_url!(B06, "Qwen3-Embedding-0.6B-f16.gguf"),
            sha256: "421a27e58d165478cc7acb984a688c2aa41404968b0203e7cd743ece44c54340",
            bytes: 1_197_629_632,
        },
        "reference for rank overlap; official f16",
    ),
    qwen(
        "qwen3-4b-q4",
        Qwen::B4,
        "Q4_K_M",
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
        WeightsFile {
            file_name: "Qwen3-Embedding-8B-Q8_0.gguf",
            url: qwen_url!(B8, "Qwen3-Embedding-8B-Q8_0.gguf"),
            sha256: "d20ddc71e8a5c4344f2343481e242233a997dc5eaff442427a945836c97b4deb",
            bytes: 8_047_105_824,
        },
        "official Q8_0; GPU, service mode",
    ),
];

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
            assert!(!p.name.contains(':') && !p.name.contains('/'), "{}", p.name);
            assert!(p.min_dims <= p.dims, "{}", p.name);
            let w = p.weights;
            assert_eq!(w.sha256.len(), 64, "{}", p.name);
            assert!(w.sha256.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)), "{}", p.name);
            assert!(w.bytes > 0, "{}", p.name);
            assert!(w.url.starts_with("https://") && w.url.ends_with(w.file_name), "{}", p.name);
            assert!(w.url.contains(p.revision), "{}: the URL pins the revision", p.name);
            assert!(files.insert(w.sha256), "duplicate file in {}", p.name);
            assert_eq!(p.status == PresetStatus::Available, p.pullable().is_ok(), "{}", p.name);
        }
        assert_eq!(PRESETS[0].name, DEFAULT_PRESET);
        assert!(preset("nope").is_err());
    }

    #[test]
    fn default_preset_is_the_official_q8() {
        let p = preset(DEFAULT_PRESET).unwrap();
        let w = p.weights;
        assert_eq!(w.sha256, "06507c7b42688469c4e7298b0a1e16deff06caf291cf0a5b278c308249c3e439");
        assert_eq!(w.bytes, 639_150_592);
        assert_eq!((p.dims, p.pooling, p.status), (1024, Pooling::Last, PresetStatus::Available));
        assert!(matches!(p.query, QueryStyle::Instruct { instruction } if instruction == DEFAULT_CODE_INSTRUCTION));
    }
}
