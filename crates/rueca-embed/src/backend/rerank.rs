//! Qwen3-Reranker on llama.cpp (RFC 0007 §7.5).
//!
//! Qwen3-Reranker is a causal LM asked whether a document meets a query; its
//! score is `softmax(logit("yes"), logit("no"))[yes]` after the model card's
//! chat prompt. llama.cpp's `convert_hf_to_gguf.py` (class `Qwen3Model`,
//! `conversion/qwen.py` at the vendored commit) recognises the reranker,
//! keeps the `yes` and `no` rows of the LM head as a two-way classifier
//! (`cls.output.weight`, `[yes, no]`) and sets rank pooling; llama.cpp then
//! reads the last token of each sequence through that head and applies the
//! softmax in the graph (`build_pooling`, `LLAMA_POOLING_TYPE_RANK`), so a
//! pooled read is `[p(yes), p(no)]`.
//!
//! Per call, the shared prefix (system prompt, instruction, query and
//! `<Document>:`) is decoded once as sequence 0; each chunk of documents
//! then gets the prefix's KV cells (a sequence copy in the unified cache, no
//! recomputation) and decodes only its own tokens (` {document}` cut to the
//! budget, then the assistant suffix). Documents are chunked in input order,
//! so a call is deterministic on one build and backend.

use std::num::NonZeroU32;
use std::path::Path;
use std::sync::Mutex;

use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::context::params::{LlamaContextParams, LlamaPoolingType};
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::LlamaModel;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::token::LlamaToken;

use super::llama::{ALL_LAYERS, MAX_SEQS, backend, backend_kind, default_threads, runtime};
use crate::error::{Error, Result};
use crate::key::Backend;
use crate::presets::{Preset, Role};
use crate::{RerankOptions, Reranker};

/// The system turn and the opening of the user turn (model card).
const SYSTEM: &str = "<|im_start|>system\nJudge whether the Document meets the requirements based on the Query \
and the Instruct provided. Note that the answer can only be \"yes\" or \"no\".<|im_end|>\n<|im_start|>user\n";
/// The end of the user turn and an empty thinking block (model card).
const SUFFIX: &str = "<|im_end|>\n<|im_start|>assistant\n<think>\n\n</think>\n\n";
/// The sequence holding the shared prefix.
const PREFIX_SEQ: i32 = 0;

/// A Qwen3-Reranker GGUF file converted with a classifier head.
#[derive(Debug)]
pub struct LlamaReranker {
    model: LlamaModel,
    preset: &'static Preset,
    model_key: String,
    instruction: String,
    max_doc_tokens: usize,
    batch_tokens: usize,
    threads: usize,
    gpu: bool,
    system: Vec<LlamaToken>,
    suffix: Vec<LlamaToken>,
    lock: Mutex<()>,
}

impl LlamaReranker {
    /// Loads `weights` (already verified against `preset`).
    ///
    /// # Errors
    ///
    /// A preset that is not a reranker, a file without the two-way
    /// classifier head (a plain conversion), runtime failures.
    pub fn load(preset: &'static Preset, weights: &Path, opts: &RerankOptions) -> Result<Self> {
        if preset.role != Role::Reranker {
            return Err(Error::Invalid(format!("`{}` is not a reranker preset", preset.name)));
        }
        if opts.max_doc_tokens == 0 {
            return Err(Error::Invalid("the reranker's document budget must be at least one token".into()));
        }
        let backend = backend()?;
        let gpu = !opts.cpu && backend.supports_gpu_offload();
        let params = LlamaModelParams::default().with_n_gpu_layers(if gpu { ALL_LAYERS } else { 0 });
        let model = LlamaModel::load_from_file(backend, weights, &params).map_err(runtime("cannot load model"))?;
        if model.n_cls_out() != 2 {
            return Err(Error::Runtime(format!(
                "{} has no yes/no classifier head (n_cls_out = {}): convert Qwen3-Reranker with llama.cpp's \
                 convert_hf_to_gguf.py (docs/how-it-works.md §10, Reranker)",
                weights.display(),
                model.n_cls_out()
            )));
        }
        let system = model.vocab().tokenize(SYSTEM.as_bytes(), false, true);
        let suffix = model.vocab().tokenize(SUFFIX.as_bytes(), false, true);
        Ok(Self {
            model_key: opts.model_key(preset)?,
            instruction: opts.instruction(preset).to_string(),
            max_doc_tokens: opts.max_doc_tokens,
            batch_tokens: opts.batch_tokens.max(opts.max_doc_tokens + suffix.len()),
            threads: opts.threads.unwrap_or_else(default_threads).max(1),
            model,
            preset,
            gpu,
            system,
            suffix,
            lock: Mutex::new(()),
        })
    }

    /// The compute backend.
    #[must_use]
    pub fn backend(&self) -> Backend {
        backend_kind(self.gpu)
    }

    /// Plain text, special-token strings left as text.
    fn plain(&self, text: &str) -> Vec<LlamaToken> {
        self.model.vocab().tokenize(text.as_bytes(), false, false)
    }

    /// The shared prefix: system turn, instruction, query, `<Document>:`.
    /// Tokens end before the space that opens the document, where the
    /// tokenizer splits the whole prompt too.
    fn prefix(&self, query: &str) -> Vec<LlamaToken> {
        let mut out = self.system.clone();
        out.extend(self.plain(&format!("<Instruct>: {}\n<Query>: {query}\n<Document>:", self.instruction)));
        out
    }

    /// ` {document}` cut to the budget, then the assistant suffix.
    fn document(&self, doc: &str) -> Vec<LlamaToken> {
        let mut out = self.plain(&format!(" {doc}"));
        out.truncate(self.max_doc_tokens);
        out.extend_from_slice(&self.suffix);
        out
    }

    /// A context for `n_ctx` cells, batches of `n_batch` tokens and
    /// `n_seqs` sequences (llama.cpp needs `n_batch ≥ n_seqs`).
    fn context(&self, n_ctx: usize, n_batch: usize, n_seqs: usize) -> Result<LlamaContext<'_>> {
        let ctx = u32::try_from(n_ctx).map_err(runtime("context size"))?;
        let batch = u32::try_from(n_batch).map_err(runtime("batch size"))?;
        let seqs = u32::try_from(n_seqs).map_err(runtime("sequences"))?;
        let threads = i32::try_from(self.threads).unwrap_or(i32::MAX);
        let params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(ctx))
            .with_n_batch(batch)
            .with_n_ubatch(batch)
            .with_n_seq_max(seqs)
            .with_kv_unified(true)
            .with_embeddings(true)
            .with_pooling_type(LlamaPoolingType::Rank)
            .with_n_threads(threads)
            .with_n_threads_batch(threads)
            .with_offload_kqv(self.gpu)
            .with_op_offload(self.gpu);
        self.model.new_context(backend()?, params).map_err(runtime("cannot create context"))
    }

    /// Decodes one chunk of documents after the prefix and reads `p(yes)`
    /// of each, then drops their cells.
    fn run_chunk(
        &self,
        ctx: &mut LlamaContext<'_>,
        batch: &mut LlamaBatch<'_>,
        start: i32,
        docs: &[Vec<LlamaToken>],
    ) -> Result<Vec<f32>> {
        batch.clear();
        for (j, toks) in docs.iter().enumerate() {
            let seq = i32::try_from(j + 1).map_err(runtime("sequence id"))?;
            ctx.copy_kv_cache_seq(PREFIX_SEQ, seq, None, None).map_err(runtime("prefix copy"))?;
            for (i, &t) in toks.iter().enumerate() {
                let pos = start + i32::try_from(i).map_err(runtime("position"))?;
                batch.add(t, pos, &[seq], true).map_err(runtime("batch"))?;
            }
        }
        ctx.decode(batch).map_err(runtime("decode"))?;
        let mut out = Vec::with_capacity(docs.len());
        for j in 0..docs.len() {
            let seq = i32::try_from(j + 1).map_err(runtime("sequence id"))?;
            let probs = ctx.embeddings_seq_ith(seq).map_err(runtime("scores"))?;
            out.push(probs.first().copied().ok_or_else(|| Error::Runtime("empty classifier output".into()))?);
            let seq = u32::try_from(seq).map_err(runtime("sequence id"))?;
            ctx.clear_kv_cache_seq(Some(seq), None, None).map_err(runtime("cache"))?;
        }
        Ok(out)
    }
}

/// Consecutive ranges of documents whose tokens fit `budget` (one document
/// at least, at most `MAX_SEQS - 1` per range).
fn chunks(lens: &[usize], budget: usize) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    let (mut start, mut used) = (0, 0);
    for (i, &n) in lens.iter().enumerate() {
        if i > start && (used + n > budget || i - start >= MAX_SEQS - 1) {
            out.push(start..i);
            (start, used) = (i, 0);
        }
        used += n;
    }
    if start < lens.len() {
        out.push(start..lens.len());
    }
    out
}

impl Reranker for LlamaReranker {
    fn rerank(&self, query: &str, docs: &[&str]) -> Result<Vec<f32>> {
        if docs.is_empty() {
            return Ok(Vec::new());
        }
        let prefix = self.prefix(query);
        let toks: Vec<Vec<LlamaToken>> = docs.iter().map(|d| self.document(d)).collect();
        let lens: Vec<usize> = toks.iter().map(Vec::len).collect();
        let ranges = chunks(&lens, self.batch_tokens);
        let widest = ranges.iter().map(|r| lens[r.clone()].iter().sum::<usize>()).max().unwrap_or(0);
        let n_seqs = ranges.iter().map(ExactSizeIterator::len).max().unwrap_or(0) + 1;
        let n_batch = widest.max(prefix.len()).max(n_seqs);
        let _guard = self.lock.lock().map_err(|_| Error::Runtime("reranker lock poisoned".into()))?;
        let mut ctx = self.context(prefix.len() + widest, n_batch, n_seqs)?;
        let seqs = i32::try_from(n_seqs).map_err(runtime("sequences"))?;
        let mut batch = LlamaBatch::new(n_batch, seqs);
        batch.add_sequence(&prefix, PREFIX_SEQ, true).map_err(runtime("batch"))?;
        ctx.decode(&mut batch).map_err(runtime("decode"))?;
        let start = i32::try_from(prefix.len()).map_err(runtime("position"))?;
        let mut out = Vec::with_capacity(docs.len());
        for r in ranges {
            out.extend(self.run_chunk(&mut ctx, &mut batch, start, &toks[r])?);
        }
        Ok(out)
    }

    fn preset(&self) -> &str {
        self.preset.name
    }

    fn model_key(&self) -> &str {
        &self.model_key
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_fill_the_budget_in_order() {
        assert_eq!(chunks(&[10, 10, 10], 25), vec![0..2, 2..3]);
        assert_eq!(chunks(&[30, 5], 25), vec![0..1, 1..2]);
        assert_eq!(chunks(&[], 25), Vec::<std::ops::Range<usize>>::new());
        let many = vec![1; MAX_SEQS + 3];
        let c = chunks(&many, 10_000);
        assert_eq!(c[0], 0..MAX_SEQS - 1);
        assert_eq!(c.len(), 2);
    }
}
