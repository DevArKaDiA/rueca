//! Files with parse errors per detected dialect over a directory tree (the
//! dialect coverage table of the crate docs):
//! `cargo run --release -p rueca-lang-shell --example dialects -- <dir>... [--files] [--evidence]`.
//!
//! Shell files are the ones the indexer reads: by path
//! (`Language::from_path`) or, for names without an extension, by their
//! `#!` line; `.git`, `node_modules` and `vendor` directories are skipped.
//! `--files` lists every file with an error (dialect, grammar, path);
//! `--evidence` adds the count of files per detection rule; `--tsv` prints
//! one line per file instead (path, dialect, rule, grammar, whether the
//! tree has errors, FNV-1a of the repaired text), which the bench uses to
//! check its port of these rules (`bench/src/rueca_bench/shelldialect.py`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rueca_lang_shell::{Grammar, stats};
use rueca_model::Language;

fn is_shell(path: &Path) -> bool {
    let rel = path.to_string_lossy();
    if let Some(l) = Language::from_path(&rel) {
        return l == Language::Shell;
    }
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if name.trim_start_matches('.').contains('.') {
        return false;
    }
    let mut head = [0u8; 256];
    let n = std::fs::File::open(path).and_then(|mut f| std::io::Read::read(&mut f, &mut head)).unwrap_or(0);
    Language::from_shebang(&head[..n]) == Some(Language::Shell)
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.filter_map(Result::ok).map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if p.is_symlink() {
            continue;
        }
        if p.is_dir() {
            if !matches!(name, ".git" | "node_modules" | "vendor") {
                walk(&p, out);
            }
        } else if is_shell(&p) {
            out.push(p);
        }
    }
}

/// `:line: text` of the first ERROR / MISSING node, as `grammar` reads the file.
fn first_error(bytes: &[u8], grammar: Grammar) -> String {
    let language: rueca_extract::tree_sitter::Language = match grammar {
        Grammar::Bash => tree_sitter_bash::LANGUAGE.into(),
        Grammar::Zsh => tree_sitter_zsh::LANGUAGE.into(),
    };
    let source = rueca_lang_shell::repaired_source(bytes, grammar);
    let mut parser = rueca_extract::tree_sitter::Parser::new();
    if parser.set_language(&language).is_err() {
        return String::new();
    }
    let Some(tree) = parser.parse(&source, None) else { return String::new() };
    let mut stack = vec![tree.root_node()];
    while let Some(n) = stack.pop() {
        if n.is_error() || n.is_missing() {
            let row = n.start_position().row;
            let text = String::from_utf8_lossy(&source);
            let line = text.lines().nth(row).unwrap_or("").trim();
            let kind = if n.is_missing() { "MISSING" } else { "ERROR" };
            return format!(" :{}:{} {kind} | {line}", row + 1, n.start_position().column + 1);
        }
        if n.has_error() {
            let mut c = n.walk();
            let children: Vec<_> = n.children(&mut c).collect();
            stack.extend(children.into_iter().rev());
        }
    }
    String::new()
}

/// What the command line asks for.
struct Options {
    list: bool,
    evidence: bool,
    tsv: bool,
}

/// The totals of a run: dialect -> (files, files with errors, files read
/// with the other grammar), and files per detection rule.
#[derive(Default)]
struct Totals {
    by: BTreeMap<&'static str, (u32, u32, u32)>,
    rules: BTreeMap<String, u32>,
}

fn percent(part: u32, whole: u32) -> f64 {
    if whole > 0 { 100.0 * f64::from(part) / f64::from(whole) } else { 0.0 }
}

/// The `--tsv` line of a file.
fn print_tsv(root: &str, rel: &str, bytes: &[u8], s: &rueca_lang_shell::Stats) {
    let repaired = rueca_lang_shell::repaired_source(bytes, s.grammar);
    let hash =
        repaired.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3));
    let g = if s.grammar == Grammar::Zsh { "zsh" } else { "bash" };
    let err = u8::from(s.parse_errors > 0);
    println!("{root}/{rel}\t{}\t{:?}\t{g}\t{err}\t{hash:016x}", s.dialect.as_str(), s.evidence);
}

/// Reads one file: its `--tsv` line, or its counts (and its `--files` line).
fn file(root: &str, path: &Path, options: &Options, totals: &mut Totals) {
    let Ok(bytes) = std::fs::read(path) else { return };
    if bytes.len() > 2 * 1024 * 1024 {
        return;
    }
    let rel = path.strip_prefix(root).unwrap_or(path).to_string_lossy().to_string();
    let Some(s) = stats(&rel, &bytes) else {
        println!("SKIP {rel}");
        return;
    };
    if options.tsv {
        return print_tsv(root, &rel, &bytes, &s);
    }
    let e = totals.by.entry(s.dialect.as_str()).or_default();
    e.0 += 1;
    e.1 += u32::from(s.parse_errors > 0);
    e.2 += u32::from(s.grammar != s.dialect.grammar());
    *totals.rules.entry(format!("{} by {:?}", s.dialect.as_str(), s.evidence)).or_default() += 1;
    if options.list && s.parse_errors > 0 {
        let g = if s.grammar == Grammar::Zsh { "zsh-grammar" } else { "bash-grammar" };
        println!("{} {g} {root}/{rel} {}{}", s.dialect.as_str(), s.parse_errors, first_error(&bytes, s.grammar));
    }
}

/// The table per dialect, the total, and the rules with `--evidence`.
fn print_totals(totals: Totals, evidence: bool) {
    let (mut files, mut errors) = (0, 0);
    for (d, (n, e, other)) in &totals.by {
        let pct = percent(*e, *n);
        println!("{d}\tfiles {n}\twith errors {e}\t{pct:.2}%\tother grammar {other}");
        files += n;
        errors += e;
    }
    let pct = percent(errors, files);
    println!("all\tfiles {files}\twith errors {errors}\t{pct:.2}%");
    if evidence {
        for (k, v) in totals.rules {
            println!("  {k}: {v}");
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| args.iter().any(|a| a == name);
    let options = Options { list: flag("--files"), evidence: flag("--evidence"), tsv: flag("--tsv") };
    let roots: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    assert!(!roots.is_empty(), "usage: dialects <dir>... [--files] [--evidence]");
    let mut totals = Totals::default();
    for root in roots {
        let mut files = Vec::new();
        walk(Path::new(root), &mut files);
        for f in files {
            file(root, &f, &options, &mut totals);
        }
    }
    if !options.tsv {
        print_totals(totals, options.evidence);
    }
}
