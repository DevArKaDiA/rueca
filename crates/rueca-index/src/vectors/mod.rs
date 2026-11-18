//! The vectors pass of `rueca index --embed` (RFC 0007 §5.6).
//!
//! 1. **Units and texts.** Every callable, type and Markdown section with
//!    a file ([`text::unit_kind`]) gets its canonical text ([`text`]) and a
//!    vector key, BLAKE3(embed key ‖ text) (`rueca_embed::VectorKey`).
//! 2. **Cache lookup** in the store vector cache
//!    (`rueca_segment::vcache`, `<store>/vectors/<embed key>/`).
//! 3. **Publication.** When every key hits, the segment is written once
//!    with its `vectors.*` components. Otherwise the lexical segment is
//!    published and registered first (queries stay lexical meanwhile), the
//!    misses are embedded in fixed-size batches ordered by key, each batch
//!    appended to the cache as it finishes (an interrupted run resumes), and
//!    the segment is published again: a copy of the bundle plus the vectors
//!    under a new segment id, registered in its place
//!    (`Store::republish_with_vectors`).
//!
//! The model is loaded only when something must be embedded: a re-index of
//! an unchanged tree embeds nothing and never loads it. Vectors are cached
//! at the model's full width in `f16`; the segment stores `dims`
//! components (Matryoshka truncation, re-normalized) as int8.

/// The canonical text builder, shared with the query side's reranker
/// (`rueca_segment::canonical`).
pub use rueca_segment::canonical as text;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::Context;
use rayon::prelude::*;
use rueca_embed::{EmbedInput, EmbedOptions, Embedder, ModelKey, ModelMeta, VectorEncoding, VectorKey};
use rueca_model::{EdgeKind, FileEntry, Label, Node, ResolvedGraph};
use rueca_segment::vcache::{CacheIndex, VectorCache, vector_root};
use rueca_segment::{VectorsMeta, VectorsWrite};
use tracing::info;

pub use text::{TEXT_BUILDER_VERSION, UnitKind, canonical_text, unit_kind};

/// Texts embedded per call (and appended to the cache per batch).
const BATCH_UNITS: usize = 256;
/// Rows read back from the cache at a time while writing the segment.
const READ_ROWS: usize = 2048;

/// How `rueca index` embeds (RFC 0007 §8.3).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EmbedConfig {
    /// Preset (`rueca models list`).
    pub preset: String,
    /// Width stored in the segment; `None` = the model's full width.
    pub dims: Option<usize>,
    /// Token budget per text.
    pub max_tokens: usize,
    /// Tokens per batch.
    pub batch_tokens: usize,
    /// Worker threads; `None` = the runtime's default.
    pub threads: Option<usize>,
    /// Run on the CPU even with a GPU backend.
    pub cpu: bool,
    /// An explicit request (`--embed`): missing weights or a failing model
    /// is an error. Otherwise (configuration, environment) the run warns and
    /// stays lexical.
    pub required: bool,
}

impl Default for EmbedConfig {
    fn default() -> Self {
        EmbedConfig {
            preset: rueca_embed::DEFAULT_PRESET.to_string(),
            dims: None,
            max_tokens: rueca_embed::DEFAULT_MAX_TOKENS,
            batch_tokens: rueca_embed::DEFAULT_BATCH_TOKENS,
            threads: None,
            cpu: false,
            required: true,
        }
    }
}

impl EmbedConfig {
    /// The embedder options: full width (the cache's), the canonical text
    /// version. A remote preset's full width is the one asked of the
    /// provider (`dims`), which is also what its vectors are keyed by.
    pub fn options(&self) -> EmbedOptions {
        EmbedOptions {
            dims: if rueca_embed::is_remote_preset(&self.preset) { self.dims } else { None },
            max_tokens: self.max_tokens,
            batch_tokens: self.batch_tokens,
            threads: self.threads,
            cpu: self.cpu,
            query_instruction: None,
            text_builder_version: TEXT_BUILDER_VERSION,
        }
    }
}

/// What a run asks of the vectors pass (`rueca index [--embed[=P] |
/// --no-embed]`).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EmbedRequest {
    /// The configured preset, if any (`RUECA_EMBED_PRESET`, the
    /// organization file, `<store>/rueca.toml` and its profile defaults);
    /// a failure warns and the run stays lexical.
    #[default]
    Configured,
    /// `--embed[=PRESET]`: that preset, else the configured one, else the
    /// default; a failure is an error.
    Required(Option<String>),
    /// `--no-embed`: lexical, whatever is configured.
    Off,
}

/// The embedding of a run: `request` against the configuration
/// (precedence: argument, environment, organization file, store file and
/// its profile, built-in default; RFC 0007 §8.3). `org` is the
/// organization's `[retrieval]` when the run indexes for one: a `remote:`
/// preset then needs its `allow_remote = true` (RFC 0007 §3.5).
///
/// # Errors
///
/// An invalid store file; a remote preset the organization forbids.
pub fn resolve_embed(
    store: &Path,
    request: &EmbedRequest,
    org: Option<(&str, &rueca_embed::RetrievalConfig)>,
) -> anyhow::Result<Option<EmbedConfig>> {
    if *request == EmbedRequest::Off {
        return Ok(None);
    }
    let file = rueca_embed::RetrievalConfig::load(store)?;
    let file = match org {
        Some((_, o)) => o.clone().or(file),
        None => file,
    };
    let env = std::env::var("RUECA_EMBED_PRESET").ok().filter(|p| !p.is_empty());
    let configured = env.or_else(|| file.preset.clone()).filter(|p| p != "off");
    let (preset, required) = match request {
        EmbedRequest::Required(Some(p)) => (p.clone(), true),
        EmbedRequest::Required(None) => (configured.unwrap_or_else(|| rueca_embed::DEFAULT_PRESET.to_string()), true),
        _ => match configured {
            Some(p) => (p, false),
            None => return Ok(None),
        },
    };
    if let Some((name, o)) = org
        && rueca_embed::is_remote_preset(&preset)
        && !o.remote_allowed()
    {
        anyhow::bail!(
            "organization `{name}` does not allow remote presets (`{preset}` would send its code to the provider): \
             set `[retrieval] allow_remote = true` in its rueca-org.toml to opt in"
        );
    }
    Ok(Some(EmbedConfig { preset, dims: file.dims, required, ..EmbedConfig::default() }))
}

/// Where the indexer gets its embedder; tests inject a deterministic one.
pub trait EmbedderLoader: Sync {
    /// The model record, without loading the model.
    ///
    /// # Errors
    ///
    /// Unknown preset or invalid options.
    fn describe(&self, cfg: &EmbedConfig) -> anyhow::Result<ModelMeta>;
    /// Whether the weights are at hand (the error names `rueca models pull`).
    ///
    /// # Errors
    ///
    /// Missing or mismatched weights.
    fn check(&self, cfg: &EmbedConfig) -> anyhow::Result<()>;
    /// Loads the model.
    ///
    /// # Errors
    ///
    /// Missing weights or runtime failures.
    fn load(&self, cfg: &EmbedConfig) -> anyhow::Result<Box<dyn Embedder>>;
}

/// The presets of `rueca-embed`, weights from the store's model cache.
#[derive(Debug, Clone)]
pub struct PresetLoader {
    /// The store (its `models/`, or `$RUECA_MODEL_DIR`).
    pub store: PathBuf,
}

impl EmbedderLoader for PresetLoader {
    fn describe(&self, cfg: &EmbedConfig) -> anyhow::Result<ModelMeta> {
        Ok(rueca_embed::backend::describe_preset(&cfg.preset, &cfg.options())?)
    }

    fn check(&self, cfg: &EmbedConfig) -> anyhow::Result<()> {
        Ok(rueca_embed::backend::check_preset(&rueca_embed::ModelCache::for_store(&self.store), &cfg.preset)?)
    }

    fn load(&self, cfg: &EmbedConfig) -> anyhow::Result<Box<dyn Embedder>> {
        let cache = rueca_embed::ModelCache::for_store(&self.store);
        Ok(rueca_embed::backend::load_preset(&cache, &cfg.preset, &cfg.options())?)
    }
}

/// What the vectors pass did (`IndexReport::vectors`).
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct VectorsReport {
    /// Preset.
    pub preset: String,
    /// Model key of the segment's vectors (embed key, `dims`, `i8`).
    pub model_key: String,
    /// Width stored in the segment.
    pub dims: usize,
    /// Symbols with a vector.
    pub units: u64,
    /// Vectors found in the store cache.
    pub cached: u64,
    /// Vectors embedded by this run.
    pub embedded: u64,
    /// Tokens embedded by this run.
    pub tokens: u64,
    /// Time spent embedding (model load included).
    pub embed_ms: u64,
    /// Segment publications: 1 when every vector was cached, else 2
    /// (lexical first, then with vectors).
    pub publications: u32,
    /// The store vector cache directory.
    pub cache_dir: String,
}

/// One symbol to embed.
struct Unit {
    node: u32,
    key: [u8; 32],
    text: String,
}

/// The pass between the lexical write and the vector publication.
pub(crate) struct Pass {
    cfg: EmbedConfig,
    meta: ModelMeta,
    dims: usize,
    units: Vec<Unit>,
    cache: VectorCache,
    index: CacheIndex,
    misses: Vec<usize>,
    report: VectorsReport,
}

impl Pass {
    /// Texts, keys and the cache lookup; reads each file with units once
    /// through `content`.
    pub(crate) fn prepare(
        graph: &ResolvedGraph,
        content: &(dyn Fn(&FileEntry) -> anyhow::Result<Vec<u8>> + Sync),
        cfg: &EmbedConfig,
        loader: &dyn EmbedderLoader,
        store: &Path,
    ) -> anyhow::Result<Pass> {
        let meta = loader.describe(cfg)?;
        let dims = cfg.dims.unwrap_or(meta.max_dims);
        anyhow::ensure!(
            (1..=meta.max_dims).contains(&dims) && meta.dims == meta.max_dims,
            "`dims` = {dims} is outside 1..={} for `{}`",
            meta.max_dims,
            cfg.preset
        );
        let mut units = build_units(graph, content, &meta)?;
        units.sort_by_key(|u| u.node);
        let root = vector_root(store);
        let cache = VectorCache::create(&root, &meta.embed_key.to_string(), meta.dims)
            .with_context(|| format!("opening the vector cache under {}", root.display()))?;
        let index = cache.index()?;
        let misses: Vec<usize> = (0..units.len()).filter(|&i| index.get(&units[i].key).is_none()).collect();
        let report = VectorsReport {
            preset: meta.preset.clone(),
            model_key: ModelKey::new(&meta.embed_key, dims, VectorEncoding::I8).to_string(),
            dims,
            units: units.len() as u64,
            cached: (units.len() - misses.len()) as u64,
            cache_dir: cache.dir().display().to_string(),
            ..VectorsReport::default()
        };
        info!(
            preset = %meta.preset,
            units = units.len(),
            cached = report.cached,
            to_embed = misses.len(),
            "vectors: cache lookup"
        );
        Ok(Pass { cfg: cfg.clone(), meta, dims, units, cache, index, misses, report })
    }

    /// Whether a failure is an error (`--embed`) rather than a warning.
    pub(crate) fn required(&self) -> bool {
        self.cfg.required
    }

    /// Whether every unit's vector is in the cache.
    pub(crate) fn all_cached(&self) -> bool {
        self.misses.is_empty()
    }

    /// Loads the model and embeds the misses in batches ordered by key,
    /// appending each batch to the cache.
    pub(crate) fn embed_misses(&mut self, loader: &dyn EmbedderLoader) -> anyhow::Result<()> {
        if self.misses.is_empty() {
            return Ok(());
        }
        let started = Instant::now();
        let embedder = loader.load(&self.cfg)?;
        anyhow::ensure!(
            embedder.meta().embed_key == self.meta.embed_key && embedder.dims() == self.cache.dims(),
            "the loaded model does not match its record (embed key or width)"
        );
        let mut order = self.misses.clone();
        order.sort_by_key(|&i| self.units[i].key);
        let total = order.len();
        let (mut done, mut tokens) = (0usize, 0u64);
        for batch in order.chunks(BATCH_UNITS) {
            let inputs: Vec<EmbedInput> =
                batch.iter().map(|&i| EmbedInput::document(self.units[i].text.clone())).collect();
            tokens += inputs.iter().map(|i| embedder.token_count(i) as u64).sum::<u64>();
            let vectors = embedder.embed(&inputs)?;
            let entries: Vec<([u8; 32], &[f32])> =
                batch.iter().zip(&vectors).map(|(&i, v)| (self.units[i].key, v.as_slice())).collect();
            self.cache.append(&entries)?;
            done += batch.len();
            let secs = started.elapsed().as_secs_f64().max(1e-9);
            info!(
                embedded = done,
                of = total,
                cached = self.report.cached,
                tokens_per_sec = (tokens as f64 / secs).round(),
                "vectors: embedding"
            );
        }
        self.index = self.cache.index()?;
        self.report.embedded = total as u64;
        self.report.tokens = tokens;
        self.report.embed_ms = started.elapsed().as_millis() as u64;
        self.misses.clear();
        Ok(())
    }

    /// The segment's vectors, read back from the cache.
    pub(crate) fn build(&self) -> anyhow::Result<VectorsWrite> {
        let mut out = VectorsWrite::new(self.vectors_meta()?);
        let located: Vec<(&Unit, (u8, u32))> =
            self.units.iter().filter_map(|u| self.index.get(&u.key).map(|loc| (u, loc))).collect();
        anyhow::ensure!(located.len() == self.units.len(), "vector cache lost entries while indexing");
        for chunk in located.chunks(READ_ROWS) {
            let locs: Vec<(u8, u32)> = chunk.iter().map(|(_, l)| *l).collect();
            let rows = self.cache.read_located(&locs)?;
            for ((u, _), row) in chunk.iter().zip(&rows) {
                out.push(u.node, u.key, row)?;
            }
        }
        Ok(out)
    }

    /// `vectors.meta`: the model record with the segment's width, model key
    /// and the embedded labels.
    fn vectors_meta(&self) -> anyhow::Result<VectorsMeta> {
        let serde_json::Value::Object(mut extra) = serde_json::to_value(&self.meta)? else {
            anyhow::bail!("model record is not an object");
        };
        for k in [
            "preset",
            "embed_key",
            "model_key",
            "model_sha256",
            "dims",
            "max_dims",
            "encoding",
            "text_builder_version",
            "token_budget",
        ] {
            extra.remove(k);
        }
        let labels: Vec<&str> = Label::ALL.iter().filter(|l| unit_kind(**l).is_some()).map(|l| l.as_str()).collect();
        extra.insert("labels".into(), serde_json::json!(labels));
        Ok(VectorsMeta {
            preset: self.meta.preset.clone(),
            embed_key: self.meta.embed_key.to_string(),
            model_key: self.report.model_key.clone(),
            model_sha256: self.meta.model_sha256.clone(),
            dims: self.dims,
            max_dims: self.meta.max_dims,
            encoding: "i8".into(),
            rows: 0,
            text_builder_version: self.meta.text_builder_version,
            token_budget: self.meta.token_budget,
            extra,
        })
    }

    /// The report, with the number of publications.
    pub(crate) fn finish(mut self, publications: u32) -> VectorsReport {
        self.report.publications = publications;
        self.report
    }
}

/// Every unit of the graph with its canonical text and key, file by file.
fn build_units(
    graph: &ResolvedGraph,
    content: &(dyn Fn(&FileEntry) -> anyhow::Result<Vec<u8>> + Sync),
    meta: &ModelMeta,
) -> anyhow::Result<Vec<Unit>> {
    let mut by_file: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for (i, n) in graph.nodes.iter().enumerate() {
        if let (Some(_), Some(f), Some(_)) = (unit_kind(n.label), n.file, n.span) {
            by_file.entry(f).or_default().push(i as u32);
        }
    }
    let members = type_members(graph);
    let files: Vec<(u32, Vec<u32>)> = by_file.into_iter().collect();
    let per_file: Vec<anyhow::Result<Vec<Unit>>> = files
        .par_iter()
        .map(|(f, nodes)| {
            let file = &graph.files[*f as usize];
            let bytes = content(file).with_context(|| format!("reading {} for its vectors", file.path))?;
            Ok(nodes
                .iter()
                .map(|&n| {
                    let node = &graph.nodes[n as usize];
                    let ms: Vec<&Node> = members
                        .get(&n)
                        .map_or_else(Vec::new, |v| v.iter().map(|&m| &graph.nodes[m as usize]).collect());
                    let text = canonical_text(node, &file.path, &bytes, &ms, meta.token_budget);
                    let key = *VectorKey::new(&meta.embed_key, &text).as_bytes();
                    Unit { node: n, key, text }
                })
                .collect())
        })
        .collect();
    let mut units = Vec::new();
    for r in per_file {
        units.extend(r?);
    }
    Ok(units)
}

/// Children defined by each type node (`DEFINES` edges).
fn type_members(graph: &ResolvedGraph) -> HashMap<u32, Vec<u32>> {
    let mut out: HashMap<u32, Vec<u32>> = HashMap::new();
    for e in &graph.edges {
        let src = &graph.nodes[e.src as usize];
        if e.kind == EdgeKind::Defines && unit_kind(src.label) == Some(UnitKind::Type) && e.src != e.dst {
            out.entry(e.src).or_default().push(e.dst);
        }
    }
    out
}
