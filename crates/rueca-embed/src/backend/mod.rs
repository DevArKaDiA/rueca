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
use crate::{EmbedOptions, Embedder, ModelCache, Preset};

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
