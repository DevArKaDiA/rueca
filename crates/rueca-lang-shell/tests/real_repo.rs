//! Real-repository smoke test. Ignored by default; run with
//!
//! ```sh
//! RUECA_SHELL_REPO=/path/to/checkout cargo test -p rueca-lang-shell --test real_repo -- --ignored --nocapture
//! ```
//!
//! With `RUECA_SHELL_SHOW_ERRORS=1` it also prints the first parse error of
//! every file that has one (`ERR path:line: source line`).
//!
//! Extracts every shell file under the directory (by extension, shell
//! start-up file name, or `#!` line for extension-less files; hidden
//! directories, `vendor/` and `node_modules/` skipped), checks the
//! extractor's invariants and determinism, and prints totals, the `source`
//! imports that resolve to a file of the checkout, env reads and
//! providers, the share of files with parse errors (bash and zsh apart) and
//! timing. Cross-file resolution is tested (not ignored) in
//! `rueca-resolve/tests/shell.rs`.
//!
//! Recorded on 2026-11-25 (extractor version 1): nvm v0.41.0 — 9 files, 0
//! with parse errors; acme.sh v3.1.7 — 304 files, 0 with parse errors;
//! bats-core v1.13.0 — 293 files, 1 with parse errors (a fixture written to
//! fail); ohmyzsh at `799f9a8` — 497 files, see `docs/how-it-works.md`
//! (*Shell*) for the zsh share.

use std::path::{Path, PathBuf};
use std::time::Instant;

use rueca_extract::{Budget, LanguagePack, ProjectFiles};
use rueca_lang_shell::ShellPack;
use rueca_model::{FileFacts, Import, Label, Language, RefKind};

fn is_shell(path: &Path) -> bool {
    let rel = path.to_string_lossy();
    if Language::from_path(&rel) == Some(Language::Shell) {
        return true;
    }
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if name.trim_start_matches('.').contains('.') {
        return false;
    }
    let head = std::fs::read(path).map(|b| b.into_iter().take(256).collect::<Vec<u8>>()).unwrap_or_default();
    Language::from_shebang(&head) == Some(Language::Shell)
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>, all: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if path.is_dir() {
            if !name.starts_with('.') && !matches!(name, "vendor" | "node_modules") {
                walk(&path, out, all);
            }
            continue;
        }
        all.push(path.clone());
        if is_shell(&path) {
            out.push(path);
        }
    }
}

/// Prints the line of the first parse error of a file.
fn print_first_error(p: &str, source: &[u8]) {
    let language = tree_sitter_bash::LANGUAGE.into();
    let mut parser = rueca_extract::tree_sitter::Parser::new();
    if parser.set_language(&language).is_err() {
        return;
    }
    let source = &rueca_lang_shell::repaired_source(source)[..];
    let Some(tree) = parser.parse(source, None) else { return };
    let mut stack = vec![tree.root_node()];
    while let Some(n) = stack.pop() {
        if n.is_error() || n.is_missing() {
            let row = n.start_position().row;
            let line = String::from_utf8_lossy(source).lines().nth(row).unwrap_or("").trim().to_string();
            eprintln!("ERR {p}:{}: {line}", row + 1);
            return;
        }
        if n.has_error() {
            let mut c = n.walk();
            let children: Vec<_> = n.children(&mut c).collect();
            stack.extend(children.into_iter().rev());
        }
    }
}

#[derive(Default)]
struct Totals {
    files: usize,
    zsh_files: usize,
    with_errors: usize,
    zsh_with_errors: usize,
    functions: usize,
    tests: usize,
    calls: usize,
    script_runs: usize,
    env_reads: usize,
    providers: usize,
    imports: usize,
    imports_resolved: usize,
}

impl Totals {
    fn record(&mut self, p: &str, f: &FileFacts, project: &ProjectFiles) {
        let zsh = p.ends_with(".zsh") || p.ends_with(".zsh-theme");
        self.files += 1;
        self.zsh_files += usize::from(zsh);
        if f.parse_errors > 0 {
            self.with_errors += 1;
            self.zsh_with_errors += usize::from(zsh);
        }
        for d in &f.definitions {
            assert!(
                d.span.start_byte <= d.name_span.start_byte && d.name_span.end_byte <= d.span.end_byte,
                "{p}: {d:?}"
            );
            if let Some(parent) = d.parent {
                assert!((parent as usize) < f.definitions.len(), "{p}: dangling parent");
            }
            match d.label {
                Label::Function => self.functions += 1,
                Label::Test => self.tests += 1,
                Label::EnvVar => self.providers += 1,
                _ => {}
            }
        }
        for r in &f.references {
            assert!(r.enclosing.is_none_or(|e| (e as usize) < f.definitions.len()), "{p}: dangling enclosing");
            match (r.kind, r.receiver.as_deref()) {
                (RefKind::Call, Some(rueca_lang_shell::SCRIPT_RECEIVER)) => self.script_runs += 1,
                (RefKind::Call, _) => self.calls += 1,
                (RefKind::EnvVar, _) => self.env_reads += 1,
                _ => {}
            }
        }
        self.imports += f.imports.len();
        self.imports_resolved +=
            f.imports.iter().filter(|i: &&Import| ShellPack.resolve_import(p, i, project).is_some()).count();
    }
}

#[test]
#[ignore = "needs RUECA_SHELL_REPO pointing at a checkout with shell scripts"]
fn extract_real_repository() {
    let Ok(root) = std::env::var("RUECA_SHELL_REPO") else {
        eprintln!("RUECA_SHELL_REPO not set; skipping");
        return;
    };
    let root = PathBuf::from(root);
    let (mut files, mut all) = (Vec::new(), Vec::new());
    walk(&root, &mut files, &mut all);
    assert!(!files.is_empty(), "no shell files under {}", root.display());
    let rel = |p: &Path| p.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/");
    let project = ProjectFiles { paths: all.iter().map(|p| rel(p)).collect(), ..Default::default() };
    let show_errors = std::env::var("RUECA_SHELL_SHOW_ERRORS").is_ok();
    let budget = Budget::default();
    let mut totals = Totals::default();
    let started = Instant::now();
    for path in &files {
        let p = rel(path);
        let source = std::fs::read(path).unwrap();
        match ShellPack.extract(&p, Language::Shell, &source, &budget) {
            Ok(f) => {
                assert_eq!(
                    ShellPack.extract(&p, Language::Shell, &source, &budget).unwrap(),
                    f,
                    "{p}: not deterministic"
                );
                if show_errors && f.parse_errors > 0 {
                    print_first_error(&p, &source);
                }
                totals.record(&p, &f, &project);
            }
            Err(e) => eprintln!("SKIP {p}: {e}"),
        }
    }
    let t = &totals;
    let pct = |a: usize, b: usize| if b == 0 { 0.0 } else { 100.0 * a as f64 / b as f64 };
    eprintln!(
        "{} files ({} zsh) in {:?}; parse errors: {} files ({:.1}%), zsh {} ({:.1}%), bash/sh {} ({:.1}%)",
        t.files,
        t.zsh_files,
        started.elapsed(),
        t.with_errors,
        pct(t.with_errors, t.files),
        t.zsh_with_errors,
        pct(t.zsh_with_errors, t.zsh_files),
        t.with_errors - t.zsh_with_errors,
        pct(t.with_errors - t.zsh_with_errors, t.files - t.zsh_files)
    );
    eprintln!(
        "functions {} tests {} calls {} script runs {} env reads {} providers {} imports {} ({} resolved)",
        t.functions, t.tests, t.calls, t.script_runs, t.env_reads, t.providers, t.imports, t.imports_resolved
    );
}
