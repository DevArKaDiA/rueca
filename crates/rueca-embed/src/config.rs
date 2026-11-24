//! `[retrieval]` of the store's configuration file, `<store>/rueca.toml`
//! (RFC 0007 §8.3):
//!
//! ```toml
//! [retrieval]
//! preset = "qwen3-0.6b-q8"   # embedding preset ("off": none)
//! dims = 1024                # width stored in segments (Matryoshka)
//! alpha = 0.5                # hybrid fusion weight of BM25
//! fusion_n = 100             # candidates per list before fusion
//! normalization = "minmax"   # or "theoretical"
//! ```
//!
//! Every key is optional. Command-line arguments and environment variables
//! (`RUECA_EMBED`, `RUECA_EMBED_PRESET`, and the experiment knobs
//! `RUECA_FUSION_ALPHA`, `RUECA_FUSION_N`, `RUECA_FUSION_NORM`) take
//! precedence over the file, and the file over the built-in defaults.
//!
//! The file **never enables embedding** by itself: `rueca index` embeds
//! only with `--embed[=PRESET]` or `RUECA_EMBED` set (RFC 0007 §8.3); the
//! file chooses the preset and parameters used once it is enabled. Reading
//! the file never downloads or embeds anything.

use std::path::Path;

use serde::Deserialize;

use crate::error::{Error, Result};

/// The configuration file's name inside the store.
pub const CONFIG_FILE: &str = "rueca.toml";

/// `[retrieval]`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetrievalConfig {
    /// Embedding preset (`"off"`: none).
    pub preset: Option<String>,
    /// Width stored in segments (Matryoshka truncation); unset = the
    /// model's full width.
    pub dims: Option<usize>,
    /// Hybrid fusion weight of the BM25 score, in `[0, 1]` (default 0.5).
    pub alpha: Option<f32>,
    /// Candidates taken from each list before fusion (default 100).
    pub fusion_n: Option<usize>,
    /// Score normalization before fusion: `minmax` (default) or
    /// `theoretical`.
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

/// Environment variable that enables embedding in `rueca index`:
/// `1` / `true` / `on` / `yes` with the configured preset, a preset name to
/// pick one; `0`, empty, `false`, `off` (or `no`) leave it disabled.
pub const EMBED_ENV: &str = "RUECA_EMBED";

/// Environment variable naming the preset (it does not enable embedding).
pub const EMBED_PRESET_ENV: &str = "RUECA_EMBED_PRESET";

/// What `RUECA_EMBED` asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmbedEnv {
    /// Unset, or a falsy value: `0`, empty, `false`, `off`, `no`.
    Off,
    /// A truthy value (`1`, `true`, `on`, `yes`): the configured preset.
    On,
    /// Any other value: that preset.
    Preset(String),
}

impl EmbedEnv {
    /// Parses a value of `RUECA_EMBED` (case-insensitive, trimmed); `None`
    /// (unset) is [`EmbedEnv::Off`].
    #[must_use]
    pub fn parse(value: Option<&str>) -> EmbedEnv {
        let Some(v) = value.map(str::trim) else { return EmbedEnv::Off };
        match v.to_ascii_lowercase().as_str() {
            "" | "0" | "false" | "off" | "no" => EmbedEnv::Off,
            "1" | "true" | "on" | "yes" => EmbedEnv::On,
            _ => EmbedEnv::Preset(v.to_string()),
        }
    }
}

/// The `--embed` / `--no-embed` arguments of `rueca index`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EmbedArg<'a> {
    /// Neither flag: the environment decides.
    #[default]
    Unset,
    /// `--no-embed`.
    Off,
    /// `--embed` (`None`) or `--embed=PRESET`.
    On(Option<&'a str>),
}

impl RetrievalConfig {
    /// The preset `rueca index` embeds with, or `None` when embedding is
    /// disabled. **Enablement** (RFC 0007 §8.3, opt-in): argument, else
    /// `RUECA_EMBED` (`env_embed`), else off; this file alone never enables
    /// it. **Preset**, once enabled: `--embed=PRESET`, else
    /// `RUECA_EMBED=<preset>`, else `RUECA_EMBED_PRESET` (`env_preset`),
    /// else `[retrieval] preset` (unless `"off"`), else
    /// [`crate::DEFAULT_PRESET`].
    #[must_use]
    pub fn index_preset(&self, arg: EmbedArg<'_>, env_embed: Option<&str>, env_preset: Option<&str>) -> Option<String> {
        let env = EmbedEnv::parse(env_embed);
        let explicit = match (arg, env) {
            (EmbedArg::Off, _) | (EmbedArg::Unset, EmbedEnv::Off) => return None,
            (EmbedArg::On(Some(p)), _) if !p.is_empty() => Some(p.to_string()),
            (_, EmbedEnv::Preset(p)) => Some(p),
            _ => None,
        };
        let env_preset = env_preset.map(str::trim).filter(|p| !p.is_empty());
        Some(explicit.unwrap_or_else(|| self.preset_or_default(env_preset).to_string()))
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
        std::fs::write(
            dir.path().join(CONFIG_FILE),
            "[retrieval]\ndims = 512\nalpha = 0.7\nfusion_n = 50\nnormalization = \"theoretical\"\n",
        )
        .unwrap();
        let tuned = RetrievalConfig::load(dir.path()).unwrap();
        assert_eq!((tuned.dims, tuned.alpha, tuned.fusion_n), (Some(512), Some(0.7), Some(50)));
        assert_eq!(tuned.normalization.as_deref(), Some("theoretical"));
        std::fs::write(dir.path().join(CONFIG_FILE), "[retrieval]\nbogus = 1\n").unwrap();
        assert!(RetrievalConfig::load(dir.path()).is_err());
    }

    #[test]
    fn embed_env_values() {
        for off in [None, Some(""), Some("0"), Some("false"), Some("OFF"), Some(" no ")] {
            assert_eq!(EmbedEnv::parse(off), EmbedEnv::Off, "{off:?}");
        }
        for on in ["1", "true", "On", "yes"] {
            assert_eq!(EmbedEnv::parse(Some(on)), EmbedEnv::On, "{on}");
        }
        assert_eq!(EmbedEnv::parse(Some("qwen3-4b-q8")), EmbedEnv::Preset("qwen3-4b-q8".into()));
    }

    /// Argument over environment over configuration; the file alone never
    /// enables embedding.
    #[test]
    fn index_preset_precedence() {
        let none = RetrievalConfig::default();
        let file = RetrievalConfig { preset: Some("qwen3-0.6b-f16".into()), ..RetrievalConfig::default() };
        let d = crate::DEFAULT_PRESET;
        // Off unless an argument or the environment enables it.
        assert_eq!(file.index_preset(EmbedArg::Unset, None, None), None);
        assert_eq!(file.index_preset(EmbedArg::Unset, None, Some("qwen3-4b-q8")), None);
        assert_eq!(file.index_preset(EmbedArg::Unset, Some("0"), None), None);
        // Environment.
        assert_eq!(none.index_preset(EmbedArg::Unset, Some("1"), None).as_deref(), Some(d));
        assert_eq!(file.index_preset(EmbedArg::Unset, Some("1"), None).as_deref(), Some("qwen3-0.6b-f16"));
        assert_eq!(file.index_preset(EmbedArg::Unset, Some("1"), Some("qwen3-4b-q8")).as_deref(), Some("qwen3-4b-q8"));
        assert_eq!(file.index_preset(EmbedArg::Unset, Some("qwen3-8b-q8"), None).as_deref(), Some("qwen3-8b-q8"));
        // Arguments win over the environment.
        assert_eq!(file.index_preset(EmbedArg::Off, Some("1"), None), None);
        assert_eq!(none.index_preset(EmbedArg::On(None), Some("0"), None).as_deref(), Some(d));
        assert_eq!(
            file.index_preset(EmbedArg::On(Some("qwen3-4b-q4")), Some("qwen3-8b-q8"), None).as_deref(),
            Some("qwen3-4b-q4")
        );
        assert_eq!(file.index_preset(EmbedArg::On(None), Some("qwen3-8b-q8"), None).as_deref(), Some("qwen3-8b-q8"));
        assert_eq!(file.index_preset(EmbedArg::On(None), Some("0"), None).as_deref(), Some("qwen3-0.6b-f16"));
        // "off" in the file only drops the file's preset.
        let off = RetrievalConfig { preset: Some("off".into()), ..RetrievalConfig::default() };
        assert_eq!(off.index_preset(EmbedArg::On(None), None, None).as_deref(), Some(d));
    }
}
