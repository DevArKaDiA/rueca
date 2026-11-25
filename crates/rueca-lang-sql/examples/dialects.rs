//! Statement parse-failure rates per detected dialect over a directory
//! tree (the dialect coverage table of the crate README):
//! `cargo run -p rueca-lang-sql --example dialects -- <dir> [--files] [--heads] [--heads-full <first words>]`.
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

/// Command-line options.
struct Options {
    /// `--files`: list the files with a strict error.
    list: bool,
    /// `--heads`: histogram of the head-only statements.
    heads: bool,
    /// `--heads-full "CREATE INDEX"`: print the head-only statements whose
    /// first two words start with the text.
    full: Option<String>,
}

/// Per dialect: files, files with a parse error, statements, statements
/// with an error, parse errors, statements read by the pack's own readers,
/// then the strict counts (files, statements), head-only statements and
/// statements read by productions.
type Table = BTreeMap<&'static str, [u32; 10]>;

fn options(args: &[String]) -> Options {
    let full = args.iter().position(|a| a == "--heads-full").and_then(|i| args.get(i + 1)).map(|s| s.to_uppercase());
    Options {
        list: args.iter().any(|a| a == "--files"),
        heads: full.is_some() || args.iter().any(|a| a == "--heads"),
        full,
    }
}

fn count(by: &mut Table, s: &rueca_lang_sql::Stats) {
    let e = by.entry(s.dialect.as_str()).or_default();
    let add = [
        1,
        u32::from(s.unparsed > 0),
        s.statements,
        s.unparsed,
        s.errors,
        s.own,
        u32::from(s.unparsed_strict > 0),
        s.unparsed_strict,
        s.head_only,
        s.productions,
    ];
    for (total, n) in e.iter_mut().zip(add) {
        *total += n;
    }
}

fn count_heads(opts: &Options, rel: &str, bytes: &[u8], head_words: &mut BTreeMap<String, u32>) {
    for h in rueca_lang_sql::head_only_statements(rel, bytes) {
        let key = h.split_whitespace().take(2).collect::<Vec<_>>().join(" ").to_ascii_uppercase();
        if opts.full.as_ref().is_some_and(|f| key.starts_with(f.as_str())) {
            println!("{rel}: {h}");
        }
        *head_words.entry(key).or_default() += 1;
    }
}

fn file(opts: &Options, root: &str, f: &Path, by: &mut Table, head_words: &mut BTreeMap<String, u32>) {
    let Ok(bytes) = std::fs::read(f) else { return };
    if bytes.len() > 2 * 1024 * 1024 {
        return;
    }
    let rel = f.strip_prefix(root).unwrap_or(f).to_string_lossy().to_string();
    let s = rueca_lang_sql::stats(&rel, &bytes);
    count(by, &s);
    if opts.list && s.unparsed_strict > 0 {
        println!("{} {} strict {}/{} lenient {}", s.dialect.as_str(), rel, s.unparsed_strict, s.statements, s.unparsed);
    }
    if opts.heads && s.head_only > 0 {
        count_heads(opts, &rel, &bytes, head_words);
    }
}

fn pct(n: u32, d: u32) -> f64 {
    if d > 0 { 100.0 * f64::from(n) / f64::from(d) } else { 0.0 }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let root = args.get(1).expect("usage: dialects <dir> [--files] [--heads] [--heads-full <first words>]");
    let opts = options(&args);
    let mut files = Vec::new();
    walk(Path::new(root), &mut files);
    let mut head_words: BTreeMap<String, u32> = BTreeMap::new();
    let mut by = Table::new();
    for f in files {
        file(&opts, root, &f, &mut by, &mut head_words);
    }
    for (d, [files, ferr, st, un, errors, own, ferr_strict, un_strict, head_only, productions]) in by {
        println!(
            "{d}\tfiles {files}\tstrict: files with errors {ferr_strict} ({:.1}%) statements {un_strict}/{st} ({:.1}%) head-only {head_only}\tlenient: files {ferr} ({:.1}%) statements {un} errors {errors}\town readers {own} productions {productions}",
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
