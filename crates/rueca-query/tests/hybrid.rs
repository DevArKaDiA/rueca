//! Hybrid search (RFC 0007 §7, §8) end to end with the deterministic
//! hashing embedder: the mode fallback matrix, the envelope's `retrieval`,
//! a natural-language query that BM25 misses, and an organization whose
//! members are not all embedded.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use rueca_embed::hashing::HashingEmbedder;
use rueca_embed::{Embedder, ModelMeta};
use rueca_index::org::{OrgConfig, OrgIndexOptions, index_org, index_org_with, update_orgs_containing};
use rueca_index::vectors::TEXT_BUILDER_VERSION;
use rueca_index::{EmbedConfig, EmbedderLoader, IndexOptions, index_worktree_with};
use rueca_query::{Engine, QueryEmbedderLoader};
use rueca_segment::VectorsMeta;
use rueca_segment::org::{OrgIndex, OrgSelection, RouteParams, RouteQuery};
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
    index_worktree_with(&opts, &IndexLoader).unwrap();
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

fn org_options(file: &Path, store: &Path) -> OrgIndexOptions {
    OrgIndexOptions {
        config: OrgConfig::load(file).unwrap(),
        store: store.to_path_buf(),
        budget: Default::default(),
        threads: Some(2),
        only: None,
    }
}

#[test]
fn organization_model_key_decides_who_is_fused() {
    let dir = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(dir.path()).unwrap();
    let store = base.join("store");
    app(&base.join("app"));
    write(&base.join("other"), "other/settings.py", "def settings_where_read(x):\n    return x\n");
    let file = base.join("rueca-org.toml");
    let text =
        "name = \"acme\"\n[[repo]]\npath = \"app\"\n[[repo]]\npath = \"other\"\n[retrieval]\npreset = \"hashing\"\n";
    fs::write(&file, text).unwrap();
    // The organization's preset embeds every member.
    let report = index_org_with(&org_options(&file, &store), &IndexLoader).unwrap();
    let model = report.model.clone().expect("the organization records its model");
    assert_eq!((model.preset.as_str(), model.dims), ("hashing", DIMS));
    assert!(report.indexed.iter().all(|r| r.vectors.as_ref().is_some_and(|v| v.embedded > 0)));
    assert!(report.vectors.iter().all(|m| m.status == "fused"), "{:?}", report.vectors);
    // A second run reuses every vector.
    let again = index_org_with(&org_options(&file, &store), &IndexLoader).unwrap();
    assert!(again.indexed.iter().all(|r| r.vectors.as_ref().is_some_and(|v| v.embedded == 0)));
    // One member re-embedded at another width: another model key.
    let cfg = EmbedConfig { preset: "hashing".into(), dims: Some(64), ..EmbedConfig::default() };
    let opts =
        IndexOptions { root: base.join("other"), store: store.clone(), embed: Some(cfg), ..IndexOptions::default() };
    index_worktree_with(&opts, &IndexLoader).unwrap();
    update_orgs_containing(&store, &base.join("other")).unwrap();
    let g = rueca_segment::org::current_generation(&store, "acme").unwrap().unwrap();
    assert_eq!(g.model.as_ref(), Some(&model), "a plain re-index keeps the organization's model");
    let coverage = rueca_index::org::vector_coverage(&store, &g).unwrap();
    assert_eq!(coverage.iter().find(|m| m.repo == "local/other").unwrap().status, "other model");
    let e =
        Engine::for_org(store.clone(), "acme").unwrap().with_query_embedder(Arc::new(QueryLoader { weights: true }));
    let r = e.call("search_symbols", json!({"query": QUESTION, "limit": 10})).unwrap();
    assert_eq!(r["retrieval"]["mode"], "hybrid", "{}", r["retrieval"]);
    assert_eq!(r["retrieval"]["lexical"], json!(["local/other"]));
    let why = r["retrieval"]["lexical_reasons"]["local/other"].as_str().unwrap();
    assert!(why.starts_with("vectors of another model (hashing:"), "{why}");
    assert!(names(&r).contains(&"settings_where_read".to_string()));
}

#[test]
fn remote_presets_need_the_organization_to_allow_them() {
    let dir = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(dir.path()).unwrap();
    let store = base.join("store");
    app(&base.join("app"));
    let file = base.join("rueca-org.toml");
    let members = "name = \"acme\"\n[[repo]]\npath = \"app\"\n";
    fs::write(&file, format!("{members}[retrieval]\npreset = \"remote:voyage/voyage-code-3\"\n")).unwrap();
    let err = index_org_with(&org_options(&file, &store), &IndexLoader).unwrap_err();
    assert!(format!("{err:#}").contains("allow_remote = true"), "{err:#}");
    // Built lexically, the organization forbids remote presets for its
    // members too: a plain index of one with a remote preset is refused.
    fs::write(&file, members).unwrap();
    index_org(&org_options(&file, &store)).unwrap();
    let cfg = EmbedConfig { preset: "remote:openai/text-embedding-3-large".into(), ..EmbedConfig::default() };
    let opts =
        IndexOptions { root: base.join("app"), store: store.clone(), embed: Some(cfg), ..IndexOptions::default() };
    let err = index_worktree_with(&opts, &IndexLoader).unwrap_err();
    assert!(format!("{err:#}").contains("organization `acme`"), "{err:#}");
}

/// Two repositories no query word reaches lexically; the hashing model's
/// synonyms tie the query to one of them.
#[test]
fn centroid_routing_follows_the_query_vector() {
    let dir = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(dir.path()).unwrap();
    let store = base.join("store");
    write(
        &base.join("billing"),
        "billing/invoice.py",
        "def issue_invoice(account):\n    return payment(account)\n\n\ndef refund_payment(invoice):\n    return invoice\n",
    );
    write(
        &base.join("ui"),
        "ui/widget.py",
        "def draw_widget(canvas):\n    return canvas\n\n\ndef paint_canvas(c):\n    return c\n",
    );
    let file = base.join("rueca-org.toml");
    let text =
        "name = \"acme\"\n[[repo]]\npath = \"billing\"\n[[repo]]\npath = \"ui\"\n[retrieval]\npreset = \"hashing\"\n";
    fs::write(&file, text).unwrap();
    index_org_with(&org_options(&file, &store), &IndexLoader).unwrap();
    let idx = OrgIndex::open(&store, "acme").unwrap();
    let g = idx.generation();
    let (billing, ui) = (g.repo_index("local/billing").unwrap(), g.repo_index("local/ui").unwrap());
    let c = idx.profile(billing).and_then(|p| p.centroid.clone()).expect("billing has a centroid");
    assert_eq!(c.dims, DIMS);
    assert!((c.vector.iter().map(|x| x * x).sum::<f32>() - 1.0).abs() < 1e-4);
    // "charge the customer": nothing lexical; the model reads invoice, account.
    let q = HashingEmbedder::new(DIMS, TEXT_BUILDER_VERSION, rueca_embed::DEFAULT_MAX_TOKENS)
        .with_synonyms(&[("charge", "invoice"), ("customer", "account")])
        .embed_query("charge the customer")
        .unwrap();
    let key = g.model.as_ref().unwrap().embed_key.clone();
    let vis = [billing, ui];
    let params = RouteParams::default();
    let view = OrgSelection::default();
    let flat = idx.route_view(QUESTION_NONE, &vis, &view, &params).unwrap();
    assert!(flat.iter().all(|r| r.score == flat[0].score && r.centroid.is_none()), "{flat:?}");
    let routes = idx
        .route_view_with(QUESTION_NONE, &vis, &view, &params, Some(RouteQuery { embed_key: &key, vector: &q }))
        .unwrap();
    assert_eq!(routes[0].repo, billing, "{routes:?}");
    assert_eq!((routes[0].centroid, routes[1].centroid), (Some(1.0), Some(0.0)));
    assert!(routes[1].score < routes[0].score);
    // Weight 0 turns the signal off; a vector of another model is ignored.
    let off = RouteParams { centroid: 0.0, ..params };
    let r = idx
        .route_view_with(QUESTION_NONE, &vis, &view, &off, Some(RouteQuery { embed_key: &key, vector: &q }))
        .unwrap();
    assert!(r.iter().all(|x| x.centroid.is_none()));
    let r = idx
        .route_view_with(QUESTION_NONE, &vis, &view, &params, Some(RouteQuery { embed_key: "x", vector: &q }))
        .unwrap();
    assert!(r.iter().all(|x| x.centroid.is_none()));
}

/// A query whose words no repository holds.
const QUESTION_NONE: &str = "charge the customer";
