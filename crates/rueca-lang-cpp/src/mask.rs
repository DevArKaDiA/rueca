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
//!    name cannot be followed by another; also `LUA_API lua_State
//!    *(lua_newstate) (…)`). A macro with arguments before `[` is masked
//!    only before an attribute (`[[…]]`): `MP_STATE_PORT(h)[i] = c` is a
//!    subscripted call.
//! 4. **Trailing specifier**: containing `_`, after the `)` of a parameter
//!    list (not of a cast or an `if` condition; `operator=(…)` included),
//!    or a method's `const`, `override` or `noexcept` (not a pointer's:
//!    `const char* const NAME = …` declares `NAME`), possibly with
//!    arguments (`GTEST_LOCK_EXCLUDED_(mu)`), and followed by `;`, `{`,
//!    `=`, `:`, `->`, `const`, `override`, `final` or another such macro
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
    let mut lexer = Lexer { src, literals, i: 0, line: 0, at_line_start: true, toks: Vec::new() };
    while lexer.i < src.len() {
        lexer.step();
    }
    lexer.toks
}

/// State of [`lex_impl`]: the position, its line, and the tokens so far.
struct Lexer<'a> {
    src: &'a [u8],
    /// Whether literals become [`Kind::Literal`] tokens (else they are skipped).
    literals: bool,
    i: usize,
    line: u32,
    at_line_start: bool,
    toks: Vec<Tok>,
}

impl Lexer<'_> {
    /// Consumes the whitespace, comment or token at `self.i`.
    fn step(&mut self) {
        let c = self.src[self.i];
        if c == b'\n' {
            self.line += 1;
            self.at_line_start = true;
            self.i += 1;
            return;
        }
        if c.is_ascii_whitespace() {
            self.i += 1;
            return;
        }
        if self.at_line_start && c == b'#' {
            self.directive();
            return;
        }
        self.at_line_start = false;
        match (c, self.src.get(self.i + 1)) {
            (b'/', Some(b'/')) => self.i = line_end(self.src, self.i),
            (b'/', Some(b'*')) => self.i = skip_block_comment(self.src, self.i, &mut self.line),
            (b'"' | b'\'', _) => {
                let (start, first_line) = (self.i, self.line);
                self.i = skip_quoted(self.src, self.i, &mut self.line);
                self.push_literal(start, first_line);
            }
            _ if c.is_ascii_alphabetic() || c == b'_' => self.word(),
            _ if c.is_ascii_digit() => self.number(),
            _ => {
                self.toks.push(Tok { start: self.i, end: self.i + 1, line: self.line, kind: Kind::Punct(c) });
                self.i += 1;
            }
        }
    }

    /// Preprocessor line, with `\` continuations; a block comment that
    /// starts on it may run over several lines (the line ends after the
    /// comment).
    fn directive(&mut self) {
        let (start, first_line) = (self.i, self.line);
        loop {
            self.i = directive_line_end(self.src, self.i, &mut self.line);
            if !continued(self.src, start, self.i) {
                break;
            }
            self.i += 1;
            self.line += 1;
        }
        self.toks.push(Tok { start, end: self.i, line: first_line, kind: Kind::Directive });
    }

    /// An identifier, or a literal with a raw or encoding prefix
    /// (`R"x(…)x"`, `u8"…"`, `L'x'`).
    fn word(&mut self) {
        let start = self.i;
        while self.src.get(self.i).is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_') {
            self.i += 1;
        }
        let word = &self.src[start..self.i];
        let quote = self.src.get(self.i).copied();
        let first_line = self.line;
        // Raw string literal: R"delim( … )delim" (also u8R, LR, uR, UR).
        if quote == Some(b'"') && matches!(word, b"R" | b"u8R" | b"LR" | b"uR" | b"UR") {
            self.i = skip_raw(self.src, self.i, &mut self.line);
            self.push_literal(start, first_line);
            return;
        }
        // Encoding prefix of a literal: u8"…", L'x'.
        if self.literals && matches!(quote, Some(b'"' | b'\'')) && matches!(word, b"L" | b"u" | b"U" | b"u8") {
            self.i = skip_quoted(self.src, self.i, &mut self.line);
            self.push_literal(start, first_line);
            return;
        }
        self.toks.push(Tok { start, end: self.i, line: self.line, kind: Kind::Ident });
    }

    fn number(&mut self) {
        let start = self.i;
        while self.src.get(self.i).is_some_and(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'\'')) {
            self.i += 1;
        }
        self.toks.push(Tok { start, end: self.i, line: self.line, kind: Kind::Punct(b'0') });
    }

    /// Records the literal `start..self.i` when literals are lexed.
    fn push_literal(&mut self, start: usize, line: u32) {
        if self.literals {
            self.toks.push(Tok { start, end: self.i, line, kind: Kind::Literal });
        }
    }
}

/// Index of the `\n` ending the line at `i` (or the end of `src`).
fn line_end(src: &[u8], mut i: usize) -> usize {
    while i < src.len() && src[i] != b'\n' {
        i += 1;
    }
    i
}

/// Index after the block comment opening at `i`, counting its newlines.
fn skip_block_comment(src: &[u8], mut i: usize, line: &mut u32) -> usize {
    let n = src.len();
    i += 2;
    while i + 1 < n && !(src[i] == b'*' && src[i + 1] == b'/') {
        if src[i] == b'\n' {
            *line += 1;
        }
        i += 1;
    }
    (i + 2).min(n)
}

/// End of one physical line of a directive from `i`: the `\n` (or the
/// end of `src`), skipping block comments, which may span lines.
fn directive_line_end(src: &[u8], mut i: usize, line: &mut u32) -> usize {
    while i < src.len() && src[i] != b'\n' {
        match (src[i], src.get(i + 1)) {
            (b'/', Some(b'*')) => i = skip_block_comment(src, i, line),
            (b'/', Some(b'/')) => return line_end(src, i),
            _ => i += 1,
        }
    }
    i
}

/// Whether the directive `start..i` ends with a `\` continuation (`i` at
/// its `\n`).
fn continued(src: &[u8], start: usize, i: usize) -> bool {
    let mut j = i;
    if j > start && src[j - 1] == b'\r' {
        j -= 1;
    }
    i < src.len() && j > start && src[j - 1] == b'\\'
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
    let view = View { src, toks: &toks };
    let mut planner = Planner {
        view,
        levels: declaration_levels(&toks, src),
        masked: vec![false; toks.len()],
        blanks: Vec::new(),
        conditions: Vec::new(),
    };
    for i in 0..toks.len() {
        planner.visit(i);
    }
    planner.blanks.sort_unstable();
    Plan { blanks: planner.blanks, conditions: planner.conditions }
}

/// Keywords before a `(` that make it a condition, a cast or an operand,
/// not a parameter list (rule 4).
const NOT_A_FUNCTION: &[&str] =
    &["if", "while", "for", "switch", "return", "sizeof", "alignof", "decltype", "catch", "case"];

/// Qualifiers of a method that a trailing specifier may follow (rule 4).
const METHOD_QUALIFIERS: &[&str] = &["const", "override", "noexcept"];

/// Whether `t` is the punctuation `c`.
fn punct(t: Option<&Tok>, c: u8) -> bool {
    t.is_some_and(|t| t.kind == Kind::Punct(c))
}

/// Whether `t` is an identifier.
fn ident(t: Option<&Tok>) -> bool {
    t.is_some_and(|t| t.kind == Kind::Ident)
}

/// The tokens of the file being planned, with their text: the read-only
/// side of [`plan`].
#[derive(Clone, Copy)]
struct View<'a> {
    src: &'a [u8],
    toks: &'a [Tok],
}

impl<'a> View<'a> {
    fn get(self, i: usize) -> Option<&'a Tok> {
        self.toks.get(i)
    }

    fn text(self, t: &Tok) -> &'a [u8] {
        &self.src[t.start..t.end]
    }

    fn is_ident(self, t: Option<&Tok>, w: &str) -> bool {
        t.is_some_and(|t| t.kind == Kind::Ident && self.text(t) == w.as_bytes())
    }

    fn ident_in(self, t: Option<&Tok>, set: &[&str]) -> bool {
        t.is_some_and(|t| t.kind == Kind::Ident && set.iter().any(|w| w.as_bytes() == self.text(t)))
    }

    /// A macro-looking identifier of at least `min` bytes, containing `_`
    /// when `underscore`.
    fn macro_tok(self, t: Option<&Tok>, min: usize, underscore: bool) -> bool {
        t.is_some_and(|t| {
            t.kind == Kind::Ident && is_macro(self.text(t), min) && (!underscore || self.text(t).contains(&b'_'))
        })
    }

    /// Rule 2: the index after the macros of `class MACRO… name` starting
    /// at `j` (`j` itself when there is none).
    fn class_macros_end(self, mut j: usize) -> usize {
        while self.macro_tok(self.get(j), 2, false) {
            // `class FMT_SO_VISIBILITY("default") name`.
            let end = if punct(self.get(j + 1), b'(') { matching_paren(self.toks, j + 1) } else { Some(j) };
            match end {
                Some(e) if ident(self.get(e + 1)) => j = e + 1,
                _ => break,
            }
        }
        j
    }

    /// Rule 3: the chain of specifier macros from `i`, as (first, last)
    /// token of each (the last is the `)` of an argument list).
    fn macro_chain(self, i: usize) -> Vec<(usize, usize)> {
        let mut j = i;
        let mut ends = Vec::new();
        while self.macro_tok(self.get(j), 3, true) {
            let mut end = j;
            if punct(self.get(j + 1), b'(') {
                let Some(close) = matching_paren(self.toks, j + 1) else { break };
                end = close;
            }
            ends.push((j, end));
            j = end + 1;
        }
        ends
    }

    /// Rule 3: whether what follows the chain `ends` makes it a run of
    /// declaration specifiers.
    fn specifiers_follow(self, ends: &[(usize, usize)]) -> bool {
        let Some(&(first, last)) = ends.last() else { return false };
        let (f, g) = (self.get(last + 1), self.get(last + 2));
        // `MACRO(args)[i] = x;` is a subscripted call, unless an attribute
        // follows (`MACRO(args) [[nodiscard]]`).
        let with_args = first != last;
        self.ident_in(f, FOLLOWERS)
            || (self.is_ident(f, "const") && self.ident_in(g, FUNDAMENTAL))
            || punct(f, b'~')
            || (punct(f, b'[') && (!with_args || punct(g, b'[')))
            || self.qualified_name_at(last + 1)
            || self.type_and_name_at(last + 1)
            || self.constructor_at(last + 1, ends.len())
    }

    /// `ns::…` at `k`.
    fn qualified_name_at(self, k: usize) -> bool {
        ident(self.get(k)) && punct(self.get(k + 1), b':') && punct(self.get(k + 2), b':')
    }

    /// `Type name(` / `Type* name` at `k`: a type name cannot be followed
    /// by another type name.
    fn type_and_name_at(self, k: usize) -> bool {
        let (f, g) = (self.get(k), self.get(k + 1));
        if !ident(f) || self.ident_in(f, &["const", "volatile"]) {
            return false;
        }
        let pointer = punct(g, b'*') || punct(g, b'&');
        ident(g)
            || ((pointer || punct(g, b'<')) && ident(self.get(k + 2)))
            // `LUA_API lua_State *(lua_newstate) (…)`.
            || (pointer && punct(self.get(k + 2), b'(') && ident(self.get(k + 3)) && punct(self.get(k + 4), b')'))
    }

    /// `Name(args) [noexcept] : init(…)` at `k`: a constructor after the
    /// macros; also after two or more macros (`MACRO1 MACRO2 Name(args)`:
    /// two macros cannot be a type and a name).
    fn constructor_at(self, k: usize, macros: usize) -> bool {
        if !ident(self.get(k)) || !punct(self.get(k + 1), b'(') {
            return false;
        }
        macros >= 2 || matching_paren(self.toks, k + 1).is_some_and(|c| self.initializer_list_at(c + 1))
    }

    /// A constructor's `: init(…)` at `k`, after qualifiers such as
    /// `noexcept(…)`.
    fn initializer_list_at(self, mut k: usize) -> bool {
        while self.ident_in(self.get(k), &["noexcept", "const", "override"]) {
            k += 1;
            if punct(self.get(k), b'(') {
                let Some(e) = matching_paren(self.toks, k) else { return false };
                k = e + 1;
            }
        }
        punct(self.get(k), b':') && !punct(self.get(k + 1), b':')
    }

    /// Rule 5: the call ending at `close` ends its line before a
    /// declaration.
    fn call_ends_line(self, close: usize) -> bool {
        let after = self.get(close + 1);
        after.is_none_or(|a| a.line > self.toks[close].line || a.kind == Kind::Directive)
            && after.is_none_or(|a| matches!(a.kind, Kind::Ident | Kind::Directive | Kind::Punct(b'~' | b'[' | b'}')))
    }

    /// Rule 5: `FMT_TYPE_CONSTANT(int, int_type);`: types as arguments,
    /// which no call expression can have.
    fn typed_args(self, open: usize, close: usize) -> bool {
        punct(self.get(close + 1), b';')
            && (open + 1..close).any(|k| {
                let t = Some(&self.toks[k]);
                self.ident_in(t, FUNDAMENTAL) || self.ident_in(t, &["const", "struct", "class", "typename"])
            })
    }

    /// Rule 4: token `i` follows the `)` of a parameter list (not of a
    /// cast: the `(` must follow a name).
    fn after_parameter_list(self, i: usize) -> bool {
        i > 0
            && punct(self.get(i - 1), b')')
            && matching_open(self.toks, i - 1).is_some_and(|o| {
                (o > 0 && ident(self.get(o - 1)) && !self.ident_in(self.get(o - 1), NOT_A_FUNCTION))
                    // `operator=(…)`, `operator()(…)`.
                    || (o.saturating_sub(3)..o).any(|k| self.is_ident(self.get(k), "operator"))
            })
    }

    /// Rule 4: token `i` follows a qualifier of a method (`f() const
    /// MACRO`), not of a pointer (`const char* const NAME = …` declares
    /// `NAME`).
    fn after_method_qualifier(self, i: usize) -> bool {
        i >= 2
            && self.ident_in(self.get(i - 1), METHOD_QUALIFIERS)
            && (punct(self.get(i - 2), b')') || self.ident_in(self.get(i - 2), METHOD_QUALIFIERS))
    }

    /// Rule 4: the last token of the trailing macros from `i`, with the
    /// arguments of the first (`GTEST_LOCK_EXCLUDED_(mutex_)`).
    fn trailing_end(self, i: usize) -> usize {
        let mut j = i;
        if punct(self.get(i + 1), b'(')
            && let Some(close) = matching_paren(self.toks, i + 1)
        {
            j = close;
        }
        while self.macro_tok(self.get(j + 1), 3, true) {
            j += 1;
        }
        j
    }

    /// Rule 4: what follows token `j` ends a declarator (`;`, `{`, `=`,
    /// `:`, `->`, `const`, `override`, `final`).
    fn ends_declarator(self, j: usize) -> bool {
        let f = self.get(j + 1);
        f.is_some_and(|t| matches!(t.kind, Kind::Punct(b';' | b'{' | b'=' | b':')))
            || (punct(f, b'-') && punct(self.get(j + 2), b'>'))
            || self.ident_in(f, &["const", "override", "final"])
    }
}

/// The token a rule looks at, with its neighbours.
struct Here<'a> {
    i: usize,
    prev: Option<&'a Tok>,
    next: Option<&'a Tok>,
    /// Whether the previous token is masked already.
    prev_masked: bool,
}

/// One masking rule of [`Planner::visit`]: whether it masked the token.
type Rule<'a> = fn(&mut Planner<'a>, &Here<'a>) -> bool;

/// State of [`plan`]: what is masked so far.
struct Planner<'a> {
    view: View<'a>,
    /// [`declaration_levels`] of the tokens.
    levels: Vec<bool>,
    masked: Vec<bool>,
    blanks: Vec<(usize, usize)>,
    conditions: Vec<(usize, usize)>,
}

impl<'a> Planner<'a> {
    /// Masks tokens `a..=b` and everything between them (string literals
    /// and comments inside a macro's arguments).
    fn mask_range(&mut self, a: usize, b: usize) {
        for m in self.masked.iter_mut().take(b + 1).skip(a) {
            *m = true;
        }
        self.blanks.push((self.view.toks[a].start, self.view.toks[b].end));
    }

    /// Applies the rules to token `i`.
    fn visit(&mut self, i: usize) {
        let view = self.view;
        let tok = &view.toks[i];
        if tok.kind == Kind::Directive {
            self.directive(i);
            return;
        }
        if self.masked[i] || tok.kind != Kind::Ident {
            return;
        }
        let here = Here {
            i,
            prev: i.checked_sub(1).map(|p| &view.toks[p]),
            next: view.get(i + 1),
            prev_masked: i.checked_sub(1).is_some_and(|p| self.masked[p]),
        };
        if view.ident_in(Some(tok), &["class", "struct", "union"]) {
            self.class_head(i);
            return;
        }
        if self.bare_line(&here) || !view.macro_tok(Some(tok), 3, true) {
            return;
        }
        // In order; the first rule that masks the token wins.
        let rules: [Rule<'a>; 7] = [
            Self::if_constexpr,
            Self::template_parameter,
            Self::leading_specifiers,
            Self::declaration_call,
            Self::parameter_attribute,
            Self::parameter_prefix,
            Self::trailing_specifiers,
        ];
        for rule in rules {
            if rule(self, &here) {
                return;
            }
        }
    }

    /// A preprocessor line: its `#if` condition, a null directive, block
    /// comments inside it, `#ifdef __cplusplus` braces.
    fn directive(&mut self, i: usize) {
        let t = &self.view.toks[i];
        if let Some(range) = include_test_condition(self.view.src, t) {
            self.conditions.push(range);
        }
        let line = self.view.text(t);
        // The null directive (`#` alone) is valid C the grammars reject.
        if line[1..].iter().all(|b| b.is_ascii_whitespace() || *b == b'\\') {
            self.blanks.push((t.start, t.end));
            return;
        }
        self.blanks.extend(inner_block_comments(line).into_iter().map(|(a, b)| (t.start + a, t.start + b)));
        if is_cplusplus_test(line) {
            self.cplusplus_braces(i);
        }
    }

    /// `#ifdef __cplusplus` / `extern "C" {` / `#endif` and the matching
    /// `}`: braces the grammar sees unbalanced. `i` is the `#ifdef`.
    fn cplusplus_braces(&mut self, i: usize) {
        let view = self.view;
        let toks = view.toks;
        let body: Vec<usize> = (i + 1..toks.len()).take_while(|&k| toks[k].kind != Kind::Directive).collect();
        let closes = body.last().and_then(|&k| toks.get(k + 1)).is_some_and(|t| t.kind == Kind::Directive);
        let open =
            body.len() == 2 && view.is_ident(Some(&toks[body[0]]), "extern") && punct(Some(&toks[body[1]]), b'{');
        let close = body.len() == 1 && punct(Some(&toks[body[0]]), b'}');
        if closes && (open || close) {
            self.mask_range(body[0], body[body.len() - 1]);
        }
    }

    /// Rule 2: `class` / `struct` / `union` MACRO… name `{`.
    fn class_head(&mut self, i: usize) {
        let view = self.view;
        let j = view.class_macros_end(i + 1);
        if j == i + 1 {
            return;
        }
        let after = view.get(j + 1);
        if punct(after, b'{') || punct(after, b':') || punct(after, b'<') || view.is_ident(after, "final") {
            self.mask_range(i + 1, j - 1);
        }
    }

    /// Rule 1: a macro alone on its line, between declarations.
    fn bare_line(&mut self, here: &Here<'a>) -> bool {
        let tok = &self.view.toks[here.i];
        if !is_macro(self.view.text(tok), 3) {
            return false;
        }
        let alone = here.prev.is_none_or(|p| p.line < tok.line || p.kind == Kind::Directive)
            && here.next.is_none_or(|n| n.line > tok.line);
        let prev_ok = here.prev_masked
            || here.prev.is_none_or(|p| matches!(p.kind, Kind::Directive | Kind::Punct(b';' | b'{' | b'}' | b':')));
        let next_ok =
            here.next.is_none_or(|n| matches!(n.kind, Kind::Ident | Kind::Directive | Kind::Punct(b'~' | b'[')));
        let hit = alone && prev_ok && next_ok;
        if hit {
            self.mask_range(here.i, here.i);
        }
        hit
    }

    /// Rule 7: `if MACRO (cond)` (a macro for `constexpr`), also with
    /// arguments: `if MACRO(x) (cond)`.
    fn if_constexpr(&mut self, here: &Here<'a>) -> bool {
        let view = self.view;
        if !view.is_ident(here.prev, "if") || !punct(here.next, b'(') {
            return false;
        }
        let end = match matching_paren(view.toks, here.i + 1) {
            Some(c) if punct(view.get(c + 1), b'(') => c,
            _ => here.i,
        };
        self.mask_range(here.i, end);
        true
    }

    /// Rule 6: an enable-if macro in a template parameter list, masked with
    /// the comma before it (or after it, first in the list).
    fn template_parameter(&mut self, here: &Here<'a>) -> bool {
        let view = self.view;
        let (after_comma, after_angle) = (punct(here.prev, b','), punct(here.prev, b'<'));
        if !(after_comma || after_angle) || !punct(here.next, b'(') {
            return false;
        }
        let Some(close) = matching_paren(view.toks, here.i + 1) else { return false };
        let before_comma = punct(view.get(close + 1), b',');
        if !(punct(view.get(close + 1), b'>') || before_comma) || !in_template_head(view.toks, view.src, here.i) {
            return false;
        }
        let from = if after_comma { here.i - 1 } else { here.i };
        let to = if after_angle && before_comma { close + 1 } else { close };
        self.mask_range(from, to);
        true
    }

    /// Rule 3: leading specifier macros, possibly a chain, possibly with
    /// an argument list.
    fn leading_specifiers(&mut self, here: &Here<'a>) -> bool {
        let view = self.view;
        // `public:` too, but not `ns::`.
        let after_label = punct(here.prev, b':') && !(here.i >= 2 && punct(view.get(here.i - 2), b':'));
        let start_ok = here.prev_masked
            || after_label
            || here.prev.is_none_or(|p| {
                matches!(p.kind, Kind::Directive | Kind::Punct(b';' | b'{' | b'}' | b'>' | b']'))
                    || view.ident_in(Some(p), SPECIFIERS)
            });
        if !start_ok {
            return false;
        }
        let ends = view.macro_chain(here.i);
        if !view.specifiers_follow(&ends) {
            return false;
        }
        for (a, b) in ends {
            self.mask_range(a, b);
        }
        true
    }

    /// Rule 5: a statement-like macro call at declaration level, alone at
    /// the end of its line (`FMT_PRAGMA_GCC(push_options)`).
    fn declaration_call(&mut self, here: &Here<'a>) -> bool {
        let view = self.view;
        let decl_start = here.prev_masked
            || here.prev.is_none_or(|p| matches!(p.kind, Kind::Directive | Kind::Punct(b';' | b'{' | b'}' | b':')));
        if !decl_start || !self.levels[here.i] || !punct(here.next, b'(') {
            return false;
        }
        let Some(close) = matching_paren(view.toks, here.i + 1) else { return false };
        if !view.call_ends_line(close) && !view.typed_args(here.i + 1, close) {
            return false;
        }
        self.mask_range(here.i, close);
        true
    }

    /// Rule 8: an attribute macro after a parameter name
    /// (`const T& x FMT_LIFETIMEBOUND, int y`).
    fn parameter_attribute(&mut self, here: &Here<'a>) -> bool {
        let hit = (punct(here.next, b',') || punct(here.next, b')'))
            && here.prev.is_some_and(|p| p.kind == Kind::Ident || p.kind == Kind::Punct(b']'))
            && !self.view.ident_in(here.prev, NOT_A_NAME)
            && self.levels[here.i];
        if hit {
            self.mask_range(here.i, here.i);
        }
        hit
    }

    /// Rule 9: a macro before a parameter's type and name
    /// (`f(EV_P_ ev_io *w)`, `f(UNUSED_ATTR int x)`).
    fn parameter_prefix(&mut self, here: &Here<'a>) -> bool {
        let after = self.view.get(here.i + 2);
        let hit = (punct(here.prev, b'(') || punct(here.prev, b','))
            && self.levels[here.i]
            && ident(here.next)
            && after.is_some_and(|t| t.kind == Kind::Ident || matches!(t.kind, Kind::Punct(b'*' | b'&')))
            && !punct(after, b'(');
        if hit {
            self.mask_range(here.i, here.i);
        }
        hit
    }

    /// Rule 4: a trailing specifier after a parameter list or a method
    /// qualifier.
    fn trailing_specifiers(&mut self, here: &Here<'a>) -> bool {
        let view = self.view;
        let i = here.i;
        if !(view.after_parameter_list(i) || view.after_method_qualifier(i) || here.prev_masked) {
            return false;
        }
        let j = view.trailing_end(i);
        let hit = view.ends_declarator(j) && (!punct(here.next, b'(') || j > i);
        if hit {
            self.mask_range(i, j);
        }
        hit
    }
}

/// Block comments inside a directive line followed by code on it
/// (`#define F(x) (x) /* … */ \`): they break the grammar's macro bodies.
/// A trailing one stays: blanked, it would leave spaces where the grammar
/// wants the line end right after a valueless `#define X`.
fn inner_block_comments(line: &[u8]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut k = 0;
    while let Some(open) = line[k..].windows(2).position(|w| w == b"/*") {
        let a = k + open;
        let Some(close) = line[a + 2..].windows(2).position(|w| w == b"*/") else { break };
        let b = a + 2 + close + 2;
        if !line[b..].iter().all(|c| c.is_ascii_whitespace()) {
            out.push((a, b));
        }
        k = b;
    }
    out
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
        replace_condition(&mut out, a, b);
    }
    Some(out)
}

/// Replaces the condition `a..b` of an `#if` line by `1` and spaces,
/// keeping its line ends and `\` continuations.
fn replace_condition(out: &mut [u8], a: usize, b: usize) {
    let mut first = true;
    for k in a..b {
        let c = out[k];
        let continuation = c == b'\\' && matches!(out.get(k + 1), Some(b'\n' | b'\r'));
        if matches!(c, b'\n' | b'\r') || continuation {
            continue;
        }
        out[k] = if first && !c.is_ascii_whitespace() {
            first = false;
            b'1'
        } else {
            b' '
        };
    }
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
        assert_eq!(masked_words("LUA_API lua_State *(lua_newstate) (lua_Alloc f);"), ["LUA_API"]);
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
