//! Grammar comparison over a directory tree: files with at least one
//! ERROR / MISSING node under tree-sitter-bash (with the pack's repairs)
//! and under tree-sitter-zsh (raw text), per file kind:
//! `cargo run --release -p rueca-lang-shell --example grammars -- <dir> [--files] [--facts]`.
//!
//! With `--facts`, every file both grammars read without an error is
//! extracted through each (`extract_with`) and the facts compared:
//! definitions (label, name, lines), references (kind, name, receiver,
//! enclosing definition, start) and imports must be the same whatever the
//! grammar; differences are printed.

use std::collections::BTreeMap;
use std::path::Path;

use rueca_extract::tree_sitter::{Language, Parser};

fn walk(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.filter_map(Result::ok).map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
        if p.is_dir() {
            if name != ".git" && name != "node_modules" && name != "vendor" {
                walk(&p, out);
            }
        } else if rueca_model::Language::from_path(&p.to_string_lossy()) == Some(rueca_model::Language::Shell) {
            out.push(p);
        }
    }
}

fn errors(lang: &Language, src: &[u8]) -> bool {
    let mut parser = Parser::new();
    parser.set_language(lang).expect("grammar");
    parser.parse(src, None).is_none_or(|t| t.root_node().has_error())
}

/// The facts of a file as comparable lines.
fn fact_lines(rel: &str, bytes: &[u8], grammar: rueca_lang_shell::Grammar) -> Vec<String> {
    let budget = rueca_extract::Budget::default();
    let Ok(f) = rueca_lang_shell::extract_with(rel, bytes, grammar, &budget) else { return vec!["<error>".into()] };
    let mut out = Vec::new();
    for d in &f.definitions {
        let parent = d.parent.map(|p| f.definitions[p as usize].qualified_name.clone());
        out.push(format!(
            "def {:?} {} {}-{} parent={parent:?} sig={:?}",
            d.label, d.qualified_name, d.span.start_line, d.span.end_line, d.signature
        ));
    }
    for r in &f.references {
        let enc = r.enclosing.map(|e| f.definitions[e as usize].qualified_name.clone());
        out.push(format!(
            "ref {:?} {} recv={:?} in={enc:?} @{}:{}",
            r.kind, r.name, r.receiver, r.span.start_line, r.span.start_col
        ));
    }
    for i in &f.imports {
        out.push(format!("import {} @{}", i.module, i.span.start_line));
    }
    out.sort();
    out
}

/// Extracts a file through each grammar and prints the facts only one
/// gives; whether they differ.
fn facts_differ(rel: &str, bytes: &[u8]) -> bool {
    let a = fact_lines(rel, bytes, rueca_lang_shell::Grammar::Bash);
    let b = fact_lines(rel, bytes, rueca_lang_shell::Grammar::Zsh);
    if a == b {
        return false;
    }
    println!("DIFF {rel}");
    for l in a.iter().filter(|l| !b.contains(l)).take(6) {
        println!("  bash-only {l}");
    }
    for l in b.iter().filter(|l| !a.contains(l)).take(6) {
        println!("  zsh-only  {l}");
    }
    true
}

/// The two grammars and the counts of a run.
struct Run {
    bash: Language,
    zsh: Language,
    list: bool,
    facts: bool,
    compared: u32,
    differing: u32,
    /// kind -> (files, bash raw, bash repaired, zsh raw)
    by: BTreeMap<String, [u32; 4]>,
}

impl Run {
    fn file(&mut self, rel: &str, bytes: &[u8]) {
        let ext = rel.rsplit('.').next().unwrap_or("").to_string();
        let repaired = rueca_lang_shell::repaired_source(bytes, rueca_lang_shell::Grammar::Bash);
        let r = [errors(&self.bash, bytes), errors(&self.bash, &repaired), errors(&self.zsh, bytes)];
        let e = self.by.entry(ext).or_default();
        e[0] += 1;
        for (k, v) in r.iter().enumerate() {
            e[k + 1] += u32::from(*v);
        }
        if self.list {
            println!("{rel}\tbash={} repaired={} zsh={}", r[0], r[1], r[2]);
        }
        let zsh_repaired = rueca_lang_shell::repaired_source(bytes, rueca_lang_shell::Grammar::Zsh);
        if self.facts && !r[1] && !errors(&self.zsh, &zsh_repaired) {
            self.compared += 1;
            self.differing += u32::from(facts_differ(rel, bytes));
        }
    }
}

fn main() {
    let root = std::env::args().nth(1).expect("usage: grammars <dir> [--files]");
    let mut files = Vec::new();
    walk(Path::new(&root), &mut files);
    let mut run = Run {
        bash: tree_sitter_bash::LANGUAGE.into(),
        zsh: tree_sitter_zsh::LANGUAGE.into(),
        list: std::env::args().any(|a| a == "--files"),
        facts: std::env::args().any(|a| a == "--facts"),
        compared: 0,
        differing: 0,
        by: BTreeMap::new(),
    };
    for f in files {
        let Ok(bytes) = std::fs::read(&f) else { continue };
        let rel = f.strip_prefix(&root).unwrap_or(&f).to_string_lossy().to_string();
        run.file(&rel, &bytes);
    }
    if run.facts {
        println!("facts compared on {} files read by both grammars: {} differ", run.compared, run.differing);
    }
    for (k, v) in run.by {
        println!("{k}\tfiles {}\tbash {}\tbash+repairs {}\tzsh {}", v[0], v[1], v[2], v[3]);
    }
}
