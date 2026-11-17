//! The vectors pass of `rueca index --embed` (RFC 0007 §5.6) with the
//! deterministic hashing embedder: lexical first, then vectors; a
//! re-index of an unchanged tree embeds nothing and publishes once; the
//! store cache is shared across repositories.

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use rueca_embed::hashing::HashingEmbedder;
use rueca_embed::{Embedder, ModelMeta};
use rueca_index::vectors::TEXT_BUILDER_VERSION;
use rueca_index::{EmbedConfig, EmbedderLoader, IndexOptions, IndexReport, index_worktree_with};
use rueca_segment::Store;

/// A loader of the hashing embedder that counts model loads and can
/// pretend its weights are missing.
struct Hashing {
    dims: usize,
    loads: AtomicUsize,
    weights: bool,
}

impl Hashing {
    fn new(dims: usize) -> Hashing {
        Hashing { dims, loads: AtomicUsize::new(0), weights: true }
    }

    fn embedder(&self, cfg: &EmbedConfig) -> HashingEmbedder {
        HashingEmbedder::new(self.dims, TEXT_BUILDER_VERSION, cfg.max_tokens)
    }
}

impl EmbedderLoader for Hashing {
    fn describe(&self, cfg: &EmbedConfig) -> anyhow::Result<ModelMeta> {
        Ok(self.embedder(cfg).meta().clone())
    }

    fn check(&self, _: &EmbedConfig) -> anyhow::Result<()> {
        anyhow::ensure!(self.weights, "weights missing; run `rueca models pull hashing`");
        Ok(())
    }

    fn load(&self, cfg: &EmbedConfig) -> anyhow::Result<Box<dyn Embedder>> {
        self.loads.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(self.embedder(cfg)))
    }
}

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

fn repo(root: &Path) {
    write(
        root,
        "app/config.py",
        "def load_settings(path):\n    \"\"\"Read the settings file.\"\"\"\n    return parse(open(path).read())\n\n\nclass Store:\n    def get(self, key):\n        return self.data[key]\n",
    );
    write(root, "app/draw.py", "def draw_widget(canvas):\n    canvas.paint()\n\nLIMIT = 3\n");
    write(root, "README.md", "# App\n\n## Settings\n\nWhere settings come from.\n");
}

fn opts(root: &Path, store: &Path, cfg: Option<EmbedConfig>) -> IndexOptions {
    IndexOptions { root: root.to_path_buf(), store: store.to_path_buf(), embed: cfg, ..IndexOptions::default() }
}

fn embed(dims: Option<usize>) -> Option<EmbedConfig> {
    Some(EmbedConfig { preset: "hashing".into(), dims, ..EmbedConfig::default() })
}

fn vectors_of(store: &Path, report: &IndexReport) -> (usize, usize) {
    let seg = Store::open(store).unwrap().open_segment(&report.segment.id).unwrap();
    let v = seg.vectors().unwrap().expect("segment has vectors");
    (v.len(), v.dims())
}

#[test]
fn cold_then_warm_index() {
    let dir = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(dir.path()).unwrap();
    let (root, store) = (base.join("app-repo"), base.join("store"));
    repo(&root);
    let loader = Hashing::new(64);

    // Cold: lexical first, then the copy with vectors replaces it.
    let cold = index_worktree_with(&opts(&root, &store, embed(Some(32))), &loader).unwrap();
    let v = cold.vectors.clone().expect("vectors report");
    // load_settings, Store, Store.get, draw_widget, two sections; not LIMIT, not files.
    assert_eq!((v.units, v.cached, v.embedded, v.publications, v.dims), (6, 0, 6, 2, 32));
    assert!(v.tokens > 0);
    assert_eq!(vectors_of(&store, &cold), (6, 32));
    assert_eq!(loader.loads.load(Ordering::SeqCst), 1);
    let s = Store::open(&store).unwrap();
    let entries = s.entries().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].segment_id, cold.segment.id);
    // The lexical segment of the first publication was replaced and removed.
    let segs = fs::read_dir(store.join("segments")).unwrap().count();
    assert_eq!(segs, 1);

    // Warm: every vector cached, one publication, no model load.
    let warm = index_worktree_with(&opts(&root, &store, embed(Some(32))), &loader).unwrap();
    let w = warm.vectors.clone().unwrap();
    assert_eq!((w.units, w.cached, w.embedded, w.publications), (6, 6, 0, 1));
    assert_eq!(loader.loads.load(Ordering::SeqCst), 1);
    assert_eq!(vectors_of(&store, &warm), (6, 32));

    // A one-file change embeds only that file's changed symbols.
    write(&root, "app/draw.py", "def draw_widget(canvas):\n    canvas.paint(fast=True)\n\nLIMIT = 3\n");
    let edit = index_worktree_with(&opts(&root, &store, embed(Some(32))), &loader).unwrap();
    assert_eq!(edit.vectors.unwrap().embedded, 1);

    // Another width re-quantizes from the cache: nothing is embedded.
    let wide = index_worktree_with(&opts(&root, &store, embed(None)), &loader).unwrap();
    assert_eq!(wide.vectors.clone().unwrap().embedded, 0);
    assert_eq!(vectors_of(&store, &wide), (6, 64));
}

#[test]
fn cache_is_shared_across_repositories() {
    let dir = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(dir.path()).unwrap();
    let store = base.join("store");
    let (a, b) = (base.join("one"), base.join("two"));
    repo(&a);
    repo(&b);
    let loader = Hashing::new(64);
    let first = index_worktree_with(&opts(&a, &store, embed(None)), &loader).unwrap();
    assert_eq!(first.vectors.unwrap().embedded, 6);
    // Same texts (paths and qualified names are repository-relative).
    let second = index_worktree_with(&opts(&b, &store, embed(None)), &loader).unwrap();
    let r = second.vectors.unwrap();
    assert_eq!((r.embedded, r.cached, r.publications), (0, 6, 1));
}

#[test]
fn missing_weights_fail_when_required_and_warn_otherwise() {
    let dir = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(dir.path()).unwrap();
    let (root, store) = (base.join("r"), base.join("store"));
    repo(&root);
    let loader = Hashing { weights: false, ..Hashing::new(16) };
    let err = index_worktree_with(&opts(&root, &store, embed(None)), &loader).unwrap_err();
    assert!(format!("{err:#}").contains("rueca models pull"), "{err:#}");
    // Nothing was published.
    assert!(Store::open(&store).unwrap().entries().unwrap().is_empty());
    // Configured, not required: lexical with a warning.
    let cfg = EmbedConfig { required: false, ..embed(None).unwrap() };
    let lexical = index_worktree_with(&opts(&root, &store, Some(cfg)), &loader).unwrap();
    assert!(lexical.vectors.is_none());
    let seg = Store::open(&store).unwrap().open_segment(&lexical.segment.id).unwrap();
    assert!(seg.vectors().unwrap().is_none());
    // Without `embed`, nothing about vectors.
    let plain = index_worktree_with(&opts(&root, &store, None), &loader).unwrap();
    assert!(plain.vectors.is_none());
    assert_eq!(loader.loads.load(Ordering::SeqCst), 0);
    // A width beyond the model's is refused.
    let wide = index_worktree_with(&opts(&root, &store, embed(Some(999))), &Hashing::new(16));
    assert!(wide.is_err());
}

/// `cargo test -p rueca-index --test vectors -- --ignored --nocapture`:
/// the cost of opening this repository's segment with and without its
/// vectors (1024 dimensions, the hashing embedder standing in for the
/// model: sizes are what matter), and of the first dense use.
#[test]
#[ignore = "measurement"]
fn open_cost_on_this_repository() {
    use std::time::Instant;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (lex_store, vec_store) = (dir.path().join("lexical"), dir.path().join("vectors"));
    let lexical = index_worktree_with(&opts(&root, &lex_store, None), &Hashing::new(1024)).unwrap();
    let hybrid = index_worktree_with(&opts(&root, &vec_store, embed(None)), &Hashing::new(1024)).unwrap();
    let v = hybrid.vectors.clone().unwrap();
    let time = |store: &Path, id: &str| {
        let s = Store::open(store).unwrap();
        let mut best = f64::MAX;
        for _ in 0..5 {
            let t = Instant::now();
            let seg = s.open_segment(id).unwrap();
            best = best.min(t.elapsed().as_secs_f64() * 1e3);
            drop(seg);
        }
        best
    };
    let lex_ms = time(&lex_store, &lexical.segment.id);
    let vec_ms = time(&vec_store, &hybrid.segment.id);
    let seg = Store::open(&vec_store).unwrap().open_segment(&hybrid.segment.id).unwrap();
    let t = Instant::now();
    let vecs = seg.vectors().unwrap().unwrap();
    let verify_ms = t.elapsed().as_secs_f64() * 1e3;
    let q = vec![1.0 / 32.0; 1024];
    let t = Instant::now();
    let _ = vecs.top_n(&q, 100, |_| true);
    let scan_ms = t.elapsed().as_secs_f64() * 1e3;
    println!(
        "{} units × {} dims; segment {:.1} MB lexical, {:.1} MB with vectors; open {lex_ms:.1} ms lexical, \
         {vec_ms:.1} ms with vectors (lazy); first-use checksum {verify_ms:.1} ms; one scan {scan_ms:.1} ms",
        v.units,
        v.dims,
        lexical.segment.bytes as f64 / 1e6,
        hybrid.segment.bytes as f64 / 1e6,
    );
}
