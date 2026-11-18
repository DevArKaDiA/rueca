//! The crate's error type.

use std::path::PathBuf;

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Everything that can go wrong while resolving a preset, managing weights
/// or embedding.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The preset name is not in the manifest.
    #[error("unknown preset `{0}`; `rueca models list` shows the presets")]
    UnknownPreset(String),
    /// The preset exists but cannot be used by this build yet.
    #[error("preset `{preset}` is {status}: {reason}")]
    Unavailable {
        /// Preset name.
        preset: String,
        /// Its manifest status (`unavailable`, `planned`, `phase 3`).
        status: String,
        /// Why, and what to use instead.
        reason: String,
    },
    /// The weights are not in the cache.
    #[error(
        "weights for preset `{preset}` are missing at {}; run `rueca models pull {preset}` \
         (downloads {url}) or set RUECA_MODEL_DIR to a directory that holds them",
        path.display()
    )]
    WeightsMissing {
        /// Preset name.
        preset: String,
        /// Where the file was expected.
        path: PathBuf,
        /// Where `rueca models pull` downloads it from.
        url: String,
    },
    /// A file's sha256 differs from the manifest.
    #[error("sha256 mismatch for {}: expected {expected}, got {actual}", path.display())]
    Sha256Mismatch {
        /// The file.
        path: PathBuf,
        /// Pinned in the manifest.
        expected: String,
        /// Computed.
        actual: String,
    },
    /// A download failed.
    #[error("download of {url} failed: {message}")]
    Download {
        /// The URL.
        url: String,
        /// What happened.
        message: String,
    },
    /// A filesystem operation failed.
    #[error("{context}: {source}")]
    Io {
        /// What was being done.
        context: String,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },
    /// The embedding runtime failed (model load, context, decode).
    #[error("embedding runtime: {0}")]
    Runtime(String),
    /// An argument is out of range or inconsistent.
    #[error("invalid argument: {0}")]
    Invalid(String),
    /// A remote provider failed or answered something unusable.
    #[error("remote provider `{provider}`: {message}")]
    Remote {
        /// The provider (`voyage`, `openai`, `gemini`, `cohere`).
        provider: String,
        /// What happened.
        message: String,
    },
    /// The crate was built without the `remote` feature.
    #[error("this build has no remote providers (feature `remote` is off); rebuild with default features")]
    NoRemote,
    /// The crate was built without the `embeddings` feature.
    #[error(
        "this build has no embedding runtime (feature `embeddings` is off); \
         rebuild with default features to embed or download weights"
    )]
    NoRuntime,
}

impl Error {
    /// Wraps an I/O error with what was being done.
    pub fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io { context: context.into(), source }
    }
}
