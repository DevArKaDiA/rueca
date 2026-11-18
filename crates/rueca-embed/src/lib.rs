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
//!   file, tokenizes with the tokenizer inside it, pools (the preset's
//!   pooling) and L2-normalizes; with `dims` below the model width it truncates
//!   (Matryoshka) and re-normalizes.
//! - Queries get the preset's instruction, Qwen3's `Instruct: {task}\nQuery:{query}`
//!   ([`presets::DEFAULT_CODE_INSTRUCTION`] by default), or an encoder's
//!   fixed prefix (CodeRankEmbed); documents get none. Pooling follows the
//!   preset: last token (Qwen3), `[CLS]` (BERT-family encoders) or mean.
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
//! # Remote presets
//!
//! `remote:<provider>/<model>` names an embedder or reranker behind a
//! provider's API (`remote`, feature `remote`, part of `embeddings`):
//! Voyage, OpenAI and Gemini embeddings, Voyage and Cohere rerank. The
//! `backend::*_preset` functions resolve any preset name, local or remote.
//!
//! # Rerankers
//!
//! [`Reranker`] scores documents against a query (RFC 0007 §7.5);
//! [`NoReranker`] keeps the first-stage order. The llama.cpp backend
//! (`backend::load_reranker`) runs Qwen3-Reranker converted by llama.cpp's
//! `convert_hf_to_gguf.py`, which keeps the `yes` / `no` rows of the LM head
//! as a two-way classifier read through rank pooling: the score is
//! `softmax(yes, no)[yes]` after the model card's chat prompt, the shared
//! prefix (system prompt, instruction, query) decoded once per call
//! ([`RerankOptions`]).

pub mod backend;
pub mod cache;
pub mod config;
#[cfg(feature = "embeddings")]
mod download;
mod error;
pub mod hashing;
#[cfg(feature = "remote")]
mod http;
pub mod key;
pub mod presets;
#[cfg(feature = "remote")]
pub mod remote;
pub mod vector;

pub use cache::{CacheState, MODEL_DIR_ENV, ModelCache};
pub use config::{Profile, RetrievalConfig};
pub use error::{Error, Result};
pub use key::{Backend, EmbedKey, ModelKey, ModelMeta, TEXT_BUILDER_VERSION, VectorEncoding, VectorKey};
pub use presets::{
    DEFAULT_PRESET, DEFAULT_RERANK_PRESET, PRESETS, Preset, PresetStatus, Role, is_remote_preset, preset,
};

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
    /// Inputs are cut to this many tokens, special tokens included.
    pub max_tokens: usize,
    /// Tokens per batch (raised to `max_tokens` when smaller).
    pub batch_tokens: usize,
    /// Worker threads; `None` uses the physical cores.
    pub threads: Option<usize>,
    /// Run on the CPU even when a GPU backend is compiled in.
    pub cpu: bool,
    /// Query instruction for `Instruct` presets; for `Prefix` presets
    /// (encoders) the whole query prefix, used verbatim. `None` uses the
    /// preset's.
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

    /// The query prefix in effect for a preset: Qwen3's `Instruct:` template
    /// around the instruction, or an encoder's fixed prefix (CodeRankEmbed's
    /// `Represent this query for searching relevant code: `), which
    /// [`EmbedOptions::query_instruction`] replaces verbatim when set.
    #[must_use]
    pub fn query_prefix(&self, preset: &Preset) -> String {
        match preset.query {
            QueryStyle::Instruct { instruction } => {
                let task = self.query_instruction.as_deref().unwrap_or(instruction);
                format!("Instruct: {task}\nQuery:")
            }
            QueryStyle::Prefix { prefix } => self.query_instruction.as_deref().unwrap_or(prefix).to_string(),
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
/// Invalid options ([`EmbedOptions::output_dims`]) or a preset without
/// weights.
pub fn model_meta(preset: &Preset, opts: &EmbedOptions, rt: RuntimeInfo<'_>) -> Result<ModelMeta> {
    if preset.role != Role::Embedder {
        return Err(Error::Invalid(format!(
            "`{}` is a {} preset, not an embedder (`rueca models list`)",
            preset.name,
            preset.role.as_str()
        )));
    }
    let dims = opts.output_dims(preset)?;
    let sha256 = preset.weights()?.sha256;
    let embed_key =
        EmbedKey::new(sha256, preset.pooling, preset.document_prefix, opts.text_builder_version, opts.max_tokens);
    let encoding = VectorEncoding::F32;
    let batch = opts.batch_tokens.max(opts.max_tokens);
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

/// Default tokens of a reranked document (RFC 0007 §7.5: the canonical
/// text cut to 256 tokens).
pub const DEFAULT_RERANK_DOC_TOKENS: usize = 256;
/// Default tokens per reranker batch: documents are decoded in chunks of
/// about this many tokens after the shared prefix.
pub const DEFAULT_RERANK_BATCH_TOKENS: usize = 2048;

/// How a reranker is loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RerankOptions {
    /// Documents are cut to this many tokens.
    pub max_doc_tokens: usize,
    /// Document tokens per decode call (raised to fit one document).
    pub batch_tokens: usize,
    /// Worker threads; `None` uses the physical cores.
    pub threads: Option<usize>,
    /// Run on the CPU even when a GPU backend is compiled in.
    pub cpu: bool,
    /// The prompt's instruction; `None` uses the preset's.
    pub instruction: Option<String>,
}

impl Default for RerankOptions {
    fn default() -> Self {
        Self {
            max_doc_tokens: DEFAULT_RERANK_DOC_TOKENS,
            batch_tokens: DEFAULT_RERANK_BATCH_TOKENS,
            threads: None,
            cpu: false,
            instruction: None,
        }
    }
}

impl RerankOptions {
    /// The instruction in effect for a preset.
    #[must_use]
    pub fn instruction<'a>(&'a self, preset: &'a Preset) -> &'a str {
        match (&self.instruction, preset.query) {
            (Some(i), _) => i,
            (None, QueryStyle::Instruct { instruction }) => instruction,
            (None, QueryStyle::Prefix { prefix }) => prefix,
        }
    }

    /// What a reranker's scores can be compared with: BLAKE3 (hex) of the
    /// weights' sha256, the prompt format, the instruction and the document
    /// budget.
    ///
    /// # Errors
    ///
    /// A preset without weights.
    pub fn model_key(&self, preset: &Preset) -> Result<String> {
        let sha256 = preset.weights()?.sha256;
        let text =
            format!("{sha256}\n{}\n{}\n{}", RERANK_PROMPT_VERSION, self.instruction(preset), self.max_doc_tokens);
        Ok(blake3::hash(text.as_bytes()).to_hex().to_string())
    }
}

/// Version of the reranker prompt format (Qwen3-Reranker's chat prompt),
/// hashed into the reranker's model key.
pub const RERANK_PROMPT_VERSION: u32 = 1;

/// Scores documents against a query (second stage, RFC 0007 §7.5).
pub trait Reranker: Send + Sync {
    /// One score per document, higher is more relevant, in document order.
    ///
    /// # Errors
    ///
    /// Runtime failures.
    fn rerank(&self, query: &str, docs: &[&str]) -> Result<Vec<f32>>;

    /// The preset (`none` for [`NoReranker`]).
    fn preset(&self) -> &str {
        "none"
    }

    /// What the scores can be compared with ([`RerankOptions::model_key`]).
    fn model_key(&self) -> &str {
        ""
    }
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
    fn query_prefix_of_an_encoder_is_the_fixed_prefix() {
        let p = preset("coderank-137m").unwrap();
        let mut o = EmbedOptions::default();
        assert_eq!(o.query_prefix(p), presets::CODERANK_QUERY_PREFIX);
        let q = EmbedInput::query("where is login");
        assert_eq!(
            model_input(&q, &o.query_prefix(p), p.document_prefix),
            "Represent this query for searching relevant code: where is login"
        );
        assert_eq!(model_input(&EmbedInput::document("fn a()"), &o.query_prefix(p), p.document_prefix), "fn a()");
        o.query_instruction = Some("Find: ".into());
        assert_eq!(o.query_prefix(p), "Find: ");
        assert!(!o.query_prefix(p).contains("Instruct"));
        let meta = model_meta(
            p,
            &EmbedOptions::default(),
            RuntimeInfo { runtime: "x", version: "", backend: Backend::Cpu, threads: 1 },
        )
        .unwrap();
        assert_eq!((meta.dims, meta.max_dims, meta.pooling), (768, 768, presets::Pooling::Cls));
        assert_eq!(meta.query_prefix, presets::CODERANK_QUERY_PREFIX);
        let long = EmbedOptions { max_tokens: 8192, ..EmbedOptions::default() };
        assert!(long.output_dims(p).is_ok());
        let longer = EmbedOptions { max_tokens: 8193, ..EmbedOptions::default() };
        assert!(longer.output_dims(p).is_err());
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
    fn reranker_presets_are_not_embedders() {
        let p = preset(DEFAULT_RERANK_PRESET).unwrap();
        let rt = RuntimeInfo { runtime: "x", version: "", backend: Backend::Cpu, threads: 1 };
        let err = model_meta(p, &EmbedOptions::default(), rt).unwrap_err().to_string();
        assert!(err.contains("reranker preset"), "{err}");
        let o = RerankOptions::default();
        assert_eq!(o.instruction(p), presets::RERANK_INSTRUCTION);
        let k = o.model_key(p).unwrap();
        assert_eq!(k.len(), 64);
        let other = RerankOptions { instruction: Some("x".into()), ..RerankOptions::default() };
        assert_ne!(other.model_key(p).unwrap(), k);
    }

    #[test]
    fn no_reranker_keeps_the_order() {
        let s = NoReranker.rerank("q", &["a", "b", "c"]).unwrap();
        assert!(s.windows(2).all(|w| w[0] > w[1]));
        assert!(NoReranker.rerank("q", &[]).unwrap().is_empty());
    }
}
