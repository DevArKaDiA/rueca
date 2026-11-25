//! Statement parse-failure rates per detected dialect over a directory
//! tree (the dialect coverage table of the crate README):
//! `cargo run -p rueca-lang-sql --example dialects -- <dir> [--files] [--heads]`.
//! Two rates: *strict* (a statement accepted by its head alone counts as a
//! parse error; what `Facts::parse_errors` and the health metric use) and
//! *lenient* (only statements nothing reads but the token scanner).

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
    // dialect -> (files, files with a parse error, statements, statements
    // with an error, parse errors, statements read by the pack's own readers)
    let heads = std::env::args().any(|a| a == "--heads");
    let mut head_words: BTreeMap<String, u32> = BTreeMap::new();
    let mut by: BTreeMap<&'static str, (u32, u32, u32, u32, u32, u32, u32, u32, u32)> = BTreeMap::new();
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
        e.4 += s.errors;
        e.5 += s.own;
        e.6 += u32::from(s.unparsed_strict > 0);
        e.7 += s.unparsed_strict;
        e.8 += s.head_only;
        if list && s.unparsed_strict > 0 {
            println!(
                "{} {} strict {}/{} lenient {}",
                s.dialect.as_str(),
                rel,
                s.unparsed_strict,
                s.statements,
                s.unparsed
            );
        }
        if heads && s.head_only > 0 {
            for h in rueca_lang_sql::head_only_statements(&rel, &bytes) {
                *head_words.entry(h).or_default() += 1;
            }
        }
    }
    for (d, (files, ferr, st, un, errors, own, ferr_strict, un_strict, head_only)) in by {
        let pct = |n: u32, d: u32| if d > 0 { 100.0 * f64::from(n) / f64::from(d) } else { 0.0 };
        println!(
            "{d}\tfiles {files}\tstrict: files with errors {ferr_strict} ({:.1}%) statements {un_strict}/{st} ({:.1}%) head-only {head_only}\tlenient: files {ferr} ({:.1}%) statements {un} errors {errors}\town readers {own}",
            pct(ferr_strict, files),
            pct(un_strict, st),
            pct(ferr, files),
        );
    }
    let mut words: Vec<_> = head_words.into_iter().collect();
    words.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    for (w, n) in words {
        println!("head-only\t{n}\t{w}");
    }
}
