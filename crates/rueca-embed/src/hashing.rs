//! A deterministic bag-of-words embedder for tests: no weights, no runtime.
//!
//! Each lower-cased alphanumeric word (and each part of a `snake_case` or
//! `camelCase` identifier) adds 1 to a dimension chosen by BLAKE3 of the
//! word, and the vector is L2-normalized; a text without words gets a fixed
//! unit vector. Texts that share words have a positive cosine, texts that
//! share none are orthogonal up to collisions. Optional synonyms map query
//! words onto document words, so a test can model a query whose words are
//! not the code's (the case semantic search exists for).
//!
//! Its keys use a fake model sha256 (`hashing-<dims>`), so its vectors are
//! never compared with a real model's.

use std::collections::HashMap;

use crate::key::{Backend, ModelKey, ModelMeta};
use crate::presets::Pooling;
use crate::vector::l2_normalize;
use crate::{EmbedInput, Embedder, Error, InputKind, Result};

/// Preset name of [`HashingEmbedder`].
pub const HASHING_PRESET: &str = "hashing";

/// The test embedder.
#[derive(Debug, Clone)]
pub struct HashingEmbedder {
    meta: ModelMeta,
    synonyms: HashMap<String, String>,
}

impl HashingEmbedder {
    /// An embedder of `dims` dimensions whose keys use `text_builder_version`
    /// and `token_budget`, like a preset loaded with those options.
    #[must_use]
    pub fn new(dims: usize, text_builder_version: u32, token_budget: usize) -> Self {
        let sha = format!("hashing-{dims}");
        let embed_key = crate::EmbedKey::new(&sha, Pooling::Mean, "", text_builder_version, token_budget);
        let encoding = crate::VectorEncoding::F32;
        let meta = ModelMeta {
            preset: HASHING_PRESET.to_string(),
            model: "rueca-embed hashing".to_string(),
            model_sha256: sha,
            quantization: "none".to_string(),
            dims,
            max_dims: dims,
            pooling: Pooling::Mean,
            query_prefix: String::new(),
            document_prefix: String::new(),
            token_budget,
            text_builder_version,
            encoding,
            embed_key,
            model_key: ModelKey::new(&embed_key, dims, encoding),
            runtime: "hashing".to_string(),
            runtime_version: "1".to_string(),
            backend: Backend::Cpu,
            cpu_level: crate::key::cpu_level().to_string(),
            batch_tokens: token_budget,
            ubatch_tokens: token_budget,
            threads: 1,
        };
        Self { meta, synonyms: HashMap::new() }
    }

    /// Query words in `pairs` count as the paired document word.
    #[must_use]
    pub fn with_synonyms(mut self, pairs: &[(&str, &str)]) -> Self {
        self.synonyms.extend(pairs.iter().map(|(a, b)| ((*a).to_string(), (*b).to_string())));
        self
    }

    fn vector(&self, input: &EmbedInput) -> Vec<f32> {
        let dims = self.meta.dims;
        let mut v = vec![0.0_f32; dims];
        let mut any = false;
        for word in words(&input.text) {
            let word = match input.kind {
                InputKind::Query => self.synonyms.get(&word).cloned().unwrap_or(word),
                InputKind::Document => word,
            };
            let h = blake3::hash(word.as_bytes());
            let b = h.as_bytes();
            let slot = u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]);
            #[allow(clippy::cast_possible_truncation)]
            let i = (slot % dims as u64) as usize;
            v[i] += 1.0;
            any = true;
        }
        if !any {
            v[0] = 1.0;
        }
        l2_normalize(&mut v);
        v
    }
}

/// Lower-cased words: alphanumeric runs, split at `_` and at lower → upper
/// case changes.
fn words(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for run in text.split(|c: char| !c.is_alphanumeric()) {
        let mut cur = String::new();
        let mut prev_lower = false;
        for c in run.chars() {
            if c.is_uppercase() && prev_lower && !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            prev_lower = c.is_lowercase() || c.is_ascii_digit();
            cur.extend(c.to_lowercase());
        }
        if !cur.is_empty() {
            out.push(cur);
        }
    }
    out
}

impl Embedder for HashingEmbedder {
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
        words(&input.text).len().max(1)
    }

    fn embed(&self, inputs: &[EmbedInput]) -> Result<Vec<Vec<f32>>> {
        if self.meta.dims == 0 {
            return Err(Error::Invalid("hashing embedder with 0 dimensions".into()));
        }
        Ok(inputs.iter().map(|i| self.vector(i)).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vector::dot;

    #[test]
    fn shared_words_score_higher() {
        let e = HashingEmbedder::new(256, 1, 512).with_synonyms(&[("configuration", "config")]);
        let v = e
            .embed(&[
                EmbedInput::document("fn parse_config(path)"),
                EmbedInput::document("fn draw_widget()"),
                EmbedInput::query("where is the configuration parsed"),
            ])
            .unwrap();
        assert!(dot(&v[2], &v[0]) > dot(&v[2], &v[1]));
        assert!((dot(&v[0], &v[0]) - 1.0).abs() < 1e-5);
        assert_eq!(words("parseConfig HTTPServer x_y"), vec!["parse", "config", "httpserver", "x", "y"]);
    }

    #[test]
    fn keys_follow_the_options() {
        let a = HashingEmbedder::new(64, 1, 512);
        assert_eq!(a.meta().embed_key, HashingEmbedder::new(64, 1, 512).meta().embed_key);
        assert_ne!(a.meta().embed_key, HashingEmbedder::new(64, 0, 512).meta().embed_key);
        assert_ne!(a.meta().embed_key, HashingEmbedder::new(32, 1, 512).meta().embed_key);
    }
}
