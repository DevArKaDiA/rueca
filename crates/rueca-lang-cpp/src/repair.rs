//! Error-driven repairs: rewrites applied only where the grammar reports a
//! parse error (`preprocess.rs` runs them and keeps a round only when the
//! error count drops).
//!
//! Every repair replaces bytes with the same number of bytes (a keyword or
//! punctuation padded with spaces, or spaces), never touching newlines, so
//! offsets and lines of the original source stay valid. Each one targets a
//! construct that only a macro or a grammar gap explains:
//!
//! | Repair | Before | After |
//! |---|---|---|
//! | type argument | `va_arg(ap, char*)`, `va_arg(ap, robj*)`, `SIGNAL(f(int))` | `va_arg(ap, 0    )`, `va_arg(ap, robj )`, `SIGNAL(f(0  ))` |
//! | call before a block | `jv_array_foreach(a, i, x) {`, `TEST("x") {` | `jv_array_foreach(a, i, x);{` (the call is kept), else the call blanked |
//! | statement macro without `;` | `test_cond(x == 1)` + newline, `GET_TYPE(p)` before `if` | `;` and spaces (an empty statement) |
//! | bare statement macro | `VERIFY_FAILS_END` + newline | `;` and spaces |
//! | unknown specifiers | `static redisAtomic size_t n;`, `int CJSON_CDECL main(void)`, `CFSocketRef __unused s` | the macro blanked |
//! | trailing macro | `jv f(const char*, ...) JV_PRINTF_LIKE(1, 2);`, `(*f)(void *p) REDISMODULE_ATTR;` | blanked |
//! | calling convention | `void *(CJSON_CDECL *allocate)(size_t)` | blanked |
//! | attribute | `__attribute__ ((deprecated)) name = {…}` | blanked |
//! | brace default | `locale_ref loc = {}`, `T x = {a, b}` in a parameter list | `= 0 `, `= (a, b)` |
//! | enum base | `enum : unsigned {` | `enum            {` |
//! | `typename` in an expression | `f(typename T::value_type(), 0)` | `typename` blanked |
//! | adjacent operands | `ev_loop(EV_DEFAULT_ 0)`, `"%" PRIu64 "\n"` | the macro blanked |
//! | Qt sections | `signals:`, `public slots:` | `public :`, `public      :` |
//! | `if`-like macro | `run_with_period(100) cron();` | `if   (100) cron();` |
//! | `if` with a macro condition | `if ISZERO(dn) {` | `if(ISZERO(dn)){` |
//! | block macro | `FMT_TRY {` | the macro blanked |
//! | declaration macro | `MOCK_METHOD(int, f, (int));` in a class | the call blanked (`;` stays) |
//! | string macro | `STRINGIFY(LEN) " chars"` | `""` and spaces |
//! | `using typename` | `using typename Base::type;` | `typename` blanked |
//! | explicit instantiation | `extern template auto f<char>(…) -> R;`, `template class A<int>;` | `extern` / `template` blanked |
//! | `throw` as an argument | `EXPECT_THROW(throw E(1), X)` | `throw` blanked |
//! | keyword argument | `TEST(suite, enum) {` | `TEST(suite, _num) {` (the name is read from the original) |
//! | `decltype(a, b)` | a comma expression in `decltype` | the comma becomes `\|` |
//! | pointer default argument | `typename P = const T*>` | `typename P =       T >` |
//! | deleted declaration | `template <…> void f(T) = delete;` | `= delete` blanked |
//! | member pointers | `v.*(&G::c)`, `p->*m`, `int (T::*)()`, `A<&T::m>` | `+`, `int (   *)()`, `&` blanked |
//! | dangling `=` | `N =` before `}` (an X-macro table removed) | `=` blanked |

use crate::mask::{Kind, Tok, matching_paren};

/// A same-length replacement: bytes `start..end` become `with` followed by
/// spaces (newlines kept).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Edit {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) with: &'static [u8],
}

impl Edit {
    fn blank(start: usize, end: usize) -> Edit {
        Edit { start, end, with: b"" }
    }
}

/// Where the grammar failed.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Site {
    pub(crate) start: usize,
    pub(crate) end: usize,
    /// Inside a function body (statements), else at declaration level.
    pub(crate) in_body: bool,
}

/// Type keywords a type argument or a declaration head may contain.
const TYPE_WORDS: &[&str] = &[
    "_Bool", "bool", "char", "char16_t", "char32_t", "char8_t", "double", "float", "int", "long", "short", "signed",
    "unsigned", "void", "wchar_t",
];

/// Words that are not unknown specifiers in a declaration head.
const DECL_WORDS: &[&str] = &[
    "_Atomic",
    "_Noreturn",
    "_Thread_local",
    "alignas",
    "auto",
    "class",
    "const",
    "consteval",
    "constexpr",
    "constinit",
    "enum",
    "explicit",
    "extern",
    "friend",
    "inline",
    "mutable",
    "register",
    "restrict",
    "static",
    "struct",
    "thread_local",
    "typedef",
    "typename",
    "union",
    "virtual",
    "volatile",
    "__inline",
    "__inline__",
    "__restrict",
    "__restrict__",
    "__extension__",
];

/// Statement keywords: never a macro, never a declaration head.
const STATEMENT_WORDS: &[&str] = &[
    "break",
    "case",
    "catch",
    "co_return",
    "co_yield",
    "continue",
    "default",
    "delete",
    "do",
    "else",
    "for",
    "goto",
    "if",
    "new",
    "operator",
    "return",
    "sizeof",
    "switch",
    "template",
    "throw",
    "try",
    "using",
    "while",
    "namespace",
    "public",
    "private",
    "protected",
    "decltype",
    "static_assert",
    "alignof",
    "noexcept",
    "requires",
];

struct Cx<'a> {
    src: &'a [u8],
    toks: &'a [Tok],
}

impl Cx<'_> {
    fn text(&self, i: usize) -> &[u8] {
        let t = &self.toks[i];
        &self.src[t.start..t.end]
    }
    fn is(&self, i: usize, w: &str) -> bool {
        i < self.toks.len() && self.toks[i].kind == Kind::Ident && self.text(i) == w.as_bytes()
    }
    fn punct(&self, i: usize, c: u8) -> bool {
        self.toks.get(i).is_some_and(|t| t.kind == Kind::Punct(c))
    }
    fn ident(&self, i: usize) -> bool {
        self.toks.get(i).is_some_and(|t| t.kind == Kind::Ident)
    }
    fn word_in(&self, i: usize, set: &[&str]) -> bool {
        self.ident(i) && set.iter().any(|w| w.as_bytes() == self.text(i))
    }
    /// An identifier that may name something (not a keyword).
    fn name(&self, i: usize) -> bool {
        self.ident(i)
            && !self.word_in(i, TYPE_WORDS)
            && !self.word_in(i, DECL_WORDS)
            && !self.word_in(i, STATEMENT_WORDS)
    }
    /// Looks like a macro: upper case with `_` or digits (`EV_DEFAULT_`,
    /// `PRIu64` included: a `PRI` / `SCN` format macro).
    fn macro_like(&self, i: usize) -> bool {
        if !self.name(i) {
            return false;
        }
        let w = self.text(i);
        let upper = w.iter().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || *b == b'_')
            && w.iter().any(|b| b.is_ascii_uppercase())
            && w.len() >= 2;
        upper || ((w.starts_with(b"PRI") || w.starts_with(b"SCN")) && w.len() >= 4)
    }
    /// Index of the token containing or following byte `at`.
    fn at(&self, at: usize) -> usize {
        self.toks.partition_point(|t| t.end <= at)
    }
    /// First token of the statement / declaration containing token `i`:
    /// after the previous `;`, `{`, `}`, directive or label `:` at depth 0.
    fn statement_start(&self, i: usize) -> usize {
        let mut depth = 0i32;
        let mut k = i.min(self.toks.len());
        let floor = i.saturating_sub(256);
        while k > floor {
            let p = k - 1;
            match self.toks[p].kind {
                Kind::Punct(b')' | b']') => depth += 1,
                Kind::Punct(b'(' | b'[') => {
                    if depth == 0 {
                        // Inside parentheses: the statement starts before them.
                    } else {
                        depth -= 1;
                    }
                }
                Kind::Punct(b';' | b'{' | b'}') | Kind::Directive if depth == 0 => return k,
                // A label or access specifier (`public:`, `case 1:`), not a
                // member initializer list (`) :`) or a `::`.
                Kind::Punct(b':')
                    if depth == 0
                        && !self.punct(p + 1, b':')
                        && p > 0
                        && matches!(self.toks[p - 1].kind, Kind::Ident | Kind::Punct(b'0')) =>
                {
                    return k;
                }
                _ => {}
            }
            k = p;
        }
        k
    }
    /// Token `i` starts a statement: after a boundary, or first on its
    /// line after a `)` (a previous statement macro without `;`).
    fn starts_statement(&self, i: usize) -> bool {
        if self.statement_start(i) == i || i == 0 {
            return i < self.toks.len();
        }
        if self.is(i - 1, "else") || self.is(i - 1, "do") {
            return true;
        }
        if !self.punct(i - 1, b')') {
            return false;
        }
        // After the condition of `if` / `while` / `for` / `switch`, or after
        // a previous statement macro on an earlier line.
        let cond = (0..i - 1).rev().take(256).try_fold(0i32, |d, p| match self.toks[p].kind {
            Kind::Punct(b')') => Ok(d + 1),
            Kind::Punct(b'(') if d == 0 => Err(p),
            Kind::Punct(b'(') => Ok(d - 1),
            _ => Ok(d),
        });
        let after_condition = matches!(cond, Err(o) if o > 0 && self.word_in(o - 1, &["if", "while", "for", "switch"]));
        after_condition || self.toks[i - 1].line < self.toks[i].line
    }
    fn gap_has_space(&self, a: usize, b: usize) -> bool {
        let (x, y) = (self.toks[a].end, self.toks[b].start);
        x < y && self.src[x..y].iter().any(|c| matches!(c, b' ' | b'\t'))
    }
    /// A position right after `a` (spaces or tabs only) where `;` fits.
    fn space_after(&self, a: usize) -> Option<usize> {
        let x = self.toks[a].end;
        matches!(self.src.get(x), Some(b' ' | b'\t')).then_some(x)
    }
}

/// Repairs for one error site, or `None` when no rule applies.
pub(crate) fn repair(src: &[u8], toks: &[Tok], site: Site) -> Option<Vec<Edit>> {
    let cx = Cx { src, toks };
    if toks.is_empty() {
        return None;
    }
    let k = cx.at(site.start).min(toks.len() - 1);
    // Candidates, nearest first: the tokens around the error (it may be
    // reported on the token after the culprit), the tokens inside it, then
    // the rest of the statement back to its start (a statement macro is
    // its first token).
    let n = toks.len();
    let mut cands: Vec<usize> = Vec::new();
    let push = |c: &mut Vec<usize>, x: usize| {
        if x < n && !c.contains(&x) {
            c.push(x);
        }
    };
    for d in 0..3usize {
        push(&mut cands, k.saturating_sub(d));
        push(&mut cands, k + d);
    }
    // The line the error follows (a macro call ending there).
    if k > 0 {
        let line = toks[k - 1].line;
        for x in (k.saturating_sub(16)..k - 1).rev() {
            if toks[x].line != line {
                break;
            }
            push(&mut cands, x);
        }
    }
    let near_count = cands.len();
    let mut x = k;
    while x < n && toks[x].start < site.end && x < k + 32 {
        push(&mut cands, x);
        x += 1;
    }
    let start = cx.statement_start(k.saturating_sub(1)).min(k);
    for x in (start..k.saturating_sub(2)).rev().take(64) {
        push(&mut cands, x);
    }
    // The head of the enclosing block (`FMT_TRY {` reported inside it).
    let mut depth = 0i32;
    for p in (k.saturating_sub(512)..k.min(n)).rev() {
        match toks[p].kind {
            Kind::Punct(b'}') => depth += 1,
            Kind::Punct(b'{') if depth > 0 => depth -= 1,
            Kind::Punct(b'{') => {
                if p > 0 {
                    push(&mut cands, p - 1);
                    push(&mut cands, cx.statement_start(p - 1));
                }
                break;
            }
            _ => {}
        }
    }
    // Local rewrites first, on the tokens at and inside the error; then
    // statement-level ones, nearest candidate first.
    type Rule = fn(&Cx<'_>, usize, Site) -> Option<Vec<Edit>>;
    let local: [Rule; 19] = [
        deleted_function,
        extern_template,
        member_pointers,
        dangling_assign,
        decltype_comma,
        template_default_pointer,
        type_argument,
        keyword_argument,
        qt_section,
        enum_base,
        brace_default,
        typename_expr,
        using_typename,
        throw_argument,
        attribute,
        calling_convention,
        if_macro,
        string_macro,
        adjacent_operands,
    ];
    let statement: [Rule; 8] = [
        trailing_macro,
        brace_macro,
        block_call,
        guard_call,
        statement_call,
        bare_statement,
        declaration_macro_call,
        specifiers,
    ];
    let near = &cands[..cands.len().min(near_count + (x - k))];
    for rule in local {
        for &i in near {
            if let Some(e) = rule(&cx, i, site) {
                return Some(e);
            }
        }
    }
    for &i in &cands {
        for rule in statement {
            if let Some(e) = rule(&cx, i, site) {
                return Some(e);
            }
        }
    }
    None
}

/// A type written as a call argument (`va_arg(ap, char*)`), in a function
/// body (elsewhere a parenthesized list is a parameter list).
fn type_argument(cx: &Cx<'_>, i: usize, site: Site) -> Option<Vec<Edit>> {
    if !site.in_body {
        return None;
    }
    // The innermost `(` before `i` at depth 0.
    let mut depth = 0i32;
    let mut open = None;
    for p in (i.saturating_sub(64)..=i.min(cx.toks.len() - 1)).rev() {
        match cx.toks[p].kind {
            Kind::Punct(b')') if p != i => depth += 1,
            Kind::Punct(b'(') => {
                if depth == 0 {
                    open = Some(p);
                    break;
                }
                depth -= 1;
            }
            Kind::Punct(b';' | b'{' | b'}') | Kind::Directive => return None,
            _ => {}
        }
    }
    let open = open?;
    // A call: a name before the parenthesis.
    if open == 0 || !cx.name(open - 1) {
        return None;
    }
    let close = matching_paren(cx.toks, open)?;
    let mut edits = Vec::new();
    let mut a = open + 1;
    while a < close {
        let mut b = a;
        let mut d = 0i32;
        while b < close && !(d == 0 && cx.punct(b, b',')) {
            match cx.toks[b].kind {
                Kind::Punct(b'(' | b'[' | b'{') => d += 1,
                Kind::Punct(b')' | b']' | b'}') => d -= 1,
                _ => {}
            }
            b += 1;
        }
        // Argument tokens a..b.
        let arg = a..b;
        // The argument overlaps the error (a MISSING node sits at its end).
        let contains = b > a && cx.toks[a].start <= site.end && site.start <= cx.toks[b - 1].end;
        if b > a && contains {
            let stars = arg.clone().rev().take_while(|&x| cx.punct(x, b'*') || cx.punct(x, b'&')).count();
            let core = a..b - stars;
            let keywords = core.clone().all(|x| cx.word_in(x, TYPE_WORDS) || cx.is(x, "const") || cx.is(x, "volatile"));
            let tagged = core.len() == 2 && (cx.is(a, "struct") || cx.is(a, "union") || cx.is(a, "enum"));
            if !core.is_empty() && (keywords || tagged) {
                edits.push(Edit { start: cx.toks[a].start, end: cx.toks[b - 1].end, with: b"0" });
            } else if core.len() == 1 && cx.name(a) && stars > 0 {
                edits.push(Edit::blank(cx.toks[b - stars].start, cx.toks[b - 1].end));
            }
        }
        a = b + 1;
    }
    (!edits.is_empty()).then_some(edits)
}

/// `= delete;` / `= default;` on a declaration the grammar reads as a
/// variable (`template <…> void f(T) = delete;`): the clause goes.
fn deleted_function(cx: &Cx<'_>, i: usize, _: Site) -> Option<Vec<Edit>> {
    let eq = (i.saturating_sub(2)..=i + 1)
        .find(|&k| cx.punct(k, b'=') && (cx.is(k + 1, "delete") || cx.is(k + 1, "default")) && cx.punct(k + 2, b';'))?;
    Some(vec![Edit::blank(cx.toks[eq].start, cx.toks[eq + 1].end)])
}

/// Member pointers the grammar does not take: `v.*(&T::m)` / `p->*f` (the
/// operator becomes `+`), `&T::m` as a template argument (the `&` goes),
/// `int (T::*)()` as a type (`T::` goes, a plain pointer).
fn member_pointers(cx: &Cx<'_>, i: usize, _: Site) -> Option<Vec<Edit>> {
    for k in i.saturating_sub(2)..=(i + 2).min(cx.toks.len().saturating_sub(1)) {
        // `.*` and `->*`.
        if cx.punct(k, b'*')
            && k > 0
            && (cx.punct(k - 1, b'.') || (cx.punct(k - 1, b'>') && k > 1 && cx.punct(k - 2, b'-')))
        {
            let a = if cx.punct(k - 1, b'.') { k - 1 } else { k - 2 };
            return Some(vec![Edit { start: cx.toks[a].start, end: cx.toks[k].end, with: b"+" }]);
        }
        // `(T::*)`.
        if cx.punct(k, b'(')
            && cx.ident(k + 1)
            && cx.punct(k + 2, b':')
            && cx.punct(k + 3, b':')
            && cx.punct(k + 4, b'*')
        {
            return Some(vec![Edit::blank(cx.toks[k + 1].start, cx.toks[k + 3].end)]);
        }
        // `<…, &T::m>`.
        if cx.punct(k, b'&')
            && k > 0
            && (cx.punct(k - 1, b',') || cx.punct(k - 1, b'<'))
            && cx.ident(k + 1)
            && cx.punct(k + 2, b':')
            && cx.punct(k + 3, b':')
        {
            return Some(vec![Edit::blank(cx.toks[k].start, cx.toks[k].end)]);
        }
    }
    None
}

/// `NAME =` with nothing after (the value came from removed X-macro
/// lines): the `=` goes.
fn dangling_assign(cx: &Cx<'_>, i: usize, _: Site) -> Option<Vec<Edit>> {
    let ok = cx.punct(i, b'=')
        && cx.toks.get(i + 1).is_some_and(|t| matches!(t.kind, Kind::Punct(b'}' | b',' | b';')))
        && i > 0
        && !matches!(cx.toks[i - 1].kind, Kind::Punct(_));
    ok.then(|| vec![Edit::blank(cx.toks[i].start, cx.toks[i].end)])
}

/// `decltype(a, b)` (a comma expression the grammar does not take there):
/// the comma becomes `|`.
fn decltype_comma(cx: &Cx<'_>, i: usize, _: Site) -> Option<Vec<Edit>> {
    if !cx.punct(i, b',') {
        return None;
    }
    let mut depth = 0i32;
    for p in (i.saturating_sub(64)..i).rev() {
        match cx.toks[p].kind {
            Kind::Punct(b')' | b'>' | b']') => depth += 1,
            Kind::Punct(b'(' | b'<' | b'[') if depth > 0 => depth -= 1,
            Kind::Punct(b'(') => {
                return (p > 0 && cx.is(p - 1, "decltype"))
                    .then(|| vec![Edit { start: cx.toks[i].start, end: cx.toks[i].end, with: b"|" }]);
            }
            Kind::Punct(b'<' | b'[' | b';' | b'{' | b'}') => return None,
            _ => {}
        }
    }
    None
}

/// `typename P = const T*>` (a pointer type as a default template
/// argument, which the grammar reads as an expression): the `*` goes.
fn template_default_pointer(cx: &Cx<'_>, i: usize, _: Site) -> Option<Vec<Edit>> {
    let star = (i.saturating_sub(1)..=i + 1)
        .find(|&k| (cx.punct(k, b'*') || cx.punct(k, b'&')) && (cx.punct(k + 1, b'>') || cx.punct(k + 1, b',')))?;
    // Inside `template <`, after an `=` of the same parameter.
    let mut saw_eq = false;
    let mut eq = star;
    let mut depth = 0i32;
    for p in (star.saturating_sub(64)..star).rev() {
        match cx.toks[p].kind {
            Kind::Punct(b'=') if depth == 0 && !saw_eq => {
                saw_eq = true;
                eq = p;
            }
            Kind::Punct(b',') if depth == 0 && !saw_eq => return None,
            Kind::Punct(b'>' | b')') => depth += 1,
            Kind::Punct(b'(') if depth > 0 => depth -= 1,
            Kind::Punct(b'<') if depth > 0 => depth -= 1,
            Kind::Punct(b'<') => {
                if !(saw_eq && p > 0 && cx.is(p - 1, "template")) {
                    return None;
                }
                // `const T*` → `T`: the grammar takes neither the pointer
                // nor the qualifier there.
                let mut edits = vec![Edit::blank(cx.toks[star].start, cx.toks[star].end)];
                for q in eq + 1..star {
                    if cx.is(q, "const") || cx.is(q, "volatile") {
                        edits.push(Edit::blank(cx.toks[q].start, cx.toks[q].end));
                    }
                }
                return Some(edits);
            }
            Kind::Punct(b';' | b'{' | b'}') => return None,
            _ => {}
        }
    }
    None
}

/// Qt `signals:` / `slots:` sections.
fn qt_section(cx: &Cx<'_>, i: usize, _: Site) -> Option<Vec<Edit>> {
    let w = cx.text(i);
    if !cx.ident(i) || !cx.punct(i + 1, b':') || cx.punct(i + 2, b':') {
        return None;
    }
    match w {
        b"signals" | b"Q_SIGNALS" => Some(vec![Edit { start: cx.toks[i].start, end: cx.toks[i].end, with: b"public" }]),
        b"slots" | b"Q_SLOTS" if i > 0 && cx.word_in(i - 1, &["public", "private", "protected"]) => {
            Some(vec![Edit::blank(cx.toks[i].start, cx.toks[i].end)])
        }
        _ => None,
    }
}

/// `enum : unsigned {` (an anonymous enum with a base): the base goes.
fn enum_base(cx: &Cx<'_>, i: usize, _: Site) -> Option<Vec<Edit>> {
    if !cx.is(i, "enum") || !cx.punct(i + 1, b':') {
        return None;
    }
    let brace = (i + 2..(i + 8).min(cx.toks.len())).find(|&k| cx.punct(k, b'{'))?;
    Some(vec![Edit::blank(cx.toks[i + 1].start, cx.toks[brace - 1].end)])
}

/// `= {}` / `= {a, b}` as a default argument.
fn brace_default(cx: &Cx<'_>, i: usize, _: Site) -> Option<Vec<Edit>> {
    let eq = (i.saturating_sub(3)..=i).rev().find(|&k| cx.punct(k, b'=') && cx.punct(k + 1, b'{'))?;
    let open = eq + 1;
    // Inside a parameter list: an unclosed `(` before `=` in the statement.
    let mut depth = 0i32;
    let mut in_params = false;
    for p in (eq.saturating_sub(64)..eq).rev() {
        match cx.toks[p].kind {
            Kind::Punct(b')') => depth += 1,
            Kind::Punct(b'(') => {
                if depth == 0 {
                    in_params = true;
                    break;
                }
                depth -= 1;
            }
            Kind::Punct(b';' | b'{' | b'}') => break,
            _ => {}
        }
    }
    if !in_params {
        return None;
    }
    let mut d = 0i32;
    let close = (open..(open + 64).min(cx.toks.len())).find(|&k| {
        match cx.toks[k].kind {
            Kind::Punct(b'{') => d += 1,
            Kind::Punct(b'}') => d -= 1,
            _ => {}
        }
        d == 0
    })?;
    let (a, b) = (cx.toks[open].start, cx.toks[close].end);
    if close == open + 1 {
        return Some(vec![Edit { start: a, end: b, with: b"0" }]);
    }
    Some(vec![Edit { start: a, end: a + 1, with: b"(" }, Edit { start: b - 1, end: b, with: b")" }])
}

/// `typename` where an expression stands (`f(typename T::x(), 0)`).
fn typename_expr(cx: &Cx<'_>, i: usize, _: Site) -> Option<Vec<Edit>> {
    if !cx.is(i, "typename") || i == 0 {
        return None;
    }
    let p = &cx.toks[i - 1];
    let expr = matches!(p.kind, Kind::Punct(b'(' | b',' | b'=' | b'!' | b'?' | b'+' | b'-' | b'|' | b'&'))
        || cx.is(i - 1, "return");
    expr.then(|| vec![Edit::blank(cx.toks[i].start, cx.toks[i].end)])
}

/// `__attribute__((…))` / `__declspec(…)` where the grammar does not take
/// one.
fn attribute(cx: &Cx<'_>, i: usize, _: Site) -> Option<Vec<Edit>> {
    if !(cx.is(i, "__attribute__") || cx.is(i, "__attribute") || cx.is(i, "__declspec")) || !cx.punct(i + 1, b'(') {
        return None;
    }
    let close = matching_paren(cx.toks, i + 1)?;
    Some(vec![Edit::blank(cx.toks[i].start, cx.toks[close].end)])
}

/// `( MACRO * name )` in a declarator: a calling convention.
fn calling_convention(cx: &Cx<'_>, i: usize, _: Site) -> Option<Vec<Edit>> {
    if i == 0 || !cx.punct(i - 1, b'(') || !cx.macro_like(i) || !cx.punct(i + 1, b'*') {
        return None;
    }
    Some(vec![Edit::blank(cx.toks[i].start, cx.toks[i].end)])
}

/// `f(params) MACRO(args);` / `(*f)(params) MACRO;`: an attribute macro
/// after a declarator.
fn trailing_macro(cx: &Cx<'_>, i: usize, site: Site) -> Option<Vec<Edit>> {
    if site.in_body || i == 0 || !cx.punct(i - 1, b')') || !cx.name(i) {
        return None;
    }
    if cx.word_in(i, &["override", "final", "const", "noexcept", "throw", "try", "requires"]) {
        return None;
    }
    let end = if cx.punct(i + 1, b'(') { matching_paren(cx.toks, i + 1)? } else { i };
    let next = cx.toks.get(end + 1)?;
    let ok = matches!(next.kind, Kind::Punct(b';' | b'{' | b'=' | b',')) || cx.is(end + 1, "const");
    // The `)` closes a parameter list: a name or `)` before its `(`.
    let open = (0..i - 1).rev().try_fold(0i32, |d, p| match cx.toks[p].kind {
        Kind::Punct(b')') => Ok(d + 1),
        Kind::Punct(b'(') if d == 0 => Err(Some(p)),
        Kind::Punct(b'(') => Ok(d - 1),
        Kind::Punct(b';' | b'{' | b'}') => Err(None),
        _ => Ok(d),
    });
    let declarator = match open {
        Err(Some(o)) => o > 0 && (cx.name(o - 1) || cx.punct(o - 1, b')')),
        _ => false,
    };
    (ok && declarator).then(|| vec![Edit::blank(cx.toks[i].start, cx.toks[end].end)])
}

/// Whether the arguments of the call whose `(` is at `open` read as
/// expressions (no `...`, no type keyword, no declaration).
fn expression_args(cx: &Cx<'_>, open: usize, close: usize) -> bool {
    (open + 1..close).all(|k| {
        !cx.word_in(k, TYPE_WORDS)
            && !cx.word_in(k, DECL_WORDS)
            && !(cx.punct(k, b'.') && cx.punct(k + 1, b'.'))
            && !(cx.punct(k, b'&') && (cx.punct(k + 1, b')') || cx.punct(k + 1, b',')))
    })
}

/// `NAME(args) {` in a function body: a loop or block macro.
fn block_call(cx: &Cx<'_>, i: usize, site: Site) -> Option<Vec<Edit>> {
    if !site.in_body || !cx.name(i) || !cx.punct(i + 1, b'(') || !cx.starts_statement(i) {
        return None;
    }
    let close = matching_paren(cx.toks, i + 1)?;
    if !cx.punct(close + 1, b'{') {
        return None;
    }
    if expression_args(cx, i + 1, close)
        && let Some(at) = cx.space_after(close).filter(|_| cx.gap_has_space(close, close + 1))
    {
        return Some(vec![Edit { start: at, end: at + 1, with: b";" }]);
    }
    Some(vec![Edit::blank(cx.toks[i].start, cx.toks[close].end)])
}

/// `NAME(args) stmt;` on one line in a function body: a macro standing for
/// `if (…)` (`run_with_period(100) cron();`) becomes `if`.
fn guard_call(cx: &Cx<'_>, i: usize, site: Site) -> Option<Vec<Edit>> {
    if !site.in_body || !cx.name(i) || !cx.punct(i + 1, b'(') || !cx.starts_statement(i) {
        return None;
    }
    let close = matching_paren(cx.toks, i + 1)?;
    let next = cx.toks.get(close + 1)?;
    let same_line = next.line == cx.toks[close].line;
    if !same_line || !cx.name(close + 1) || !expression_args(cx, i + 1, close) {
        return None;
    }
    Some(vec![Edit { start: cx.toks[i].start, end: cx.toks[i].end, with: b"if" }])
}

/// `if NAME(args) {`: a macro that expands to a parenthesized condition
/// gets the parentheses `if` needs (`if(NAME(args)){`), else is blanked.
fn if_macro(cx: &Cx<'_>, i: usize, _: Site) -> Option<Vec<Edit>> {
    if i == 0 || !cx.is(i - 1, "if") || !cx.name(i) || !cx.punct(i + 1, b'(') {
        return None;
    }
    let close = matching_paren(cx.toks, i + 1)?;
    let before = cx.gap_has_space(i - 1, i).then_some(cx.toks[i - 1].end);
    let after = cx.space_after(close).filter(|_| close + 1 < cx.toks.len() && cx.gap_has_space(close, close + 1));
    match (before, after) {
        (Some(a), Some(b)) => {
            Some(vec![Edit { start: a, end: a + 1, with: b"(" }, Edit { start: b, end: b + 1, with: b")" }])
        }
        _ => Some(vec![Edit::blank(cx.toks[i].start, cx.toks[i].end)]),
    }
}

/// `MACRO {` in a function body (`FMT_TRY { … }`): the macro goes.
fn brace_macro(cx: &Cx<'_>, i: usize, site: Site) -> Option<Vec<Edit>> {
    if !site.in_body || !cx.macro_like(i) || !cx.punct(i + 1, b'{') || !cx.starts_statement(i) {
        return None;
    }
    Some(vec![Edit::blank(cx.toks[i].start, cx.toks[i].end)])
}

/// `MACRO(args);` at declaration level (`MOCK_METHOD(int, f, (int));`,
/// `DECLARE_X(a, b);`): a declaration macro, blanked (the `;` stays, an
/// empty declaration).
fn declaration_macro_call(cx: &Cx<'_>, i: usize, site: Site) -> Option<Vec<Edit>> {
    if site.in_body || !cx.macro_like(i) || !cx.punct(i + 1, b'(') || !cx.starts_statement(i) {
        return None;
    }
    let close = matching_paren(cx.toks, i + 1)?;
    cx.punct(close + 1, b';').then(|| vec![Edit::blank(cx.toks[i].start, cx.toks[close].end)])
}

/// `MACRO(args) "text"` / `"text" MACRO(args)`: a macro producing a string
/// literal becomes `""`.
fn string_macro(cx: &Cx<'_>, i: usize, _: Site) -> Option<Vec<Edit>> {
    if !cx.macro_like(i) || !cx.punct(i + 1, b'(') {
        return None;
    }
    let close = matching_paren(cx.toks, i + 1)?;
    let literal = |k: usize| cx.toks.get(k).is_some_and(|t| t.kind == Kind::Literal);
    (literal(close + 1) || (i > 0 && literal(i - 1)))
        .then(|| vec![Edit { start: cx.toks[i].start, end: cx.toks[close].end, with: b"\"\"" }])
}

/// `using typename Base::type;`: the `typename` goes.
fn using_typename(cx: &Cx<'_>, i: usize, _: Site) -> Option<Vec<Edit>> {
    (i > 0 && cx.is(i, "typename") && cx.is(i - 1, "using"))
        .then(|| vec![Edit::blank(cx.toks[i].start, cx.toks[i].end)])
}

/// Explicit instantiations the grammar does not take: `extern template auto
/// f<char>(…) -> R;` (the `extern` goes) and `template class A<int>;` (the
/// `template` goes: a declaration of the specialization remains).
fn extern_template(cx: &Cx<'_>, i: usize, _: Site) -> Option<Vec<Edit>> {
    let start = cx.statement_start(i);
    let e = [i, i.saturating_sub(1), start].into_iter().find(|&k| cx.is(k, "extern") || cx.is(k, "template"))?;
    if cx.is(e, "extern") && cx.is(e + 1, "template") && !cx.punct(e + 2, b'<') {
        return Some(vec![Edit::blank(cx.toks[e].start, cx.toks[e].end)]);
    }
    let class = cx.is(e, "template") && (cx.is(e + 1, "class") || cx.is(e + 1, "struct")) && cx.punct(e + 3, b'<');
    class.then(|| vec![Edit::blank(cx.toks[e].start, cx.toks[e].end)])
}

/// `M(throw E(…), T)`: a throw expression as a macro argument loses its
/// `throw`.
fn throw_argument(cx: &Cx<'_>, i: usize, _: Site) -> Option<Vec<Edit>> {
    let t = (i.saturating_sub(2)..=i)
        .find(|&k| cx.is(k, "throw") && k > 0 && (cx.punct(k - 1, b'(') || cx.punct(k - 1, b',')))?;
    Some(vec![Edit::blank(cx.toks[t].start, cx.toks[t].end)])
}

/// A keyword as a whole argument of a declaration-level macro
/// (`TEST(suite, enum)`): written `_num` for the parser (names are read
/// from the original text).
fn keyword_argument(cx: &Cx<'_>, i: usize, site: Site) -> Option<Vec<Edit>> {
    if site.in_body || !cx.ident(i) || cx.name(i) || cx.word_in(i, TYPE_WORDS) || cx.is(i, "const") {
        return None;
    }
    let lone =
        i > 0 && (cx.punct(i - 1, b',') || cx.punct(i - 1, b'(')) && (cx.punct(i + 1, b',') || cx.punct(i + 1, b')'));
    if !lone {
        return None;
    }
    // The call: a macro name before the `(`.
    let open = (i.saturating_sub(16)..i).rev().find(|&k| cx.punct(k, b'('))?;
    (open > 0 && cx.macro_like(open - 1))
        .then(|| vec![Edit { start: cx.toks[i].start, end: cx.toks[i].start + 1, with: b"_" }])
}

/// `NAME(args)` + newline + another statement: a statement macro without
/// `;` becomes an empty statement (or gets its `;` after a space).
fn statement_call(cx: &Cx<'_>, i: usize, site: Site) -> Option<Vec<Edit>> {
    if !cx.name(i) || !cx.punct(i + 1, b'(') || !cx.starts_statement(i) {
        return None;
    }
    let close = matching_paren(cx.toks, i + 1)?;
    let next = cx.toks.get(close + 1);
    // `TEST(group, name) {` at declaration level: a test macro head, left
    // to the extractor.
    if !site.in_body && cx.punct(close + 1, b'{') {
        return None;
    }
    let starts_statement = next.is_none_or(|n| {
        n.line > cx.toks[close].line
            && (matches!(n.kind, Kind::Ident | Kind::Directive | Kind::Punct(b'{' | b'}' | b'*' | b'~' | b'#')))
    });
    if !starts_statement {
        return None;
    }
    if let Some(at) = cx.space_after(close) {
        return Some(vec![Edit { start: at, end: at + 1, with: b";" }]);
    }
    Some(vec![Edit { start: cx.toks[i].start, end: cx.toks[close].end, with: b";" }])
}

/// A lone identifier statement (`VERIFY_FAILS_END`) followed by another
/// statement on a later line.
fn bare_statement(cx: &Cx<'_>, i: usize, site: Site) -> Option<Vec<Edit>> {
    if !cx.macro_like(i) || !cx.starts_statement(i) {
        return None;
    }
    let next = cx.toks.get(i + 1);
    let alone = next.is_none_or(|n| {
        n.line > cx.toks[i].line && matches!(n.kind, Kind::Ident | Kind::Directive | Kind::Punct(b'{' | b'}' | b'~'))
    });
    if !alone {
        return None;
    }
    // At declaration level (`YY_DECL` before a body, `FMT_END_EXPORT`) the
    // macro goes; in a body it becomes an empty statement.
    let with: &'static [u8] = if site.in_body { b";" } else { b"" };
    Some(vec![Edit { start: cx.toks[i].start, end: cx.toks[i].end, with }])
}

/// Unknown specifiers in a declaration head or a parameter: identifiers
/// that are neither the type nor the declared name.
fn specifiers(cx: &Cx<'_>, i: usize, _: Site) -> Option<Vec<Edit>> {
    // The head: from the statement start (or the parameter start) to the
    // declared name.
    let start = {
        let s = cx.statement_start(i);
        // Inside a parameter list: after the nearest `(` / `,` at depth 0.
        let mut depth = 0i32;
        let mut p_start = None;
        for p in (s..i).rev() {
            match cx.toks[p].kind {
                Kind::Punct(b')') => depth += 1,
                Kind::Punct(b'(') => {
                    if depth == 0 {
                        p_start = Some(p + 1);
                        break;
                    }
                    depth -= 1;
                }
                Kind::Punct(b',') if depth == 0 => {
                    p_start = Some(p + 1);
                    break;
                }
                _ => {}
            }
        }
        p_start.unwrap_or(s)
    };
    // A template head is not part of the declaration head.
    let mut start = start;
    while cx.is(start, "template") && cx.punct(start + 1, b'<') {
        let mut d = 0i32;
        let mut e = start + 1;
        while e < cx.toks.len() && e < start + 256 {
            match cx.toks[e].kind {
                Kind::Punct(b'<') => d += 1,
                Kind::Punct(b'>') => d -= 1,
                Kind::Punct(b';' | b'{' | b'}') => return None,
                _ => {}
            }
            e += 1;
            if d == 0 {
                break;
            }
        }
        start = e;
    }
    // Walk the head: identifiers, `*`, `&`, `::` chains, template arguments.
    let mut k = start;
    let mut idents: Vec<usize> = Vec::new();
    while k < cx.toks.len() && k < start + 32 {
        match cx.toks[k].kind {
            Kind::Ident => {
                if cx.word_in(k, STATEMENT_WORDS) || cx.is(k, "operator") {
                    return None;
                }
                idents.push(k);
                k += 1;
            }
            Kind::Punct(b'*' | b'&') => k += 1,
            // `extern "C"`.
            Kind::Literal if k > 0 && cx.is(k - 1, "extern") => k += 1,
            Kind::Punct(b':') if cx.punct(k + 1, b':') => k += 2,
            Kind::Punct(b'<') => {
                let mut d = 0i32;
                while k < cx.toks.len() {
                    match cx.toks[k].kind {
                        Kind::Punct(b'<') => d += 1,
                        Kind::Punct(b'>') => d -= 1,
                        Kind::Punct(b';' | b'{' | b'}') => return None,
                        _ => {}
                    }
                    k += 1;
                    if d == 0 {
                        break;
                    }
                }
            }
            _ => break,
        }
    }
    // The head must end the way a declarator does.
    let stop = cx.toks.get(k)?;
    if !matches!(stop.kind, Kind::Punct(b'(' | b'=' | b';' | b',' | b'[' | b')' | b'{')) {
        return None;
    }
    // Function-call statements (`f(x);`) are not declarations.
    if idents.len() < 2 {
        return None;
    }
    let name = *idents.last()?;
    if !cx.name(name) {
        return None;
    }
    // Members of a qualified chain (`ns::Type`, `parse_context<Char>::`)
    // are never unknown specifiers.
    let chain = |x: usize| {
        if x >= 2 && cx.punct(x - 1, b':') && cx.punct(x - 2, b':') {
            return true;
        }
        let mut e = x + 1;
        if cx.punct(e, b'<') {
            let mut d = 0i32;
            while e < cx.toks.len() && e < x + 64 {
                match cx.toks[e].kind {
                    Kind::Punct(b'<') => d += 1,
                    Kind::Punct(b'>') => d -= 1,
                    _ => {}
                }
                e += 1;
                if d == 0 {
                    break;
                }
            }
        }
        cx.punct(e, b':') && cx.punct(e + 1, b':')
    };
    let before: Vec<usize> = idents[..idents.len() - 1].to_vec();
    let has_type_word = before.iter().any(|&x| cx.word_in(x, TYPE_WORDS));
    let explicit = before.iter().any(|&x| cx.is(x, "explicit"));
    // Tags: `struct X` names a type.
    let tag_names: Vec<usize> = before
        .iter()
        .filter(|&&x| cx.word_in(x, &["struct", "union", "enum", "class", "typename"]))
        .map(|&x| x + 1)
        .collect();
    let chained_type = before.iter().any(|&x| chain(x));
    let candidates: Vec<usize> =
        before.iter().copied().filter(|&x| cx.name(x) && !chain(x) && !tag_names.contains(&x)).collect();
    let attribute_like = |x: usize| cx.text(x).starts_with(b"__") || cx.macro_like(x);
    // `MACRO Name(params) : init(…)`: only a constructor takes a member
    // initializer list, so it has no return type.
    let constructor = cx.punct(k, b'(')
        && matching_paren(cx.toks, k).is_some_and(|c| {
            let mut e = c + 1;
            while cx.word_in(e, &["noexcept", "const", "override"]) {
                e += 1;
            }
            cx.punct(e, b':') && !cx.punct(e + 1, b':')
        });
    let blank: Vec<usize> = if has_type_word || !tag_names.is_empty() || explicit || constructor || chained_type {
        // `int CJSON_CDECL main`, `const void __unused *p` with a keyword
        // type, `explicit FMT_CONSTEXPR name(…)`, `MACRO ns::T f()`: no
        // other name is a type.
        candidates
    } else {
        // `static redisAtomic size_t n`, `CFSocketRef __unused s`: the type
        // is the last name that does not look like an attribute macro.
        let ty = candidates.iter().rev().find(|&&x| !attribute_like(x)).or(candidates.last()).copied();
        candidates.into_iter().filter(|&x| Some(x) != ty).collect()
    };
    let blank: Vec<usize> = blank.into_iter().filter(|&x| x != name).collect();
    if blank.is_empty() {
        return None;
    }
    Some(
        blank
            .into_iter()
            .map(|x| {
                // A macro with arguments goes with them.
                let end = if cx.punct(x + 1, b'(') { matching_paren(cx.toks, x + 1).unwrap_or(x) } else { x };
                Edit::blank(cx.toks[x].start, cx.toks[end].end)
            })
            .collect(),
    )
}

/// Two operands in a row where one is a macro (`EV_DEFAULT_ c`,
/// `"%" PRIu64 "\n"`): the macro goes.
fn adjacent_operands(cx: &Cx<'_>, i: usize, _: Site) -> Option<Vec<Edit>> {
    let operand =
        |k: usize| cx.toks.get(k).is_some_and(|t| matches!(t.kind, Kind::Ident | Kind::Literal | Kind::Punct(b'0')));
    if !cx.macro_like(i) {
        return None;
    }
    let prev_operand = i > 0
        && operand(i - 1)
        && !cx.word_in(i - 1, STATEMENT_WORDS)
        && !cx.word_in(i - 1, DECL_WORDS)
        && !cx.word_in(i - 1, TYPE_WORDS);
    let next_operand = operand(i + 1) && !cx.word_in(i + 1, STATEMENT_WORDS);
    // Inside parentheses or after a literal: an expression, not a declaration.
    let in_expr = (i > 0 && cx.toks[i - 1].kind == Kind::Literal)
        || cx.toks.get(i + 1).is_some_and(|t| t.kind == Kind::Literal)
        || (i > 0 && cx.punct(i - 1, b'(') && next_operand)
        || (i > 0 && cx.punct(i - 1, b',') && next_operand);
    ((prev_operand || next_operand) && in_expr).then(|| vec![Edit::blank(cx.toks[i].start, cx.toks[i].end)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mask::lex_literals;

    /// Applies the repair for the first token `marker` points at.
    fn fix(src: &str, marker: &str, in_body: bool) -> String {
        let toks = lex_literals(src.as_bytes());
        let at = src.find(marker).unwrap();
        let site = Site { start: at, end: at + 1, in_body };
        let edits = repair(src.as_bytes(), &toks, site).unwrap_or_default();
        let mut out = src.as_bytes().to_vec();
        crate::preprocess::apply(&mut out, &edits);
        assert_eq!(out.len(), src.len());
        String::from_utf8(out).unwrap()
    }

    /// `src` with `word` replaced by spaces.
    fn blanked(src: &str, word: &str) -> String {
        src.replacen(word, &" ".repeat(word.len()), 1)
    }

    #[test]
    fn type_arguments() {
        assert_eq!(fix("x = va_arg(ap,char*);", "*", true), "x = va_arg(ap,0    );");
        assert_eq!(fix("n = va_arg(ap, long long);", "long", true), "n = va_arg(ap, 0        );");
        assert_eq!(fix("p = va_arg(ap, robj*);", "*", true), "p = va_arg(ap, robj );");
        assert_eq!(fix("s = va_arg(ap, struct x *);", "struct", true), "s = va_arg(ap, 0         );");
    }

    #[test]
    fn statement_macros() {
        assert_eq!(fix("void f() {\n  each(a, i, x) {\n  }\n}", "each", true), "void f() {\n  each(a, i, x);{\n  }\n}");
        assert_eq!(fix("void f() {\n  each(a, i, x){\n  }\n}", "each", true), "void f() {\n               {\n  }\n}");
        assert_eq!(
            fix("void f() {\n  CHECK(a == 1)\n  g();\n}", "CHECK", true),
            "void f() {\n  ;            \n  g();\n}"
        );
        assert_eq!(
            fix("void f() {\n  CHECK(a == 1) \n  g();\n}", "CHECK", true),
            "void f() {\n  CHECK(a == 1);\n  g();\n}"
        );
        assert_eq!(fix("void f() {\n  g();\n  END_IT\n}", "END_IT", true), "void f() {\n  g();\n  ;     \n}");
        assert_eq!(fix("OP(A, 1)\nOP(B, 2)\n", "OP(B", false), "OP(A, 1)\n;       \n");
    }

    #[test]
    fn declaration_heads() {
        assert_eq!(fix("static redisAtomic size_t n = 0;", "size_t", false), "static             size_t n = 0;");
        assert_eq!(fix("int CJSON_CDECL main(void)", "main", false), "int             main(void)");
        assert_eq!(
            fix("void f(CFSocketRef __unused s, int t);", "s,", false),
            "void f(CFSocketRef          s, int t);"
        );
        assert_eq!(
            fix("explicit FMT_CONSTEXPR result(const S& f) {}", "result", false),
            "explicit               result(const S& f) {}"
        );
        let src = "struct C {\n template <typename T, typename U = V<T>>\n  explicit FMT_CONSTEXPR C(T t) {}\n};";
        assert_eq!(fix(src, "C(T", false), blanked(src, "FMT_CONSTEXPR"));
        let src = "struct C {\n  FMT_CONSTEXPR C(int a, int b = 0) : a_(a) {}\n};";
        assert_eq!(fix(src, "C(int", false), blanked(src, "FMT_CONSTEXPR"));
        let src = "ffc_internal ffc_inline\nuint64_t f(int x) {}";
        assert_eq!(fix(src, "uint64_t", false), blanked(src, "ffc_internal ffc_inline"));
        // A qualified type is kept.
        assert_eq!(fix("static ns::Foo* make(int);", "make", false), "static ns::Foo* make(int);");
        let src = "template <typename C>\nvoid ctx<C>::check(int id) {}";
        assert_eq!(fix(src, "check", false), src);
        let src = "MY_API std::string ns::name() {}";
        assert_eq!(fix(src, "name", false), blanked(src, "MY_API"));
        // A call is not a declaration.
        assert_eq!(fix("f(a, b);", "b", true), "f(a, b);");
    }

    #[test]
    fn declarator_macros() {
        assert_eq!(
            fix("void *(CJSON_CDECL *allocate)(size_t size);", "CJSON", false),
            "void *(            *allocate)(size_t size);"
        );
        let src = "jv f(const char*, ...) JV_PRINTF_LIKE(1, 2);";
        assert_eq!(fix(src, "JV_", false), blanked(src, "JV_PRINTF_LIKE(1, 2)"));
        assert_eq!(fix("API void (*Free)(void *p) ATTR;", "ATTR", false), "API void (*Free)(void *p)     ;");
        let src = "x_t a = {1},\n  __attribute__ ((deprecated)) b = {2};";
        assert_eq!(fix(src, "__attr", false), blanked(src, "__attribute__ ((deprecated))"));
        assert_eq!(fix("auto f(locale_ref loc = {}) -> int;", "{}", false), "auto f(locale_ref loc = 0 ) -> int;");
        assert_eq!(fix("void f(T x = {1, 2});", "{1", false), "void f(T x = (1, 2));");
        assert_eq!(
            fix("struct A { enum : unsigned { x = 1 }; };", "enum", false),
            "struct A { enum            { x = 1 }; };"
        );
        assert_eq!(fix("auto x = f(typename T::v(), 0);", "typename", true), "auto x = f(         T::v(), 0);");
    }

    #[test]
    fn macro_statements_and_keywords() {
        assert_eq!(
            fix("void f() {\n  if ISZERO(dn) {\n  }\n}", "ISZERO", true),
            "void f() {\n  if(ISZERO(dn)){\n  }\n}"
        );
        assert_eq!(fix("void f() {\n  FMT_TRY { g(); }\n}", "FMT_TRY", true), "void f() {\n          { g(); }\n}");
        let src = "void f() {\n  FMT_CATCH(...) {}\n}";
        assert_eq!(fix(src, "FMT_CATCH", true), blanked(src, "FMT_CATCH(...)"));
        assert_eq!(fix("void f() {\n  every(100) cron();\n}", "cron", true), "void f() {\n  if   (100) cron();\n}");
        let src = "class A {\n  MOCK_METHOD(int, f, (int));\n};";
        assert_eq!(fix(src, "MOCK", false), blanked(src, "MOCK_METHOD(int, f, (int))"));
        assert_eq!(fix("const char *s = STR(LEN) \" chars\";", "STR", true), "const char *s = \"\"       \" chars\";");
        assert_eq!(
            fix("struct B : A { using typename A::type; };", "typename", false),
            "struct B : A { using          A::type; };"
        );
        assert_eq!(
            fix("extern template auto f<char>(int) -> int;", "extern", false),
            "       template auto f<char>(int) -> int;"
        );
        assert_eq!(fix("template class A<int, &B::c>;", ">;", false), "         class A<int, &B::c>;");
        assert_eq!(
            fix("void f() { EXPECT_THROW(throw E(1), X); }", "throw", true),
            "void f() { EXPECT_THROW(      E(1), X); }"
        );
        assert_eq!(fix("TEST(suite, enum) {}", "enum", false), "TEST(suite, _num) {}");
    }

    #[test]
    fn grammar_gaps() {
        let src = "auto f(T* p) -> decltype(g(*p), std::true_type());";
        assert_eq!(fix(src, ", std", false), "auto f(T* p) -> decltype(g(*p)| std::true_type());");
        let src = "template <bool B, typename T, typename Ptr = const T*>\nclass A;";
        assert_eq!(fix(src, "*>", false), "template <bool B, typename T, typename Ptr =       T >\nclass A;");
    }

    #[test]
    fn deleted_declarations() {
        let src = "template <typename T> void f(T) = delete;";
        assert_eq!(fix(src, "= delete", false), blanked(src, "= delete"));
    }

    #[test]
    fn member_pointer_syntax() {
        assert_eq!(fix("int f(V v) { return v.*(&G::c); }", ".*", true), "int f(V v) { return v+ (&G::c); }");
        assert_eq!(fix("int f(V* p) { return p->*m; }", "->*", true), "int f(V* p) { return p+  m; }");
        assert_eq!(fix("bool b = is_f<int(s::*)>::value;", "s::", true), "bool b = is_f<int(   *)>::value;");
        let src = "using X = A<T, std::filebuf, &std::filebuf::_M>;";
        assert_eq!(fix(src, "&std", false), "using X = A<T, std::filebuf,  std::filebuf::_M>;");
    }

    #[test]
    fn dangling_assignment() {
        assert_eq!(fix("enum {\n  N =\n\n};", "=", false), "enum {\n  N  \n\n};");
    }

    #[test]
    fn operands_and_qt() {
        assert_eq!(fix("void f() { ev_loop(EV_DEFAULT_ 0); }", "EV_", true), "void f() { ev_loop(            0); }");
        assert_eq!(fix("auto s = \"%\" PRIu64 \"\\n\";", "PRI", true), "auto s = \"%\"        \"\\n\";");
        assert_eq!(
            fix("class A {\nsignals:\n  void s();\n};", "signals", false),
            "class A {\npublic :\n  void s();\n};"
        );
        assert_eq!(
            fix("class A {\npublic slots:\n  void s();\n};", "slots", false),
            "class A {\npublic      :\n  void s();\n};"
        );
    }
}
