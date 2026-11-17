//! Embedding runtime for Rueca (RFC 0007 §4): presets, weights, keys and
//! the runtimes behind two traits.
//!
//! # Design
//!
//! - [`Embedder`] turns [`EmbedInput`]s (documents and queries) into unit
//!   vectors, batched; [`Reranker`] scores documents against a query.
//!   `rueca-segment` stays free of model dependencies: phase 1 injects an
//!   embedder into `rueca-index`'s vectors pass.
//! - [`presets`] is the manifest compiled into the binary: per preset the
//!   weights file (URL pinned to an upstream revision, sha256, size),
//!   quantization, dimensions, pooling and query formatting. Weights are
//!   never in the binary.
//! - [`ModelCache`] keeps weights under `<store>/models/<preset>/` (or
//!   `$RUECA_MODEL_DIR`), downloads them with `ModelCache::pull` (feature
//!   `embeddings`), verifies their sha256 once and refuses a mismatch.
//! - The llama.cpp backend (`backend::llama`, feature `embeddings`, default
//!   on; Metal on Apple silicon, `cuda` as an opt-in feature) loads a GGUF
//!   file, tokenizes with the tokenizer inside it, pools the last token and
//!   L2-normalizes; with `dims` below the model width it truncates
//!   (Matryoshka) and re-normalizes.
//! - Queries get the preset's instruction, Qwen3's `Instruct: {task}\nQuery:{query}`
//!   ([`presets::DEFAULT_CODE_INSTRUCTION`] by default); documents get none.
//!
//! # Determinism (RFC 0007 §9)
//!
//! Every input fits one micro-batch (micro-batch = batch ≥ token budget),
//! and inputs are embedded in a fixed order, by token length then BLAKE3 of
//! the model input ([`vector::batch_order`]); results come back in input
//! order. Vectors are identified by an [`EmbedKey`] (model sha256, pooling,
//! document prefix, text builder version, token budget) and a [`ModelKey`]
//! (embed key, dimensions, encoding). Runtime, version, backend and CPU
//! level are not keyed but recorded in [`ModelMeta`] for audit; the query
//! instruction is not keyed either, since documents carry none.
//!
//! # Rerankers
//!
//! Phase 0 ships the [`Reranker`] trait and [`NoReranker`], which keeps the
//! first-stage order. The llama.cpp cross-encoder (Qwen3-Reranker, RFC 0007
//! §7.5) arrives in phase 2.

pub mod backend;
pub mod cache;
#[cfg(feature = "embeddings")]
mod download;
mod error;
pub mod key;
pub mod presets;
pub mod vector;

pub use cache::{CacheState, MODEL_DIR_ENV, ModelCache};
pub use error::{Error, Result};
pub use key::{Backend, EmbedKey, ModelKey, ModelMeta, TEXT_BUILDER_VERSION, VectorEncoding};
pub use presets::{DEFAULT_PRESET, PRESETS, Preset, PresetStatus, preset};

use presets::QueryStyle;

/// Whether a text is a document or a query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InputKind {
    /// Embedded as is.
    #[default]
    Document,
    /// Gets the preset's query instruction or prefix.
    Query,
}

/// One text to embed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbedInput {
    /// The text.
    pub text: String,
    /// Document or query.
    pub kind: InputKind,
}

impl EmbedInput {
    /// A document.
    pub fn document(text: impl Into<String>) -> Self {
        Self { text: text.into(), kind: InputKind::Document }
    }

    /// A query.
    pub fn query(text: impl Into<String>) -> Self {
        Self { text: text.into(), kind: InputKind::Query }
    }
}

/// Default token budget per input (RFC 0007 §5.2).
pub const DEFAULT_MAX_TOKENS: usize = 512;
/// Default tokens per batch.
pub const DEFAULT_BATCH_TOKENS: usize = 2048;

/// How an embedder is loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbedOptions {
    /// Output width (Matryoshka); `None` keeps the model width.
    pub dims: Option<usize>,
    /// Inputs are cut to this many tokens, special tokens included.
    pub max_tokens: usize,
    /// Tokens per batch (raised to `max_tokens` when smaller).
    pub batch_tokens: usize,
    /// Worker threads; `None` uses the physical cores.
    pub threads: Option<usize>,
    /// Run on the CPU even when a GPU backend is compiled in.
    pub cpu: bool,
    /// Query instruction for `Instruct` presets; `None` uses the preset's.
    pub query_instruction: Option<String>,
}

impl Default for EmbedOptions {
    fn default() -> Self {
        Self {
            dims: None,
            max_tokens: DEFAULT_MAX_TOKENS,
            batch_tokens: DEFAULT_BATCH_TOKENS,
            threads: None,
            cpu: false,
            query_instruction: None,
        }
    }
}

impl EmbedOptions {
    /// Checks the options against a preset and returns the output width.
    ///
    /// # Errors
    ///
    /// [`Error::Invalid`] for a width outside the preset's Matryoshka range
    /// or a zero token budget.
    pub fn output_dims(&self, preset: &Preset) -> Result<usize> {
        if self.max_tokens < 2 || self.max_tokens > preset.context {
            return Err(Error::Invalid(format!(
                "--max-tokens must be within 2..={} for `{}`",
                preset.context, preset.name
            )));
        }
        let dims = self.dims.unwrap_or(preset.dims);
        if dims < preset.min_dims || dims > preset.dims {
            return Err(Error::Invalid(format!(
                "--dims must be within {}..={} for `{}`",
                preset.min_dims, preset.dims, preset.name
            )));
        }
        Ok(dims)
    }

    /// The query prefix in effect for a preset.
    #[must_use]
    pub fn query_prefix(&self, preset: &Preset) -> String {
        match preset.query {
            QueryStyle::Instruct { instruction } => {
                let task = self.query_instruction.as_deref().unwrap_or(instruction);
                format!("Instruct: {task}\nQuery:")
            }
            QueryStyle::Prefix { prefix } => prefix.to_string(),
        }
    }
}

/// The text the model sees for an input.
#[must_use]
pub fn model_input(input: &EmbedInput, query_prefix: &str, document_prefix: &str) -> String {
    match input.kind {
        InputKind::Query => format!("{query_prefix}{}", input.text),
        InputKind::Document => format!("{document_prefix}{}", input.text),
    }
}

/// Turns texts into unit vectors.
pub trait Embedder: Send + Sync {
    /// What this embedder's vectors can be compared with.
    fn model_key(&self) -> &ModelKey;

    /// Output width.
    fn dims(&self) -> usize;

    /// The audit record.
    fn meta(&self) -> &ModelMeta;

    /// Tokens an input costs as embedded (prefix, budget cut and special
    /// tokens included).
    fn token_count(&self, input: &EmbedInput) -> usize;

    /// Embeds a batch; one L2-normalized vector of [`Embedder::dims`]
    /// components per input, in input order.
    ///
    /// # Errors
    ///
    /// Runtime failures.
    fn embed(&self, inputs: &[EmbedInput]) -> Result<Vec<Vec<f32>>>;

    /// Embeds documents.
    ///
    /// # Errors
    ///
    /// Runtime failures.
    fn embed_documents(&self, docs: &[&str]) -> Result<Vec<Vec<f32>>> {
        let inputs: Vec<EmbedInput> = docs.iter().map(|d| EmbedInput::document(*d)).collect();
        self.embed(&inputs)
    }

    /// Embeds one query.
    ///
    /// # Errors
    ///
    /// Runtime failures.
    fn embed_query(&self, query: &str) -> Result<Vec<f32>> {
        let mut v = self.embed(&[EmbedInput::query(query)])?;
        v.pop().ok_or_else(|| Error::Runtime("no vector returned for the query".into()))
    }
}

/// Scores documents against a query (second stage, RFC 0007 §7.5).
pub trait Reranker: Send + Sync {
    /// One score per document, higher is more relevant, in document order.
    ///
    /// # Errors
    ///
    /// Runtime failures.
    fn rerank(&self, query: &str, docs: &[&str]) -> Result<Vec<f32>>;
}

/// The reranker that is off: strictly decreasing scores in input order, so
/// any stable sort by score keeps the first-stage ranking.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoReranker;

impl Reranker for NoReranker {
    fn rerank(&self, _query: &str, docs: &[&str]) -> Result<Vec<f32>> {
        #[allow(clippy::cast_precision_loss)]
        let n = docs.len() as f32;
        #[allow(clippy::cast_precision_loss)]
        Ok((0..docs.len()).map(|i| 1.0 - i as f32 / n.max(1.0)).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_prefix_uses_the_instruction_override() {
        let p = preset("qwen3-0.6b-q8").unwrap();
        let mut o = EmbedOptions::default();
        assert_eq!(o.query_prefix(p), format!("Instruct: {}\nQuery:", presets::DEFAULT_CODE_INSTRUCTION));
        o.query_instruction = Some("Find it".into());
        assert_eq!(o.query_prefix(p), "Instruct: Find it\nQuery:");
        let q = EmbedInput::query("where is login");
        assert_eq!(model_input(&q, &o.query_prefix(p), ""), "Instruct: Find it\nQuery:where is login");
        assert_eq!(model_input(&EmbedInput::document("fn a()"), "x", ""), "fn a()");
    }

    #[test]
    fn output_dims_respects_the_matryoshka_range() {
        let p = preset("qwen3-0.6b-q8").unwrap();
        let mut o = EmbedOptions::default();
        assert_eq!(o.output_dims(p).unwrap(), 1024);
        o.dims = Some(256);
        assert_eq!(o.output_dims(p).unwrap(), 256);
        o.dims = Some(16);
        assert!(o.output_dims(p).is_err());
        o.dims = Some(2048);
        assert!(o.output_dims(p).is_err());
        o.dims = None;
        o.max_tokens = 1;
        assert!(o.output_dims(p).is_err());
    }

    #[test]
    fn no_reranker_keeps_the_order() {
        let s = NoReranker.rerank("q", &["a", "b", "c"]).unwrap();
        assert!(s.windows(2).all(|w| w[0] > w[1]));
        assert!(NoReranker.rerank("q", &[]).unwrap().is_empty());
    }
}
