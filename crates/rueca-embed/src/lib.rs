//! Embedding runtime for Rueca (RFC 0007 §4): presets, weights, keys and
//! the runtime behind one trait.
//!
//! # Design
//!
//! - [`Embedder`] turns [`EmbedInput`]s (documents and queries) into unit
//!   vectors, batched. `rueca-segment` stays free of model dependencies: an
//!   embedder is injected where vectors are needed (`rueca embed` today).
//!   [`hashing::HashingEmbedder`] is a deterministic test embedder with no
//!   weights.
//! - [`presets`] is the manifest compiled into the binary: per preset the
//!   weights file (URL pinned to an upstream revision, sha256, size),
//!   quantization, dimensions, pooling and query formatting. Weights are
//!   never in the binary.
//! - [`ModelCache`] keeps weights under `<store>/models/<preset>/` (or
//!   `$RUECA_MODEL_DIR`), downloads them with `ModelCache::pull` (feature
//!   `embeddings`) only when asked (`rueca models pull`), verifies their
//!   sha256 once and refuses a mismatch. Nothing downloads implicitly.
//! - The llama.cpp backend (`backend::llama`, feature `embeddings`, default
//!   on; Metal on Apple silicon, `cuda` as an opt-in feature) loads a GGUF
//!   file, tokenizes with the tokenizer inside it, pools the last token and
//!   L2-normalizes; with `dims` below the model width it truncates
//!   (Matryoshka) and re-normalizes.
//! - Queries get the preset's instruction, Qwen3's `Instruct: {task}\nQuery:{query}`
//!   ([`presets::DEFAULT_CODE_INSTRUCTION`] by default); documents get none.
//!   Documents are cut at the keyed token budget ([`EmbedOptions::max_tokens`],
//!   512 by default); queries at their own budget, 512 capped by the
//!   preset's context, whatever the documents' budget.
//! - [`RetrievalConfig`] reads `[retrieval]` of `<store>/rueca.toml`.
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
//! instruction and the query budget are not keyed either, since no query
//! vector is stored.

pub mod backend;
pub mod cache;
pub mod config;
#[cfg(feature = "embeddings")]
mod download;
mod error;
pub mod hashing;
pub mod key;
pub mod presets;
pub mod vector;

pub use cache::{CacheState, MODEL_DIR_ENV, ModelCache};
pub use config::RetrievalConfig;
pub use error::{Error, Result};
pub use key::{Backend, EmbedKey, ModelKey, ModelMeta, TEXT_BUILDER_VERSION, VectorEncoding, VectorKey};
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
/// Default tokens per batch: 512 holds peak memory near 1.0 GB with the
/// default preset (2.2 GB at 2048) for about 4% less throughput (measured
/// in phase 0, `docs/how-it-works.md` §10).
pub const DEFAULT_BATCH_TOKENS: usize = 512;

/// How an embedder is loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbedOptions {
    /// Output width (Matryoshka); `None` keeps the model width.
    pub dims: Option<usize>,
    /// Documents are cut to this many tokens, special tokens included
    /// (keyed: the documents' token budget).
    pub max_tokens: usize,
    /// Queries are cut to this many tokens; `None` is
    /// [`DEFAULT_MAX_TOKENS`] capped by the preset's context, whatever the
    /// documents' budget. Not keyed: no query vector is stored.
    pub query_max_tokens: Option<usize>,
    /// Tokens per batch (raised to `max_tokens` when smaller).
    pub batch_tokens: usize,
    /// Worker threads; `None` uses the physical cores.
    pub threads: Option<usize>,
    /// Run on the CPU even when a GPU backend is compiled in.
    pub cpu: bool,
    /// Query instruction for `Instruct` presets; `None` uses the preset's.
    pub query_instruction: Option<String>,
    /// Version of the text builder that produced the documents, keyed in
    /// the [`EmbedKey`]: [`TEXT_BUILDER_VERSION`] (0) for caller text, the
    /// indexer's canonical text version for symbol vectors.
    pub text_builder_version: u32,
}

impl Default for EmbedOptions {
    fn default() -> Self {
        Self {
            dims: None,
            max_tokens: DEFAULT_MAX_TOKENS,
            query_max_tokens: None,
            batch_tokens: DEFAULT_BATCH_TOKENS,
            threads: None,
            cpu: false,
            query_instruction: None,
            text_builder_version: TEXT_BUILDER_VERSION,
        }
    }
}

impl EmbedOptions {
    /// Checks the options against a preset and returns the output width.
    ///
    /// # Errors
    ///
    /// [`Error::Invalid`] for a width outside the preset's Matryoshka range
    /// or a token budget outside `2..=context`.
    pub fn output_dims(&self, preset: &Preset) -> Result<usize> {
        if self.max_tokens < 2 || self.max_tokens > preset.context {
            return Err(Error::Invalid(format!(
                "--max-tokens must be within 2..={} for `{}`",
                preset.context, preset.name
            )));
        }
        if self.query_max_tokens.is_some_and(|q| q < 2 || q > preset.context) {
            return Err(Error::Invalid(format!(
                "the query token budget must be within 2..={} for `{}`",
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

    /// The query token budget for a model of `context` tokens
    /// ([`EmbedOptions::query_max_tokens`]).
    #[must_use]
    pub fn query_budget(&self, context: usize) -> usize {
        self.query_max_tokens.unwrap_or(DEFAULT_MAX_TOKENS.min(context))
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

/// Where an embedder runs, for its audit record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeInfo<'a> {
    /// Runtime family (`llama.cpp`).
    pub runtime: &'a str,
    /// Runtime version.
    pub version: &'a str,
    /// Compute backend.
    pub backend: Backend,
    /// Worker threads.
    pub threads: usize,
}

/// The audit record of `preset` embedded with `opts` on `rt`, computed
/// without loading the weights: keys come from the manifest's sha256.
///
/// # Errors
///
/// Invalid options ([`EmbedOptions::output_dims`]).
pub fn model_meta(preset: &Preset, opts: &EmbedOptions, rt: RuntimeInfo<'_>) -> Result<ModelMeta> {
    let dims = opts.output_dims(preset)?;
    let sha256 = preset.weights.sha256;
    let embed_key =
        EmbedKey::new(sha256, preset.pooling, preset.document_prefix, opts.text_builder_version, opts.max_tokens);
    let encoding = VectorEncoding::F32;
    let query_budget = opts.query_budget(preset.context);
    let batch = opts.batch_tokens.max(opts.max_tokens).max(query_budget);
    Ok(ModelMeta {
        preset: preset.name.to_string(),
        model: format!("{}@{}", preset.model, preset.revision),
        model_sha256: sha256.to_string(),
        quantization: preset.quantization.to_string(),
        dims,
        max_dims: preset.dims,
        pooling: preset.pooling,
        query_prefix: opts.query_prefix(preset),
        document_prefix: preset.document_prefix.to_string(),
        token_budget: opts.max_tokens,
        query_token_budget: query_budget,
        text_builder_version: opts.text_builder_version,
        encoding,
        embed_key,
        model_key: ModelKey::new(&embed_key, dims, encoding),
        runtime: rt.runtime.to_string(),
        runtime_version: rt.version.to_string(),
        backend: rt.backend,
        cpu_level: key::cpu_level().to_string(),
        batch_tokens: batch,
        ubatch_tokens: batch,
        threads: rt.threads.max(1),
    })
}

impl ModelMeta {
    /// The token budget of an input of `kind`: [`ModelMeta::token_budget`]
    /// for documents, [`ModelMeta::query_token_budget`] for queries.
    #[must_use]
    pub fn input_budget(&self, kind: InputKind) -> usize {
        match kind {
            InputKind::Document => self.token_budget,
            InputKind::Query => self.query_token_budget,
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
    fn queries_keep_their_budget_whatever_the_documents_budget() {
        let rt = RuntimeInfo { runtime: "x", version: "", backend: Backend::Cpu, threads: 1 };
        let p = preset(DEFAULT_PRESET).unwrap();
        let full = model_meta(p, &EmbedOptions::default(), rt).unwrap();
        let short = model_meta(p, &EmbedOptions { max_tokens: 128, ..EmbedOptions::default() }, rt).unwrap();
        assert_ne!(full.embed_key, short.embed_key);
        assert_eq!((full.query_token_budget, short.query_token_budget, short.token_budget), (512, 512, 128));
        assert!(short.ubatch_tokens >= 512, "a 512-token query fits one micro-batch");
        assert_eq!((short.input_budget(InputKind::Document), short.input_budget(InputKind::Query)), (128, 512));
        let long = EmbedOptions { max_tokens: 1024, ..EmbedOptions::default() };
        assert_eq!(model_meta(p, &long, rt).unwrap().query_token_budget, 512);
        let own = EmbedOptions { query_max_tokens: Some(64), ..EmbedOptions::default() };
        assert_eq!(model_meta(p, &own, rt).unwrap().query_token_budget, 64);
        let bad = EmbedOptions { query_max_tokens: Some(1), ..EmbedOptions::default() };
        assert!(bad.output_dims(p).is_err());
    }
}
