//! Prints the facts the SQL pack extracts from one file:
//! `cargo run -p rueca-lang-sql --example facts -- path/to/file.sql`.

use rueca_extract::{Budget, LanguagePack};
use rueca_lang_sql::SqlPack;
use rueca_model::Language;

fn main() {
    let path = std::env::args().nth(1).expect("usage: facts <file.sql>");
    let bytes = std::fs::read(&path).expect("read");
    let facts = SqlPack.extract(&path, Language::Sql, &bytes, &Budget::default()).expect("extract");
    println!(
        "dialect {:?}, parse errors {}",
        rueca_lang_sql::detect_dialect(&path, &String::from_utf8_lossy(&bytes)),
        facts.parse_errors
    );
    for (i, d) in facts.definitions.iter().enumerate() {
        println!(
            "def {i} {:?} {} parent={:?} line {} :: {}",
            d.label,
            d.qualified_name,
            d.parent,
            d.span.start_line + 1,
            d.signature.as_deref().unwrap_or("")
        );
    }
    for r in &facts.references {
        let enc = r.enclosing.map_or("-".into(), |e| facts.definitions[e as usize].qualified_name.clone());
        println!(
            "ref {:?} {}{} <- {} line {}",
            r.kind,
            r.receiver.as_deref().map(|s| format!("{s} ")).unwrap_or_default(),
            r.name,
            enc,
            r.span.start_line + 1
        );
    }
    if std::env::var_os("UNPARSED").is_some() {
        for (stmt, err) in rueca_lang_sql::unparsed_statements(&path, &bytes) {
            println!("--- unparsed: {err}\n{stmt}");
        }
    }
    for i in &facts.imports {
        println!("import {}", i.module);
    }
}
