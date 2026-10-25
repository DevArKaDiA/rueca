//! Masking of macro tokens that tree-sitter cannot parse.
//!
//! The pack parses each file without a preprocessor, so a macro that expands
//! to declaration syntax derails the parser: `FMT_BEGIN_NAMESPACE` alone on
//! a line, `class FMT_API format_error {`, `FMT_CONSTEXPR auto f()`,
//! `void f() FMT_NOEXCEPT;`. Before parsing, such macro tokens are replaced
//! by spaces (byte offsets and lines are preserved, so spans and the text
//! read from the original source stay exact). A token is masked only when
//! it looks like a macro (upper case, digits and `_`) **and** its position
//! makes any other reading invalid C/C++:
//!
//! 1. **Bare line**: the only token on its line, after `;`, `{`, `}`, `:`, a
//!    preprocessor line, a masked macro or the start of the file, and followed by an
//!    identifier, `~`, `[`, a preprocessor line or the end of the file
//!    (`FMT_BEGIN_NAMESPACE`, `Q_OBJECT`, `G_BEGIN_DECLS`). Not before
//!    `}`, `,` or an operator (a last enumerator, an expression).
//! 2. **Between `class` / `struct` / `union` and the name**:
//!    `class FMT_API format_error : …`, `class FMT_VIS("default") x {`, when
//!    the name is followed by `{`, `:`, `<` or `final`.
//! 3. **Leading specifier**: at the start of a declaration (after `;`,
//!    `{`, `}`, `>`, `]`, `public:`, a preprocessor line, or a specifier
//!    keyword such as `static` / `inline`), containing `_`, and followed by a keyword
//!    that cannot follow a type name (`void`, `auto`, `static`, `inline`,
//!    `constexpr`, `class`, `template`, …, `const` before a fundamental
//!    type), by `~`, `[`, a qualified name (`FMT_API std::string f()`) or
//!    by another such macro. A macro with arguments in that position is
//!    masked with its argument list (`JSON_HEDLEY_DEPRECATED_FOR(…) static`),
//!    and before a type and a name (`FFC_INLINE ffc_result f(…)`: a type
//!    name cannot be followed by another).
//! 4. **Trailing specifier**: containing `_`, after the `)` of a parameter
//!    list (not of a cast or an `if` condition), `const`, `override` or
//!    `noexcept`, and followed by `;`, `{`, `=`, `:`, `->`, `const`,
//!    `override`, `final` or another such macro
//!    (`void f() const FMT_NOEXCEPT;`).
//! 5. **Statement-like call at declaration level**: `MACRO(args)` at the
//!    start of a declaration in a file, namespace, `extern "C"` or class
//!    body, ending its line, not followed by `{`, `;`, `,` or an operator
//!    (`FMT_PRAGMA_GCC(push_options)`, `Q_DISABLE_COPY(Foo)`).
//! 6. **Template-parameter macro**: `MACRO(args)` inside a `template <…>`
//!    head after `,` or `<` and before `,` or `>`, masked with the comma
//!    before it (`template <typename T, FMT_ENABLE_IF(is_integral<T>())>`).
//! 7. **`if MACRO (`**: a macro standing for `constexpr`
//!    (`if FMT_CONSTEXPR20 (cond)`).
//!
//! 8. **Attribute after a parameter name**: containing `_`, after a name
//!    (not a keyword or a type keyword) and before `,` or `)`
//!    (`const T& x FMT_LIFETIMEBOUND, int y`).
//! 9. **Macro before a parameter**: containing `_`, after `(` or `,` and
//!    before a type and a name (`f(EV_P_ ev_io *w)`).
//!
//! In addition, every `#if` / `#elif` condition is replaced by `1` (the
//! grammars cannot parse many of them: `__has_include(<x>)`,
//! `FOO(clang::attr)`; which branches the parser sees is `cond.rs`'s
//! decision, from the original conditions), the null directive (`#` alone)
//! is blanked, and block comments inside directives are blanked when code
//! follows them on the directive (they break the grammars' macro bodies; a
//! trailing one stays, so a valueless `#define X` keeps its line end).
//! This is the first stage of `preprocess.rs`.
//! The rules apply at declaration level (file, namespace, `extern "C"` and
//! class bodies) where noted; function bodies are left alone, so no call
//! or read is lost there.
//!
//! Preprocessor lines, comments and string literals are never masked. The
//! rules are heuristics: a masked macro that expanded to a namespace
//! (`FMT_BEGIN_NAMESPACE`) contributes nothing to qualified names.

/// Upper bound on tokens scanned for a macro's argument list.
const MAX_ARG_TOKENS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Ident,
    /// A preprocessor line (`#…` with its continuation lines).
    Directive,
    /// A string or character literal (only from [`lex_literals`]).
    Literal,
    /// Punctuation; `Punct(b'0')` is a number.
    Punct(u8),
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Tok {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) line: u32,
    pub(crate) kind: Kind,
}

/// Keywords that cannot follow a type name in a declaration (rule 3).
const FOLLOWERS: &[&str] = &[
    "auto",
    "bool",
    "char",
    "class",
    "consteval",
    "constexpr",
    "constinit",
    "double",
    "enum",
    "explicit",
    "extern",
    "float",
    "friend",
    "inline",
    "int",
    "long",
    "namespace",
    "short",
    "signed",
    "static",
    "struct",
    "template",
    "thread_local",
    "typedef",
    "typename",
    "union",
    "unsigned",
    "using",
    "virtual",
    "void",
];

/// Fundamental types: `MACRO const char*` (rule 3).
const FUNDAMENTAL: &[&str] =
    &["auto", "bool", "char", "double", "float", "int", "long", "short", "signed", "unsigned", "void"];

/// Words before a macro that make it a type or a value, not an attribute
/// after a parameter name (rule 8).
const NOT_A_NAME: &[&str] = &[
    "auto",
    "bool",
    "case",
    "char",
    "class",
    "const",
    "delete",
    "do",
    "double",
    "else",
    "enum",
    "float",
    "goto",
    "int",
    "long",
    "new",
    "return",
    "short",
    "signed",
    "sizeof",
    "struct",
    "throw",
    "typename",
    "union",
    "unsigned",
    "void",
    "volatile",
    "co_return",
    "co_yield",
    "co_await",
    "decltype",
    "alignof",
];

/// Keywords after which a declaration specifier may follow (rule 3).
const SPECIFIERS: &[&str] = &["constexpr", "explicit", "extern", "friend", "inline", "static", "template", "virtual"];

/// Tokens of `src` without string and character literals (the masking
/// rules look at code tokens only).
pub(crate) fn lex(src: &[u8]) -> Vec<Tok> {
    lex_impl(src, false)
}

/// Tokens of `src` with string and character literals as [`Kind::Literal`]
/// (an encoding prefix such as `u8` or `L` is part of the literal).
pub(crate) fn lex_literals(src: &[u8]) -> Vec<Tok> {
    lex_impl(src, true)
}

fn lex_impl(src: &[u8], literals: bool) -> Vec<Tok> {
    let mut toks = Vec::new();
    let mut i = 0;
    let mut line = 0u32;
    let mut at_line_start = true;
    let n = src.len();
    while i < n {
        let c = src[i];
        if c == b'\n' {
            line += 1;
            at_line_start = true;
            i += 1;
            continue;
        }
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if at_line_start && c == b'#' {
            // Preprocessor line, with `\` continuations; a block comment
            // that starts on it may run over several lines (the line ends
            // after the comment).
            let start = i;
            let first_line = line;
            loop {
                while i < n && src[i] != b'\n' {
                    if src[i] == b'/' && src.get(i + 1) == Some(&b'*') {
                        i += 2;
                        while i + 1 < n && !(src[i] == b'*' && src[i + 1] == b'/') {
                            if src[i] == b'\n' {
                                line += 1;
                            }
                            i += 1;
                        }
                        i = (i + 2).min(n);
                        continue;
                    }
                    if src[i] == b'/' && src.get(i + 1) == Some(&b'/') {
                        while i < n && src[i] != b'\n' {
                            i += 1;
                        }
                        break;
                    }
                    i += 1;
                }
                let mut j = i;
                if j > start && src[j - 1] == b'\r' {
                    j -= 1;
                }
                if i < n && j > start && src[j - 1] == b'\\' {
                    i += 1;
                    line += 1;
                    continue;
                }
                break;
            }
            toks.push(Tok { start, end: i, line: first_line, kind: Kind::Directive });
            continue;
        }
        at_line_start = false;
        if c == b'/' && i + 1 < n && src[i + 1] == b'/' {
            while i < n && src[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if c == b'/' && i + 1 < n && src[i + 1] == b'*' {
            i += 2;
            while i + 1 < n && !(src[i] == b'*' && src[i + 1] == b'/') {
                if src[i] == b'\n' {
                    line += 1;
                }
                i += 1;
            }
            i = (i + 2).min(n);
            continue;
        }
        if c == b'"' || c == b'\'' {
            let (start, first_line) = (i, line);
            i = skip_quoted(src, i, &mut line);
            if literals {
                toks.push(Tok { start, end: i, line: first_line, kind: Kind::Literal });
            }
            continue;
        }
        if c.is_ascii_alphabetic() || c == b'_' {
            let start = i;
            while i < n && (src[i].is_ascii_alphanumeric() || src[i] == b'_') {
                i += 1;
            }
            let word = &src[start..i];
            // Raw string literal: R"delim( … )delim" (also u8R, LR, uR, UR).
            if i < n && src[i] == b'"' && matches!(word, b"R" | b"u8R" | b"LR" | b"uR" | b"UR") {
                let first_line = line;
                i = skip_raw(src, i, &mut line);
                if literals {
                    toks.push(Tok { start, end: i, line: first_line, kind: Kind::Literal });
                }
                continue;
            }
            // Encoding prefix of a literal: u8"…", L'x'.
            if literals && i < n && matches!(src[i], b'"' | b'\'') && matches!(word, b"L" | b"u" | b"U" | b"u8") {
                let first_line = line;
                i = skip_quoted(src, i, &mut line);
                toks.push(Tok { start, end: i, line: first_line, kind: Kind::Literal });
                continue;
            }
            toks.push(Tok { start, end: i, line, kind: Kind::Ident });
            continue;
        }
        if c.is_ascii_digit() {
            let start = i;
            while i < n && (src[i].is_ascii_alphanumeric() || matches!(src[i], b'_' | b'.' | b'\'')) {
                i += 1;
            }
            toks.push(Tok { start, end: i, line, kind: Kind::Punct(b'0') });
            continue;
        }
        toks.push(Tok { start: i, end: i + 1, line, kind: Kind::Punct(c) });
        i += 1;
    }
    toks
}

fn skip_quoted(src: &[u8], mut i: usize, line: &mut u32) -> usize {
    let quote = src[i];
    i += 1;
    while i < src.len() {
        match src[i] {
            b'\\' => i += 2,
            b'\n' => {
                // Unterminated literal: stop at the end of the line.
                *line += 1;
                return i + 1;
            }
            c if c == quote => return i + 1,
            _ => i += 1,
        }
    }
    src.len()
}

fn skip_raw(src: &[u8], i: usize, line: &mut u32) -> usize {
    // `i` is at the opening quote.
    let n = src.len();
    let mut j = i + 1;
    while j < n && src[j] != b'(' && src[j] != b'\n' && j - i <= 17 {
        j += 1;
    }
    if j >= n || src[j] != b'(' {
        return skip_quoted(src, i, line);
    }
    let delim = &src[i + 1..j];
    let mut k = j + 1;
    while k < n {
        if src[k] == b'\n' {
            *line += 1;
        }
        if src[k] == b')' && src[k + 1..].starts_with(delim) && src.get(k + 1 + delim.len()) == Some(&b'"') {
            return k + delim.len() + 2;
        }
        k += 1;
    }
    n
}

/// Upper-case macro-looking identifier of at least `min` bytes.
pub(crate) fn is_macro(word: &[u8], min: usize) -> bool {
    word.len() >= min
        && word[0].is_ascii_uppercase()
        && word.iter().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || *b == b'_')
}

/// `#include` directives outside comments: (module, byte range of the
/// directive line). `"x.h"` gives `x.h`, `<x.h>` gives `<x.h>`; macro
/// includes (`#include FOO_H`) are skipped. Read lexically, so includes
/// survive parse errors around them.
pub(crate) fn includes(src: &[u8]) -> Vec<(String, usize, usize)> {
    let mut out = Vec::new();
    for t in lex(src) {
        if t.kind != Kind::Directive {
            continue;
        }
        let line = String::from_utf8_lossy(&src[t.start..t.end]);
        let rest = line[1..].trim_start();
        let Some(rest) = rest.strip_prefix("include").or_else(|| rest.strip_prefix("import")) else { continue };
        let rest = rest.trim_start();
        let module = match rest.chars().next() {
            Some('"') => rest[1..].split('"').next().map(str::to_string),
            Some('<') => rest.split_once('>').map(|(a, _)| format!("{a}>")),
            _ => None,
        };
        if let Some(m) = module.filter(|m| !m.is_empty() && m != "<>" && !m.contains('\n')) {
            out.push((m, t.start, t.end));
        }
    }
    out
}

/// Matching `(` for the `)` at `close`, scanning backwards.
fn matching_open(toks: &[Tok], close: usize) -> Option<usize> {
    let mut depth = 0i32;
    for k in (close.saturating_sub(MAX_ARG_TOKENS)..=close).rev() {
        match toks[k].kind {
            Kind::Punct(b')') => depth += 1,
            Kind::Punct(b'(') => {
                depth -= 1;
                if depth == 0 {
                    return Some(k);
                }
            }
            Kind::Punct(b';' | b'{' | b'}') | Kind::Directive => return None,
            _ => {}
        }
    }
    None
}

/// For every token, whether it is at declaration level: every enclosing
/// brace opens a namespace, an `extern "C"` block or a class body (not a
/// function body or an initializer).
pub(crate) fn declaration_levels(toks: &[Tok], src: &[u8]) -> Vec<bool> {
    let text = |t: &Tok| &src[t.start..t.end];
    let mut stack: Vec<bool> = Vec::new();
    let mut out = Vec::with_capacity(toks.len());
    for (i, t) in toks.iter().enumerate() {
        out.push(stack.iter().all(|d| *d));
        match t.kind {
            Kind::Punct(b'{') => {
                let mut decl = false;
                // Walk back over the head: `namespace a::b`, `extern`,
                // `class X : public Y<Z>`.
                for k in (i.saturating_sub(MAX_ARG_TOKENS)..i).rev() {
                    let p = &toks[k];
                    match p.kind {
                        Kind::Ident if matches!(text(p), b"namespace" | b"class" | b"struct" | b"union" | b"enum") => {
                            decl = true;
                            break;
                        }
                        Kind::Ident if k + 1 == i && text(p) == b"extern" => {
                            decl = true;
                            break;
                        }
                        Kind::Ident | Kind::Punct(b':' | b'<' | b'>' | b',' | b'0') => {}
                        _ => break,
                    }
                }
                stack.push(decl);
            }
            Kind::Punct(b'}') => {
                stack.pop();
            }
            _ => {}
        }
    }
    out
}

/// What [`mask`] changes: macro token ranges to blank, and `#if` / `#elif`
/// conditions to replace by `1`.
#[derive(Debug, Default)]
pub(crate) struct Plan {
    pub(crate) blanks: Vec<(usize, usize)>,
    pub(crate) conditions: Vec<(usize, usize)>,
}

/// Ranges of `src` to blank out before parsing (see the module docs).
#[cfg(test)]
pub(crate) fn macro_ranges(src: &[u8]) -> Vec<(usize, usize)> {
    plan(src).blanks
}

/// Computes the [`Plan`] for `src` (see the module docs).
pub(crate) fn plan(src: &[u8]) -> Plan {
    let toks = lex(src);
    let text = |t: &Tok| &src[t.start..t.end];
    let is_ident = |t: Option<&Tok>, w: &str| t.is_some_and(|t| t.kind == Kind::Ident && text(t) == w.as_bytes());
    let punct = |t: Option<&Tok>, c: u8| t.is_some_and(|t| t.kind == Kind::Punct(c));
    let ident_in = |t: Option<&Tok>, set: &[&str]| {
        t.is_some_and(|t| t.kind == Kind::Ident && set.iter().any(|w| w.as_bytes() == text(t)))
    };
    let macro_tok = |t: Option<&Tok>, min: usize, underscore: bool| {
        t.is_some_and(|t| t.kind == Kind::Ident && is_macro(text(t), min) && (!underscore || text(t).contains(&b'_')))
    };
    let levels = declaration_levels(&toks, src);
    let mut masked = vec![false; toks.len()];
    let mut out = Vec::new();
    let mut conditions = Vec::new();
    // Masks tokens `a..=b` and everything between them (string literals
    // and comments inside a macro's arguments).
    let mask_range = |masked: &mut Vec<bool>, out: &mut Vec<(usize, usize)>, a: usize, b: usize| {
        for m in masked.iter_mut().take(b + 1).skip(a) {
            *m = true;
        }
        out.push((toks[a].start, toks[b].end));
    };

    for i in 0..toks.len() {
        if toks[i].kind == Kind::Directive {
            if let Some(range) = include_test_condition(src, &toks[i]) {
                conditions.push(range);
            }
            // The null directive (`#` alone) is valid C the grammars reject.
            let line = &src[toks[i].start..toks[i].end];
            if line[1..].iter().all(|b| b.is_ascii_whitespace() || *b == b'\\') {
                out.push((toks[i].start, toks[i].end));
                continue;
            }
            // Block comments inside a directive (`#define F(x) (x) /* … */ \`)
            // break the grammar's macro bodies. A trailing one stays: blanked,
            // it would leave spaces where the grammar wants the line end
            // right after a valueless `#define X`.
            let line = &src[toks[i].start..toks[i].end];
            let mut k = 0;
            while let Some(open) = line[k..].windows(2).position(|w| w == b"/*") {
                let a = k + open;
                let Some(close) = line[a + 2..].windows(2).position(|w| w == b"*/") else { break };
                let b = a + 2 + close + 2;
                let trailing = line[b..].iter().all(|c| c.is_ascii_whitespace());
                if !trailing {
                    out.push((toks[i].start + a, toks[i].start + b));
                }
                k = b;
            }
            // `#ifdef __cplusplus` / `extern "C" {` / `#endif` and the matching
            // `}`: braces the grammar sees unbalanced.
            if is_cplusplus_test(text(&toks[i])) {
                let body: Vec<usize> = (i + 1..toks.len()).take_while(|&k| toks[k].kind != Kind::Directive).collect();
                let closes = body.last().and_then(|&k| toks.get(k + 1)).is_some_and(|t| t.kind == Kind::Directive);
                let open =
                    body.len() == 2 && is_ident(Some(&toks[body[0]]), "extern") && punct(Some(&toks[body[1]]), b'{');
                let close = body.len() == 1 && punct(Some(&toks[body[0]]), b'}');
                if closes && (open || close) {
                    mask_range(&mut masked, &mut out, body[0], *body.last().unwrap());
                }
            }
            continue;
        }
        if masked[i] || toks[i].kind != Kind::Ident {
            continue;
        }
        let prev = i.checked_sub(1).map(|p| &toks[p]);
        let prev_masked = i.checked_sub(1).is_some_and(|p| masked[p]);
        let next = toks.get(i + 1);

        // Rule 2: class / struct / union MACRO… name {
        if ident_in(Some(&toks[i]), &["class", "struct", "union"]) {
            let mut j = i + 1;
            loop {
                if !macro_tok(toks.get(j), 2, false) {
                    break;
                }
                // `class FMT_SO_VISIBILITY("default") name`.
                let end = if punct(toks.get(j + 1), b'(') { matching_paren(&toks, j + 1) } else { Some(j) };
                match end {
                    Some(e) if toks.get(e + 1).is_some_and(|t| t.kind == Kind::Ident) => j = e + 1,
                    _ => break,
                }
            }
            if j > i + 1 {
                let after = toks.get(j + 1);
                let ok = punct(after, b'{') || punct(after, b':') || punct(after, b'<') || is_ident(after, "final");
                if ok {
                    mask_range(&mut masked, &mut out, i + 1, j - 1);
                }
            }
            continue;
        }

        // Rule 1: bare line.
        if is_macro(text(&toks[i]), 3) {
            let alone = prev.is_none_or(|p| p.line < toks[i].line || p.kind == Kind::Directive)
                && next.is_none_or(|n| n.line > toks[i].line);
            let prev_ok = prev_masked
                || prev.is_none_or(|p| matches!(p.kind, Kind::Directive | Kind::Punct(b';' | b'{' | b'}' | b':')));
            let next_ok =
                next.is_none_or(|n| matches!(n.kind, Kind::Ident | Kind::Directive | Kind::Punct(b'~' | b'[')));
            if alone && prev_ok && next_ok {
                mask_range(&mut masked, &mut out, i, i);
                continue;
            }
        }

        if !macro_tok(Some(&toks[i]), 3, true) {
            continue;
        }

        // Rule 7: `if MACRO (cond)` (a macro for `constexpr`), also with
        // arguments: `if MACRO(x) (cond)`.
        if is_ident(prev, "if") && punct(next, b'(') {
            match matching_paren(&toks, i + 1) {
                Some(c) if punct(toks.get(c + 1), b'(') => mask_range(&mut masked, &mut out, i, c),
                _ => mask_range(&mut masked, &mut out, i, i),
            }
            continue;
        }

        // Rule 6: an enable-if macro in a template parameter list.
        if (punct(prev, b',') || punct(prev, b'<'))
            && punct(next, b'(')
            && let Some(close) = matching_paren(&toks, i + 1)
            && (punct(toks.get(close + 1), b'>') || punct(toks.get(close + 1), b','))
            && in_template_head(&toks, src, i)
        {
            let from = if punct(prev, b',') { i - 1 } else { i };
            let to = if punct(prev, b'<') && punct(toks.get(close + 1), b',') { close + 1 } else { close };
            mask_range(&mut masked, &mut out, from, to);
            continue;
        }

        // Rule 3: leading specifier macros, possibly a chain, possibly with
        // an argument list.
        // `public:` too, but not `ns::`.
        let after_label = punct(prev, b':') && !(i >= 2 && punct(toks.get(i - 2), b':'));
        let start_ok = prev_masked
            || after_label
            || prev.is_none_or(|p| {
                matches!(p.kind, Kind::Directive | Kind::Punct(b';' | b'{' | b'}' | b'>' | b']'))
                    || ident_in(Some(p), SPECIFIERS)
            });
        if start_ok {
            let mut j = i;
            let mut ends = Vec::new();
            loop {
                if !macro_tok(toks.get(j), 3, true) {
                    break;
                }
                let mut end = j;
                if punct(toks.get(j + 1), b'(') {
                    match matching_paren(&toks, j + 1) {
                        Some(close) => end = close,
                        None => break,
                    }
                }
                ends.push((j, end));
                j = end + 1;
            }
            if let Some(&(_, last)) = ends.last() {
                let f = toks.get(last + 1);
                let g = toks.get(last + 2);
                let qualified =
                    f.is_some_and(|t| t.kind == Kind::Ident) && punct(g, b':') && punct(toks.get(last + 3), b':');
                // `MACRO Type name(` / `MACRO Type* name`: a type name cannot
                // be followed by another type name.
                let two_types = f.is_some_and(|t| t.kind == Kind::Ident)
                    && !ident_in(f, &["const", "volatile"])
                    && (g.is_some_and(|t| t.kind == Kind::Ident)
                        || ((punct(g, b'*') || punct(g, b'&') || punct(g, b'<'))
                            && toks.get(last + 3).is_some_and(|t| t.kind == Kind::Ident)));
                // `MACRO Name(args) [noexcept] : init(…)`: a constructor; also
                // `MACRO1 MACRO2 Name(args)` (two macros cannot be a type and
                // a name).
                let constructor = f.is_some_and(|t| t.kind == Kind::Ident)
                    && punct(g, b'(')
                    && (ends.len() >= 2
                        || matching_paren(&toks, last + 2).is_some_and(|c| {
                            let mut k = c + 1;
                            while ident_in(toks.get(k), &["noexcept", "const", "override"]) {
                                k += 1;
                                if punct(toks.get(k), b'(') {
                                    match matching_paren(&toks, k) {
                                        Some(e) => k = e + 1,
                                        None => return false,
                                    }
                                }
                            }
                            punct(toks.get(k), b':') && !punct(toks.get(k + 1), b':')
                        }));
                let follows = ident_in(f, FOLLOWERS)
                    || (is_ident(f, "const") && ident_in(g, FUNDAMENTAL))
                    || punct(f, b'~')
                    || punct(f, b'[')
                    || qualified
                    || two_types
                    || constructor;
                if follows {
                    for &(a, b) in &ends {
                        mask_range(&mut masked, &mut out, a, b);
                    }
                    continue;
                }
            }
        }

        // Rule 5: a statement-like macro call at declaration level, alone at
        // the end of its line (`FMT_PRAGMA_GCC(push_options)`).
        let decl_start = prev_masked
            || prev.is_none_or(|p| matches!(p.kind, Kind::Directive | Kind::Punct(b';' | b'{' | b'}' | b':')));
        if decl_start
            && levels[i]
            && punct(next, b'(')
            && let Some(close) = matching_paren(&toks, i + 1)
        {
            let after = toks.get(close + 1);
            let line_end = after.is_none_or(|a| a.line > toks[close].line || a.kind == Kind::Directive);
            let after_ok =
                after.is_none_or(|a| matches!(a.kind, Kind::Ident | Kind::Directive | Kind::Punct(b'~' | b'[' | b'}')));
            // `FMT_TYPE_CONSTANT(int, int_type);`: types as arguments, which
            // no call expression can have.
            let typed_args = punct(after, b';')
                && (i + 2..close).any(|k| {
                    ident_in(Some(&toks[k]), FUNDAMENTAL)
                        || ident_in(Some(&toks[k]), &["const", "struct", "class", "typename"])
                });
            if (line_end && after_ok) || typed_args {
                mask_range(&mut masked, &mut out, i, close);
                continue;
            }
        }

        // Rule 8: an attribute macro after a parameter name
        // (`const T& x FMT_LIFETIMEBOUND, int y`).
        if (punct(next, b',') || punct(next, b')'))
            && prev.is_some_and(|p| p.kind == Kind::Ident || p.kind == Kind::Punct(b']'))
            && !ident_in(prev, NOT_A_NAME)
            && levels[i]
        {
            mask_range(&mut masked, &mut out, i, i);
            continue;
        }

        // Rule 9: a macro before a parameter's type and name
        // (`f(EV_P_ ev_io *w)`, `f(UNUSED_ATTR int x)`).
        if (punct(prev, b'(') || punct(prev, b','))
            && levels[i]
            && next.is_some_and(|t| t.kind == Kind::Ident)
            && toks.get(i + 2).is_some_and(|t| t.kind == Kind::Ident || matches!(t.kind, Kind::Punct(b'*' | b'&')))
            && !punct(toks.get(i + 2), b'(')
        {
            mask_range(&mut masked, &mut out, i, i);
            continue;
        }

        // Rule 4: trailing specifier after a parameter list (not a cast:
        // the `(` must follow a name).
        let after_params = punct(prev, b')')
            && matching_open(&toks, i - 1).is_some_and(|o| {
                o > 0
                    && toks[o - 1].kind == Kind::Ident
                    && !ident_in(
                        Some(&toks[o - 1]),
                        &["if", "while", "for", "switch", "return", "sizeof", "alignof", "decltype", "catch", "case"],
                    )
            });
        let after_decl = after_params || ident_in(prev, &["const", "override", "noexcept"]) || prev_masked;
        if after_decl {
            let mut j = i;
            while macro_tok(toks.get(j + 1), 3, true) {
                j += 1;
            }
            let f = toks.get(j + 1);
            let follows = punct(f, b';')
                || punct(f, b'{')
                || punct(f, b'=')
                || punct(f, b':')
                || (punct(f, b'-') && punct(toks.get(j + 2), b'>'))
                || ident_in(f, &["const", "override", "final"]);
            if follows && !punct(next, b'(') {
                mask_range(&mut masked, &mut out, i, j);
            }
        }
    }
    out.sort_unstable();
    Plan { blanks: out, conditions }
}

/// Directive kind of a preprocessor line: `if` (also `ifdef` / `ifndef`),
/// `elif` (also `elifdef` / `elifndef`), `else`, `endif`, or `None`.
pub(crate) fn directive_kind(line: &[u8]) -> Option<&'static str> {
    let rest = line.get(1..)?;
    let start = rest.iter().position(|b| !matches!(b, b' ' | b'\t'))?;
    let word: Vec<u8> = rest[start..].iter().take_while(|b| b.is_ascii_alphabetic()).copied().collect();
    match word.as_slice() {
        b"if" | b"ifdef" | b"ifndef" => Some("if"),
        b"elif" | b"elifdef" | b"elifndef" => Some("elif"),
        b"else" => Some("else"),
        b"endif" => Some("endif"),
        _ => None,
    }
}

/// `#ifdef __cplusplus` / `#if defined(__cplusplus)`.
pub(crate) fn is_cplusplus_test(line: &[u8]) -> bool {
    let words: Vec<&[u8]> =
        line.split(|b| !(b.is_ascii_alphanumeric() || *b == b'_')).filter(|w| !w.is_empty()).collect();
    matches!(words.as_slice(), [b"ifdef", b"__cplusplus"] | [b"if", b"defined", b"__cplusplus"])
}

/// Whether token `i` is inside a `template <…>` head: a `template` `<`
/// before it in the same statement with unclosed angle brackets.
fn in_template_head(toks: &[Tok], src: &[u8], i: usize) -> bool {
    let mut angle = 0i32;
    for k in (i.saturating_sub(MAX_ARG_TOKENS)..i).rev() {
        match toks[k].kind {
            Kind::Punct(b'>') => angle -= 1,
            Kind::Punct(b'<') => {
                angle += 1;
                if angle > 0 && k > 0 && &src[toks[k - 1].start..toks[k - 1].end] == b"template" {
                    return true;
                }
            }
            Kind::Punct(b';' | b'{' | b'}') | Kind::Directive => return false,
            _ => {}
        }
    }
    false
}

/// The condition of an `#if` / `#elif` line, replaced by `1`: conditions
/// are never evaluated (both branches are parsed), and the grammars cannot
/// parse many of them (`__has_include(<x>)`, `FOO(clang::attr)`).
fn include_test_condition(src: &[u8], t: &Tok) -> Option<(usize, usize)> {
    let line = &src[t.start..t.end];
    let mut i = 1;
    while i < line.len() && matches!(line[i], b' ' | b'\t') {
        i += 1;
    }
    let kw_len =
        if line[i..].starts_with(b"if") && !line[i..].starts_with(b"ifdef") && !line[i..].starts_with(b"ifndef") {
            2
        } else if line[i..].starts_with(b"elif")
            && !line[i..].starts_with(b"elifdef")
            && !line[i..].starts_with(b"elifndef")
        {
            4
        } else {
            return None;
        };
    let cond = i + kw_len;
    if cond >= line.len() || !matches!(line[cond], b' ' | b'\t' | b'(') {
        return None;
    }
    let body = &line[cond..];
    (!body.iter().all(|b| b.is_ascii_whitespace())).then_some((t.start + cond, t.end))
}

/// Index of the `)` matching the `(` at `open`, within [`MAX_ARG_TOKENS`].
pub(crate) fn matching_paren(toks: &[Tok], open: usize) -> Option<usize> {
    let mut depth = 0i32;
    for (k, t) in toks.iter().enumerate().skip(open).take(MAX_ARG_TOKENS) {
        match t.kind {
            Kind::Punct(b'(') => depth += 1,
            Kind::Punct(b')') => {
                depth -= 1;
                if depth == 0 {
                    return Some(k);
                }
            }
            Kind::Punct(b';' | b'{' | b'}') | Kind::Directive => return None,
            _ => {}
        }
    }
    None
}

/// `src` with the [`plan`] applied (macro tokens replaced by spaces,
/// header-test conditions by `1`), or `None` when there is nothing to
/// change.
pub(crate) fn mask(src: &[u8]) -> Option<Vec<u8>> {
    let plan = plan(src);
    if plan.blanks.is_empty() && plan.conditions.is_empty() {
        return None;
    }
    let mut out = src.to_vec();
    for (a, b) in plan.blanks {
        for byte in &mut out[a..b] {
            if *byte != b'\n' && *byte != b'\r' {
                *byte = b' ';
            }
        }
    }
    for (a, b) in plan.conditions {
        let mut first = true;
        for k in a..b {
            let c = out[k];
            let continuation = c == b'\\' && matches!(out.get(k + 1), Some(b'\n' | b'\r'));
            if matches!(c, b'\n' | b'\r') || continuation {
                continue;
            }
            if first && !c.is_ascii_whitespace() {
                out[k] = b'1';
                first = false;
            } else {
                out[k] = b' ';
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn masked_words(src: &str) -> Vec<String> {
        macro_ranges(src.as_bytes()).into_iter().map(|(a, b)| src[a..b].to_string()).collect()
    }

    #[test]
    fn bare_lines() {
        assert_eq!(
            masked_words("FMT_BEGIN_NAMESPACE\nvoid f();\nFMT_END_NAMESPACE\n"),
            ["FMT_BEGIN_NAMESPACE", "FMT_END_NAMESPACE"]
        );
        assert_eq!(masked_words("class A {\n  Q_OBJECT\npublic:\n};\n"), ["Q_OBJECT"]);
        assert_eq!(masked_words("#include <x>\nG_BEGIN_DECLS\nint g;\n"), ["G_BEGIN_DECLS"]);
        // After a masked statement-like call.
        assert_eq!(
            masked_words("}\nFMT_PRAGMA_GCC(pop_options)\nFMT_END_EXPORT\nFMT_END_NAMESPACE\n"),
            ["FMT_PRAGMA_GCC(pop_options)", "FMT_END_EXPORT", "FMT_END_NAMESPACE"]
        );
        // Last enumerator, expression continuation, statement macro with `;`.
        assert!(masked_words("enum E {\n  A,\n  LAST\n};\n").is_empty());
        assert!(masked_words("int x = a +\n  FOO\n  + 1;\n").is_empty());
        assert!(masked_words("void f() {\n  DO_IT;\n}\n").is_empty());
        // Inside a define body or a comment.
        assert!(masked_words("#define X \\\n  FOO_BAR\n").is_empty());
        assert!(masked_words("/*\nFOO_BAR\n*/\nint x;\n").is_empty());
    }

    #[test]
    fn class_export_macros() {
        assert_eq!(masked_words("class FMT_API format_error : public std::runtime_error {};"), ["FMT_API"]);
        assert_eq!(masked_words("struct A_EXPORT B_DEPRECATED name final {};"), ["A_EXPORT B_DEPRECATED"]);
        // `struct FOO bar;` is a variable of type `struct FOO`.
        assert!(masked_words("struct FOO bar;").is_empty());
        assert!(masked_words("class FOO {};").is_empty());
    }

    #[test]
    fn leading_and_trailing_specifiers() {
        assert_eq!(masked_words("FMT_API void vprint(int);"), ["FMT_API"]);
        assert_eq!(masked_words("FMT_NODISCARD FMT_INLINE auto f() -> int;"), ["FMT_NODISCARD", "FMT_INLINE"]);
        assert_eq!(masked_words("FMT_CONSTEXPR basic_string_view(const Char* s) : data_(s) {}"), ["FMT_CONSTEXPR"]);
        assert_eq!(masked_words("FMT_CONSTEXPR buffer(int g) noexcept : g_(g) {}"), ["FMT_CONSTEXPR"]);
        assert_eq!(masked_words("FMT_CONSTEXPR FMT_INLINE value(int x) {}"), ["FMT_CONSTEXPR", "FMT_INLINE"]);
        assert!(masked_words("MY_TYPE make(int x) {}").is_empty());
        assert_eq!(masked_words("FMT_API std::string vformat();"), ["FMT_API"]);
        assert_eq!(masked_words("static FMT_INLINE int g();"), ["FMT_INLINE"]);
        assert_eq!(
            masked_words("JSON_HEDLEY_RETURNS_NON_NULL const char* type_name() const;"),
            ["JSON_HEDLEY_RETURNS_NON_NULL"]
        );
        assert_eq!(
            masked_words("JSON_HEDLEY_DEPRECATED_FOR(3.1, x) static int f();"),
            ["JSON_HEDLEY_DEPRECATED_FOR(3.1, x)"]
        );
        assert_eq!(masked_words("void f() const FMT_NOEXCEPT;"), ["FMT_NOEXCEPT"]);
        assert_eq!(masked_words("int g() FMT_OVERRIDE {}"), ["FMT_OVERRIDE"]);
        // A type followed by a name, a call, a type before `const x`.
        assert!(masked_words("MY_TYPE value;").is_empty());
        assert!(masked_words("x = FOO_BAR(1);").is_empty());
        assert!(masked_words("MY_TYPE const x = 1;").is_empty());
        assert!(masked_words("s = \"FOO_BAR void\";").is_empty());
    }

    #[test]
    fn calls_templates_conditions_and_casts() {
        assert_eq!(masked_words("FMT_PRAGMA_GCC(push_options)\nint x;\n"), ["FMT_PRAGMA_GCC(push_options)"]);
        assert_eq!(
            masked_words("FMT_PRAGMA_GCC(optimize(\"Og\"))\nFMT_PRAGMA_MSVC(warning(disable : 4702))\nint x;"),
            ["FMT_PRAGMA_GCC(optimize(\"Og\"))", "FMT_PRAGMA_MSVC(warning(disable : 4702))"]
        );
        assert_eq!(masked_words("namespace a {\nQ_DISABLE_COPY(Foo)\nvoid f();\n}\n"), ["Q_DISABLE_COPY(Foo)"]);
        assert_eq!(
            masked_words("FMT_TYPE_CONSTANT(long long, long_long_type);"),
            ["FMT_TYPE_CONSTANT(long long, long_long_type)"]
        );
        assert!(masked_words("DECLARE_THING(foo, bar);").is_empty());
        assert_eq!(
            masked_words("#ifdef __cplusplus\nextern \"C\" {\n#endif\nint f(void);\n#ifdef __cplusplus\n}\n#endif\n"),
            ["extern \"C\" {", "}"]
        );
        // Not inside a function body, and never before `{` (a test macro).
        assert!(masked_words("void f() {\n  LOG_IT(x)\n  g();\n}\n").is_empty());
        assert!(masked_words("TEST_F(Suite, Name)\n{\n}\n").is_empty());
        assert_eq!(
            masked_words("template <typename T, FMT_ENABLE_IF(is_x<T>::value)>\nvoid f(T);"),
            [", FMT_ENABLE_IF(is_x<T>::value)"]
        );
        assert!(masked_words("int y = f(a, FOO_BAR(x));").is_empty());
        assert_eq!(masked_words("void f() { if FMT_CONSTEXPR20 (x > 1) { g(); } }"), ["FMT_CONSTEXPR20"]);
        // A cast of a macro constant is not a trailing specifier.
        assert!(masked_words("obj = (void*)REDIS_REPLY_NIL;").is_empty());
        assert!(masked_words("if (x) RETURN_IF;").is_empty());
        assert_eq!(masked_words("FFC_IMPL_INLINE ffc_result ffc_parse(const char *s);"), ["FFC_IMPL_INLINE"]);
        assert!(masked_words("MY_TYPE value, other;").is_empty());
        let p =
            plan(b"#if defined(X) && __has_include(<a.h>) \\\n  || 0\nint x;\n#ifdef __has_include\n#elif X\n#endif\n");
        assert_eq!(p.conditions.len(), 2);
        assert_eq!(masked_words("class FMT_VIS(\"default\") format_error : public E {};"), ["FMT_VIS(\"default\")"]);
        assert_eq!(masked_words("extern template FMT_API auto f<char>(int) -> int;"), ["FMT_API"]);
        assert_eq!(masked_words("auto join(const T& t FMT_LIFETIMEBOUND, int s);"), ["FMT_LIFETIMEBOUND"]);
        assert!(masked_words("int n = sizeof(struct FOO_S) + f(a, B_C) + g(return_ FOO_X);").len() <= 1);
        assert!(masked_words("typedef unsigned long MY_ULONG;").is_empty());
        assert_eq!(masked_words("static void cb(EV_P_ ev_io *w, int r);"), ["EV_P_"]);
        assert_eq!(masked_words("int f(char *argv[] G_UNUSED);"), ["G_UNUSED"]);
        assert!(masked_words("void f(MY_TYPE x, int y);").is_empty());
        assert_eq!(masked_words("int a;\n#\nint b;\n"), ["#"]);
        let m = mask(b"#if __has_include(<a.h>) \\\n  || 0\nint x;\n").unwrap();
        assert_eq!(String::from_utf8(m).unwrap(), "#if 1                    \\\n      \nint x;\n");
    }

    #[test]
    fn mask_preserves_offsets() {
        let src = b"FMT_BEGIN_NAMESPACE\r\nvoid f();\n";
        let m = mask(src).unwrap();
        assert_eq!(m.len(), src.len());
        assert_eq!(&m[..21], b"                   \r\n");
        assert!(mask(b"int x;").is_none());
        let m = mask(b"#define F(x) ((x) /* note */ \\\n  + 1)\n").unwrap();
        assert_eq!(String::from_utf8(m).unwrap(), "#define F(x) ((x)            \\\n  + 1)\n");
        assert!(mask(b"#define X /* trailing\n note */\nint y;\n").is_none());
        let inc = includes(b"#include \"a.h\"\n  #  include <b/c.h> // x\n/* #include \"no.h\" */\n#include FOO\n#include_next <d.h>\n");
        let mods: Vec<&str> = inc.iter().map(|(m, ..)| m.as_str()).collect();
        assert_eq!(mods, ["a.h", "<b/c.h>"]);
        // Raw strings are skipped as a whole.
        assert!(masked_words("auto s = R\"x(\nFOO_BAR\n)x\";\n").is_empty());
    }
}
