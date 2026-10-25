//! Regression golden for the shipped packs: the resolved graph of every
//! language fixture (`crates/rueca-lang-*/tests/fixtures`) must stay byte
//! for byte the same unless a change means to alter extraction or
//! resolution. Additive model or contract changes (new languages, labels,
//! hooks with defaults) must leave it untouched.
//!
//! The golden lives in `tests/golden/fixtures.graph`, one JSON value per
//! line (files, nodes with their ids, edges, resolver statistics), so a
//! diff shows exactly which node or edge changed. Regenerate it with
//! `RUECA_BLESS=1 cargo test -p rueca-index golden` when a change is
//! intended, and explain the diff in the pull request.
//!
//! `dump_graph` (ignored) renders any directory the same way, for
//! before / after comparisons on real repositories:
//! `RUECA_GOLDEN_ROOT=path RUECA_GOLDEN_OUT=out.graph cargo test -p rueca-index dump_graph -- --ignored`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use super::*;

const FIXTURES: &[(&str, &str)] = &[
    ("python", "../rueca-lang-python/tests/fixtures"),
    ("typescript", "../rueca-lang-typescript/tests/fixtures"),
    ("php", "../rueca-lang-php/tests/fixtures"),
    ("markdown", "../rueca-lang-markdown/tests/fixtures"),
    ("data", "../rueca-lang-data/tests/fixtures"),
];

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    let mut entries: Vec<_> = fs::read_dir(from).unwrap().map(|e| e.unwrap()).collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let target = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &target);
        } else {
            fs::copy(e.path(), target).unwrap();
        }
    }
}

/// Extracts and resolves `root` with the default registry and a fixed
/// snapshot identity, and renders the graph one JSON value per line.
pub(crate) fn render_graph(root: &Path) -> String {
    let paths = walk(root).unwrap();
    let registry = default_registry();
    let cache = tempfile::tempdir().unwrap();
    let kept = AtomicU64::new(0);
    let budget = Budget::default();
    let ctx = ExtractCtx {
        root,
        registry: &registry,
        budget: &budget,
        facts_dir: &cache.path().join("facts"),
        kept_bytes: &kept,
        cpp_headers: has_cpp(&paths),
    };
    let loaded: Vec<Loaded> = paths.par_iter().map(|p| ctx.load(p).unwrap()).collect();
    let mut configs = std::collections::BTreeMap::new();
    let mut files = Vec::new();
    for l in loaded {
        if let Some(text) = l.config {
            configs.insert(l.input.path.clone(), text);
        }
        files.push(l.input);
    }
    let project = ProjectFiles { paths: files.iter().map(|f| f.path.clone()).collect(), configs };
    let snapshot = SnapshotInfo {
        repo: "local/golden".into(),
        commit: "golden".into(),
        dirty: false,
        root: None,
        indexed_at: 0,
        rueca_version: "golden".into(),
    };
    let out = rueca_resolve::resolve(snapshot, files, &project, &registry);
    out.graph.validate().unwrap();

    let mut s = String::new();
    writeln!(s, "# files {}", out.graph.files.len()).unwrap();
    for f in &out.graph.files {
        writeln!(s, "{}", line(f)).unwrap();
    }
    writeln!(s, "# nodes {}", out.graph.nodes.len()).unwrap();
    for (i, n) in out.graph.nodes.iter().enumerate() {
        writeln!(s, "{i} {}", line(n)).unwrap();
    }
    writeln!(s, "# edges {}", out.graph.edges.len()).unwrap();
    for e in &out.graph.edges {
        writeln!(s, "{}", line(e)).unwrap();
    }
    let st = &out.stats;
    writeln!(
        s,
        "# stats total={} resolved={} builtin={} external={} by_strategy={:?}",
        st.references_total,
        st.references_resolved,
        st.references_builtin,
        st.references_external,
        st.resolved_by_strategy
    )
    .unwrap();
    s
}

fn line<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap()
}

fn golden_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/fixtures.graph")
}

#[test]
fn golden_fixtures_graph_is_unchanged() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let tmp = tempfile::tempdir().unwrap();
    for (name, rel) in FIXTURES {
        copy_dir(&manifest.join(rel), &tmp.path().join(name));
    }
    let actual = render_graph(tmp.path());
    let path = golden_path();
    if std::env::var_os("RUECA_BLESS").is_some() {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &actual).unwrap();
        return;
    }
    let expected = fs::read_to_string(&path).expect("golden missing; run with RUECA_BLESS=1");
    if expected != actual {
        let diff: Vec<String> = expected
            .lines()
            .zip(actual.lines())
            .filter(|(a, b)| a != b)
            .take(10)
            .map(|(a, b)| format!("- {a}\n+ {b}"))
            .collect();
        panic!(
            "resolved fixture graph changed ({} → {} lines); first differences:\n{}",
            expected.lines().count(),
            actual.lines().count(),
            diff.join("\n")
        );
    }
}

#[test]
#[ignore = "manual: RUECA_GOLDEN_ROOT=<dir> RUECA_GOLDEN_OUT=<file>"]
fn dump_graph() {
    let root = std::env::var("RUECA_GOLDEN_ROOT").expect("RUECA_GOLDEN_ROOT");
    let out = std::env::var("RUECA_GOLDEN_OUT").expect("RUECA_GOLDEN_OUT");
    let root = fs::canonicalize(root).unwrap();
    fs::write(out, render_graph(&root)).unwrap();
}
