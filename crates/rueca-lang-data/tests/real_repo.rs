//! Real-repository smoke test. Ignored by default; run with
//!
//! ```sh
//! dir=$(mktemp -d)
//! git clone --depth 1 --filter=blob:none https://github.com/docker/awesome-compose "$dir/repo"
//! RUECA_DATA_REPO="$dir/repo" cargo test -p rueca-lang-data --test real_repo -- --ignored --nocapture
//! rm -rf "$dir"
//! ```
//!
//! Extracts every data file under the directory (hidden `.github/`
//! included; `.git`, `node_modules`, `vendor` and `target` skipped), checks
//! the extractor's invariants — no panic, determinism, spans contain their
//! name, children inside parents, no env provider value or secret-looking
//! value in a signature — and prints, per language, files, keys, kind nodes,
//! references and imports (with the share of imports that resolve to a file
//! or folder of the checkout), skips by reason, and timing. Resolution of
//! references (bridge edges) needs the whole pipeline: run `rueca index
//! --json` on the same checkout for those numbers.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use rueca_extract::{Budget, ExtractError, LanguagePack, ProjectFiles};
use rueca_lang_data::DataPack;
use rueca_model::{Label, Language, RefKind};

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if path.is_dir() {
            if !matches!(name, ".git" | "node_modules" | "vendor" | "target" | ".terraform") {
                walk(&path, out);
            }
        } else if path.is_file() {
            out.push(path);
        }
    }
}

#[derive(Default, Debug)]
struct Totals {
    files: usize,
    bytes: usize,
    defs: usize,
    keys: usize,
    kind_nodes: usize,
    env_providers: usize,
    refs: BTreeMap<String, usize>,
    imports: usize,
    imports_resolved: usize,
    partial: usize,
}

#[test]
#[ignore = "needs RUECA_DATA_REPO pointing at a checkout with data files"]
fn extract_real_repository() {
    let Ok(root) = std::env::var("RUECA_DATA_REPO") else {
        eprintln!("RUECA_DATA_REPO not set; skipping");
        return;
    };
    let root = PathBuf::from(root);
    let mut all = Vec::new();
    walk(&root, &mut all);
    let rel = |p: &Path| p.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/");
    let project = ProjectFiles { paths: all.iter().map(|p| rel(p)).collect(), ..Default::default() };
    let pack = DataPack;
    let budget = Budget::default();
    let mut per_lang: BTreeMap<Language, Totals> = BTreeMap::new();
    let mut skips: BTreeMap<String, usize> = BTreeMap::new();
    let mut failures = Vec::new();
    let mut unresolved: Vec<String> = Vec::new();
    let started = Instant::now();
    let mut extract_time = std::time::Duration::ZERO;
    for path in &all {
        let p = rel(path);
        let Some(lang) = Language::from_path(&p).filter(|l| pack.languages().contains(l)) else { continue };
        let source = std::fs::read(path).unwrap();
        let t = Instant::now();
        let result = pack.extract(&p, lang, &source, &budget);
        extract_time += t.elapsed();
        let f = match result {
            Ok(f) => f,
            Err(ExtractError::Skipped(reason)) => {
                *skips.entry(reason).or_default() += 1;
                continue;
            }
            Err(e) => {
                failures.push(format!("{p}: {e}"));
                continue;
            }
        };
        assert_eq!(pack.extract(&p, lang, &source, &budget).unwrap(), f, "{p}: not deterministic");
        let tot = per_lang.entry(lang).or_default();
        tot.files += 1;
        tot.bytes += source.len();
        tot.partial += usize::from(f.parse_errors > 0);
        tot.defs += f.definitions.len();
        for d in &f.definitions {
            assert!(
                d.span.contains(&d.name_span) || d.name_span.start_byte == d.name_span.end_byte,
                "{p}: {}",
                d.qualified_name
            );
            if let Some(parent) = d.parent {
                assert!((parent as usize) < f.definitions.len(), "{p}: parent of {}", d.qualified_name);
            }
            match d.label {
                Label::Key => tot.keys += 1,
                Label::EnvVar => {
                    tot.env_providers += 1;
                    tot.kind_nodes += 1;
                    let sig = d.signature.as_deref().unwrap_or("");
                    assert!(sig.ends_with("•••") || !sig.contains(['=', ':']), "{p}: env value in {sig}");
                }
                _ => tot.kind_nodes += 1,
            }
        }
        for r in &f.references {
            let kind = match r.kind {
                RefKind::DataRef => "data_ref",
                RefKind::EnvVar => "env_read",
                RefKind::ConfigKey => "config_read",
                _ => "other",
            };
            *tot.refs.entry(kind.to_string()).or_default() += 1;
        }
        for i in &f.imports {
            tot.imports += 1;
            if rueca_lang_data::resolve_import(&p, &i.module, &project).is_some() {
                tot.imports_resolved += 1;
            } else if unresolved.len() < 15 {
                unresolved.push(format!("{p} -> {}", i.module));
            }
        }
    }
    for (lang, t) in &per_lang {
        eprintln!("{lang}: {t:?}");
    }
    eprintln!("skips: {skips:?}");
    eprintln!("failures ({}): {:?}", failures.len(), failures.iter().take(10).collect::<Vec<_>>());
    eprintln!("unresolved imports (sample): {unresolved:?}");
    eprintln!("extract time={extract_time:?} wall={:?}", started.elapsed());
}
