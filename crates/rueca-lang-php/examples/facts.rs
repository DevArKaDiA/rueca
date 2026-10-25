//! Prints the facts of one file: `cargo run --example facts -- <path>`.
use rueca_extract::{Budget, LanguagePack};
use rueca_lang_php::PhpPack;
use rueca_model::Language;

fn main() {
    let path = std::env::args().nth(1).expect("usage: facts <path>");
    let src = std::fs::read(&path).expect("read");
    let lang = Language::from_path(&path).expect("language");
    let facts = PhpPack::default().extract(&path, lang, &src, &Budget::default()).expect("extract");
    for d in &facts.definitions {
        println!(
            "DEF {:?} {} parent={:?} exp={} test={} sig={:?} doc={:?} dec={:?} bases={:?} impl={:?}",
            d.label,
            d.qualified_name,
            d.parent,
            d.exported,
            d.is_test,
            d.signature,
            d.doc,
            d.decorators,
            d.bases,
            d.implements
        );
    }
    for r in &facts.references {
        println!(
            "REF {:?} {} recv={:?} enc={:?} line={}",
            r.kind,
            r.name,
            r.receiver,
            r.enclosing,
            r.span.start_line + 1
        );
    }
    for i in &facts.imports {
        println!(
            "IMP {} names={:?} alias={:?} type={} reexport={}",
            i.module, i.names, i.module_alias, i.type_only, i.reexport
        );
    }
    println!("errors={} lines={}", facts.parse_errors, facts.line_count);
}
