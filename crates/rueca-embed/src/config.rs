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
//! route_centroid = 1.0       # weight of the embedding-centroid routing signal
//! ```
//!
//! The same table in `rueca-org.toml` (RFC 0006 §7) configures an
//! organization, and only there `allow_remote = true` lets a `remote:`
//! preset embed or rerank the organization's code (default `false`, RFC
//! 0007 §3.5); a repository indexed outside any organization opts in by
//! naming the preset alone.
//!
//! # Profiles
//!
//! Unset keys take the defaults of the [`Profile`] in effect
//! (`RUECA_PROFILE`, `local` by default): `local` keeps every built-in
//! default (no embedding unless asked); `service` (RFC 0008 service mode,
//! RFC 0007 §8.3) embeds with `qwen3-4b-q4` when this build runs models on a
//! GPU (`cuda`, or Metal on Apple silicon) and stays lexical otherwise, with
//! the reranker off and `rerank_k = 50`: the reranker's default turns on
//! only once its gate (RFC 0007 §7.5, §11.2) holds on the bench.
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
    /// Weight of the embedding-centroid routing signal against the term
    /// distribution and repository document (1: equal weight; 0: off).
    pub route_centroid: Option<f32>,
    /// `rueca-org.toml` only: whether a `remote:` preset may embed or
    /// rerank the organization's code (default `false`).
    pub allow_remote: Option<bool>,
}

/// Which built-in defaults apply to unset keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Profile {
    /// A developer machine: today's defaults.
    #[default]
    Local,
    /// Service mode (RFC 0008): GPU defaults of RFC 0007 §8.3.
    Service,
}

/// The service profile's embedder on a GPU build (RFC 0007 §8.3).
pub const SERVICE_GPU_PRESET: &str = "qwen3-4b-q4";
/// The service profile's reranker candidates.
pub const SERVICE_RERANK_K: usize = 50;

impl Profile {
    /// `RUECA_PROFILE` (`local` or `service`); unset or unknown is `local`.
    #[must_use]
    pub fn from_env() -> Profile {
        match std::env::var("RUECA_PROFILE").ok().as_deref().map(str::trim) {
            Some("service") => Profile::Service,
            _ => Profile::Local,
        }
    }

    /// The defaults of this profile on this build: `gpu` says whether the
    /// build runs models on a GPU ([`crate::backend::gpu_compiled`]).
    #[must_use]
    pub fn defaults(self, gpu: bool) -> RetrievalConfig {
        match self {
            Profile::Local => RetrievalConfig::default(),
            // `rerank` stays false until the reranker's gate (RFC 0007
            // §7.5, §11.2) holds; CPU-only service deployments serve
            // lexical search (RFC 0008 §7.11).
            Profile::Service => RetrievalConfig {
                preset: gpu.then(|| SERVICE_GPU_PRESET.to_string()),
                rerank: Some(false),
                rerank_k: Some(SERVICE_RERANK_K),
                ..RetrievalConfig::default()
            },
        }
    }
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
        let gpu = crate::backend::gpu_compiled().is_some();
        Ok(Self::load_file(store)?.or(Profile::from_env().defaults(gpu)))
    }

    /// `[retrieval]` of `<store>/rueca.toml` alone, no profile defaults.
    ///
    /// # Errors
    ///
    /// An unreadable or invalid file; `allow_remote` (an organization key).
    pub fn load_file(store: &Path) -> Result<RetrievalConfig> {
        let path = store.join(CONFIG_FILE);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(RetrievalConfig::default()),
            Err(e) => return Err(Error::io(format!("cannot read {}", path.display()), e)),
        };
        let file: File = toml::from_str(&text).map_err(|e| Error::Invalid(format!("{}: {e}", path.display())))?;
        if file.retrieval.allow_remote.is_some() {
            return Err(Error::Invalid(format!(
                "{}: `allow_remote` belongs in an organization file (rueca-org.toml); a repository opts in to a \
                 remote preset by naming it",
                path.display()
            )));
        }
        Ok(file.retrieval)
    }

    /// Every key of `self`, else of `fallback`.
    #[must_use]
    pub fn or(self, fallback: RetrievalConfig) -> RetrievalConfig {
        RetrievalConfig {
            preset: self.preset.or(fallback.preset),
            dims: self.dims.or(fallback.dims),
            alpha: self.alpha.or(fallback.alpha),
            fusion_n: self.fusion_n.or(fallback.fusion_n),
            normalization: self.normalization.or(fallback.normalization),
            expand: self.expand.or(fallback.expand),
            expand_decay: self.expand_decay.or(fallback.expand_decay),
            expand_seeds: self.expand_seeds.or(fallback.expand_seeds),
            expand_per_seed: self.expand_per_seed.or(fallback.expand_per_seed),
            expand_total: self.expand_total.or(fallback.expand_total),
            centrality_beta: self.centrality_beta.or(fallback.centrality_beta),
            rerank: self.rerank.or(fallback.rerank),
            rerank_k: self.rerank_k.or(fallback.rerank_k),
            rerank_preset: self.rerank_preset.or(fallback.rerank_preset),
            route_centroid: self.route_centroid.or(fallback.route_centroid),
            allow_remote: self.allow_remote.or(fallback.allow_remote),
        }
    }

    /// Whether a `remote:` preset may embed or rerank this organization's
    /// code (`allow_remote`, default `false`).
    #[must_use]
    pub fn remote_allowed(&self) -> bool {
        self.allow_remote.unwrap_or(false)
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
        std::fs::write(dir.path().join(CONFIG_FILE), "[retrieval]\nallow_remote = true\n").unwrap();
        let err = RetrievalConfig::load(dir.path()).unwrap_err().to_string();
        assert!(err.contains("rueca-org.toml"), "{err}");
    }

    #[test]
    fn profiles_fill_unset_keys() {
        assert_eq!(Profile::Local.defaults(true), RetrievalConfig::default());
        let gpu = Profile::Service.defaults(true);
        assert_eq!(
            (gpu.embed_preset(), gpu.rerank, gpu.rerank_k),
            (Some(SERVICE_GPU_PRESET), Some(false), Some(SERVICE_RERANK_K))
        );
        assert_eq!(Profile::Service.defaults(false).embed_preset(), None, "CPU-only service stays lexical");
        let file = RetrievalConfig { preset: Some("off".into()), rerank_k: Some(20), ..RetrievalConfig::default() };
        let c = file.or(gpu);
        assert_eq!((c.embed_preset(), c.rerank_k, c.rerank), (None, Some(20), Some(false)));
    }
}
