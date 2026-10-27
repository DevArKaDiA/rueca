//! Real-repository smoke test. Ignored by default; run with
//!
//! ```sh
//! RUECA_PHP_REPO=/path/to/checkout cargo test -p rueca-lang-php --test real_repo -- --ignored --nocapture
//! ```
//!
//! Extracts every `.php` file under the directory (skipping `vendor/`,
//! `node_modules/` and hidden directories), checks the extractor's
//! invariants and determinism, and prints totals, import resolution through
//! Composer autoload, namespace / PSR-4 agreement and timing. Cross-file
//! resolution numbers come from `rueca index` (see `docs/how-it-works.md`).

use std::path::{Path, PathBuf};
use std::time::Instant;

use rueca_extract::{Budget, LanguagePack, ProjectFiles};
use rueca_lang_php::{PhpPack, expected_namespace};
use rueca_model::{FileFacts, Language, RefKind};

fn walk(dir: &Path, out: &mut Vec<PathBuf>, configs: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if path.is_dir() {
            if !name.starts_with('.') && name != "vendor" && name != "node_modules" {
                walk(&path, out, configs);
            }
        } else if name.ends_with(".php") {
            out.push(path);
        } else if name == "composer.json" {
            configs.push(path);
        }
    }
}

/// Totals over the extracted files.
#[derive(Default)]
struct Stats {
    defs: usize,
    refs: usize,
    calls: usize,
    typed: usize,
    errors: usize,
    errored_files: usize,
    lines: usize,
    bytes: usize,
    uses: usize,
    uses_in_project: usize,
    includes: usize,
    includes_resolved: usize,
    ns_checked: usize,
    ns_agree: usize,
    tests: usize,
    mismatches: Vec<String>,
}

impl Stats {
    /// Counts definitions, references and parse errors; checks name spans.
    fn add_file(&mut self, p: &str, f: &FileFacts) {
        self.defs += f.definitions.len();
        self.refs += f.references.len();
        self.calls += f.references.iter().filter(|r| matches!(r.kind, RefKind::Call | RefKind::New)).count();
        self.typed += f.references.iter().filter(|r| r.receiver.as_deref().is_some_and(|x| x.ends_with("->"))).count();
        self.tests += f.definitions.iter().filter(|d| d.is_test).count();
        self.errors += f.parse_errors as usize;
        self.errored_files += usize::from(f.parse_errors > 0);
        self.lines += f.line_count as usize;
        for d in &f.definitions {
            assert!(d.span.contains(&d.name_span) || d.parent.is_some(), "{p}: {}", d.qualified_name);
        }
    }

    /// Counts `include` / `require` paths and `use` imports, and how many resolve.
    fn add_imports(&mut self, pack: &PhpPack, p: &str, f: &FileFacts, project: &ProjectFiles) {
        for i in &f.imports {
            let resolved = pack.resolve_import(p, i, project).is_some();
            if i.module.contains('/') || i.module.contains('.') {
                self.includes += 1;
                self.includes_resolved += usize::from(resolved);
            } else if i.names.is_empty() {
                self.uses += 1;
                self.uses_in_project += usize::from(resolved);
            }
        }
    }

    /// Declared namespace (from the first class-like definition) versus what
    /// PSR-4 / PSR-0 expect for the path.
    fn check_namespace(&mut self, p: &str, f: &FileFacts, project: &ProjectFiles) {
        let Some(d) = f.definitions.iter().find(|d| d.label.is_type() && d.parent.is_none()) else { return };
        let Some(expected) = expected_namespace(p, project) else { return };
        let declared = d.qualified_name.rsplit_once('\\').map_or("", |(ns, _)| ns);
        self.ns_checked += 1;
        if declared.eq_ignore_ascii_case(&expected) {
            self.ns_agree += 1;
        } else if self.mismatches.len() < 5 {
            self.mismatches.push(format!("{p}: declared `{declared}`, PSR expects `{expected}`"));
        }
    }
}

#[test]
#[ignore = "needs RUECA_PHP_REPO pointing at a PHP checkout"]
fn extract_real_repository() {
    let Ok(root) = std::env::var("RUECA_PHP_REPO") else {
        eprintln!("RUECA_PHP_REPO not set; skipping");
        return;
    };
    let root = PathBuf::from(root);
    let (mut files, mut configs) = (Vec::new(), Vec::new());
    walk(&root, &mut files, &mut configs);
    assert!(!files.is_empty(), "no PHP files under {}", root.display());

    let rel = |p: &Path| p.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/");
    let mut project = ProjectFiles { paths: files.iter().map(|p| rel(p)).collect(), ..Default::default() };
    for c in &configs {
        if let Ok(text) = std::fs::read_to_string(c) {
            project.configs.insert(rel(c), text);
        }
    }

    let pack = PhpPack::default();
    let budget = Budget::default();
    let mut stats = Stats::default();
    let mut failures = Vec::new();
    let start = Instant::now();
    for path in &files {
        let source = std::fs::read(path).unwrap();
        stats.bytes += source.len();
        let p = rel(path);
        match pack.extract(&p, Language::Php, &source, &budget) {
            Ok(f) => {
                stats.add_file(&p, &f);
                stats.add_imports(&pack, &p, &f, &project);
                stats.check_namespace(&p, &f, &project);
                // Determinism.
                assert_eq!(pack.extract(&p, Language::Php, &source, &budget).unwrap(), f);
            }
            Err(e) => failures.push(format!("{p}: {e}")),
        }
    }
    let elapsed = start.elapsed();
    let Stats {
        defs,
        refs,
        calls,
        typed,
        errors,
        errored_files,
        lines,
        bytes,
        uses,
        uses_in_project,
        includes,
        includes_resolved,
        ns_checked,
        ns_agree,
        tests,
        mismatches,
    } = stats;
    eprintln!(
        "files={} lines={lines} bytes={bytes} defs={defs} tests={tests} refs={refs} calls={calls} \
         typed_receivers={typed} composer.json={} use_imports={uses} (in project via autoload={uses_in_project}) \
         includes={includes} (resolved={includes_resolved}) parse_errors={errors} in {errored_files} files \
         namespace=psr ({ns_agree}/{ns_checked}) failures={} time={:?} (x2 for the determinism re-run)",
        files.len(),
        configs.len(),
        failures.len(),
        elapsed
    );
    eprintln!("namespace mismatches (first 5): {mismatches:#?}");
    assert!(failures.is_empty(), "{failures:#?}");
}
