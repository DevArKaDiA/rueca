//! Statements no dialect parses under a directory tree, grouped by the
//! parser error of the file's dialect (for diagnosing coverage):
//! `cargo run -p rueca-lang-sql --example unparsed -- <dir> [--full] [--grep <text>]`.

use std::collections::BTreeMap;
use std::path::Path;

fn walk(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.filter_map(Result::ok).map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.file_name().is_some_and(|n| n == ".git") {
            continue;
        }
        if p.is_dir() {
            walk(&p, out);
        } else if rueca_model::Language::from_path(&p.to_string_lossy()) == Some(rueca_model::Language::Sql) {
            out.push(p);
        }
    }
}

/// Statements per error class: count, files, first sample.
type Classes = BTreeMap<String, (u32, u32, String)>;

/// The text shown for one unparsed statement: all of it, or the line the
/// parser stopped at.
fn sample(rel: &str, text: &str, at: &str, full: bool) -> String {
    if full {
        return format!("{rel} [{at}]\n{}", text.trim());
    }
    let line: usize = at.split(',').next().and_then(|l| l.trim().parse().ok()).unwrap_or(1);
    let l = text.lines().nth(line.saturating_sub(1)).unwrap_or("").trim();
    format!("{rel} [{at}] {}", l.chars().take(160).collect::<String>())
}

fn file(root: &str, f: &Path, full: bool, grep: Option<&str>, by: &mut Classes) {
    let Ok(bytes) = std::fs::read(f) else { return };
    if bytes.len() > 2 * 1024 * 1024 {
        return;
    }
    let rel = f.strip_prefix(root).unwrap_or(f).to_string_lossy().to_string();
    let mut seen = std::collections::BTreeSet::new();
    for (text, err) in rueca_lang_sql::unparsed_statements(&rel, &bytes) {
        let class = err.split(" at Line").next().unwrap_or("").to_string();
        if grep.is_some_and(|g| !class.contains(g) && !text.contains(g)) {
            continue;
        }
        let at = err.split(" at Line: ").nth(1).unwrap_or("").to_string();
        let sample = sample(&rel, &text, &at, full);
        if full {
            println!("### {class}\n{sample}\n");
        }
        let e = by.entry(class.clone()).or_insert((0, 0, sample));
        e.0 += 1;
        e.1 += u32::from(seen.insert(class));
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let root = args.get(1).expect("usage: unparsed <dir> [--full] [--grep <text>]");
    let full = args.iter().any(|a| a == "--full");
    let grep = args.iter().position(|a| a == "--grep").and_then(|i| args.get(i + 1)).cloned();
    let mut files = Vec::new();
    walk(Path::new(root), &mut files);
    let mut by = Classes::new();
    for f in files {
        file(root, &f, full, grep.as_deref(), &mut by);
    }
    if full {
        return;
    }
    let mut rows: Vec<_> = by.into_iter().collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.1.0));
    for (class, (n, files, sample)) in rows {
        println!("{n}\t{files} files\t{class}\n\t\t{sample}");
    }
}
