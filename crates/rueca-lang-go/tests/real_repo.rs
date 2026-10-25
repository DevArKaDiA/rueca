//! Real-repository smoke test. Ignored by default; run with
//!
//! ```sh
//! RUECA_GO_REPO=/path/to/checkout cargo test -p rueca-lang-go --test real_repo -- --ignored --nocapture
//! ```
//!
//! Extracts every `.go` file under the directory (skipping `vendor/`,
//! `node_modules/` and hidden directories), checks the extractor's
//! invariants and determinism, and prints totals, typed receivers, import
//! resolution through `go.mod` and timing. Cross-file resolution numbers
//! come from `rueca index --json` (see `docs/how-it-works.md`). The RFC 0003
//! bench repositories are `github.com/gin-gonic/gin` and `github.com/urfave/cli`.

use std::path::{Path, PathBuf};
use std::time::Instant;

use rueca_extract::{Budget, LanguagePack, ProjectFiles};
use rueca_lang_go::GoPack;
use rueca_model::{Label, Language, RefKind};

fn walk(dir: &Path, out: &mut Vec<PathBuf>, configs: &mut Vec<PathBuf>, all: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if path.is_dir() {
            if !name.starts_with('.') && name != "vendor" && name != "node_modules" {
                walk(&path, out, configs, all);
            }
            continue;
        }
        all.push(path.clone());
        if name.ends_with(".go") {
            out.push(path);
        } else if name == "go.mod" || name == "go.work" {
            configs.push(path);
        }
    }
}

#[test]
#[ignore = "needs RUECA_GO_REPO pointing at a Go checkout"]
fn extract_real_repository() {
    let Ok(root) = std::env::var("RUECA_GO_REPO") else {
        eprintln!("RUECA_GO_REPO not set; skipping");
        return;
    };
    let root = PathBuf::from(root);
    let (mut files, mut configs, mut all) = (Vec::new(), Vec::new(), Vec::new());
    walk(&root, &mut files, &mut configs, &mut all);
    assert!(!files.is_empty(), "no Go files under {}", root.display());

    let rel = |p: &Path| p.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/");
    let mut project = ProjectFiles { paths: all.iter().map(|p| rel(p)).collect(), ..Default::default() };
    for c in &configs {
        if let Ok(text) = std::fs::read_to_string(c) {
            project.configs.insert(rel(c), text);
        }
    }

    let pack = GoPack::default();
    let budget = Budget::default();
    let (mut defs, mut refs, mut calls, mut typed, mut selfs, mut errors, mut errored_files, mut lines) =
        (0, 0, 0, 0, 0, 0, 0, 0);
    let (mut imports, mut in_project, mut tests, mut methods, mut bridge) = (0, 0, 0, 0, 0);
    let mut failures = Vec::new();
    let mut errored = Vec::new();
    let start = Instant::now();
    for path in &files {
        let source = std::fs::read(path).unwrap();
        let p = rel(path);
        match pack.extract(&p, Language::Go, &source, &budget) {
            Ok(f) => {
                defs += f.definitions.len();
                refs += f.references.len();
                calls += f.references.iter().filter(|r| matches!(r.kind, RefKind::Call | RefKind::New)).count();
                typed += f
                    .references
                    .iter()
                    .filter(|r| r.receiver.as_deref().is_some_and(|x| x.starts_with("$type:")))
                    .count();
                selfs += f.references.iter().filter(|r| r.receiver.as_deref() == Some("$self")).count();
                bridge +=
                    f.references.iter().filter(|r| matches!(r.kind, RefKind::EnvVar | RefKind::ConfigKey)).count();
                tests += f.definitions.iter().filter(|d| d.is_test).count();
                methods += f.definitions.iter().filter(|d| d.label == Label::Method && d.parent.is_none()).count();
                errors += f.parse_errors as usize;
                if f.parse_errors > 0 {
                    errored_files += 1;
                    if errored.len() < 5 {
                        errored.push(p.clone());
                    }
                }
                lines += f.line_count as usize;
                for d in &f.definitions {
                    assert!(d.span.contains(&d.name_span), "{p}: {}", d.qualified_name);
                }
                for i in &f.imports {
                    imports += 1;
                    in_project += usize::from(pack.resolve_import(&p, i, &project).is_some());
                }
                // Determinism.
                assert_eq!(pack.extract(&p, Language::Go, &source, &budget).unwrap(), f);
            }
            Err(e) => failures.push(format!("{p}: {e}")),
        }
    }
    let elapsed = start.elapsed();
    eprintln!(
        "files={} lines={lines} defs={defs} methods={methods} tests={tests} refs={refs} calls={calls} \
         typed_receivers={typed} self_receivers={selfs} bridge={bridge} go.mod/go.work={} imports={imports} \
         (in project={in_project}) parse_errors={errors} in {errored_files} files {errored:?} failures={} \
         time={:?} (x2 for the determinism re-run)",
        files.len(),
        configs.len(),
        failures.len(),
        elapsed
    );
    assert!(failures.is_empty(), "{failures:#?}");
}
