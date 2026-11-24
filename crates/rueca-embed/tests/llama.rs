//! End-to-end tests against real weights. They run only with
//! `RUECA_EMBED_TESTS=1` and the `qwen3-0.6b-q8` weights in the cache
//! (`rueca models pull`; the cache is `$RUECA_MODEL_DIR`, else
//! `$RUECA_HOME/models`, else `~/.cache/rueca/models`); otherwise they pass
//! without running.

#![cfg(feature = "embeddings")]

use std::path::PathBuf;

use rueca_embed::vector::{dot, truncate_normalized};
use rueca_embed::{EmbedInput, EmbedOptions, Embedder, ModelCache};

const Q1: &str = "where is the user session token refreshed?";
const Q2: &str = "how are search results ranked with BM25?";
const D1: &str = "fn refresh_session(session: &mut Session, now: u64) -> Result<Token> {\n    if session.expires_at <= now { session.token = issue_token(&session.user)?; }\n    Ok(session.token.clone())\n}";
const D2: &str = "/// Scores documents with Okapi BM25 over names, signatures and bodies.\npub fn bm25_rank(index: &Index, query: &[Term], k1: f32, b: f32) -> Vec<(DocId, f32)> { index.top_hits(query, k1, b) }";
const D3: &str = "def parse_config(path):\n    \"\"\"Read the TOML configuration file and return a dict of settings.\"\"\"\n    with open(path) as f:\n        return toml.load(f)";

fn store() -> PathBuf {
    match std::env::var_os("RUECA_HOME") {
        Some(home) => PathBuf::from(home),
        None => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache/rueca"),
    }
}

fn embedder(opts: &EmbedOptions) -> Option<Box<dyn Embedder>> {
    if std::env::var("RUECA_EMBED_TESTS").ok().as_deref() != Some("1") {
        eprintln!("skipped: set RUECA_EMBED_TESTS=1 with the qwen3-0.6b-q8 weights pulled");
        return None;
    }
    let cache = ModelCache::for_store(&store());
    let preset = rueca_embed::preset("qwen3-0.6b-q8").expect("preset");
    Some(rueca_embed::backend::load(&cache, preset, opts).expect("weights pulled and loadable"))
}

fn inputs() -> Vec<EmbedInput> {
    vec![
        EmbedInput::query(Q1),
        EmbedInput::query(Q2),
        EmbedInput::document(D1),
        EmbedInput::document(D2),
        EmbedInput::document(D3),
    ]
}

fn best_doc(q: &[f32], docs: &[Vec<f32>]) -> usize {
    let scores: Vec<f32> = docs.iter().map(|d| dot(q, d)).collect();
    (0..docs.len()).max_by(|&a, &b| scores[a].total_cmp(&scores[b])).unwrap_or(0)
}

#[test]
fn queries_find_their_documents() {
    let Some(e) = embedder(&EmbedOptions::default()) else { return };
    let v = e.embed(&inputs()).expect("embed");
    assert_eq!(v.len(), 5);
    for x in &v {
        assert_eq!(x.len(), 1024);
        assert!((dot(x, x) - 1.0).abs() < 1e-4);
    }
    let docs = &v[2..];
    assert_eq!(best_doc(&v[0], docs), 0);
    assert_eq!(best_doc(&v[1], docs), 1);
    // The instruction makes a query differ from the same text as a document.
    let same = e.embed(&[EmbedInput::document(Q1)]).expect("embed");
    assert!(dot(&same[0], &v[0]) < 0.99);
}

#[test]
fn results_do_not_depend_on_input_order_or_batching() {
    let Some(e) = embedder(&EmbedOptions::default()) else { return };
    let forward = e.embed(&inputs()).expect("embed");
    let mut reversed_inputs = inputs();
    reversed_inputs.reverse();
    let mut reversed = e.embed(&reversed_inputs).expect("embed");
    reversed.reverse();
    for (a, b) in forward.iter().zip(&reversed) {
        assert!(dot(a, b) > 0.99999, "order changed a vector: {}", dot(a, b));
    }
    let alone = e.embed(&[EmbedInput::document(D2)]).expect("embed");
    assert!(dot(&alone[0], &forward[3]) > 0.999, "batching changed a vector: {}", dot(&alone[0], &forward[3]));
}

#[test]
fn matryoshka_dims_truncate_and_renormalize() {
    let Some(full) = embedder(&EmbedOptions::default()) else { return };
    let Some(short) = embedder(&EmbedOptions { dims: Some(256), ..EmbedOptions::default() }) else { return };
    assert_eq!(short.dims(), 256);
    assert_ne!(short.model_key(), full.model_key());
    let a = full.embed_query(Q1).expect("embed");
    let b = short.embed_query(Q1).expect("embed");
    assert_eq!(b.len(), 256);
    assert!(dot(&truncate_normalized(&a, 256), &b) > 0.99999);
}
