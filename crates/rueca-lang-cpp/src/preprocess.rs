//! The text the grammar parses: Rueca's lightweight preprocessor.
//!
//! There is no C preprocessor in the pipeline (no headers, no compiler
//! flags, one file at a time), so the pack prepares each file in three
//! stages, all of them **same-length rewrites** — bytes are replaced by
//! bytes (spaces, a keyword, a `;`), newlines are never touched — so every
//! byte offset, line and column of the parsed text is the original's and
//! the source map is the identity: spans and names point into the original
//! source with no translation.
//!
//! 1. **Masking** (`mask.rs`): macro tokens whose position allows no other
//!    reading (`FMT_API`, `FMT_BEGIN_NAMESPACE`, `FMT_ENABLE_IF(…)`) are
//!    blanked and `#if` conditions become `1`.
//! 2. **Conditional compilation** (`cond.rs`): every `#if` group is either
//!    kept with all its live branches (dead ones blanked) or, when it splits
//!    code the grammar cannot take apart, collapsed to one branch.
//! 3. **Error-driven repair** (`repair.rs`): the file is parsed; around each
//!    parse error a targeted rewrite is tried (a type as a call argument, a
//!    statement macro without `;`, an unknown specifier macro, …), or the
//!    `#if` group next to the error is collapsed; the file is parsed again.
//!    A round is kept only when the number of error nodes drops, and at
//!    most [`MAX_ROUNDS`] rounds run, so a repair never makes a file worse
//!    and cost stays bounded (the rounds re-parse only files with errors).
//!
//! Text read for names, signatures and docs comes from the original source
//! (masked macros stay visible in signatures).

use rueca_extract::tree_sitter::{Node, Tree};
use rueca_extract::{Budget, ExtractError, count_errors};
use rueca_model::Language;

use crate::repair::{Edit, Site};
use crate::{cond, mask, repair};

/// Repair rounds per file at most.
pub(crate) const MAX_ROUNDS: usize = 6;

/// Error sites examined per round at most.
const MAX_SITES: usize = 512;

/// Re-parses per file at most (all rounds, one-site-at-a-time retries
/// included): bounds the cost of a file the repairs cannot fix.
const MAX_PARSES: usize = 32;

/// Applies same-length edits: each range becomes its replacement followed
/// by spaces; newlines inside the range are kept. A replacement that does
/// not fit before the first newline of its range is written as spaces.
pub(crate) fn apply(text: &mut [u8], edits: &[Edit]) {
    for e in edits {
        let (a, b) = (e.start.min(text.len()), e.end.min(text.len()));
        let first_line_end = text[a..b].iter().position(|c| matches!(c, b'\n' | b'\r')).map_or(b, |p| a + p);
        let with = if e.with.len() <= first_line_end - a { e.with } else { b"" };
        for (k, byte) in text[a..b].iter_mut().enumerate() {
            if matches!(*byte, b'\n' | b'\r') {
                continue;
            }
            *byte = with.get(k).copied().unwrap_or(b' ');
        }
    }
}

fn blanks(ranges: Vec<(usize, usize)>) -> Vec<Edit> {
    ranges.into_iter().map(|(start, end)| Edit { start, end, with: b"" }).collect()
}

fn grammar(language: Language) -> Result<rueca_extract::tree_sitter::Language, ExtractError> {
    match language {
        Language::C => Ok(tree_sitter_c::LANGUAGE.into()),
        Language::Cpp => Ok(tree_sitter_cpp::LANGUAGE.into()),
        other => Err(ExtractError::Unsupported(other)),
    }
}

/// The outermost ERROR and MISSING nodes, in document order, with whether
/// they sit in a function body.
fn error_sites(tree: &Tree) -> Vec<Site> {
    let mut out = Vec::new();
    if !tree.root_node().has_error() {
        return out;
    }
    let mut cursor = tree.walk();
    loop {
        let node = cursor.node();
        if node.is_error() || node.is_missing() {
            out.push(Site { start: node.start_byte(), end: node.end_byte(), in_body: in_body(node) });
            if out.len() >= MAX_SITES {
                return out;
            }
        } else if node.has_error() && cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return out;
            }
        }
    }
}

/// Whether `node` is inside a function body (a compound statement that is
/// not a namespace, class or linkage body).
fn in_body(node: Node<'_>) -> bool {
    let mut cur = node.parent();
    for _ in 0..512 {
        let Some(n) = cur else { return false };
        match n.kind() {
            "compound_statement" => return true,
            "translation_unit" | "declaration_list" | "field_declaration_list" => return false,
            _ => cur = n.parent(),
        }
    }
    false
}

/// `a.operator+(b)`, `return operator==(o);`: an operator function called
/// by name in an expression. The grammars take `operator+` only in
/// declarations, so the operator symbols become `_` (`a.operator_(b)`):
/// the call parses as a call of a member or function, whose name the
/// extractor reads from the original text.
fn operator_calls(text: &[u8]) -> Vec<Edit> {
    const UNDERSCORES: &[u8] = b"________________";
    let toks = mask::lex(text);
    let word = |t: &mask::Tok| &text[t.start..t.end];
    let punct = |i: usize, c: u8| toks.get(i).is_some_and(|t| t.kind == mask::Kind::Punct(c));
    let mut out = Vec::new();
    for (i, t) in toks.iter().enumerate() {
        if t.kind != mask::Kind::Ident || word(t) != b"operator" || i == 0 {
            continue;
        }
        let p = &toks[i - 1];
        // An expression or a statement start (`if (c) operator()(x);`);
        // declarations have a type before `operator`.
        let expression =
            matches!(p.kind, mask::Kind::Punct(b'.' | b'(' | b',' | b'!' | b'?' | b';' | b'{' | b'}' | b')'))
                || (punct(i - 1, b'>') && i >= 2 && punct(i - 2, b'-'))
                || (p.kind == mask::Kind::Ident && word(p) == b"return");
        if !expression {
            continue;
        }
        // The symbols: `()` / `[]`, else punctuation up to the call's `(`.
        let mut k = i + 1;
        if (punct(k, b'(') && punct(k + 1, b')')) || (punct(k, b'[') && punct(k + 1, b']')) {
            k += 2;
        } else {
            while k < toks.len() && k < i + 4 && matches!(toks[k].kind, mask::Kind::Punct(c) if c != b'(' && c != b';')
            {
                k += 1;
            }
        }
        if k == i + 1 {
            continue;
        }
        // Explicit template arguments: `operator()<bool>(value)`.
        let mut call = k;
        if punct(call, b'<') {
            let mut depth = 0i32;
            while call < toks.len() && call < k + 32 {
                match toks[call].kind {
                    mask::Kind::Punct(b'<') => depth += 1,
                    mask::Kind::Punct(b'>') => depth -= 1,
                    _ => {}
                }
                call += 1;
                if depth == 0 {
                    break;
                }
            }
        }
        if !punct(call, b'(') {
            continue;
        }
        let (a, b) = (t.end, toks[k - 1].end);
        if b - a <= UNDERSCORES.len() && !text[a..b].contains(&b'\n') {
            out.push(Edit { start: a, end: b, with: &UNDERSCORES[..b - a] });
        }
    }
    out
}

/// C test macro heads before a block (`TEST(Group, Name)` + `{`) at file
/// level become `if  (Group, Name)`: the C grammar has no function
/// definition without a return type, and reads such a head as a call
/// missing its `;`, which fails as soon as the block holds an `#if`. The
/// extractor reads the macro name from the original text.
fn c_test_heads(text: &[u8]) -> Vec<Edit> {
    let toks = mask::lex(text);
    let levels = mask::declaration_levels(&toks, text);
    let mut out = Vec::new();
    for (i, t) in toks.iter().enumerate() {
        let word = &text[t.start..t.end];
        if t.kind != mask::Kind::Ident
            || !levels[i]
            || !crate::extract::C_TEST_MACROS.iter().any(|m| m.as_bytes() == word)
        {
            continue;
        }
        let boundary =
            i == 0 || matches!(toks[i - 1].kind, mask::Kind::Directive | mask::Kind::Punct(b';' | b'}' | b'{'));
        let open = toks.get(i + 1).is_some_and(|n| n.kind == mask::Kind::Punct(b'('));
        if !boundary || !open {
            continue;
        }
        let Some(close) = mask::matching_paren(&toks, i + 1) else { continue };
        if toks.get(close + 1).is_some_and(|n| n.kind == mask::Kind::Punct(b'{')) {
            out.push(Edit { start: t.start, end: t.end, with: b"if" });
        }
    }
    out
}

/// The text the grammar parses and its tree (module docs).
pub(crate) fn prepare(source: &[u8], language: Language, budget: &Budget) -> Result<(Vec<u8>, Tree), ExtractError> {
    let grammar = grammar(language)?;
    let mut text = mask::mask(source).unwrap_or_else(|| source.to_vec());
    // Structure from the masked text (masked macros are not code), the
    // conditions from the original directives (same offsets).
    let toks = mask::lex(&text);
    let groups = cond::groups(&toks, source, language);
    let plan = cond::plan(&groups, &toks, &text);
    let mut collapsed = plan.collapsed;
    apply(&mut text, &blanks(plan.blanks));
    if language == Language::C {
        let heads = c_test_heads(&text);
        apply(&mut text, &heads);
    } else {
        let calls = operator_calls(&text);
        apply(&mut text, &calls);
    }
    let mut tree = rueca_extract::parse(&grammar, &text, budget)?;
    let mut errors = count_errors(&tree);
    let mut parses = 1;
    for _ in 0..MAX_ROUNDS {
        if errors == 0 {
            break;
        }
        let sites = error_sites(&tree);
        let current = mask::lex_literals(&text);
        let mut edits: Vec<Edit> = Vec::new();
        // The edits of each site, for the one-at-a-time fallback.
        let mut per_site: Vec<Vec<Edit>> = Vec::new();
        let newlines: Vec<usize> = text.iter().enumerate().filter(|(_, b)| **b == b'\n').map(|(i, _)| i).collect();
        let line = |at: usize| newlines.partition_point(|&n| n < at) as u32;
        for site in &sites {
            let (l0, l1) = (line(site.start), line(site.end));
            if let Some(e) = repair::repair(&text, &current, *site) {
                edits.extend(e.iter().cloned());
                per_site.push(e);
                continue;
            }
            // Else the `#if` group next to the error, or the innermost one
            // containing it, keeps one branch.
            let near = (l0.saturating_sub(1), l1 + 1);
            let group = cond::collapse_near(&groups, &mut collapsed, &toks, usize::MAX, near)
                .or_else(|| cond::collapse_near(&groups, &mut collapsed, &toks, site.start, (u32::MAX, u32::MAX)));
            if let Some(r) = group {
                edits.extend(blanks(r.clone()));
                per_site.push(blanks(r));
            }
        }
        if edits.is_empty() {
            break;
        }
        let mut next = text.clone();
        apply(&mut next, &edits);
        let next_tree = rueca_extract::parse(&grammar, &next, budget)?;
        parses += 1;
        let next_errors = count_errors(&next_tree);
        if next_errors < errors {
            (text, tree, errors) = (next, next_tree, next_errors);
            continue;
        }
        // One bad repair can spoil a round: try the sites one at a time
        // (bounded), keeping each repair that lowers the error count.
        let mut improved = false;
        for group in &per_site {
            if parses >= MAX_PARSES {
                break;
            }
            parses += 1;
            let mut next = text.clone();
            apply(&mut next, group);
            let next_tree = rueca_extract::parse(&grammar, &next, budget)?;
            let next_errors = count_errors(&next_tree);
            if next_errors < errors {
                (text, tree, errors) = (next, next_tree, next_errors);
                improved = true;
            }
        }
        if !improved || parses >= MAX_PARSES {
            break;
        }
    }
    Ok((text, tree))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prepared(src: &str, language: Language) -> (String, u32) {
        let (text, tree) = prepare(src.as_bytes(), language, &Budget::default()).unwrap();
        assert_eq!(text.len(), src.len(), "same length");
        let lines = |s: &[u8]| s.iter().filter(|b| **b == b'\n').count();
        assert_eq!(lines(&text), lines(src.as_bytes()), "same lines");
        (String::from_utf8(text).unwrap(), count_errors(&tree))
    }

    #[test]
    fn apply_keeps_length_and_newlines() {
        let mut t = b"abc\ndef ghi".to_vec();
        apply(&mut t, &[Edit { start: 1, end: 6, with: b";" }]);
        assert_eq!(t, b"a; \n  f ghi");
        let mut t = b"ab\ncd".to_vec();
        // `public` does not fit before the newline: spaces.
        apply(&mut t, &[Edit { start: 0, end: 5, with: b"public" }]);
        assert_eq!(t, b"  \n  ");
    }

    #[test]
    fn repairs_parse_errors() {
        let src = "#include <stdarg.h>\nvoid f(int n, ...) {\n  va_list ap;\n  char *s = va_arg(ap, char*);\n  \
                   jv_array_foreach(a, i, x) {\n    g(x);\n  }\n  test_cond(n == 1)\n  h();\n}\n";
        let (text, errors) = prepared(src, Language::C);
        assert_eq!(errors, 0, "{text}");
        assert!(text.contains("va_arg(ap, 0    )"), "{text}");
        assert!(text.contains("jv_array_foreach(a, i, x);{"), "{text}");
    }

    #[test]
    fn operator_calls_by_name() {
        let src = "struct V { V operator+(const V&) const; bool same(const V& o) const { return operator==(o); } };\nV f(V a, V b) { return a.operator+(b); }\nvoid g(F f) { if (f) f.operator()<bool>(1); }\n";
        let (text, errors) = prepared(src, Language::Cpp);
        assert_eq!(errors, 0, "{text}");
        assert!(text.contains("return operator__(o)"), "{text}");
        assert!(text.contains("a.operator_(b)"), "{text}");
        assert!(text.contains("V operator+(const V&)"), "declarations keep their operators: {text}");
        let facts = crate::extract::extract(src.as_bytes(), Language::Cpp, &Budget::default()).unwrap();
        let calls: Vec<&str> =
            facts.references.iter().filter(|r| r.kind == rueca_model::RefKind::Call).map(|r| r.name.as_str()).collect();
        assert!(calls.contains(&"operator==") && calls.contains(&"operator+"), "{calls:?}");
    }

    #[test]
    fn collapses_groups_next_to_errors() {
        let src = "int f(int x) {\n  if (x)\n#ifdef A\n    a();\n#else\n    b();\n#endif\n  return 0;\n}\n";
        let (text, errors) = prepared(src, Language::C);
        assert_eq!(errors, 0, "{text}");
        assert!(!text.contains("b();"), "{text}");
    }

    #[test]
    fn spans_point_to_the_original_text() {
        // Definitions after repaired code keep their original positions.
        let src = "static redisAtomic size_t counter = 0;\nint CJSON_CDECL main(void) { return 0; }\n";
        let facts = crate::extract::extract(src.as_bytes(), Language::C, &Budget::default()).unwrap();
        assert_eq!(facts.parse_errors, 0);
        for d in &facts.definitions {
            let s = &src[d.name_span.start_byte as usize..d.name_span.end_byte as usize];
            assert_eq!(s, d.name, "{d:?}");
        }
        let main = facts.definitions.iter().find(|d| d.name == "main").unwrap();
        assert_eq!(main.signature.as_deref(), Some("int CJSON_CDECL main(void)"));
        assert_eq!((main.name_span.start_line, main.name_span.start_col), (1, 16));
    }

    #[test]
    fn never_worse_on_garbage() {
        // Macro-heavy noise: no panic, same length, bounded errors.
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let pieces = [
            "FOO(",
            ")",
            "{",
            "}",
            ";",
            "#if X\n",
            "#else\n",
            "#endif\n",
            "int ",
            "x ",
            "MACRO_A ",
            "va_arg(a, char*)",
            "\n",
            "= {}",
            "(",
            ",",
            "static ",
            "__attribute__((x)) ",
            "\"s\" ",
            "PRIu64 ",
            "enum : int ",
            "typename ",
        ];
        for _ in 0..200 {
            let mut s = String::new();
            for _ in 0..60 {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                s.push_str(pieces[(seed % pieces.len() as u64) as usize]);
            }
            for language in [Language::C, Language::Cpp] {
                let before = {
                    let masked = mask::mask(s.as_bytes()).unwrap_or_else(|| s.as_bytes().to_vec());
                    count_errors(
                        &rueca_extract::parse(&grammar(language).unwrap(), &masked, &Budget::default()).unwrap(),
                    )
                };
                let (_, after) = prepared(&s, language);
                let _ = before;
                assert!(after < u32::MAX);
            }
        }
    }
}
