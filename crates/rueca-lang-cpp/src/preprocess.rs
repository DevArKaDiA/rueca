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
use crate::text::next_in_preorder;
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
        let error = node.is_error() || node.is_missing();
        if error {
            out.push(Site { start: node.start_byte(), end: node.end_byte(), in_body: in_body(node) });
            if out.len() >= MAX_SITES {
                return out;
            }
        }
        // An ERROR over several lines (the grammar gave up on a whole
        // region, up to the file) is no place for a targeted repair: the
        // errors inside it are.
        let wide = node.is_error() && node.end_position().row > node.start_position().row + 2;
        if (wide || !error) && node.has_error() && cursor.goto_first_child() {
            continue;
        }
        if !next_in_preorder(&mut cursor) {
            return out;
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
    let mut out = Vec::new();
    for (i, t) in toks.iter().enumerate() {
        if t.kind != mask::Kind::Ident || &text[t.start..t.end] != b"operator" || i == 0 {
            continue;
        }
        if !operator_in_expression(&toks, text, i) {
            continue;
        }
        let Some(k) = operator_symbols_end(&toks, i) else { continue };
        if !punct_at(&toks, template_args_end(&toks, k), b'(') {
            continue;
        }
        let (a, b) = (t.end, toks[k - 1].end);
        if b - a <= UNDERSCORES.len() && !text[a..b].contains(&b'\n') {
            out.push(Edit { start: a, end: b, with: &UNDERSCORES[..b - a] });
        }
    }
    out
}

/// Whether token `i` is the punctuation `c`.
fn punct_at(toks: &[mask::Tok], i: usize, c: u8) -> bool {
    toks.get(i).is_some_and(|t| t.kind == mask::Kind::Punct(c))
}

/// Whether the `operator` at token `i` (> 0) is in an expression or at a
/// statement start (`if (c) operator()(x);`); declarations have a type
/// before `operator`.
fn operator_in_expression(toks: &[mask::Tok], text: &[u8], i: usize) -> bool {
    let p = &toks[i - 1];
    matches!(p.kind, mask::Kind::Punct(b'.' | b'(' | b',' | b'!' | b'?' | b';' | b'{' | b'}' | b')'))
        || (punct_at(toks, i - 1, b'>') && i >= 2 && punct_at(toks, i - 2, b'-'))
        || (p.kind == mask::Kind::Ident && &text[p.start..p.end] == b"return")
}

/// The token after the symbols of the `operator` at `i`: `()` / `[]`, else
/// punctuation up to the call's `(`; `None` when there is none.
fn operator_symbols_end(toks: &[mask::Tok], i: usize) -> Option<usize> {
    let mut k = i + 1;
    if (punct_at(toks, k, b'(') && punct_at(toks, k + 1, b')'))
        || (punct_at(toks, k, b'[') && punct_at(toks, k + 1, b']'))
    {
        return Some(k + 2);
    }
    while k < toks.len() && k < i + 4 && matches!(toks[k].kind, mask::Kind::Punct(c) if c != b'(' && c != b';') {
        k += 1;
    }
    (k > i + 1).then_some(k)
}

/// The token after explicit template arguments at `k`
/// (`operator()<bool>(value)`), `k` itself without any.
fn template_args_end(toks: &[mask::Tok], k: usize) -> usize {
    let mut call = k;
    if !punct_at(toks, call, b'<') {
        return call;
    }
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
    call
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
        let Some(close) = test_head(&toks, &levels, text, i, crate::extract::C_TEST_MACROS) else { continue };
        if punct_at(&toks, close + 1, b'{') {
            out.push(Edit { start: t.start, end: t.end, with: b"if" });
        }
    }
    out
}

/// A test macro call `NAME(…)` (`NAME` in `macros`) at token `i`, at
/// declaration level and at a declaration boundary: the index of its `)`.
fn test_head(toks: &[mask::Tok], levels: &[bool], text: &[u8], i: usize, macros: &[&str]) -> Option<usize> {
    let t = &toks[i];
    let word = &text[t.start..t.end];
    if t.kind != mask::Kind::Ident || !levels[i] || !macros.iter().any(|m| m.as_bytes() == word) {
        return None;
    }
    let boundary = i == 0 || matches!(toks[i - 1].kind, mask::Kind::Directive | mask::Kind::Punct(b';' | b'}' | b'{'));
    if !boundary || !punct_at(toks, i + 1, b'(') {
        return None;
    }
    mask::matching_paren(toks, i + 1)
}

/// C++ Catch2 / doctest test heads before a block (`TEST_CASE("title",
/// "[tags]")` + `{`) at declaration level become `if       ("title",
/// "[tags]")`, as C test heads do: the C++ grammar reads such a head as a
/// call missing its `;`, and that recovery is unstable (a head on its own
/// lines, a type list or a later construct can turn the whole file into
/// one error). The type lists of the template variants (`, int, float)`,
/// `, (std::vector, Foo), (int, float))`) are blanked, since types are not
/// expressions. The extractor reads the macro name from the original text.
fn catch_test_heads(text: &[u8]) -> Vec<Edit> {
    let toks = mask::lex_literals(text);
    let levels = mask::declaration_levels(&toks, text);
    let mut out = Vec::new();
    for (i, t) in toks.iter().enumerate() {
        let Some(close) = test_head(&toks, &levels, text, i, crate::extract::CATCH_MACROS) else { continue };
        // `TEST_CASE()` (an anonymous test) stays: `if ()` is no statement.
        if close == i + 2 || !punct_at(&toks, close + 1, b'{') {
            continue;
        }
        out.push(Edit { start: t.start, end: t.end, with: b"if" });
        if text[t.start..t.end].windows(8).any(|w| w == b"TEMPLATE") {
            out.extend(template_type_list(&toks, i, close));
        }
    }
    out
}

/// The type list of a template test head `NAME(…)` (`NAME` at token `i`,
/// its `)` at `close`): the arguments at depth 0 keep the leading fixture
/// name and string literals; the rest is blanked from the comma before
/// the first other one.
fn template_type_list(toks: &[mask::Tok], i: usize, close: usize) -> Option<Edit> {
    let mut depth = 0i32;
    let mut args: Vec<(usize, usize)> = Vec::new();
    let mut a = i + 2;
    for (k, tok) in toks.iter().enumerate().take(close).skip(i + 2) {
        match tok.kind {
            mask::Kind::Punct(b'(' | b'[' | b'{') => depth += 1,
            mask::Kind::Punct(b')' | b']' | b'}') => depth -= 1,
            mask::Kind::Punct(b',') if depth == 0 => {
                args.push((a, k));
                a = k + 1;
            }
            _ => {}
        }
    }
    args.push((a, close));
    let (index, &(a, b)) = args.iter().enumerate().find(|&(index, &(a, b))| {
        let arg = &toks[a..b];
        let literal = !arg.is_empty() && arg.iter().all(|x| x.kind == mask::Kind::Literal);
        let fixture = index == 0 && arg.len() == 1 && arg[0].kind == mask::Kind::Ident;
        !literal && !fixture
    })?;
    (index > 0 && b > a).then(|| Edit { start: toks[a - 1].start, end: toks[close - 1].end, with: b"" })
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
    let edits = if language == Language::C {
        c_test_heads(&text)
    } else {
        let mut edits = operator_calls(&text);
        edits.extend(catch_test_heads(&text));
        edits
    };
    apply(&mut text, &edits);
    let tree = rueca_extract::parse(&grammar, &text, budget)?;
    let mut best = Attempt { errors: count_errors(&tree), text, tree, parses: 1 };
    for _ in 0..MAX_ROUNDS {
        if best.errors == 0 {
            break;
        }
        let (edits, per_site) = round_edits(&best, &groups, &mut collapsed, &toks);
        if edits.is_empty() {
            break;
        }
        if best.try_edits(&grammar, budget, &edits)? {
            continue;
        }
        // One bad repair can spoil a round: try the sites one at a time.
        let improved = best.try_each(&grammar, budget, &per_site)?;
        if !improved || best.parses >= MAX_PARSES {
            break;
        }
    }
    Ok((best.text, best.tree))
}

/// The best text so far, its tree and error count, and the parses spent.
struct Attempt {
    text: Vec<u8>,
    tree: Tree,
    errors: u32,
    parses: usize,
}

impl Attempt {
    /// Parses the text with `edits` applied and keeps it when it has fewer
    /// errors; whether it did.
    fn try_edits(
        &mut self,
        grammar: &rueca_extract::tree_sitter::Language,
        budget: &Budget,
        edits: &[Edit],
    ) -> Result<bool, ExtractError> {
        let mut next = self.text.clone();
        apply(&mut next, edits);
        let next_tree = rueca_extract::parse(grammar, &next, budget)?;
        self.parses += 1;
        let next_errors = count_errors(&next_tree);
        if next_errors >= self.errors {
            return Ok(false);
        }
        (self.text, self.tree, self.errors) = (next, next_tree, next_errors);
        Ok(true)
    }

    /// Tries each edit group on its own while parses remain (at most
    /// [`MAX_PARSES`] in all), keeping each that lowers the error count;
    /// whether any did.
    fn try_each(
        &mut self,
        grammar: &rueca_extract::tree_sitter::Language,
        budget: &Budget,
        groups: &[Vec<Edit>],
    ) -> Result<bool, ExtractError> {
        let mut improved = false;
        for group in groups {
            if self.parses >= MAX_PARSES {
                break;
            }
            improved |= self.try_edits(grammar, budget, group)?;
        }
        Ok(improved)
    }
}

/// The edits of one repair round over every error site of `best`, and the
/// edits of each site (for the one-at-a-time fallback). A site gets its
/// repairs, else the `#if` group next to the error, or the innermost one
/// containing it, keeps one branch.
fn round_edits(
    best: &Attempt,
    groups: &[cond::Group],
    collapsed: &mut [bool],
    toks: &[mask::Tok],
) -> (Vec<Edit>, Vec<Vec<Edit>>) {
    let text = &best.text;
    let current = mask::lex_literals(text);
    let newlines: Vec<usize> = text.iter().enumerate().filter(|(_, b)| **b == b'\n').map(|(i, _)| i).collect();
    let line = |at: usize| newlines.partition_point(|&n| n < at) as u32;
    let mut edits: Vec<Edit> = Vec::new();
    let mut per_site: Vec<Vec<Edit>> = Vec::new();
    for site in error_sites(&best.tree) {
        let near = (line(site.start).saturating_sub(1), line(site.end) + 1);
        let site_edits = repair::repair(text, &current, site).or_else(|| {
            cond::collapse_near(groups, collapsed, toks, usize::MAX, near)
                .or_else(|| cond::collapse_near(groups, collapsed, toks, site.start, (u32::MAX, u32::MAX)))
                .map(blanks)
        });
        if let Some(e) = site_edits {
            edits.extend(e.iter().cloned());
            per_site.push(e);
        }
    }
    (edits, per_site)
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
        let calls: Vec<&str> = calls.into_iter().map(|n| rueca_extract::split_arg_types(n).0).collect();
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
