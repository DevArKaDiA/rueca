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

use rueca_extract::{Budget, LanguagePack, ProjectFiles};
use rueca_lang_java::{JavaPack, declared_package, source_roots};
use rueca_model::{FileFacts, Language, RefKind};
use rueca_resolve::ResolveOutput;

use common::*;

/// Totals over the extracted files.
#[derive(Default)]
struct Totals {
    defs: usize,
    refs: usize,
    calls: usize,
    typed: usize,
    tests: usize,
    errors: usize,
    errored_files: usize,
    lines: usize,
    bytes: usize,
    imports: usize,
    imports_in_project: usize,
    packages_declared: usize,
}

/// The extractor's invariants on one file: names inside spans, parents and
/// enclosing definitions in range.
fn check_invariants(p: &str, f: &FileFacts) {
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
}

impl Totals {
    fn add(&mut self, p: &str, f: &FileFacts, pack: &JavaPack, project: &ProjectFiles) {
        self.defs += f.definitions.len();
        self.refs += f.references.len();
        self.calls += f.references.iter().filter(|r| matches!(r.kind, RefKind::Call | RefKind::New)).count();
        self.typed +=
            f.references.iter().filter(|r| r.receiver.as_deref().is_some_and(|x| x.starts_with("$type:"))).count();
        self.tests += f.definitions.iter().filter(|d| d.is_test).count();
        self.errors += f.parse_errors as usize;
        self.errored_files += usize::from(f.parse_errors > 0);
        self.lines += f.line_count as usize;
        self.packages_declared += usize::from(declared_package(f).is_some());
        check_invariants(p, f);
        for i in f.imports.iter().filter(|i| !i.names.is_empty()) {
            self.imports += 1;
            self.imports_in_project += usize::from(pack.resolve_import(p, i, project).is_some());
        }
    }
}

/// Resolution totals of the snapshot: graph size, references per outcome
/// and strategy, edges per kind.
fn print_resolution(out: &ResolveOutput) {
    let s = &out.stats;
    let pct = |n: u64| if s.references_total == 0 { 0.0 } else { 100.0 * n as f64 / s.references_total as f64 };
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
}

/// Asserts that every symbol named in `RUECA_JAVA_EXPECT` is in the graph.
fn check_expected(out: &ResolveOutput) {
    let Ok(expect) = std::env::var("RUECA_JAVA_EXPECT") else { return };
    let g = G(&out.graph);
    for qn in expect.split(',').map(str::trim).filter(|q| !q.is_empty()) {
        let found = g.0.nodes.iter().any(|n| {
            n.qualified_name == qn || n.qualified_name.strip_prefix(qn).is_some_and(|rest| rest.starts_with('('))
        });
        eprintln!("expected symbol {qn}: {}", if found { "found" } else { "MISSING" });
        assert!(found, "missing {qn}");
    }
}

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
    let mut t = Totals::default();
    let mut failures = Vec::new();
    let start = Instant::now();
    for (p, source) in &java {
        t.bytes += source.len();
        match pack.extract(p, Language::Java, source, &budget) {
            Ok(f) => {
                t.add(p, &f, &pack, &project);
                assert_eq!(pack.extract(p, Language::Java, source, &budget).unwrap(), f, "{p}: nondeterministic");
            }
            Err(e) => failures.push(format!("{p}: {e}")),
        }
    }
    let extract_time = start.elapsed();

    let started = Instant::now();
    let out = build(all.clone());
    let resolve_time = started.elapsed();

    eprintln!("root={}", root.display());
    eprintln!("source roots: {:?}", source_roots(&project));
    eprintln!(
        "files={} lines={} bytes={} defs={} tests={} refs={} calls={} typed_receivers={} \
         package_clauses={} parse_errors={} in {} files ({:.2}% of files) failures={}",
        java.len(),
        t.lines,
        t.bytes,
        t.defs,
        t.tests,
        t.refs,
        t.calls,
        t.typed,
        t.packages_declared,
        t.errors,
        t.errored_files,
        100.0 * t.errored_files as f64 / java.len() as f64,
        failures.len()
    );
    eprintln!(
        "imports={} in_project={} ({:.1}%)",
        t.imports,
        t.imports_in_project,
        100.0 * t.imports_in_project as f64 / t.imports.max(1) as f64
    );
    print_resolution(&out);
    eprintln!("time: extract {extract_time:?} (x2 for the determinism re-run), resolve {resolve_time:?}");

    check_expected(&out);
    assert!(failures.is_empty(), "{failures:#?}");
}
