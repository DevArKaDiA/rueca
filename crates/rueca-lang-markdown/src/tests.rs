//! Extraction and link-resolution tests.

use super::*;
use rueca_model::{Definition, Label};

fn facts(src: &str) -> FileFacts {
    MarkdownPack.extract("README.md", Language::Markdown, src.as_bytes(), &Budget::default()).unwrap()
}

fn def<'a>(f: &'a FileFacts, qn: &str) -> &'a Definition {
    f.definitions.iter().find(|d| d.qualified_name == qn).unwrap_or_else(|| panic!("no section {qn}: {f:#?}"))
}

/// `(qualified name, parent qualified name)` per section.
fn tree(f: &FileFacts) -> Vec<(&str, Option<&str>)> {
    f.definitions
        .iter()
        .map(|d| (d.qualified_name.as_str(), d.parent.map(|p| f.definitions[p as usize].qualified_name.as_str())))
        .collect()
}

/// 1-based inclusive line range of a span.
fn lines(d: &Definition) -> (u32, u32) {
    d.span.lines_1based()
}

fn modules(f: &FileFacts) -> Vec<&str> {
    f.imports.iter().map(|i| i.module.as_str()).collect()
}

const README: &str = include_str!("../tests/fixtures/README.md");
const GUIDE: &str = include_str!("../tests/fixtures/docs/guide.md");

#[test]
fn readme_sections_tree() {
    let f = facts(README);
    assert_eq!(
        tree(&f),
        vec![
            ("Demo", None),
            ("Demo > Install", Some("Demo")),
            ("Demo > Install > Linux", Some("Demo > Install")),
            ("Demo > Install > macOS", Some("Demo > Install")),
            ("Demo > Usage", Some("Demo")),
            ("Demo > Usage notes", Some("Demo")),
            ("Demo > Usage (2)", Some("Demo")),
        ]
    );
    assert!(f.definitions.iter().all(|d| d.label == Label::Section && d.exported && !d.is_test));
    assert!(f.references.is_empty());
    assert_eq!(f.parse_errors, 0);
    assert_eq!(f.line_count, README.lines().count() as u32);
}

#[test]
fn readme_section_details() {
    let f = facts(README);
    let demo = def(&f, "Demo");
    assert_eq!(demo.name, "Demo");
    assert_eq!(demo.signature.as_deref(), Some("# Demo"));
    // Badges are skipped; the first prose paragraph is the doc.
    assert_eq!(demo.doc.as_deref(), Some("Demo is a small tool for parsing things. It has a second line."));
    // `# Demo` (line 10) runs to the end of the file (last non-blank line).
    assert_eq!(lines(demo), (10, README.lines().count() as u32));

    let install = def(&f, "Demo > Install");
    assert_eq!(install.doc.as_deref(), Some("See the guide and the API."));
    // Up to the line before `## Usage`, trailing blank line trimmed.
    assert_eq!(lines(install), (18, 31));
    let linux = def(&f, "Demo > Install > Linux");
    assert_eq!(linux.name, "Linux");
    assert_eq!(linux.signature.as_deref(), Some("### Linux"));
    assert_eq!(linux.doc, None, "code fences never become docs");
    assert_eq!(lines(linux), (22, 27));
    let name = &README[linux.name_span.start_byte as usize..linux.name_span.end_byte as usize];
    assert_eq!(name, "Linux");
    assert!(linux.span.contains(&linux.name_span));
    assert_eq!(def(&f, "Demo > Install > macOS").doc.as_deref(), Some("Use Homebrew or read https://example.com."));

    let usage = def(&f, "Demo > Usage");
    assert_eq!(usage.doc.as_deref(), Some("Run demo --help. Contact us. Jump to install."));
    let setext = def(&f, "Demo > Usage notes");
    assert_eq!(setext.signature.as_deref(), Some("## Usage notes"));
    assert_eq!(setext.name_span.start_line, setext.span.start_line);
    let dup = def(&f, "Demo > Usage (2)");
    assert_eq!(dup.name, "Usage");
    assert!(dup.doc.as_deref().unwrap().starts_with("Second section"));
    // Quoted / listed headings are content of the last section.
    assert!(f.definitions.iter().all(|d| !d.name.contains("Quoted") && !d.name.contains("Listed")));
    assert_eq!(lines(dup).1, README.lines().count() as u32);
}

#[test]
fn readme_links() {
    let f = facts(README);
    assert_eq!(
        modules(&f),
        vec![
            "assets/logo.svg",
            "LICENSE",
            "docs/guide.md#install",
            "docs/api.md",
            "docs/img/flow.png",
            "src/main.py",
            "docs/",
            "CONTRIBUTING.md",
        ]
    );
    for i in &f.imports {
        assert!(i.names.is_empty() && i.relative_level == 0 && !i.reexport && !i.type_only);
    }
    // Evidence span = the link (`[guide](docs/guide.md#install)` is on line 20).
    let guide = f.imports.iter().find(|i| i.module.starts_with("docs/guide.md")).unwrap();
    assert_eq!(guide.span.start_line + 1, 20);
    assert_eq!(&README[guide.span.start_byte as usize..guide.span.end_byte as usize], "[guide](docs/guide.md#install)");
}

#[test]
fn guide_nesting_and_links() {
    let f = MarkdownPack.extract("docs/guide.md", Language::Markdown, GUIDE.as_bytes(), &Budget::default()).unwrap();
    assert_eq!(
        tree(&f),
        vec![
            ("Guide", None),
            ("Guide > Install", Some("Guide")),
            ("Guide > Install > Linux", Some("Guide > Install")),
            ("Appendix", None),
        ]
    );
    assert_eq!(lines(def(&f, "Guide")), (1, 9));
    assert_eq!(modules(&f), vec!["../README.md", "/", "api.html", "my%20notes.md", "../../outside.md"]);
}

#[test]
fn duplicates_and_levels() {
    let f = facts("# A\n## B\n## B\n### C\n## B\n# A\n## B\n### Z\n#### Y\n# A (2)\n");
    assert_eq!(
        tree(&f),
        vec![
            ("A", None),
            ("A > B", Some("A")),
            ("A > B (2)", Some("A")),
            ("A > B (2) > C", Some("A > B (2)")),
            ("A > B (3)", Some("A")),
            ("A (2)", None),
            ("A (2) > B", Some("A (2)")),
            ("A (2) > B > Z", Some("A (2) > B")),
            ("A (2) > B > Z > Y", Some("A (2) > B > Z")),
            ("A (2) (2)", None),
        ]
    );
    // A skipped level nests directly; a lower level before the first H1 is top-level.
    let f = facts("## Early\n# Top\n### Deep\n");
    assert_eq!(tree(&f), vec![("Early", None), ("Top", None), ("Top > Deep", Some("Top"))]);
}

#[test]
fn heading_text_rules() {
    let f = facts(
        "# The `run` *command* [docs](x.md)\n\n# ![Logo](logo.svg)\n\n# ![badge](b.svg) Rueca\n\n#\n\n## Closing ##\n",
    );
    let names: Vec<&str> = f.definitions.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names, ["The run command docs", "Logo", "Rueca", "Closing"]);
    assert_eq!(f.definitions[0].signature.as_deref(), Some("# The `run` *command* [docs](x.md)"));
    assert_eq!(f.definitions[3].signature.as_deref(), Some("## Closing"));
    // The empty `#` heading is skipped and does not end `Rueca`.
    assert_eq!(lines(&f.definitions[2]), (5, 9));
    assert_eq!(f.definitions[3].parent, Some(2));
    assert_eq!(modules(&f), ["x.md", "logo.svg", "b.svg"]);
}

#[test]
fn front_matter_is_skipped() {
    let f = facts("---\ntitle: X\n# not a heading\n---\n# Real\nbody\n");
    assert_eq!(tree(&f), vec![("Real", None)]);
    assert_eq!(lines(&f.definitions[0]), (5, 6));
    let f = facts("+++\ntitle = \"x\"\n+++\n\nTitle\n=====\n");
    assert_eq!(tree(&f), vec![("Title", None)]);
    assert_eq!(f.definitions[0].signature.as_deref(), Some("# Title"));
    // Unclosed: a thematic break, not front matter.
    let f = facts("---\n# A\n\ntext\n");
    assert_eq!(tree(&f), vec![("A", None)]);
    // `---` blocks after the start are breaks and setext headings, not metadata.
    let f = facts("# A\n\ntext\n\n---\nkey: v\n---\n");
    assert_eq!(tree(&f), vec![("A", None), ("A > key: v", Some("A"))]);
    // BOM.
    let f = facts("\u{feff}# Bom\n");
    assert_eq!(tree(&f), vec![("Bom", None)]);
    assert_eq!(extract::front_matter_len("---\n---\n"), 8);
}

#[test]
fn doc_is_truncated() {
    let long = "word ".repeat(300);
    let f = facts(&format!("# A\n\n{long}\n"));
    let doc = f.definitions[0].doc.as_deref().unwrap();
    assert_eq!(doc.chars().count(), MAX_DOC_CHARS);
    assert!(doc.ends_with('…'));
}

#[test]
fn links_are_deduplicated_and_filtered() {
    let f = facts(
        "[a](x.md) [b](x.md) [c](x.md#y) [d](https://e.com) [e](HTTP://E.COM) [f](//cdn/x.js) [g](#top) \
         [h](data:image/png;base64,AA) [i](tel:123) [j](<my file.md>) <a href='y.md' data-src=\"no.md\"> \
         <img srcset=\"no.png\" SRC=z.png>\n",
    );
    assert_eq!(modules(&f), ["x.md", "x.md#y", "my file.md", "y.md", "z.png"]);
}

#[test]
fn html_attribute_scanner() {
    use links::html_link_targets as t;
    assert_eq!(t(r#"<img src="a.png" alt="x">"#), ["a.png"]);
    assert_eq!(t("<a href = 'b.md'>"), ["b.md"]);
    assert_eq!(t("<a\nhref=c.md>"), ["c.md"]);
    assert_eq!(t(r#"<img data-src="no" srcset="no" src="">"#), Vec::<&str>::new());
    assert_eq!(t(r#"<img src="unterminated>"#), Vec::<&str>::new());
    assert_eq!(t("<a href"), Vec::<&str>::new());
}

fn project(paths: &[&str]) -> ProjectFiles {
    ProjectFiles { paths: paths.iter().map(|p| p.to_string()).collect(), ..Default::default() }
}

fn imp(module: &str) -> Import {
    Import {
        module: module.to_string(),
        relative_level: 0,
        names: Vec::new(),
        module_alias: None,
        type_only: false,
        reexport: false,
        span: Default::default(),
    }
}

#[test]
fn resolve_links() {
    let p = project(&[
        "README.md",
        "LICENSE",
        "src/main.py",
        "docs/README.md",
        "docs/api.md",
        "docs/guide.md",
        "docs/my notes.md",
        "docs/img/flow.png",
        "docs/sub/index.md",
        "docs/setup.md",
    ]);
    let r = |from: &str, dest: &str| MarkdownPack.resolve_import(from, &imp(dest), &p);
    let some = |s: &str| Some(s.to_string());
    // Relative to the linking file.
    assert_eq!(r("README.md", "docs/guide.md#install"), some("docs/guide.md"));
    assert_eq!(r("README.md", "./docs/api.md?plain=1"), some("docs/api.md"));
    assert_eq!(r("docs/guide.md", "../README.md"), some("README.md"));
    assert_eq!(r("docs/guide.md", "img/flow.png"), some("docs/img/flow.png"));
    assert_eq!(r("docs/guide.md", "my%20notes.md"), some("docs/my notes.md"));
    assert_eq!(r("docs/guide.md", "my notes.md"), some("docs/my notes.md"));
    // Extension and directory probing.
    assert_eq!(r("docs/guide.md", "setup"), some("docs/setup.md"));
    assert_eq!(r("docs/guide.md", "api.html#x"), some("docs/api.md"));
    assert_eq!(r("README.md", "docs/"), some("docs/README.md"));
    assert_eq!(r("README.md", "docs"), some("docs/README.md"));
    assert_eq!(r("docs/guide.md", "sub"), some("docs/sub/index.md"));
    assert_eq!(r("docs/guide.md", "./"), some("docs/README.md"));
    assert_eq!(r("docs/guide.md", "/"), some("README.md"));
    // Root-relative, and the bare-path fallback to the root.
    assert_eq!(r("docs/guide.md", "/src/main.py"), some("src/main.py"));
    assert_eq!(r("docs/guide.md", "src/main.py"), some("src/main.py"));
    assert_eq!(r("docs/guide.md", "./src/main.py"), None, "explicit ./ never falls back");
    assert_eq!(r("docs/guide.md", "LICENSE"), some("LICENSE"));
    // Misses.
    assert_eq!(r("docs/guide.md", "../../outside.md"), None);
    assert_eq!(r("README.md", "missing.md"), None);
    assert_eq!(r("README.md", "#install"), None);
    assert_eq!(r("README.md", "https://x.com/README.md"), None);
    assert_eq!(r("README.md", "mailto:a@b.c"), None);
    assert_eq!(r("README.md", "%FF.md"), None);
    assert_eq!(r("README.md", "src"), None, "directory without an index");
}

#[test]
fn module_name_keeps_extension() {
    let p = ProjectFiles::default();
    assert_eq!(MarkdownPack.module_name("docs/guide.md", &p), "docs/guide.md");
    assert_eq!(MarkdownPack.module_name("README.md", &p), "README.md");
    assert_eq!(rueca_model::Language::Markdown.family().module_separator(), ":");
}

#[test]
fn rejects_other_languages_and_large_input() {
    assert!(matches!(
        MarkdownPack.extract("a.py", Language::Python, b"", &Budget::default()),
        Err(ExtractError::Unsupported(Language::Python))
    ));
    let budget = Budget { max_bytes: 4, ..Budget::default() };
    assert!(matches!(
        MarkdownPack.extract("a.md", Language::Markdown, b"# long", &budget),
        Err(ExtractError::TooLarge { .. })
    ));
}

#[test]
fn invalid_utf8_is_partial() {
    let f = MarkdownPack.extract("a.md", Language::Markdown, b"# A\xff\n\n## B\ntext\n", &Budget::default()).unwrap();
    assert_eq!(f.parse_errors, 1);
    assert_eq!(tree(&f), vec![("A\u{fffd}", None), ("A\u{fffd} > B", Some("A\u{fffd}"))]);
    assert_eq!(lines(&f.definitions[1]), (3, 4));
}

#[test]
fn empty_and_headingless_files() {
    let f = facts("");
    assert!(f.definitions.is_empty() && f.imports.is_empty());
    assert_eq!(f.line_count, 0);
    let f = facts("just text with a [link](a.md)\n");
    assert!(f.definitions.is_empty());
    assert_eq!(modules(&f), ["a.md"]);
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

#[test]
fn random_bytes_never_panic() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let tokens: &[&[u8]] = &[
        b"# ",
        b"## ",
        b"###### ",
        b"\n",
        b"\r\n",
        b"===\n",
        b"---\n",
        b"+++\n",
        b"```",
        b"~~~",
        b"    ",
        b"> ",
        b"- ",
        b"1. ",
        b"[",
        b"]",
        b"(",
        b")",
        b"![",
        b"<",
        b">",
        b"<img src=\"",
        b"href=",
        b"\"",
        b"'",
        b"`",
        b"*",
        b"_",
        b"|",
        b"[^1]",
        b"[x]: ",
        b"#",
        b"\\",
        b"a.md",
        b"%2",
        b"\xff\xfe",
        b"\xc3",
        b"\xef\xbb\xbf",
        b"text ",
    ];
    for round in 0..600 {
        let len = (rng.next() % 600) as usize;
        let mut src = Vec::with_capacity(len * 4);
        for _ in 0..len {
            if round % 2 == 0 {
                src.push(rng.next() as u8);
            } else {
                src.extend_from_slice(tokens[(rng.next() as usize) % tokens.len()]);
            }
        }
        let a = MarkdownPack.extract("r.md", Language::Markdown, &src, &Budget::default()).unwrap();
        let b = MarkdownPack.extract("r.md", Language::Markdown, &src, &Budget::default()).unwrap();
        assert_eq!(a, b);
        let mut qns = std::collections::HashSet::new();
        for (i, d) in a.definitions.iter().enumerate() {
            assert!(d.span.contains(&d.name_span), "{src:?}: {d:#?}");
            assert!(d.span.start_line <= d.span.end_line);
            assert!(!d.name.is_empty());
            assert!(qns.insert(d.qualified_name.clone()), "duplicate qn {}", d.qualified_name);
            if let Some(p) = d.parent {
                assert!((p as usize) < i);
                assert!(a.definitions[p as usize].span.contains(&d.span));
            }
        }
        for i in &a.imports {
            let p = ProjectFiles::default();
            let _ = MarkdownPack.resolve_import("d/r.md", i, &p);
        }
    }
}
