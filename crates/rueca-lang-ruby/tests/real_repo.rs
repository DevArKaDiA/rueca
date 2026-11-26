//! Real-repository smoke test. Ignored by default; run with
//!
//! ```sh
//! RUECA_RUBY_REPO=/path/to/checkout cargo test -p rueca-lang-ruby --test real_repo -- --ignored --nocapture
//! ```
//!
//! Extracts every Ruby file under the directory (skipping hidden
//! directories and `vendor/`), checks the extractor's invariants and
//! determinism, and prints totals, receivers, env reads, `require`s that
//! resolve to files, the share of files with parse errors (the first ones
//! listed) and timing. Cross-file resolution is tested (not ignored) in
//! `rueca-resolve/tests/ruby.rs`.

use std::path::{Path, PathBuf};
use std::time::Instant;

use rueca_extract::{Budget, LanguagePack, ProjectFiles};
use rueca_lang_ruby::RubyPack;
use rueca_model::{FileFacts, Language, RefKind};

fn walk(dir: &Path, out: &mut Vec<PathBuf>, all: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
        if path.is_dir() {
            if !name.starts_with('.') && name != "vendor" && name != "node_modules" {
                walk(&path, out, all);
            }
            continue;
        }
        all.push(path.clone());
        if Language::from_path(&name) == Some(Language::Ruby) {
            out.push(path);
        }
    }
}

#[derive(Default)]
struct Stats {
    defs: usize,
    refs: usize,
    calls: usize,
    typed: usize,
    rets: usize,
    env: usize,
    imports: usize,
    in_project: usize,
    errored: Vec<String>,
    lines: usize,
}

impl Stats {
    fn record(&mut self, pack: &RubyPack, p: &str, f: &FileFacts, project: &ProjectFiles) {
        self.defs += f.definitions.len();
        self.refs += f.references.len();
        self.calls += f.references.iter().filter(|r| r.kind == RefKind::Call).count();
        let recv = |prefix: &str| {
            f.references.iter().filter(|r| r.receiver.as_deref().is_some_and(|x| x.starts_with(prefix))).count()
        };
        self.typed += recv("$type:");
        self.rets += recv("$ret:");
        self.env += f.references.iter().filter(|r| r.kind == RefKind::EnvVar).count();
        self.lines += f.line_count as usize;
        if f.parse_errors > 0 {
            self.errored.push(p.to_string());
        }
        for d in &f.definitions {
            assert!(d.span.contains(&d.name_span) || d.span == d.name_span, "{p}: {}", d.qualified_name);
            assert!(d.parent.is_none_or(|x| (x as usize) < f.definitions.len()), "{p}");
        }
        for i in &f.imports {
            self.imports += 1;
            self.in_project += usize::from(pack.resolve_import(p, i, project).is_some());
        }
    }
}

#[test]
#[ignore = "needs RUECA_RUBY_REPO pointing at a Ruby checkout"]
fn extract_real_repository() {
    let Ok(root) = std::env::var("RUECA_RUBY_REPO") else {
        eprintln!("RUECA_RUBY_REPO not set; skipping");
        return;
    };
    let root = PathBuf::from(root);
    let (mut files, mut all) = (Vec::new(), Vec::new());
    walk(&root, &mut files, &mut all);
    assert!(!files.is_empty(), "no Ruby files under {}", root.display());
    let rel = |p: &Path| p.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/");
    let project = ProjectFiles { paths: all.iter().map(|p| rel(p)).collect(), ..Default::default() };
    let pack = RubyPack;
    let budget = Budget::default();
    let mut s = Stats::default();
    let mut failures = Vec::new();
    let start = Instant::now();
    for path in &files {
        let source = std::fs::read(path).unwrap();
        let p = rel(path);
        match pack.extract(&p, Language::Ruby, &source, &budget) {
            Ok(f) => {
                s.record(&pack, &p, &f, &project);
                assert_eq!(pack.extract(&p, Language::Ruby, &source, &budget).unwrap(), f, "determinism: {p}");
            }
            Err(e) => failures.push(format!("{p}: {e}")),
        }
    }
    eprintln!(
        "files={} lines={} defs={} refs={} calls={} typed_receivers={} ret_receivers={} env_reads={} \
         requires={} (in project={}) files_with_parse_errors={} ({:.1}%) {:?} failures={} time={:?}",
        files.len(),
        s.lines,
        s.defs,
        s.refs,
        s.calls,
        s.typed,
        s.rets,
        s.env,
        s.imports,
        s.in_project,
        s.errored.len(),
        100.0 * s.errored.len() as f64 / files.len() as f64,
        s.errored.iter().take(10).collect::<Vec<_>>(),
        failures.len(),
        start.elapsed(),
    );
    assert!(failures.is_empty(), "{failures:#?}");
}
