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
//! expand = "off"             # graph expansion: calls, calls+contains, calls+contains+imports
//! expand_decay = 0.5         # a neighbour inherits its seed's score times this
//! expand_seeds = 10          # top candidates whose neighbours join
//! expand_per_seed = 20       # neighbours added per seed, at most
//! expand_total = 100         # neighbours added per query, at most
//! centrality_beta = 0.0      # centrality prior exponent (0: off)
//! rerank = false             # second stage on by default
//! rerank_k = 50              # candidates the reranker scores
//! rerank_preset = "qwen3-reranker-0.6b-q8"
//! ```
//!
//! Every key is optional. Command-line arguments, tool arguments and
//! environment variables (`RUECA_EMBED_PRESET`, `RUECA_FUSION_ALPHA`,
//! `RUECA_FUSION_N`, `RUECA_FUSION_NORM`, `RUECA_EXPAND`,
//! `RUECA_EXPAND_DECAY`, `RUECA_EXPAND_SEEDS`, `RUECA_EXPAND_PER_SEED`,
//! `RUECA_EXPAND_TOTAL`, `RUECA_CENTRALITY_BETA`, `RUECA_RERANK`,
//! `RUECA_RERANK_K`, `RUECA_RERANK_PRESET`) take precedence over the file.

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
    /// Graph expansion edge set: `off`, `calls`, `calls+contains`,
    /// `calls+contains+imports`.
    pub expand: Option<String>,
    /// Expansion decay: a neighbour inherits its seed's score times this.
    pub expand_decay: Option<f32>,
    /// Seeds of the expansion: the top candidates of the first stage.
    pub expand_seeds: Option<usize>,
    /// Neighbours added per seed, at most.
    pub expand_per_seed: Option<usize>,
    /// Neighbours added per query, at most.
    pub expand_total: Option<usize>,
    /// Exponent β of the centrality prior (0: off).
    pub centrality_beta: Option<f32>,
    /// Whether `search_symbols` reranks when the call does not say.
    pub rerank: Option<bool>,
    /// Candidates the reranker scores.
    pub rerank_k: Option<usize>,
    /// Reranker preset.
    pub rerank_preset: Option<String>,
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
        std::fs::write(
            dir.path().join(CONFIG_FILE),
            "[retrieval]\nexpand = \"calls\"\ncentrality_beta = 0.5\nrerank = true\nrerank_k = 20\n",
        )
        .unwrap();
        let c = RetrievalConfig::load(dir.path()).unwrap();
        assert_eq!(
            (c.expand.as_deref(), c.centrality_beta, c.rerank, c.rerank_k),
            (Some("calls"), Some(0.5), Some(true), Some(20))
        );
        std::fs::write(dir.path().join(CONFIG_FILE), "[retrieval]\nbogus = 1\n").unwrap();
        assert!(RetrievalConfig::load(dir.path()).is_err());
    }
}
