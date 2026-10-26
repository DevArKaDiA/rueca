//! Lists the files the index counts as partial (`parse_errors > 0` in the
//! facts) with the lines of their parse errors:
//! `cargo run -p rueca-lang-cpp --example partial -- [--c] <paths…>`
//! (every path as C++ unless `--c`; `.h` follows the flag, like the bench).
use rueca_extract::{Budget, LanguagePack};
use rueca_lang_cpp::CppPack;
use rueca_model::Language;

fn main() {
    let mut lang = Language::Cpp;
    let (mut files, mut partial) = (0usize, 0usize);
    for path in std::env::args().skip(1) {
        if path == "--c" {
            lang = Language::C;
            continue;
        }
        let src = std::fs::read(&path).expect("read");
        files += 1;
        let facts = CppPack::default().extract(&path, lang, &src, &Budget::default()).expect("extract");
        if facts.parse_errors == 0 {
            continue;
        }
        partial += 1;
        println!("=== {path} ({} errors)", facts.parse_errors);
        let offsets = rueca_lang_cpp::parse_error_offsets(&src, lang).expect("offsets");
        let parsed = rueca_lang_cpp::preprocessed(&src, lang).expect("preprocess");
        for off in offsets.iter().take(40) {
            let line = src[..*off].iter().filter(|b| **b == b'\n').count();
            let text = src.split(|b| *b == b'\n').nth(line).unwrap_or(b"");
            let seen = parsed.split(|b| *b == b'\n').nth(line).unwrap_or(b"");
            println!("{}: {}", line + 1, String::from_utf8_lossy(text).trim());
            if seen != text {
                println!("   parsed as: {}", String::from_utf8_lossy(seen).trim());
            }
        }
    }
    println!("{partial} of {files} files partial");
}
