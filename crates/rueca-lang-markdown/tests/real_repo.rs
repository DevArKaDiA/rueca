//! Real-repository smoke test. Ignored by default; run with
//!
//! ```sh
//! git clone --depth 1 https://github.com/rust-lang/book /tmp/book
//! RUECA_MD_REPO=/tmp/book cargo test -p rueca-lang-markdown --test real_repo -- --ignored --nocapture
//! ```
//!
//! Extracts every `.md` / `.markdown` file under the directory, checks the
//! extractor's invariants (spans nest, qualified names unique per file,
//! determinism) and prints totals, timing and the share of file links that
//! resolve to a file of the checkout.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use rueca_extract::{Budget, LanguagePack, ProjectFiles};
use rueca_lang_markdown::MarkdownPack;
use rueca_model::Language;

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if path.is_dir() {
            if !name.starts_with('.') && name != "node_modules" && name != "target" {
                walk(&path, out);
            }
        } else if path.is_file() {
            out.push(path);
        }
    }
}

#[test]
#[ignore = "needs RUECA_MD_REPO pointing at a checkout with Markdown files"]
fn extract_real_repository() {
    let Ok(root) = std::env::var("RUECA_MD_REPO") else {
        eprintln!("RUECA_MD_REPO not set; skipping");
        return;
    };
    let root = PathBuf::from(root);
    let mut all = Vec::new();
    walk(&root, &mut all);
    let rel = |p: &Path| p.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/");
    let project = ProjectFiles { paths: all.iter().map(|p| rel(p)).collect(), ..Default::default() };
    let files: Vec<&PathBuf> =
        all.iter().filter(|p| Language::from_path(&rel(p)) == Some(Language::Markdown)).collect();
    assert!(!files.is_empty(), "no Markdown files under {}", root.display());

    let pack = MarkdownPack;
    let budget = Budget::default();
    let (mut sections, mut with_doc, mut nested, mut dups, mut links, mut resolved, mut bytes, mut lines) =
        (0, 0, 0, 0, 0, 0, 0, 0);
    let mut partial = 0;
    let mut unresolved: BTreeMap<String, usize> = BTreeMap::new();
    let mut samples: BTreeMap<String, usize> = BTreeMap::new();
    let mut failures = Vec::new();
    let mut extract_time = std::time::Duration::ZERO;
    for path in &files {
        let source = std::fs::read(path).unwrap();
        bytes += source.len();
        let p = rel(path);
        let t = Instant::now();
        let result = pack.extract(&p, Language::Markdown, &source, &budget);
        extract_time += t.elapsed();
        let f = match result {
            Ok(f) => f,
            Err(e) => {
                failures.push(format!("{p}: {e}"));
                continue;
            }
        };
        lines += f.line_count as usize;
        partial += usize::from(f.parse_errors > 0);
        sections += f.definitions.len();
        let mut qns = HashSet::new();
        for d in &f.definitions {
            assert!(qns.insert(d.qualified_name.as_str()), "{p}: duplicate {}", d.qualified_name);
            assert!(d.span.contains(&d.name_span), "{p}: {}", d.qualified_name);
            with_doc += usize::from(d.doc.is_some());
            dups += usize::from(d.qualified_name.ends_with(')') && d.qualified_name.contains(" ("));
            if let Some(parent) = d.parent {
                nested += 1;
                assert!(f.definitions[parent as usize].span.contains(&d.span), "{p}: {}", d.qualified_name);
            }
        }
        for i in &f.imports {
            links += 1;
            if pack.resolve_import(&p, i, &project).is_some() {
                resolved += 1;
            } else {
                let key = i.module.split(['#', '?']).next().unwrap_or("").rsplit('.').next().unwrap_or("").to_string();
                *unresolved.entry(format!(".{key}")).or_default() += 1;
                let dir = p.rsplit_once('/').map_or("", |(d, _)| d);
                *samples.entry(format!("{dir}/ -> {}", i.module)).or_default() += 1;
            }
        }
        assert_eq!(pack.extract(&p, Language::Markdown, &source, &budget).unwrap(), f, "{p}: not deterministic");
    }
    let mut top: Vec<(String, usize)> = unresolved.into_iter().collect();
    top.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    top.truncate(8);
    eprintln!(
        "files={} lines={lines} bytes={bytes} sections={sections} (nested={nested}, with doc={with_doc}, \
         disambiguated={dups}) file links={links} resolved={resolved} ({:.1}%) partial={partial} failures={} \
         extract time={extract_time:?}",
        files.len(),
        if links == 0 { 0.0 } else { 100.0 * resolved as f64 / links as f64 },
        failures.len(),
    );
    eprintln!("unresolved links by extension: {top:?}");
    let mut dirs: BTreeMap<&str, usize> = BTreeMap::new();
    for (k, n) in &samples {
        *dirs.entry(k.split(" -> ").next().unwrap_or("")).or_default() += n;
    }
    eprintln!("unresolved links by linking directory: {dirs:?}");
    eprintln!("sample: {:?}", samples.keys().take(10).collect::<Vec<_>>());
    assert!(failures.is_empty(), "{failures:#?}");
}
