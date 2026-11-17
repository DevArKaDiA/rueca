//! Vector identity and the audit record (RFC 0007 §5.4, §9).
//!
//! - **embed key** = BLAKE3(model file sha256 ‖ pooling ‖ document prefix ‖
//!   text builder version ‖ token budget). The weights' quantization is
//!   inside the file hash.
//! - **vector key** = BLAKE3(embed key ‖ canonical text): the identity of
//!   one stored vector, the key of the store's vector cache.
//! - **model key** = BLAKE3(embed key ‖ dimensions ‖ vector encoding): what
//!   a segment's vectors and a query vector must share to be compared.
//!
//! The query instruction is in neither key (documents carry none), and
//! neither is the runtime, its version, the backend or the CPU level: a
//! vector embedded on Metal is reused on a CPU. Those go to [`ModelMeta`],
//! which is for audit only.
//!
//! Each field is hashed as a one-byte tag, a little-endian `u64` length and
//! the bytes, after a domain string, so no two field lists collide.

use std::fmt;

use serde::Serialize;

use crate::presets::Pooling;

/// Text builder version of caller text: `rueca embed` embeds its input as
/// is, so its keys carry version 0. The indexer's canonical symbol text
/// (RFC 0007 §5.2) is version 1 (`rueca_index::vectors::TEXT_BUILDER_VERSION`)
/// and is passed through [`crate::EmbedOptions::text_builder_version`].
pub const TEXT_BUILDER_VERSION: u32 = 0;

/// A 32-byte BLAKE3 digest printed as lower-case hex.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Digest([u8; 32]);

impl Digest {
    /// The raw bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Lower-case hex.
    #[must_use]
    pub fn to_hex(&self) -> String {
        blake3::Hash::from_bytes(self.0).to_hex().to_string()
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Digest({})", self.to_hex())
    }
}

impl Serialize for Digest {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

/// Hasher with tagged, length-prefixed fields.
struct KeyHasher(blake3::Hasher);

impl KeyHasher {
    fn new(domain: &str) -> Self {
        let mut h = blake3::Hasher::new();
        h.update(domain.as_bytes());
        Self(h)
    }

    fn field(mut self, tag: u8, bytes: &[u8]) -> Self {
        self.0.update(&[tag]);
        self.0.update(&(bytes.len() as u64).to_le_bytes());
        self.0.update(bytes);
        self
    }

    fn finish(self) -> Digest {
        Digest(*self.0.finalize().as_bytes())
    }
}

/// What a stored vector's text was embedded with (RFC 0007 §5.4).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize)]
#[serde(transparent)]
pub struct EmbedKey(Digest);

impl EmbedKey {
    /// Hashes the stable inputs of a local preset.
    #[must_use]
    pub fn new(
        model_sha256: &str,
        pooling: Pooling,
        document_prefix: &str,
        text_builder_version: u32,
        token_budget: usize,
    ) -> Self {
        Self(
            KeyHasher::new("rueca-embed/embed-key/v1")
                .field(1, model_sha256.as_bytes())
                .field(2, pooling.as_str().as_bytes())
                .field(3, document_prefix.as_bytes())
                .field(4, &text_builder_version.to_le_bytes())
                .field(5, &(token_budget as u64).to_le_bytes())
                .finish(),
        )
    }

    /// The digest.
    #[must_use]
    pub fn digest(&self) -> &Digest {
        &self.0
    }
}

impl fmt::Display for EmbedKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// The identity of one stored vector (RFC 0007 §5.4): BLAKE3 of the embed
/// key and the text as embedded. Equal text under one embed key has one
/// key on every branch, worktree and repository.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize)]
#[serde(transparent)]
pub struct VectorKey(Digest);

impl VectorKey {
    /// Hashes `text` under `embed`.
    #[must_use]
    pub fn new(embed: &EmbedKey, text: &str) -> Self {
        Self(
            KeyHasher::new("rueca-embed/vector-key/v1")
                .field(1, embed.digest().as_bytes())
                .field(2, text.as_bytes())
                .finish(),
        )
    }

    /// The raw bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
        self.0.as_bytes()
    }
}

impl fmt::Display for VectorKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// How vectors are stored. `rueca embed` writes `f32`; segments use `i8`
/// rescored from an `f16` cache (RFC 0007 §6.2).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VectorEncoding {
    /// 32-bit floats.
    F32,
    /// 16-bit floats.
    F16,
    /// int8 with a per-row scale.
    I8,
}

impl VectorEncoding {
    /// Stable name, hashed into the model key.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::F32 => "f32",
            Self::F16 => "f16",
            Self::I8 => "i8",
        }
    }
}

/// What two vectors must share to be compared (RFC 0007 §5.4).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize)]
#[serde(transparent)]
pub struct ModelKey(Digest);

impl ModelKey {
    /// Hashes the embed key with the output width and encoding.
    #[must_use]
    pub fn new(embed: &EmbedKey, dims: usize, encoding: VectorEncoding) -> Self {
        Self(
            KeyHasher::new("rueca-embed/model-key/v1")
                .field(1, embed.digest().as_bytes())
                .field(2, &(dims as u64).to_le_bytes())
                .field(3, encoding.as_str().as_bytes())
                .finish(),
        )
    }

    /// The digest.
    #[must_use]
    pub fn digest(&self) -> &Digest {
        &self.0
    }
}

impl fmt::Display for ModelKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Compute device of the runtime.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    /// Apple GPU through Metal.
    Metal,
    /// CPU only.
    Cpu,
    /// NVIDIA GPU through CUDA.
    Cuda,
}

/// The CPU feature level of this machine: `x86-64-v4`, `x86-64-v3`,
/// `x86-64-v2` or `x86-64` on x86_64 (psABI levels, by runtime detection),
/// `neon` on aarch64, else the architecture name.
#[must_use]
pub fn cpu_level() -> &'static str {
    cpu_level_impl()
}

#[cfg(target_arch = "x86_64")]
fn cpu_level_impl() -> &'static str {
    let v4 = std::arch::is_x86_feature_detected!("avx512f")
        && std::arch::is_x86_feature_detected!("avx512bw")
        && std::arch::is_x86_feature_detected!("avx512cd")
        && std::arch::is_x86_feature_detected!("avx512dq")
        && std::arch::is_x86_feature_detected!("avx512vl");
    let v3 = std::arch::is_x86_feature_detected!("avx2")
        && std::arch::is_x86_feature_detected!("fma")
        && std::arch::is_x86_feature_detected!("f16c")
        && std::arch::is_x86_feature_detected!("bmi2");
    let v2 = std::arch::is_x86_feature_detected!("sse4.2") && std::arch::is_x86_feature_detected!("popcnt");
    match (v4 && v3, v3, v2) {
        (true, _, _) => "x86-64-v4",
        (_, true, _) => "x86-64-v3",
        (_, _, true) => "x86-64-v2",
        _ => "x86-64",
    }
}

#[cfg(target_arch = "aarch64")]
fn cpu_level_impl() -> &'static str {
    "neon"
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
fn cpu_level_impl() -> &'static str {
    std::env::consts::ARCH
}

/// The audit record of an embedder (RFC 0007 §9): everything that can
/// change a vector, keyed or not. Serialized by `rueca embed --info` and,
/// in phase 1, into `vectors.meta`.
#[derive(Clone, Debug, Serialize)]
pub struct ModelMeta {
    /// Preset name.
    pub preset: String,
    /// Upstream model and revision.
    pub model: String,
    /// sha256 of the weights file.
    pub model_sha256: String,
    /// Weights quantization.
    pub quantization: String,
    /// Output width after Matryoshka truncation.
    pub dims: usize,
    /// Full model width.
    pub max_dims: usize,
    /// Pooling.
    pub pooling: Pooling,
    /// Query template with the instruction in effect (not keyed).
    pub query_prefix: String,
    /// Document prefix (keyed).
    pub document_prefix: String,
    /// Token budget per input (keyed).
    pub token_budget: usize,
    /// Text builder version (keyed).
    pub text_builder_version: u32,
    /// Vector encoding of the model key.
    pub encoding: VectorEncoding,
    /// Embed key.
    pub embed_key: EmbedKey,
    /// Model key.
    pub model_key: ModelKey,
    /// Runtime family (`llama.cpp`).
    pub runtime: String,
    /// Runtime version: binding crate version and engine commit.
    pub runtime_version: String,
    /// Compute backend.
    pub backend: Backend,
    /// CPU feature level ([`cpu_level`]).
    pub cpu_level: String,
    /// Tokens per batch.
    pub batch_tokens: usize,
    /// Tokens per micro-batch (every input fits one).
    pub ubatch_tokens: usize,
    /// Worker threads.
    pub threads: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    const Q8: &str = "06507c7b42688469c4e7298b0a1e16deff06caf291cf0a5b278c308249c3e439";

    #[test]
    fn keys_are_stable() {
        // Golden values: changing them invalidates every stored vector.
        let embed = EmbedKey::new(Q8, Pooling::Last, "", TEXT_BUILDER_VERSION, 512);
        assert_eq!(embed.to_string(), "6f5d1b78561a2e829b495325328b859985b0e12c579fbf2664b9d86ffa98e474");
        let model = ModelKey::new(&embed, 1024, VectorEncoding::F32);
        assert_eq!(model.to_string(), "cf1b0d28f866a0cd654e981c9adf8de0b659d0212221880d4b0e76feb160f637");
    }

    #[test]
    fn every_keyed_field_changes_the_key() {
        let base = EmbedKey::new(Q8, Pooling::Last, "", 0, 512);
        assert_ne!(base, EmbedKey::new(Q8, Pooling::Cls, "", 0, 512));
        assert_ne!(base, EmbedKey::new(Q8, Pooling::Last, "x", 0, 512));
        assert_ne!(base, EmbedKey::new(Q8, Pooling::Last, "", 1, 512));
        assert_ne!(base, EmbedKey::new(Q8, Pooling::Last, "", 0, 256));
        assert_ne!(base, EmbedKey::new(&Q8.replace('0', "1"), Pooling::Last, "", 0, 512));
        let m = ModelKey::new(&base, 1024, VectorEncoding::F32);
        assert_ne!(m, ModelKey::new(&base, 512, VectorEncoding::F32));
        assert_ne!(m, ModelKey::new(&base, 1024, VectorEncoding::I8));
    }

    #[test]
    fn vector_key_depends_on_text_and_embed_key() {
        let embed = EmbedKey::new(Q8, Pooling::Last, "", 1, 512);
        let a = VectorKey::new(&embed, "rust Function f");
        assert_eq!(a, VectorKey::new(&embed, "rust Function f"));
        assert_ne!(a, VectorKey::new(&embed, "rust Function g"));
        assert_ne!(a, VectorKey::new(&EmbedKey::new(Q8, Pooling::Last, "", 0, 512), "rust Function f"));
        // Golden: changing it invalidates every cached vector.
        assert_eq!(a.to_string(), "257ce8fe2ec508308c7793171d8274780bf1c97b4ead7d7bcd62df934020592c");
    }

    #[test]
    fn cpu_level_is_named() {
        assert!(!cpu_level().is_empty());
    }
}
