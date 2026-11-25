//! Real-repository smoke test. Ignored by default; run with
//!
//! ```sh
//! RUECA_CSHARP_REPO=/path/to/checkout cargo test -p rueca-lang-csharp --test real_repo -- --ignored --nocapture
//! ```
//!
//! Extracts every `.cs` file under the directory (skipping hidden
//! directories, `bin/`, `obj/` and `node_modules/`), checks the extractor's
//! invariants (parents precede children, spans inside the file, references
//! inside their enclosing definition) and determinism, and prints totals,
//! typed receivers, bridge reads, the share of files with parse errors and
//! timing. Cross-file resolution is tested (not ignored) in
//! `rueca-resolve/tests/csharp.rs`.

use std::path::{Path, PathBuf};
use std::time::Instant;

use rueca_extract::{Budget, LanguagePack};
use rueca_lang_csharp::CSharpPack;
use rueca_model::{FileFacts, Language, RefKind};

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if path.is_dir() {
            if !name.starts_with('.') && !matches!(name, "bin" | "obj" | "node_modules") {
                walk(&path, out);
            }
        } else if name.ends_with(".cs") {
            out.push(path);
        }
    }
}

/// The extractor's invariants for one file.
fn check(rel: &str, f: &FileFacts, len: usize) {
    for (i, d) in f.definitions.iter().enumerate() {
        if let Some(p) = d.parent {
            assert!((p as usize) < i, "{rel}: parent after child {}", d.qualified_name);
            assert!(f.definitions[p as usize].span.contains(&d.span), "{rel}: {} outside its parent", d.qualified_name);
        }
        assert!(d.span.end_byte as usize <= len, "{rel}: span past the end");
    }
    for r in &f.references {
        if let Some(e) = r.enclosing {
            assert!(f.definitions[e as usize].span.contains(&r.span), "{rel}: reference {} outside", r.name);
        }
    }
}

#[test]
#[ignore = "needs RUECA_CSHARP_REPO pointing at a C# checkout"]
fn extract_real_repository() {
    let Ok(root) = std::env::var("RUECA_CSHARP_REPO") else {
        eprintln!("RUECA_CSHARP_REPO not set; skipping");
        return;
    };
    let root = PathBuf::from(root);
    let mut files = Vec::new();
    walk(&root, &mut files);
    assert!(!files.is_empty(), "no C# files under {}", root.display());
    let pack = CSharpPack;
    let budget = Budget::default();
    let (mut defs, mut refs, mut typed, mut bridge, mut with_errors) = (0usize, 0usize, 0usize, 0usize, 0usize);
    let start = Instant::now();
    for path in &files {
        let rel = path.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/");
        let src = std::fs::read(path).unwrap();
        let Ok(f) = pack.extract(&rel, Language::CSharp, &src, &budget) else { continue };
        check(&rel, &f, src.len());
        let again = pack.extract(&rel, Language::CSharp, &src, &budget).unwrap();
        assert_eq!(f, again, "{rel}: extraction is not deterministic");
        defs += f.definitions.len();
        refs += f.references.len();
        let rs = &f.references;
        typed += rs.iter().filter(|r| r.receiver.as_deref().is_some_and(|x| x.starts_with('$'))).count();
        bridge += rs.iter().filter(|r| matches!(r.kind, RefKind::EnvVar | RefKind::ConfigKey)).count();
        with_errors += usize::from(f.parse_errors > 0);
    }
    let n = files.len();
    eprintln!(
        "{n} files, {defs} definitions, {refs} references ({typed} typed receivers, {bridge} bridge reads), \
         {with_errors} files with parse errors ({:.1}%), {:.2?}",
        100.0 * with_errors as f64 / n as f64,
        start.elapsed()
    );
}
