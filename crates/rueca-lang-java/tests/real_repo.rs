//! Real-repository smoke test. Ignored by default; run with
//!
//! ```sh
//! RUECA_JAVA_REPO=/path/to/checkout \
//! RUECA_JAVA_EXPECT=com.google.gson.Gson.toJson,com.google.gson.JsonParser \
//!   cargo test -p rueca-lang-java --test real_repo -- --ignored --nocapture
//! ```
//!
//! Extracts every `.java` file under the directory (skipping hidden
//! directories, `target/` and `build/`), checks the extractor's invariants
//! and determinism, resolves the snapshot with `rueca-resolve` and prints
//! totals, parse errors, import resolution, the share of references
//! resolved per strategy, timing, and whether the symbols named in
//! `RUECA_JAVA_EXPECT` (global qualified names, overload suffix optional)
//! were found.

mod common;

use std::path::PathBuf;
use std::time::Instant;

use rueca_extract::{Budget, LanguagePack};
use rueca_lang_java::{JavaPack, declared_package, source_roots};
use rueca_model::{Language, RefKind};

use common::*;

#[test]
#[ignore = "needs RUECA_JAVA_REPO pointing at a Java checkout"]
fn extract_and_resolve_real_repository() {
    let Ok(root) = std::env::var("RUECA_JAVA_REPO") else {
        eprintln!("RUECA_JAVA_REPO not set; skipping");
        return;
    };
    let root = PathBuf::from(root);
    let mut all = Vec::new();
    walk(&root, &root, &mut all);
    let project = project(&all);
    let java: Vec<&(String, Vec<u8>)> = all.iter().filter(|(p, _)| p.ends_with(".java")).collect();
    assert!(!java.is_empty(), "no Java files under {}", root.display());

    let pack = JavaPack::default();
    let budget = Budget::default();
    let (mut defs, mut refs, mut calls, mut typed, mut tests, mut errors, mut errored_files, mut lines, mut bytes) =
        (0, 0, 0, 0, 0, 0, 0, 0, 0);
    let (mut imports, mut imports_in_project, mut packages_declared) = (0, 0, 0);
    let mut failures = Vec::new();
    let start = Instant::now();
    for (p, source) in &java {
        bytes += source.len();
        match pack.extract(p, Language::Java, source, &budget) {
            Ok(f) => {
                defs += f.definitions.len();
                refs += f.references.len();
                calls += f.references.iter().filter(|r| matches!(r.kind, RefKind::Call | RefKind::New)).count();
                typed += f
                    .references
                    .iter()
                    .filter(|r| r.receiver.as_deref().is_some_and(|x| x.starts_with("$type:")))
                    .count();
                tests += f.definitions.iter().filter(|d| d.is_test).count();
                errors += f.parse_errors as usize;
                errored_files += usize::from(f.parse_errors > 0);
                lines += f.line_count as usize;
                packages_declared += usize::from(declared_package(&f).is_some());
                for d in &f.definitions {
                    assert!(d.span.contains(&d.name_span), "{p}: {} name outside span", d.qualified_name);
                    if let Some(parent) = d.parent {
                        assert!((parent as usize) < f.definitions.len(), "{p}: bad parent");
                    }
                }
                for r in &f.references {
                    if let Some(e) = r.enclosing {
                        assert!((e as usize) < f.definitions.len(), "{p}: bad enclosing");
                    }
                }
                for i in f.imports.iter().filter(|i| !i.names.is_empty()) {
                    imports += 1;
                    imports_in_project += usize::from(pack.resolve_import(p, i, &project).is_some());
                }
                assert_eq!(pack.extract(p, Language::Java, source, &budget).unwrap(), f, "{p}: nondeterministic");
            }
            Err(e) => failures.push(format!("{p}: {e}")),
        }
    }
    let extract_time = start.elapsed();

    let t = Instant::now();
    let out = build(all.clone());
    let resolve_time = t.elapsed();
    let s = &out.stats;
    let pct = |n: u64| if s.references_total == 0 { 0.0 } else { 100.0 * n as f64 / s.references_total as f64 };

    eprintln!("root={}", root.display());
    eprintln!("source roots: {:?}", source_roots(&project));
    eprintln!(
        "files={} lines={lines} bytes={bytes} defs={defs} tests={tests} refs={refs} calls={calls} typed_receivers={typed} \
         package_clauses={packages_declared} parse_errors={errors} in {errored_files} files ({:.2}% of files) failures={}",
        java.len(),
        100.0 * errored_files as f64 / java.len() as f64,
        failures.len()
    );
    eprintln!(
        "imports={imports} in_project={imports_in_project} ({:.1}%)",
        100.0 * imports_in_project as f64 / imports.max(1) as f64
    );
    eprintln!(
        "graph: nodes={} edges={}; references total={} resolved={} ({:.1}%) builtin={} ({:.1}%) external={} ({:.1}%) unresolved={:.1}%",
        out.graph.nodes.len(),
        out.graph.edges.len(),
        s.references_total,
        s.references_resolved,
        pct(s.references_resolved),
        s.references_builtin,
        pct(s.references_builtin),
        s.references_external,
        pct(s.references_external),
        100.0 - pct(s.references_resolved) - pct(s.references_builtin) - pct(s.references_external)
    );
    for (strategy, n) in &s.resolved_by_strategy {
        eprintln!("  {strategy:<16} {n:>7} ({:.1}%)", pct(*n));
    }
    let mut by_kind: std::collections::BTreeMap<String, usize> = Default::default();
    for e in &out.graph.edges {
        *by_kind.entry(e.kind.as_str().to_string()).or_default() += 1;
    }
    eprintln!("edges by kind: {by_kind:?}");
    eprintln!("time: extract {extract_time:?} (x2 for the determinism re-run), resolve {resolve_time:?}");

    if let Ok(expect) = std::env::var("RUECA_JAVA_EXPECT") {
        let g = G(&out.graph);
        for qn in expect.split(',').map(str::trim).filter(|q| !q.is_empty()) {
            let found = g.0.nodes.iter().any(|n| {
                n.qualified_name == qn || n.qualified_name.strip_prefix(qn).is_some_and(|rest| rest.starts_with('('))
            });
            eprintln!("expected symbol {qn}: {}", if found { "found" } else { "MISSING" });
            assert!(found, "missing {qn}");
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}
