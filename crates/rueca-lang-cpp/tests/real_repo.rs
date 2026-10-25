//! Real-repository smoke test. Ignored by default; run with
//!
//! ```sh
//! RUECA_CPP_REPO=/path/to/checkout cargo test -p rueca-lang-cpp --test real_repo -- --ignored --nocapture
//! ```
//!
//! Extracts every C / C++ file under the directory (skipping hidden
//! directories and `third_party` / `vendor` / `build`), detects `.h` as C++
//! when the checkout has C++ sources (as the indexer does), checks the
//! extractor's invariants and determinism, and prints totals, include
//! resolution, the files with parse errors and timing. Cross-file
//! resolution numbers come from `rueca index` (see the crate docs).

use std::path::{Path, PathBuf};
use std::time::Instant;

use rueca_extract::{Budget, LanguagePack, ProjectFiles};
use rueca_lang_cpp::CppPack;
use rueca_model::{Language, RefKind};

const SKIP_DIRS: &[&str] = &["third_party", "vendor", "build", "node_modules"];

fn walk(dir: &Path, out: &mut Vec<PathBuf>, configs: &mut Vec<PathBuf>, all: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if path.is_dir() {
            if !name.starts_with('.') && !SKIP_DIRS.contains(&name) {
                walk(&path, out, configs, all);
            }
            continue;
        }
        all.push(path.clone());
        if matches!(Language::from_path(name), Some(Language::C | Language::Cpp)) {
            out.push(path);
        } else if ProjectFiles::is_config_file(name) {
            configs.push(path);
        }
    }
}

#[test]
#[ignore = "needs RUECA_CPP_REPO pointing at a C or C++ checkout"]
fn extract_real_repository() {
    let Ok(root) = std::env::var("RUECA_CPP_REPO") else {
        eprintln!("RUECA_CPP_REPO not set; skipping");
        return;
    };
    let root = PathBuf::from(root);
    let (mut files, mut configs, mut all) = (Vec::new(), Vec::new(), Vec::new());
    walk(&root, &mut files, &mut configs, &mut all);
    assert!(!files.is_empty(), "no C/C++ files under {}", root.display());

    let rel = |p: &Path| p.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/");
    let mut project = ProjectFiles { paths: all.iter().map(|p| rel(p)).collect(), ..Default::default() };
    for c in &configs {
        if let Ok(text) = std::fs::read_to_string(c) {
            project.configs.insert(rel(c), text);
        }
    }
    let has_cpp = files.iter().any(|p| Language::from_path(&rel(p)) == Some(Language::Cpp));

    let pack = CppPack::default();
    let budget = Budget::default();
    let (mut defs, mut decls, mut refs, mut calls, mut typed, mut tests, mut macros) = (0, 0, 0, 0, 0, 0, 0);
    let (mut errors, mut lines, mut bytes) = (0usize, 0usize, 0usize);
    let (mut includes, mut includes_resolved, mut system, mut system_resolved) = (0, 0, 0, 0);
    let mut errored: Vec<(String, u32)> = Vec::new();
    let mut failures = Vec::new();
    let start = Instant::now();
    for path in &files {
        let source = std::fs::read(path).unwrap();
        bytes += source.len();
        let p = rel(path);
        let language = Language::header_language(&p, has_cpp).unwrap();
        match pack.extract(&p, language, &source, &budget) {
            Ok(f) => {
                defs += f.definitions.len();
                decls += f.definitions.iter().filter(|d| d.is_declaration).count();
                tests += f.definitions.iter().filter(|d| d.is_test).count();
                macros += f
                    .definitions
                    .iter()
                    .filter(|d| d.signature.as_deref().is_some_and(|s| s.starts_with("#define")))
                    .count();
                refs += f.references.len();
                calls += f.references.iter().filter(|r| matches!(r.kind, RefKind::Call | RefKind::New)).count();
                typed += f
                    .references
                    .iter()
                    .filter(|r| r.receiver.as_deref().is_some_and(|x| x.starts_with("$type:")))
                    .count();
                lines += f.line_count as usize;
                if f.parse_errors > 0 {
                    errors += 1;
                    errored.push((p.clone(), f.parse_errors));
                }
                for d in &f.definitions {
                    assert!(d.span.contains(&d.name_span), "{p}: {}", d.qualified_name);
                    if let Some(parent) = d.parent {
                        assert!((parent as usize) < f.definitions.len(), "{p}: {}", d.qualified_name);
                    }
                }
                for r in &f.references {
                    assert!(r.enclosing.is_none_or(|e| (e as usize) < f.definitions.len()), "{p}: {}", r.name);
                }
                for i in &f.imports {
                    let resolved = pack.resolve_import(&p, i, &project).is_some();
                    if i.module.starts_with('<') {
                        system += 1;
                        system_resolved += usize::from(resolved);
                    } else {
                        includes += 1;
                        includes_resolved += usize::from(resolved);
                    }
                }
                assert_eq!(pack.extract(&p, language, &source, &budget).unwrap(), f, "{p}: nondeterministic");
            }
            Err(e) => failures.push(format!("{p}: {e}")),
        }
    }
    let elapsed = start.elapsed();
    errored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    eprintln!(
        "files={} (c++ headers: {has_cpp}) lines={lines} bytes={bytes} defs={defs} declarations={decls} macros={macros} \
         tests={tests} refs={refs} calls={calls} typed_receivers={typed} \
         quoted_includes={includes} (resolved={includes_resolved}) system_includes={system} (in project={system_resolved}) \
         files_with_parse_errors={errors} ({:.1}%) failures={} time={:?} (x2 for the determinism re-run)",
        files.len(),
        100.0 * errors as f64 / files.len() as f64,
        failures.len(),
        elapsed
    );
    eprintln!("parse errors (worst 15): {:#?}", &errored[..errored.len().min(15)]);
    assert!(failures.is_empty(), "{failures:#?}");
}
