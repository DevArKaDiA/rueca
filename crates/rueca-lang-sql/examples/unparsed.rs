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

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let root = args.get(1).expect("usage: unparsed <dir> [--full] [--grep <text>]");
    let full = args.iter().any(|a| a == "--full");
    let grep = args.iter().position(|a| a == "--grep").and_then(|i| args.get(i + 1)).cloned();
    let mut files = Vec::new();
    walk(Path::new(root), &mut files);
    // error class -> (count, files, first sample)
    let mut by: BTreeMap<String, (u32, u32, String)> = BTreeMap::new();
    for f in files {
        let Ok(bytes) = std::fs::read(&f) else { continue };
        if bytes.len() > 2 * 1024 * 1024 {
            continue;
        }
        let rel = f.strip_prefix(root).unwrap_or(&f).to_string_lossy().to_string();
        let mut seen = std::collections::BTreeSet::new();
        for (text, err) in rueca_lang_sql::unparsed_statements(&rel, &bytes) {
            let class = err.split(" at Line").next().unwrap_or("").to_string();
            if grep.as_ref().is_some_and(|g| !class.contains(g.as_str()) && !text.contains(g.as_str())) {
                continue;
            }
            let at = err.split(" at Line: ").nth(1).unwrap_or("").to_string();
            let sample = if full {
                format!("{rel} [{at}]\n{}", text.trim())
            } else {
                let line: usize = at.split(',').next().and_then(|l| l.trim().parse().ok()).unwrap_or(1);
                let l = text.lines().nth(line.saturating_sub(1)).unwrap_or("").trim();
                format!("{rel} [{at}] {}", l.chars().take(160).collect::<String>())
            };
            if full {
                println!("### {class}\n{sample}\n");
            }
            let e = by.entry(class.clone()).or_insert((0, 0, sample));
            e.0 += 1;
            e.1 += u32::from(seen.insert(class));
        }
    }
    let mut rows: Vec<_> = by.into_iter().collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.1.0));
    for (class, (n, files, sample)) in rows {
        if !full {
            println!("{n}\t{files} files\t{class}\n\t\t{sample}");
        }
    }
}
