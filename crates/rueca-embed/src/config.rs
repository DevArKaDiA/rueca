//! `[retrieval]` of the store's configuration file, `<store>/rueca.toml`
//! (RFC 0007 §8.3):
//!
//! ```toml
//! [retrieval]
//! preset = "qwen3-0.6b-q8"   # embed on `rueca index` ("off" opts out)
//! dims = 1024                # width stored in segments (Matryoshka)
//! alpha = 0.5                # hybrid fusion weight of BM25
//! fusion_n = 100             # candidates per list
//! normalization = "theoretical"   # or "minmax"
//! ```
//!
//! Every key is optional. Command-line arguments and environment variables
//! (`RUECA_EMBED_PRESET`, `RUECA_FUSION_ALPHA`, `RUECA_FUSION_N`,
//! `RUECA_FUSION_NORM`) take precedence over the file.

use std::path::Path;

use serde::Deserialize;

use crate::error::{Error, Result};

/// The configuration file's name inside the store.
pub const CONFIG_FILE: &str = "rueca.toml";

/// `[retrieval]`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetrievalConfig {
    /// Preset embedded by `rueca index` (`"off"`: never).
    pub preset: Option<String>,
    /// Width stored in segments.
    pub dims: Option<usize>,
    /// Hybrid fusion weight of the BM25 score, in `[0, 1]`.
    pub alpha: Option<f32>,
    /// Candidates taken from each list before fusion.
    pub fusion_n: Option<usize>,
    /// Score normalization: `theoretical` or `minmax`.
    pub normalization: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct File {
    #[serde(default)]
    retrieval: RetrievalConfig,
}

impl RetrievalConfig {
    /// `[retrieval]` of `<store>/rueca.toml`; the default when the file is
    /// absent.
    ///
    /// # Errors
    ///
    /// An unreadable or invalid file.
    pub fn load(store: &Path) -> Result<RetrievalConfig> {
        let path = store.join(CONFIG_FILE);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(RetrievalConfig::default()),
            Err(e) => return Err(Error::io(format!("cannot read {}", path.display()), e)),
        };
        let file: File = toml::from_str(&text).map_err(|e| Error::Invalid(format!("{}: {e}", path.display())))?;
        Ok(file.retrieval)
    }

    /// The preset to embed with, unless it is `"off"`.
    #[must_use]
    pub fn embed_preset(&self) -> Option<&str> {
        self.preset.as_deref().filter(|p| !p.is_empty() && *p != "off")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_retrieval_and_defaults() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(RetrievalConfig::load(dir.path()).unwrap(), RetrievalConfig::default());
        std::fs::write(
            dir.path().join(CONFIG_FILE),
            "[retrieval]\npreset = \"qwen3-0.6b-q8\"\ndims = 512\nalpha = 0.7\n",
        )
        .unwrap();
        let c = RetrievalConfig::load(dir.path()).unwrap();
        assert_eq!((c.embed_preset(), c.dims, c.alpha), (Some("qwen3-0.6b-q8"), Some(512), Some(0.7)));
        std::fs::write(dir.path().join(CONFIG_FILE), "[retrieval]\npreset = \"off\"\n").unwrap();
        assert_eq!(RetrievalConfig::load(dir.path()).unwrap().embed_preset(), None);
        std::fs::write(dir.path().join(CONFIG_FILE), "[retrieval]\nbogus = 1\n").unwrap();
        assert!(RetrievalConfig::load(dir.path()).is_err());
    }
}
