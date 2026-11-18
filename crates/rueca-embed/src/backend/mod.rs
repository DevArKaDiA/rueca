//! Runtimes behind [`crate::Embedder`].
//!
//! - `llama`: llama.cpp through `llama-cpp-2` (feature `embeddings`), the
//!   primary runtime (RFC 0007 §4.2).
//! - `rerank`: Qwen3-Reranker on the same runtime (RFC 0007 §7.5).
//!
//! The `fastembed` (candle) fallback was measured in phase 0 and not kept;
//! see `docs/how-it-works.md`, "Embeddings".

#[cfg(feature = "embeddings")]
pub mod llama;
#[cfg(feature = "embeddings")]
pub mod rerank;

use std::path::Path;

use crate::error::Result;
use crate::{EmbedOptions, Embedder, ModelCache, ModelMeta, Preset, RerankOptions, Reranker};

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

/// Loads the preset's reranker from the cache with the default runtime.
///
/// # Errors
///
/// Missing or mismatched weights (the message names `rueca models pull` and
/// `RUECA_MODEL_DIR`; an unpublished conversion's preset note names `rueca
/// models import`), a file without a classifier head, runtime failures, or
/// [`crate::Error::NoRuntime`] in a build without the `embeddings` feature.
pub fn load_reranker(cache: &ModelCache, preset: &'static Preset, opts: &RerankOptions) -> Result<Box<dyn Reranker>> {
    if preset.role != crate::Role::Reranker {
        return Err(crate::Error::Invalid(format!("`{}` is not a reranker preset (`rueca models list`)", preset.name)));
    }
    let weights = cache.resolve(preset)?;
    load_reranker_file(preset, &weights, opts)
}

#[cfg(feature = "embeddings")]
fn load_reranker_file(preset: &'static Preset, weights: &Path, opts: &RerankOptions) -> Result<Box<dyn Reranker>> {
    Ok(Box::new(rerank::LlamaReranker::load(preset, weights, opts)?))
}

#[cfg(not(feature = "embeddings"))]
fn load_reranker_file(_preset: &'static Preset, _weights: &Path, _opts: &RerankOptions) -> Result<Box<dyn Reranker>> {
    Err(crate::Error::NoRuntime)
}

/// Whether this build runs embedders on a GPU when one is present: CUDA
/// (`cuda`), or Metal on Apple silicon (`embeddings`). The service profile's
/// defaults depend on it ([`crate::config::Profile`]).
#[must_use]
pub fn gpu_compiled() -> Option<crate::Backend> {
    if cfg!(feature = "cuda") {
        Some(crate::Backend::Cuda)
    } else if cfg!(all(feature = "embeddings", target_os = "macos", target_arch = "aarch64")) {
        Some(crate::Backend::Metal)
    } else {
        None
    }
}

/// The embedder of any preset name: a manifest preset from the cache
/// ([`load`]) or a `remote:` one (its API key from the environment).
///
/// # Errors
///
/// As [`load`]; for a remote preset, a missing API key, an unknown
/// provider or [`crate::Error::NoRemote`].
pub fn load_preset(cache: &ModelCache, name: &str, opts: &EmbedOptions) -> Result<Box<dyn Embedder>> {
    match remote_spec(name)? {
        Some(spec) => load_remote(spec, opts),
        None => load(cache, crate::preset(name)?, opts),
    }
}

/// The audit record of any preset name, without loading anything or
/// calling a provider ([`describe`]).
///
/// # Errors
///
/// As [`describe`]; an invalid remote name.
pub fn describe_preset(name: &str, opts: &EmbedOptions) -> Result<ModelMeta> {
    match remote_spec(name)? {
        Some(spec) => describe_remote(&spec, opts),
        None => describe(crate::preset(name)?, opts),
    }
}

/// Whether the preset's model is at hand: the weights of a local preset,
/// the API key of a remote one.
///
/// # Errors
///
/// Missing or mismatched weights; a missing API key.
pub fn check_preset(cache: &ModelCache, name: &str) -> Result<()> {
    match remote_spec(name)? {
        Some(spec) => check_remote(&spec),
        None => cache.resolve(crate::preset(name)?).map(|_| ()),
    }
}

/// The reranker of any preset name ([`load_reranker`], or a `remote:`
/// one).
///
/// # Errors
///
/// As [`load_reranker`]; for a remote preset, a missing API key or a
/// provider without rerank.
pub fn load_reranker_preset(cache: &ModelCache, name: &str, opts: &RerankOptions) -> Result<Box<dyn Reranker>> {
    match remote_spec(name)? {
        Some(spec) => load_remote_reranker(spec),
        None => load_reranker(cache, crate::preset(name)?, opts),
    }
}

/// The parsed remote name, `None` for a local preset name.
#[cfg(feature = "remote")]
fn remote_spec(name: &str) -> Result<Option<crate::remote::RemoteSpec>> {
    crate::remote::RemoteSpec::parse(name)
}

#[cfg(not(feature = "remote"))]
fn remote_spec(name: &str) -> Result<Option<std::convert::Infallible>> {
    if name.starts_with("remote:") { Err(crate::Error::NoRemote) } else { Ok(None) }
}

#[cfg(feature = "remote")]
fn load_remote(spec: crate::remote::RemoteSpec, opts: &EmbedOptions) -> Result<Box<dyn Embedder>> {
    let endpoint = crate::remote::Endpoint::from_env(spec.provider)?;
    Ok(Box::new(crate::remote::RemoteEmbedder::new(spec, endpoint, opts)?))
}

#[cfg(not(feature = "remote"))]
fn load_remote(spec: std::convert::Infallible, _opts: &EmbedOptions) -> Result<Box<dyn Embedder>> {
    match spec {}
}

#[cfg(feature = "remote")]
fn describe_remote(spec: &crate::remote::RemoteSpec, opts: &EmbedOptions) -> Result<ModelMeta> {
    crate::remote::describe(spec, opts)
}

#[cfg(not(feature = "remote"))]
fn describe_remote(spec: &std::convert::Infallible, _opts: &EmbedOptions) -> Result<ModelMeta> {
    match *spec {}
}

#[cfg(feature = "remote")]
fn check_remote(spec: &crate::remote::RemoteSpec) -> Result<()> {
    crate::remote::Endpoint::from_env(spec.provider).map(|_| ())
}

#[cfg(not(feature = "remote"))]
fn check_remote(spec: &std::convert::Infallible) -> Result<()> {
    match *spec {}
}

#[cfg(feature = "remote")]
fn load_remote_reranker(spec: crate::remote::RemoteSpec) -> Result<Box<dyn Reranker>> {
    let endpoint = crate::remote::Endpoint::from_env(spec.provider)?;
    Ok(Box::new(crate::remote::RemoteReranker::new(spec, endpoint)?))
}

#[cfg(not(feature = "remote"))]
fn load_remote_reranker(spec: std::convert::Infallible) -> Result<Box<dyn Reranker>> {
    match spec {}
}
