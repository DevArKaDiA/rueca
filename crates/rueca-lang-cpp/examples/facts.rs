//! Prints the facts of one file: `cargo run -p rueca-lang-cpp --example facts -- <path> [c|cpp]`.
use rueca_extract::{Budget, LanguagePack};
use rueca_lang_cpp::CppPack;
use rueca_model::Language;

fn main() {
    let path = std::env::args().nth(1).expect("usage: facts <path> [c|cpp]");
    let lang = match std::env::args().nth(2).as_deref() {
        Some("c") => Language::C,
        Some("cpp") => Language::Cpp,
        _ => Language::from_path(&path).expect("language"),
    };
    let src = std::fs::read(&path).expect("read");
    let facts = CppPack::default().extract(&path, lang, &src, &Budget::default()).expect("extract");
    for (i, d) in facts.definitions.iter().enumerate() {
        println!(
            "DEF {i} {:?} {} name={} parent={:?} exp={} test={} decl={} sig={:?} doc={:?} bases={:?} lines={}-{}",
            d.label,
            d.qualified_name,
            d.name,
            d.parent,
            d.exported,
            d.is_test,
            d.is_declaration,
            d.signature,
            d.doc,
            d.bases,
            d.span.start_line + 1,
            d.span.end_line + 1
        );
    }
    for r in &facts.references {
        println!(
            "REF {:?} {} recv={:?} enc={:?} arity={:?} line={}",
            r.kind,
            r.name,
            r.receiver,
            r.enclosing,
            r.arity,
            r.span.start_line + 1
        );
    }
    for i in &facts.imports {
        println!("IMP {} names={:?}", i.module, i.names);
    }
    println!("parse_errors={} lines={}", facts.parse_errors, facts.line_count);
}
