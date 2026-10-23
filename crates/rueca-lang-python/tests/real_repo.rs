//! Real-repository smoke test. Ignored by default; run with
//!
//! ```sh
//! RUECA_PY_REPO=/path/to/checkout cargo test -p rueca-lang-python --test real_repo -- --ignored --nocapture
//! ```
//!
//! Extracts every `.py` / `.pyi` file under the directory, checks the
//! extractor's invariants and prints totals and timing.

use std::path::{Path, PathBuf};
use std::time::Instant;

use rueca_extract::{Budget, LanguagePack, ProjectFiles};
use rueca_lang_python::PythonPack;
use rueca_model::{Language, RefKind};

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if path.is_dir() {
            if !name.starts_with('.') {
                walk(&path, out);
            }
        } else if name.ends_with(".py") || name.ends_with(".pyi") {
            out.push(path);
        }
    }
}

#[test]
#[ignore = "needs RUECA_PY_REPO pointing at a Python checkout"]
fn extract_real_repository() {
    let Ok(root) = std::env::var("RUECA_PY_REPO") else {
        eprintln!("RUECA_PY_REPO not set; skipping");
        return;
    };
    let root = PathBuf::from(root);
    let mut files = Vec::new();
    walk(&root, &mut files);
    assert!(!files.is_empty(), "no Python files under {}", root.display());

    let rel = |p: &Path| p.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/");
    let mut project = ProjectFiles { paths: files.iter().map(|p| rel(p)).collect(), ..Default::default() };
    for cfg in ["pyproject.toml", "setup.cfg"] {
        if let Ok(text) = std::fs::read_to_string(root.join(cfg)) {
            project.configs.insert(cfg.to_string(), text);
        }
    }

    let pack = PythonPack;
    let budget = Budget::default();
    let (mut defs, mut refs, mut imports, mut errors, mut errored_files, mut lines, mut bytes) = (0, 0, 0, 0, 0, 0, 0);
    let (mut resolved, mut relative) = (0, 0);
    let mut calls = 0;
    let mut failures = Vec::new();
    let start = Instant::now();
    for path in &files {
        let source = std::fs::read(path).unwrap();
        bytes += source.len();
        let p = rel(path);
        match pack.extract(&p, Language::Python, &source, &budget) {
            Ok(f) => {
                defs += f.definitions.len();
                refs += f.references.len();
                calls += f.references.iter().filter(|r| r.kind == RefKind::Call).count();
                imports += f.imports.len();
                errors += f.parse_errors as usize;
                errored_files += usize::from(f.parse_errors > 0);
                lines += f.line_count as usize;
                for d in &f.definitions {
                    assert!(d.span.contains(&d.name_span) || d.parent.is_some(), "{p}: {}", d.qualified_name);
                }
                for i in &f.imports {
                    relative += usize::from(i.relative_level > 0);
                    resolved += usize::from(pack.resolve_import(&p, i, &project).is_some());
                }
                // Determinism.
                assert_eq!(pack.extract(&p, Language::Python, &source, &budget).unwrap(), f);
            }
            Err(e) => failures.push(format!("{p}: {e}")),
        }
    }
    let elapsed = start.elapsed();
    let sample: Vec<String> =
        files.iter().take(5).map(|p| format!("{} -> {}", rel(p), pack.module_name(&rel(p), &project))).collect();
    eprintln!(
        "files={} lines={lines} bytes={bytes} defs={defs} refs={refs} calls={calls} imports={imports} \
         (relative={relative}, resolved in-project={resolved}) parse_errors={errors} in {errored_files} files \
         failures={} time={:?} (x2 for the determinism re-run)",
        files.len(),
        failures.len(),
        elapsed
    );
    eprintln!("module names: {sample:#?}");
    assert!(failures.is_empty(), "{failures:#?}");
}
