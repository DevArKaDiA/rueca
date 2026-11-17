//! llama.cpp backend through `llama-cpp-2` (RFC 0007 §4).
//!
//! One model per process; each [`Embedder::embed`] call creates a context
//! (embeddings on, last-token pooling, unified KV cache, micro-batch equal
//! to the batch so every input fits one micro-batch), fills batches in
//! [`batch_order`] and returns the vectors in input order. Contexts are
//! created under a lock: llama.cpp may mutate the model while creating one.

use std::num::NonZeroU32;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::context::params::{LlamaContextParams, LlamaPoolingType};
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::LlamaModel;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::token::LlamaToken;
use llama_cpp_2::{LogOptions, send_logs_to_tracing};

use crate::error::{Error, Result};
use crate::key::{Backend, EmbedKey, ModelKey, ModelMeta, TEXT_BUILDER_VERSION, VectorEncoding, cpu_level};
use crate::presets::{Pooling, Preset};
use crate::vector::{batch_order, pack_batches, restore_order, truncate_normalized};
use crate::{EmbedInput, EmbedOptions, Embedder, model_input};

/// Runtime family recorded in [`ModelMeta`].
pub const RUNTIME: &str = "llama.cpp";
/// Binding crate version and the llama.cpp commit it vendors.
pub const RUNTIME_VERSION: &str = "llama-cpp-2 0.1.161 (llama.cpp 0.6.0, d81235049384)";

/// Sequences per batch (llama.cpp's `LLAMA_MAX_SEQ`).
const MAX_SEQS: usize = 256;
/// Layers offloaded on a GPU backend: all of them.
const ALL_LAYERS: u32 = 999;

static BACKEND: OnceLock<std::result::Result<LlamaBackend, String>> = OnceLock::new();

/// The process-wide llama.cpp backend, initialized once, logging through
/// `tracing` (visible with `-v`).
fn backend() -> Result<&'static LlamaBackend> {
    let b = BACKEND.get_or_init(|| {
        send_logs_to_tracing(LogOptions::default());
        LlamaBackend::init().map_err(|e| e.to_string())
    });
    b.as_ref().map_err(|e| Error::Runtime(e.clone()))
}

fn runtime<E: std::fmt::Display>(what: &str) -> impl FnOnce(E) -> Error + '_ {
    move |e| Error::Runtime(format!("{what}: {e}"))
}

/// A GGUF embedding model loaded with llama.cpp.
#[derive(Debug)]
pub struct LlamaEmbedder {
    model: LlamaModel,
    meta: ModelMeta,
    query_prefix: String,
    document_prefix: &'static str,
    bos: Option<LlamaToken>,
    eos: Option<LlamaToken>,
    gpu: bool,
    lock: Mutex<()>,
}

impl LlamaEmbedder {
    /// Loads `weights` (already verified against `preset`).
    ///
    /// # Errors
    ///
    /// Invalid options, a model that is not a pooled embedder of the
    /// preset's width, runtime failures.
    pub fn load(preset: &'static Preset, weights: &Path, opts: EmbedOptions) -> Result<Self> {
        let dims = opts.output_dims(preset)?;
        if preset.pooling != Pooling::Last {
            return Err(Error::Invalid(format!("`{}`: only last-token pooling is wired in phase 0", preset.name)));
        }
        let backend = backend()?;
        let gpu = !opts.cpu && backend.supports_gpu_offload();
        let layers = if gpu { ALL_LAYERS } else { 0 };
        let params = LlamaModelParams::default().with_n_gpu_layers(layers);
        let model = LlamaModel::load_from_file(backend, weights, &params).map_err(runtime("cannot load model"))?;
        let width = usize::try_from(model.n_embd()).unwrap_or(0);
        if width != preset.dims {
            return Err(Error::Runtime(format!("model width {width} differs from the preset's {}", preset.dims)));
        }
        let vocab = model.vocab();
        let bos = vocab.should_add_bos().then(|| vocab.bos());
        let eos = vocab.should_add_eos().then(|| vocab.eos());
        let sha = preset.weights()?.sha256;
        let meta = build_meta(preset, sha, &opts, dims, gpu);
        let query_prefix = opts.query_prefix(preset);
        Ok(Self {
            model,
            meta,
            query_prefix,
            document_prefix: preset.document_prefix,
            bos,
            eos,
            gpu,
            lock: Mutex::new(()),
        })
    }

    fn text_of(&self, input: &EmbedInput) -> String {
        model_input(input, &self.query_prefix, self.document_prefix)
    }

    /// Tokenizes without special tokens, cuts to the budget, then adds the
    /// model's BOS / EOS (Qwen3 appends `<|endoftext|>`, which its
    /// last-token pooling reads).
    fn tokens(&self, text: &str) -> Vec<LlamaToken> {
        let mut body = self.model.vocab().tokenize(text.as_bytes(), false, false);
        let specials = usize::from(self.bos.is_some()) + usize::from(self.eos.is_some());
        body.truncate(self.meta.token_budget.saturating_sub(specials));
        let mut out = Vec::with_capacity(body.len() + specials);
        out.extend(self.bos);
        out.extend(body);
        out.extend(self.eos);
        out
    }

    fn context(&self) -> Result<LlamaContext<'_>> {
        let n = u32::try_from(self.meta.batch_tokens).map_err(runtime("batch size"))?;
        let threads = i32::try_from(self.meta.threads).unwrap_or(i32::MAX);
        let params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(n))
            .with_n_batch(n)
            .with_n_ubatch(n)
            .with_n_seq_max(MAX_SEQS as u32)
            .with_kv_unified(true)
            .with_embeddings(true)
            .with_pooling_type(LlamaPoolingType::Last)
            .with_n_threads(threads)
            .with_n_threads_batch(threads)
            .with_offload_kqv(self.gpu)
            .with_op_offload(self.gpu);
        self.model.new_context(backend()?, params).map_err(runtime("cannot create context"))
    }

    /// Decodes one batch of sequences and reads one pooled vector each.
    fn run_batch(
        &self,
        ctx: &mut LlamaContext<'_>,
        batch: &mut LlamaBatch<'_>,
        seqs: &[&[LlamaToken]],
    ) -> Result<Vec<Vec<f32>>> {
        batch.clear();
        for (seq, toks) in seqs.iter().enumerate() {
            let id = i32::try_from(seq).map_err(runtime("sequence id"))?;
            batch.add_sequence(toks, id, true).map_err(runtime("batch"))?;
        }
        ctx.clear_kv_cache();
        ctx.decode(batch).map_err(runtime("decode"))?;
        (0..seqs.len())
            .map(|seq| {
                let id = i32::try_from(seq).map_err(runtime("sequence id"))?;
                let pooled = ctx.embeddings_seq_ith(id).map_err(runtime("embeddings"))?;
                Ok(truncate_normalized(pooled, self.meta.dims))
            })
            .collect()
    }
}

impl Embedder for LlamaEmbedder {
    fn model_key(&self) -> &ModelKey {
        &self.meta.model_key
    }

    fn dims(&self) -> usize {
        self.meta.dims
    }

    fn meta(&self) -> &ModelMeta {
        &self.meta
    }

    fn token_count(&self, input: &EmbedInput) -> usize {
        self.tokens(&self.text_of(input)).len()
    }

    fn embed(&self, inputs: &[EmbedInput]) -> Result<Vec<Vec<f32>>> {
        if inputs.is_empty() {
            return Ok(Vec::new());
        }
        let texts: Vec<String> = inputs.iter().map(|i| self.text_of(i)).collect();
        let tokens: Vec<Vec<LlamaToken>> = texts.iter().map(|t| self.tokens(t)).collect();
        let lens: Vec<usize> = tokens.iter().map(Vec::len).collect();
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        let order = batch_order(&refs, &lens);
        let sorted_lens: Vec<usize> = order.iter().map(|&i| lens[i]).collect();
        let _guard = self.lock.lock().map_err(|_| Error::Runtime("embedder lock poisoned".into()))?;
        let mut ctx = self.context()?;
        let mut batch = LlamaBatch::new(self.meta.batch_tokens, MAX_SEQS as i32);
        let mut out = Vec::with_capacity(inputs.len());
        for range in pack_batches(&sorted_lens, self.meta.batch_tokens, MAX_SEQS) {
            let seqs: Vec<&[LlamaToken]> = order[range].iter().map(|&i| tokens[i].as_slice()).collect();
            out.extend(self.run_batch(&mut ctx, &mut batch, &seqs)?);
        }
        Ok(restore_order(out, &order))
    }
}

fn build_meta(preset: &Preset, sha256: &str, opts: &EmbedOptions, dims: usize, gpu: bool) -> ModelMeta {
    let embed_key =
        EmbedKey::new(sha256, preset.pooling, preset.document_prefix, TEXT_BUILDER_VERSION, opts.max_tokens);
    let encoding = VectorEncoding::F32;
    let batch = opts.batch_tokens.max(opts.max_tokens);
    ModelMeta {
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
        text_builder_version: TEXT_BUILDER_VERSION,
        encoding,
        embed_key,
        model_key: ModelKey::new(&embed_key, dims, encoding),
        runtime: RUNTIME.to_string(),
        runtime_version: RUNTIME_VERSION.to_string(),
        backend: backend_kind(gpu),
        cpu_level: cpu_level().to_string(),
        batch_tokens: batch,
        ubatch_tokens: batch,
        threads: opts.threads.unwrap_or_else(default_threads).max(1),
    }
}

fn backend_kind(gpu: bool) -> Backend {
    match (gpu, cfg!(feature = "cuda")) {
        (false, _) => Backend::Cpu,
        (true, true) => Backend::Cuda,
        (true, false) => Backend::Metal,
    }
}

/// Physical performance cores: on Apple silicon the efficiency cores slow
/// a CPU run down (measured in phase 0: 10 threads 1,254 tokens/s, 14
/// threads 500 on an M4 Pro), elsewhere every physical core.
fn default_threads() -> usize {
    performance_cores().unwrap_or_else(num_cpus::get_physical)
}

#[cfg(target_os = "macos")]
fn performance_cores() -> Option<usize> {
    let mut value: libc::c_int = 0;
    let mut size = std::mem::size_of::<libc::c_int>();
    // SAFETY: the name is a NUL-terminated C string, `value` / `size`
    // describe a writable `c_int`, and no new value is set.
    let rc = unsafe {
        libc::sysctlbyname(
            c"hw.perflevel0.physicalcpu".as_ptr(),
            (&raw mut value).cast(),
            &raw mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    usize::try_from(value).ok().filter(|&n| rc == 0 && n > 0)
}

#[cfg(not(target_os = "macos"))]
fn performance_cores() -> Option<usize> {
    None
}
