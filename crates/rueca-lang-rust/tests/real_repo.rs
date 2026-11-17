//! Real-repository smoke test. Ignored by default; run with
//!
//! ```sh
//! RUECA_RUST_REPO=/path/to/checkout cargo test -p rueca-lang-rust --test real_repo -- --ignored --nocapture
//! ```
//!
//! With `RUECA_RUST_SHOW_ERRORS=1` it also prints the first parse error of
//! every file that has one (`ERR path:line: source line`).
//!
//! Extracts every `.rs` file under the directory (skipping hidden
//! directories, `target/` and `vendor/`), checks the extractor's invariants
//! and determinism, and prints totals, receivers, env reads, module
//! resolution through `Cargo.toml`, the share of files with parse errors
//! and timing. Cross-file resolution of the rueca
//! repository itself is tested (not ignored) in
//! `rueca-resolve/tests/rust.rs`.

use std::path::{Path, PathBuf};
use std::time::Instant;

use rueca_extract::{Budget, LanguagePack, ProjectFiles};
use rueca_lang_rust::RustPack;
use rueca_model::{FileFacts, Label, Language, RefKind};

fn walk(dir: &Path, out: &mut Vec<PathBuf>, configs: &mut Vec<PathBuf>, all: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if path.is_dir() {
            if !name.starts_with('.') && !matches!(name, "target" | "vendor" | "node_modules") {
                walk(&path, out, configs, all);
            }
            continue;
        }
        all.push(path.clone());
        if name.ends_with(".rs") {
            out.push(path);
        } else if name == "Cargo.toml" {
            configs.push(path);
        }
    }
}

/// Prints the line of the first parse error of a file.
fn print_first_error(p: &str, source: &[u8]) {
    let language = tree_sitter_rust::LANGUAGE.into();
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&language).is_err() {
        return;
    }
    let Some(tree) = parser.parse(source, None) else { return };
    let mut stack = vec![tree.root_node()];
    while let Some(n) = stack.pop() {
        if n.is_error() || n.is_missing() {
            let row = n.start_position().row;
            let line = String::from_utf8_lossy(source).lines().nth(row).unwrap_or("").trim().to_string();
            eprintln!("ERR {p}:{}: {line}", row + 1);
            return;
        }
        if n.has_error() {
            let mut c = n.walk();
            let children: Vec<_> = n.children(&mut c).collect();
            stack.extend(children.into_iter().rev());
        }
    }
}

#[test]
#[ignore = "needs RUECA_RUST_REPO pointing at a Rust checkout"]
fn extract_real_repository() {
    let Ok(root) = std::env::var("RUECA_RUST_REPO") else {
        eprintln!("RUECA_RUST_REPO not set; skipping");
        return;
    };
    let root = PathBuf::from(root);
    let (mut files, mut configs, mut all) = (Vec::new(), Vec::new(), Vec::new());
    walk(&root, &mut files, &mut configs, &mut all);
    assert!(!files.is_empty(), "no Rust files under {}", root.display());

    let rel = |p: &Path| p.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/");
    let mut project = ProjectFiles { paths: all.iter().map(|p| rel(p)).collect(), ..Default::default() };
    for c in &configs {
        if let Ok(text) = std::fs::read_to_string(c) {
            project.configs.insert(rel(c), text);
        }
    }

    let show_errors = std::env::var_os("RUECA_RUST_SHOW_ERRORS").is_some();
    let pack = RustPack::default();
    let budget = Budget::default();
    let mut stats = Stats::default();
    let mut failures = Vec::new();
    let start = Instant::now();
    for path in &files {
        let source = std::fs::read(path).unwrap();
        let p = rel(path);
        match pack.extract(&p, Language::Rust, &source, &budget) {
            Ok(f) => {
                stats.record(&pack, &p, &f, &project);
                if show_errors && f.parse_errors > 0 {
                    print_first_error(&p, &source);
                }
                // Determinism.
                assert_eq!(pack.extract(&p, Language::Rust, &source, &budget).unwrap(), f);
            }
            Err(e) => failures.push(format!("{p}: {e}")),
        }
    }
    let elapsed = start.elapsed();
    let s = stats;
    eprintln!(
        "files={} lines={} defs={} impl_items={} tests={} refs={} calls={} typed_receivers={} self_receivers={} \
         member_receivers={} env_reads={} Cargo.toml={} imports={} (in project={}) \
         files_with_parse_errors={} ({:.1}%) {:?} failures={} time={elapsed:?} (x2 for the determinism re-run)",
        files.len(),
        s.lines,
        s.defs,
        s.impl_items,
        s.tests,
        s.refs,
        s.calls,
        s.typed,
        s.selfs,
        s.members,
        s.env,
        configs.len(),
        s.imports,
        s.in_project,
        s.errored_files,
        100.0 * s.errored_files as f64 / files.len() as f64,
        s.errored,
        failures.len(),
    );
    assert!(failures.is_empty(), "{failures:#?}");
}

/// Totals over the extracted files.
#[derive(Default)]
struct Stats {
    defs: usize,
    impl_items: usize,
    refs: usize,
    calls: usize,
    typed: usize,
    selfs: usize,
    members: usize,
    env: usize,
    errored_files: usize,
    lines: usize,
    imports: usize,
    in_project: usize,
    tests: usize,
    /// The first files with parse errors.
    errored: Vec<String>,
}

impl Stats {
    /// Adds the counts of one extracted file and checks its invariants.
    fn record(&mut self, pack: &RustPack, p: &str, f: &FileFacts, project: &ProjectFiles) {
        self.defs += f.definitions.len();
        self.impl_items += f.definitions.iter().filter(|d| !d.bases.is_empty() && !d.label.is_type()).count();
        self.refs += f.references.len();
        self.calls += f.references.iter().filter(|r| r.kind == RefKind::Call).count();
        let recv = |prefix: &str| {
            f.references.iter().filter(|r| r.receiver.as_deref().is_some_and(|x| x.starts_with(prefix))).count()
        };
        self.typed += recv("$type:");
        self.selfs += recv("$self");
        self.members += recv("$member:");
        self.env += f.references.iter().filter(|r| r.kind == RefKind::EnvVar).count();
        self.tests += f.definitions.iter().filter(|d| d.is_test && d.label == Label::Function).count();
        if f.parse_errors > 0 {
            self.errored_files += 1;
            if self.errored.len() < 5 {
                self.errored.push(p.to_string());
            }
        }
        self.lines += f.line_count as usize;
        for d in &f.definitions {
            assert!(d.span.contains(&d.name_span), "{p}: {}", d.qualified_name);
        }
        for r in &f.references {
            assert!(r.span.start_byte <= r.span.end_byte, "{p}: {}", r.name);
        }
        for i in f.imports.iter().filter(|i| !i.type_only) {
            self.imports += 1;
            self.in_project += usize::from(pack.resolve_import(p, i, project).is_some());
        }
    }
}
