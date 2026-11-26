//! Statement parse-failure rates per detected dialect over a directory
//! tree (the dialect coverage table of the crate README):
//! `cargo run -p rueca-lang-sql --example dialects -- <dir> [--files]`.

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
    let root = std::env::args().nth(1).expect("usage: dialects <dir> [--files]");
    let list = std::env::args().any(|a| a == "--files");
    let mut files = Vec::new();
    walk(Path::new(&root), &mut files);
    // dialect -> (files, files with an unparsed statement, statements, unparsed)
    let mut by: BTreeMap<&'static str, (u32, u32, u32, u32)> = BTreeMap::new();
    for f in files {
        let Ok(bytes) = std::fs::read(&f) else { continue };
        if bytes.len() > 2 * 1024 * 1024 {
            continue;
        }
        let rel = f.strip_prefix(&root).unwrap_or(&f).to_string_lossy().to_string();
        let s = rueca_lang_sql::stats(&rel, &bytes);
        let e = by.entry(s.dialect.as_str()).or_default();
        e.0 += 1;
        e.1 += u32::from(s.unparsed > 0);
        e.2 += s.statements;
        e.3 += s.unparsed;
        if list && s.unparsed > 0 {
            println!("{} {} {}/{}", s.dialect.as_str(), rel, s.unparsed, s.statements);
        }
    }
    for (d, (files, ferr, st, un)) in by {
        let pct = if st > 0 { 100.0 * f64::from(un) / f64::from(st) } else { 0.0 };
        println!("{d}\tfiles {files}\tfiles with unparsed {ferr}\tstatements {st}\tunparsed {un}\t{pct:.1}%");
    }
}
