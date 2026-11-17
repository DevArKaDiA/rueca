//! Runtimes behind [`crate::Embedder`].
//!
//! - `llama`: llama.cpp through `llama-cpp-2` (feature `embeddings`), the
//!   primary runtime (RFC 0007 §4.2).
//!
//! The `fastembed` (candle) fallback was measured in phase 0 and not kept;
//! see `docs/how-it-works.md`, "Embeddings".

#[cfg(feature = "embeddings")]
pub mod llama;

use std::path::Path;

use crate::error::Result;
use crate::{EmbedOptions, Embedder, ModelCache, ModelMeta, Preset};

/// Loads the preset's embedder from the cache with the default runtime.
///
/// # Errors
///
/// Missing or mismatched weights (the message names `rueca models pull`),
/// invalid options, runtime failures, or [`crate::Error::NoRuntime`] in a
/// build without the `embeddings` feature.
pub fn load(cache: &ModelCache, preset: &'static Preset, opts: &EmbedOptions) -> Result<Box<dyn Embedder>> {
    let weights = cache.resolve(preset)?;
    load_file(preset, &weights, opts)
}

#[cfg(feature = "embeddings")]
fn load_file(preset: &'static Preset, weights: &Path, opts: &EmbedOptions) -> Result<Box<dyn Embedder>> {
    Ok(Box::new(llama::LlamaEmbedder::load(preset, weights, opts.clone())?))
}

#[cfg(not(feature = "embeddings"))]
fn load_file(_preset: &'static Preset, _weights: &Path, _opts: &EmbedOptions) -> Result<Box<dyn Embedder>> {
    Err(crate::Error::NoRuntime)
}

/// The audit record (keys, dimensions, backend) of `preset` with `opts`,
/// without loading or even locating the weights: `rueca embed --info` and
/// the indexer's vector-cache lookup use it, so a fully cached run never
/// loads a model.
///
/// # Errors
///
/// Invalid options or a preset without weights.
pub fn describe(preset: &'static Preset, opts: &EmbedOptions) -> Result<ModelMeta> {
    describe_impl(preset, opts)
}

#[cfg(feature = "embeddings")]
fn describe_impl(preset: &'static Preset, opts: &EmbedOptions) -> Result<ModelMeta> {
    llama::describe(preset, opts)
}

#[cfg(not(feature = "embeddings"))]
fn describe_impl(preset: &'static Preset, opts: &EmbedOptions) -> Result<ModelMeta> {
    let threads = opts.threads.unwrap_or(1);
    crate::model_meta(
        preset,
        opts,
        crate::RuntimeInfo { runtime: "none", version: "", backend: crate::Backend::Cpu, threads },
    )
}
