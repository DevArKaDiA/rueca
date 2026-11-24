//! Hybrid search (RFC 0007 §7, §8) end to end with the deterministic
//! hashing embedder: the mode fallback matrix, the envelope's `retrieval`,
//! a natural-language query that BM25 misses, and an organization whose
//! members are not all embedded.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use rueca_embed::hashing::HashingEmbedder;
use rueca_embed::{Embedder, ModelMeta};
use rueca_index::org::{OrgConfig, OrgIndexOptions, index_org, update_orgs_containing};
use rueca_index::vectors::TEXT_BUILDER_VERSION;
use rueca_index::{EmbedConfig, EmbedderLoader, IndexOptions, index_worktree_with_embedder};
use rueca_query::{Engine, QueryEmbedderLoader};
use rueca_segment::VectorsMeta;
use serde_json::{Value, json};

const DIMS: usize = 256;
/// The "semantics" of the test model: query words that mean code words.
const SYNONYMS: &[(&str, &str)] = &[("settings", "config"), ("file", "toml"), ("parsed", "loads"), ("read", "loads")];

struct IndexLoader;

impl EmbedderLoader for IndexLoader {
    fn describe(&self, cfg: &EmbedConfig) -> anyhow::Result<ModelMeta> {
        Ok(HashingEmbedder::new(DIMS, TEXT_BUILDER_VERSION, cfg.max_tokens).meta().clone())
    }
    fn check(&self, _: &EmbedConfig) -> anyhow::Result<()> {
        Ok(())
    }
    fn load(&self, cfg: &EmbedConfig) -> anyhow::Result<Box<dyn Embedder>> {
        Ok(Box::new(HashingEmbedder::new(DIMS, TEXT_BUILDER_VERSION, cfg.max_tokens)))
    }
}

/// The query side: the same model with synonyms, or no weights at all.
struct QueryLoader {
    weights: bool,
}

impl QueryEmbedderLoader for QueryLoader {
    fn load(&self, meta: &VectorsMeta) -> anyhow::Result<Arc<dyn Embedder>> {
        anyhow::ensure!(
            self.weights,
            "weights of `{}` are missing; run `rueca models pull {}`",
            meta.preset,
            meta.preset
        );
        let e = HashingEmbedder::new(DIMS, meta.text_builder_version, meta.token_budget).with_synonyms(SYNONYMS);
        anyhow::ensure!(e.meta().embed_key.to_string() == meta.embed_key, "embed key mismatch");
        Ok(Arc::new(e))
    }
}

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

/// The configuration loader's words are not the question's: BM25 matches
/// `where`, `settings` and `read` elsewhere.
fn app(root: &Path) {
    write(root, "app/loader.py", "import toml\n\n\ndef load_config(path):\n    return toml.loads(open(path).read())\n");
    write(
        root,
        "app/ui.py",
        "def settings_panel(widget):\n    \"\"\"Draw a settings panel; read its layout.\"\"\"\n    return widget.layout\n\n\ndef read_settings_button(widget):\n    return widget.button\n\n\ndef settings_icon():\n    return 'gear'\n",
    );
    write(root, "app/math.py", "def add(a, b):\n    return a + b\n");
}

fn index(root: &Path, store: &Path, embed: bool) {
    let cfg = embed.then(|| EmbedConfig { preset: "hashing".into(), ..EmbedConfig::default() });
    let opts =
        IndexOptions { root: root.to_path_buf(), store: store.to_path_buf(), embed: cfg, ..IndexOptions::default() };
    index_worktree_with_embedder(&opts, &[], &IndexLoader).unwrap();
}

fn engine(store: &Path, root: &Path, weights: bool) -> Engine {
    Engine::new(store.to_path_buf(), Some(root.to_path_buf()))
        .unwrap()
        .with_query_embedder(Arc::new(QueryLoader { weights }))
}

/// Result names in rank order (the envelope groups them by file, so the
/// calls ask for `scores`).
fn names(r: &Value) -> Vec<String> {
    let mut v: Vec<(f64, String)> = r["results"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|g| g["symbols"].as_array().unwrap().iter())
        .map(|s| (s["score"].as_f64().unwrap_or(0.0), s["name"].as_str().unwrap().to_string()))
        .collect();
    v.sort_by(|a, b| b.0.total_cmp(&a.0));
    v.into_iter().map(|(_, n)| n).collect()
}

const QUESTION: &str = "where is the settings file parsed";

fn rank_of(r: &Value, name: &str) -> Option<usize> {
    names(r).iter().position(|n| n == name).map(|i| i + 1)
}

#[test]
fn natural_language_query_and_mode_matrix() {
    let dir = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(dir.path()).unwrap();
    let (root, store) = (base.join("app"), base.join("store"));
    app(&root);
    index(&root, &store, true);
    let e = engine(&store, &root, true);
    let call = |mut args: Value| {
        args["scores"] = json!(true);
        e.call("search_symbols", args).unwrap()
    };

    // Default: hybrid (vectors and model available).
    let hybrid = call(json!({"query": QUESTION, "limit": 5}));
    assert_eq!(hybrid["retrieval"]["mode"], "hybrid", "{}", hybrid["retrieval"]);
    assert_eq!(hybrid["retrieval"]["alpha"], 0.5);
    assert!(hybrid["retrieval"]["model"].as_str().unwrap().starts_with("hashing:"));
    let lexical = call(json!({"query": QUESTION, "limit": 5, "mode": "lexical"}));
    let semantic = call(json!({"query": QUESTION, "limit": 5, "mode": "semantic"}));
    assert_eq!(lexical["retrieval"], json!({"mode": "lexical"}));
    assert_eq!(semantic["retrieval"]["mode"], "semantic");
    assert!(semantic["retrieval"].get("alpha").is_none());
    let (h, s, l) =
        (rank_of(&hybrid, "load_config"), rank_of(&semantic, "load_config"), rank_of(&lexical, "load_config"));
    eprintln!("load_config rank: hybrid {h:?}, semantic {s:?}, lexical {l:?}");
    assert_eq!(s, Some(1), "{:?}", names(&semantic));
    // Convex fusion at α 0.5 keeps the strongest BM25 hits above a
    // dense-only one, but brings it into the top 5 that BM25 alone misses.
    assert!(h.is_some_and(|r| r <= 5), "{:?}", names(&hybrid));
    assert!(l.is_none(), "lexical should miss it: {:?}", names(&lexical));
    // A lower α (more weight on the embedding) ranks it first.
    let dense_heavy = engine(&store, &root, true)
        .with_fusion(rueca_query::FusionParams { alpha: 0.2, ..rueca_query::FusionParams::default() });
    let r = dense_heavy.call("search_symbols", json!({"query": QUESTION, "limit": 5, "scores": true})).unwrap();
    assert_eq!(rank_of(&r, "load_config"), Some(1), "{:?}", names(&r));
    assert_eq!(r["retrieval"]["alpha"], 0.2);
    // Lexical mode is today's ranking, unchanged.
    let plain = Engine::new(store.clone(), Some(root.clone())).unwrap();
    let before = plain
        .call("search_symbols", json!({"query": QUESTION, "limit": 5, "mode": "lexical", "scores": true}))
        .unwrap();
    assert_eq!(names(&before), names(&lexical));
    // Paging over the fused ranking is exact.
    let page2 = call(json!({"query": QUESTION, "limit": 2, "offset": 2}));
    let all = call(json!({"query": QUESTION, "limit": 4}));
    assert_eq!(names(&page2), names(&all)[2..].to_vec());
    // Filters apply to the dense list too.
    let ui = call(json!({"query": QUESTION, "mode": "semantic", "path_glob": "app/ui.py"}));
    assert!(names(&ui).iter().all(|n| n != "load_config"));

    // Weights missing: the default degrades with a warning; explicit is an error.
    let broken = engine(&store, &root, false);
    let r = broken.call("search_symbols", json!({"query": QUESTION})).unwrap();
    assert_eq!(r["retrieval"]["mode"], "lexical");
    let w = r["retrieval"]["warnings"][0].as_str().unwrap();
    assert!(w.contains("rueca models pull"), "{w}");
    for mode in ["hybrid", "semantic"] {
        let err = broken.call("search_symbols", json!({"query": QUESTION, "mode": mode})).unwrap_err();
        assert!(format!("{err:#}").contains("rueca models pull"), "{err:#}");
    }
}

#[test]
fn snapshots_without_vectors() {
    let dir = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(dir.path()).unwrap();
    let (root, store) = (base.join("app"), base.join("store"));
    app(&root);
    index(&root, &store, false);
    let e = engine(&store, &root, true);
    // Default: lexical, nothing to warn about.
    let r = e.call("search_symbols", json!({"query": QUESTION})).unwrap();
    assert_eq!(r["retrieval"], json!({"mode": "lexical"}));
    // The same answer as today's lexical path, byte for byte; no model is
    // loaded for a snapshot without vectors.
    let lexical = e.call("search_symbols", json!({"query": QUESTION, "mode": "lexical"})).unwrap();
    assert_eq!(r, lexical);
    let no_model = engine(&store, &root, false);
    assert_eq!(no_model.call("search_symbols", json!({"query": QUESTION})).unwrap(), lexical);
    // Explicit: an error naming `rueca index --embed`.
    for mode in ["hybrid", "semantic"] {
        let err = e.call("search_symbols", json!({"query": QUESTION, "mode": mode})).unwrap_err();
        assert!(err.to_string().contains("rueca index --embed"), "{err}");
    }
    // An unknown mode is rejected by the schema.
    assert!(e.call("search_symbols", json!({"query": "x", "mode": "fuzzy"})).is_err());
}

#[test]
fn organization_with_a_lexical_member() {
    let dir = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(dir.path()).unwrap();
    let store = base.join("store");
    app(&base.join("app"));
    write(&base.join("other"), "other/settings.py", "def settings_where_read(x):\n    return x\n");
    let file = base.join("rueca-org.toml");
    fs::write(&file, "name = \"acme\"\n[[repo]]\npath = \"app\"\n[[repo]]\npath = \"other\"\n").unwrap();
    let opts = OrgIndexOptions {
        config: OrgConfig::load(&file).unwrap(),
        store: store.clone(),
        budget: Default::default(),
        threads: Some(2),
        only: None,
    };
    index_org(&opts).unwrap();
    // Embed one member, then let the organization pick its new segment up.
    index(&base.join("app"), &store, true);
    update_orgs_containing(&store, &base.join("app")).unwrap();
    let e =
        Engine::for_org(store.clone(), "acme").unwrap().with_query_embedder(Arc::new(QueryLoader { weights: true }));
    let r = e.call("search_symbols", json!({"query": QUESTION, "limit": 10})).unwrap();
    assert_eq!(r["retrieval"]["mode"], "hybrid", "{}", r["retrieval"]);
    assert_eq!(r["retrieval"]["lexical"], json!(["local/other"]));
    let found = names(&r);
    assert!(found.contains(&"load_config".to_string()), "{found:?}");
    assert!(found.contains(&"settings_where_read".to_string()), "{found:?}");
    // Semantic mode leaves the lexical member out and says so.
    let s = e.call("search_symbols", json!({"query": QUESTION, "mode": "semantic"})).unwrap();
    assert!(!names(&s).contains(&"settings_where_read".to_string()));
    assert!(s["retrieval"]["warnings"][0].as_str().unwrap().contains("local/other"));
}
