//! `[retrieval]` of the store's configuration file, `<store>/rueca.toml`
//! (RFC 0007 §8.3):
//!
//! ```toml
//! [retrieval]
//! preset = "qwen3-0.6b-q8"   # embedding preset ("off": none)
//! ```
//!
//! Every key is optional. Command-line arguments and environment variables
//! (`RUECA_EMBED_PRESET`) take precedence over the file, and the file over
//! the built-in default. Reading the file never downloads or embeds
//! anything: `rueca embed` uses it for its defaults, and weights arrive only
//! through `rueca models pull`.

use std::path::Path;

use serde::Deserialize;

use crate::error::{Error, Result};

/// The configuration file's name inside the store.
pub const CONFIG_FILE: &str = "rueca.toml";

/// `[retrieval]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetrievalConfig {
    /// Embedding preset (`"off"`: none).
    pub preset: Option<String>,
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

    /// The configured preset, unless it is unset, empty or `"off"`.
    #[must_use]
    pub fn embed_preset(&self) -> Option<&str> {
        self.preset.as_deref().filter(|p| !p.is_empty() && *p != "off")
    }

    /// The preset a command uses when none is given: `explicit` (argument
    /// or environment), else the configured one, else
    /// [`crate::DEFAULT_PRESET`].
    #[must_use]
    pub fn preset_or_default<'a>(&'a self, explicit: Option<&'a str>) -> &'a str {
        explicit.or_else(|| self.embed_preset()).unwrap_or(crate::DEFAULT_PRESET)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_retrieval_and_defaults() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(RetrievalConfig::load(dir.path()).unwrap(), RetrievalConfig::default());
        assert_eq!(RetrievalConfig::default().preset_or_default(None), crate::DEFAULT_PRESET);
        std::fs::write(dir.path().join(CONFIG_FILE), "[retrieval]\npreset = \"qwen3-0.6b-f16\"\n").unwrap();
        let c = RetrievalConfig::load(dir.path()).unwrap();
        assert_eq!(c.embed_preset(), Some("qwen3-0.6b-f16"));
        assert_eq!(
            (c.preset_or_default(None), c.preset_or_default(Some("qwen3-4b-q8"))),
            ("qwen3-0.6b-f16", "qwen3-4b-q8")
        );
        std::fs::write(dir.path().join(CONFIG_FILE), "[retrieval]\npreset = \"off\"\n").unwrap();
        let off = RetrievalConfig::load(dir.path()).unwrap();
        assert_eq!((off.embed_preset(), off.preset_or_default(None)), (None, crate::DEFAULT_PRESET));
        std::fs::write(dir.path().join(CONFIG_FILE), "[retrieval]\nbogus = 1\n").unwrap();
        assert!(RetrievalConfig::load(dir.path()).is_err());
    }
}
