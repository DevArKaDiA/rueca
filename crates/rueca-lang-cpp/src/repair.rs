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
//! | `decltype(a, b)` | a comma expression in `decltype` (`->`, `<<`, braces inside) | the comma becomes `\|` |
//! | pointer default argument | `typename P = const T*>` | `typename P =       T >` |
//! | deleted declaration | `template <…> void f(T) = delete;` | `= delete` blanked |
//! | member pointers | `v.*(&G::c)`, `p->*m`, `int (T::*)()`, `A<&T::m>` | `+`, `int (   *)()`, `&` blanked |
//! | dangling `=` | `N =` before `}` (an X-macro table removed) | `=` blanked |
//! | access macro | `JSON_PRIVATE_UNLESS_TESTED:` at a statement start | `public` and spaces |
//! | storage class before a declaration macro | `static MP_DEFINE_CONST_DICT(d, t);` | blanked with the call (`;` stays) |
//! | wrapping macro | `LUAI_DDEC(const int t[N];)`, `LUAI_TRY(L, c, f(););` | the name, `(` and `)` blanked |
//! | parenthesized name | `T *(lua_newstate) (…);` | the parentheses blanked |
//! | operator argument | `intop(+, a, b)`, `intop(>>, a, b)` | `0` and spaces |
//! | empty argument | `BEST_TIME("x", f(), n, , r)` | a `0` in the space |
//! | empty statement argument | `EXPECT_NO_FATAL_FAILURE(;)` | the `;` blanked |
//! | attribute macros | `__ALIGN_BEGIN static T b[N] __ALIGN_END;`, `size_t n MACRO_INIT(0);` | every one of the statement blanked |
//! | `asm` macro and labels | `__ASM volatile ("…")`, `register int r __asm__("r0") = v;` | `asm`; the label blanked |
//! | subscripted call first in a block | `MP_STATE_PORT(h)[i] = c;` | a `*` in the space before (an expression) |
//! | macro in a `using` | `using testing::GMOCK_FLAG(verbose);` | the name, `(` and `)` blanked |
//! | unnamed pointer template parameter | `template <typename R, R* = nullptr>` | the `*` blanked |
//! | lower-case specifier before an operator | `really_inline ~stream();`, `really_inline bool operator!=(…)` | blanked |
//! | qualified type argument | `CHECK_THROWS_AS(f(), json::error const&)`, `CHECK(f(std::vector<int>({1})), E&)` | the `&` / `*` (and a trailing `const`) blanked |
//! | unnamed pointer parameter with a default | `typename E<T>::type* = 0`, `const void* /*hint*/ = 0` in a parameter list | `*_= 0` (a name in the space) |
//! | array of pointers after `new` | `new char *[n]`, `new (std::nothrow) void *[n]` | the `*` blanked |
//! | declaration as a macro argument | `EXPECT_THROW(string s = f(), E)` | the type blanked |
//! | argument ending in an operator | `BENCHMARK_CAPTURE(f, a / -, x)` | the operator blanked |
//! | trailing macro after a template-id or qualifier, or before `:` | `int d<int128_t>() FMT_NOEXCEPT {`, `f() final FMT_OVERRIDE;`, `S(…) FMT_NOEXCEPT : a() {}` | blanked |
//! | calling convention with `__declspec` | `__declspec(dllimport) int __stdcall f();` | both blanked |
//! | qualified explicit instantiation | `template struct detail::data<void>;` | blanked up to the `;` |
//! | member pointer type argument | `StringMaker<R C::*>` | `C::` blanked |
//! | declaration macro without `;` | `NLOHMANN_JSON_SERIALIZE_ENUM(T, { … })` before the next declaration | blanked |
//! | statement macro with non-expression arguments | `FMT_MSC_WARNING(suppress : 6102)` + newline | `;` and spaces |
//! | `if`-like macro before `else` | `CHECKED_IF(flag)` + newline + `return true;` + `else` | `if` |
//! | macro between a name and its arguments | `FMT_POSIX_CALL(fileno FMT_ARGS(file_))` | the macro blanked |
//! | specifier before a constructor | `really_inline S(…) NOEXCEPT : a(x) {}` | the specifier and the attribute macro blanked |

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
    let (cands, near_count) = candidates(&cx, k, site);
    // Local rewrites first, on the tokens at and inside the error; then
    // statement-level ones, nearest candidate first.
    let near = &cands[..near_count];
    LOCAL_RULES
        .iter()
        .find_map(|rule| near.iter().find_map(|&i| rule(&cx, i, site)))
        .or_else(|| cands.iter().find_map(|&i| STATEMENT_RULES.iter().find_map(|rule| rule(&cx, i, site))))
}

/// A repair rule: the edits for the candidate token, if its shape matches.
type Rule = fn(&Cx<'_>, usize, Site) -> Option<Vec<Edit>>;

/// Rules tried on the tokens at and inside the error, in order.
const LOCAL_RULES: [Rule; 33] = [
    new_pointer_array,
    declaration_argument,
    dangling_operator,
    specifier_before_operator,
    empty_argument,
    asm_label,
    unnamed_pointer_parameter,
    using_macro,
    empty_statement_argument,
    asm_macro,
    subscripted_call,
    wrapper_macro,
    operator_argument,
    parenthesized_name,
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

/// Statement-level rules, tried on every candidate in order.
const STATEMENT_RULES: [Rule; 9] = [
    declaration_attributes,
    trailing_macro,
    brace_macro,
    block_call,
    guard_call,
    statement_call,
    bare_statement,
    declaration_macro_call,
    specifiers,
];

/// Candidate tokens of [`repair`], nearest first, without duplicates.
struct Candidates {
    list: Vec<usize>,
    /// Token count: candidates are below it.
    n: usize,
}

impl Candidates {
    fn push(&mut self, x: usize) {
        if x < self.n && !self.list.contains(&x) {
            self.list.push(x);
        }
    }

    fn extend(&mut self, xs: impl IntoIterator<Item = usize>) {
        for x in xs {
            self.push(x);
        }
    }
}

/// The candidates for the error at token `k`, nearest first, and how many
/// of the first are near the error (the local rules' candidates): the
/// tokens around the error (it may be reported on the token after the
/// culprit), the tokens inside it, then the rest of the statement back to
/// its start (a statement macro is its first token).
fn candidates(cx: &Cx<'_>, k: usize, site: Site) -> (Vec<usize>, usize) {
    let toks = cx.toks;
    let mut c = Candidates { list: Vec::new(), n: toks.len() };
    for d in 0..3usize {
        c.push(k.saturating_sub(d));
        c.push(k + d);
    }
    // The line the error follows (a macro call ending there).
    c.extend(previous_line(toks, k));
    let near_count = c.list.len();
    let inside = (k..toks.len().min(k + 32)).take_while(|&x| toks[x].start < site.end).count();
    c.extend(k..k + inside);
    let start = cx.statement_start(k.saturating_sub(1)).min(k);
    c.extend((start..k.saturating_sub(2)).rev().take(64));
    // A long statement: its first token (a macro head) anyway, also when
    // the error follows its closing `)` (`MACRO(T, { … })` + newline).
    c.push(start);
    let before = k.checked_sub(1).map(|p| toks[p].kind);
    if before == Some(Kind::Punct(b')')) {
        c.push(cx.statement_start(k));
    }
    // A `;` missing right after a block (`MACRO T(…) : a(x) { }` read as a
    // declaration with a brace initializer): the culprit is in the head
    // of that block.
    if before == Some(Kind::Punct(b'}'))
        && site.start == site.end
        && let Some(open) = block_open_before(toks, k).filter(|&p| p > 0)
    {
        let head = cx.statement_start(open - 1);
        c.extend((head..open).rev().take(64));
    }
    // The head of the enclosing block (`FMT_TRY {` reported inside it).
    if let Some(open) = enclosing_block_open(toks, k).filter(|&p| p > 0) {
        c.push(open - 1);
        c.push(cx.statement_start(open - 1));
    }
    let near = c.list.len().min(near_count + inside);
    (c.list, near)
}

/// The tokens before `k - 1` on its line, nearest first (at most 15).
fn previous_line(toks: &[Tok], k: usize) -> Vec<usize> {
    let Some(last) = k.checked_sub(1) else { return Vec::new() };
    let line = toks[last].line;
    (k.saturating_sub(16)..last).rev().take_while(|&x| toks[x].line == line).collect()
}

/// The `{` of the block whose `}` is token `k - 1`.
fn block_open_before(toks: &[Tok], k: usize) -> Option<usize> {
    let mut depth = 0i32;
    for p in (k.saturating_sub(512)..k).rev() {
        match toks[p].kind {
            Kind::Punct(b'}') => depth += 1,
            Kind::Punct(b'{') => {
                depth -= 1;
                if depth == 0 {
                    return Some(p);
                }
            }
            _ => {}
        }
    }
    None
}

/// The unclosed `{` before token `k`.
fn enclosing_block_open(toks: &[Tok], k: usize) -> Option<usize> {
    let mut depth = 0i32;
    for p in (k.saturating_sub(512)..k.min(toks.len())).rev() {
        match toks[p].kind {
            Kind::Punct(b'}') => depth += 1,
            Kind::Punct(b'{') if depth > 0 => depth -= 1,
            Kind::Punct(b'{') => return Some(p),
            _ => {}
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
    let open = enclosing_open_paren(cx, i)?;
    // A call: a name before the parenthesis.
    if open == 0 || !cx.name(open - 1) {
        return None;
    }
    let close = matching_paren_braced(cx, open)?;
    let edits: Vec<Edit> =
        argument_ranges(cx, open, close).into_iter().filter_map(|arg| type_argument_edit(cx, arg, site)).collect();
    (!edits.is_empty()).then_some(edits)
}

/// The innermost `(` before `i` at depth 0 (brace initializers in the
/// arguments are skipped: `f(std::vector<int>({1}), json::error&)`), or
/// `None` at a statement boundary.
fn enclosing_open_paren(cx: &Cx<'_>, i: usize) -> Option<usize> {
    let (mut depth, mut braces) = (0i32, 0i32);
    for p in (i.saturating_sub(128)..=i.min(cx.toks.len() - 1)).rev() {
        match cx.toks[p].kind {
            Kind::Punct(b')') if p != i => depth += 1,
            Kind::Punct(b'(') if depth == 0 => return Some(p),
            Kind::Punct(b'(') => depth -= 1,
            Kind::Punct(b'}') if depth > 0 => braces += 1,
            Kind::Punct(b'{') if braces > 0 => braces -= 1,
            Kind::Punct(b';' | b'{' | b'}') | Kind::Directive => return None,
            _ => {}
        }
    }
    None
}

/// The token ranges of the arguments between the parentheses at `open`
/// and `close` (split at commas outside brackets).
fn argument_ranges(cx: &Cx<'_>, open: usize, close: usize) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
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
        out.push(a..b);
        a = b + 1;
    }
    out
}

/// The edit of [`type_argument`] for one argument overlapping the error
/// (a MISSING node sits at its end): a keyword type becomes `0`; a
/// pointer or reference to a (qualified) name loses its `*` / `&` (and a
/// trailing qualifier: `json::error const&`).
fn type_argument_edit(cx: &Cx<'_>, arg: std::ops::Range<usize>, site: Site) -> Option<Edit> {
    let (a, b) = (arg.start, arg.end);
    if b <= a || cx.toks[a].start > site.end || site.start > cx.toks[b - 1].end {
        return None;
    }
    let stars = arg.rev().take_while(|&x| cx.punct(x, b'*') || cx.punct(x, b'&')).count();
    let core = a..b - stars;
    let keywords = core.clone().all(|x| cx.word_in(x, TYPE_WORDS) || cx.is(x, "const") || cx.is(x, "volatile"));
    let tagged = core.len() == 2 && (cx.is(a, "struct") || cx.is(a, "union") || cx.is(a, "enum"));
    if !core.is_empty() && (keywords || tagged) {
        return Some(Edit { start: cx.toks[a].start, end: cx.toks[b - 1].end, with: b"0" });
    }
    if stars == 0 {
        return None;
    }
    let mut c = core;
    while c.start < c.end && cx.word_in(c.start, &["const", "volatile", "struct", "union", "enum"]) {
        c.start += 1;
    }
    while c.end > c.start && cx.word_in(c.end - 1, &["const", "volatile"]) {
        c.end -= 1;
    }
    qualified_name(cx, c.clone()).then(|| Edit::blank(cx.toks[c.end.min(b - stars)].start, cx.toks[b - 1].end))
}

/// The `)` matching the `(` at `open`, across balanced braces (brace
/// initializers as arguments); `None` at a `;` outside braces.
fn matching_paren_braced(cx: &Cx<'_>, open: usize) -> Option<usize> {
    let (mut parens, mut braces) = (0i32, 0i32);
    for k in open..cx.toks.len().min(open + 512) {
        match cx.toks[k].kind {
            Kind::Punct(b'(') => parens += 1,
            Kind::Punct(b')') => {
                parens -= 1;
                if parens == 0 {
                    return Some(k);
                }
            }
            Kind::Punct(b'{') => braces += 1,
            Kind::Punct(b'}') => {
                braces -= 1;
                if braces < 0 {
                    return None;
                }
            }
            Kind::Punct(b';') if braces == 0 => return None,
            Kind::Directive => return None,
            _ => {}
        }
    }
    None
}

/// Tokens `range` spell a (possibly qualified) name: `T`, `json::error`,
/// `::ns::T`.
fn qualified_name(cx: &Cx<'_>, range: std::ops::Range<usize>) -> bool {
    let (mut k, end) = (range.start, range.end);
    if k >= end {
        return false;
    }
    if cx.punct(k, b':') && cx.punct(k + 1, b':') {
        k += 2;
    }
    loop {
        if k >= end || !cx.name(k) {
            return false;
        }
        k += 1;
        if k == end {
            return true;
        }
        if !(cx.punct(k, b':') && cx.punct(k + 1, b':')) {
            return false;
        }
        k += 2;
    }
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
    (i.saturating_sub(2)..=(i + 2).min(cx.toks.len().saturating_sub(1))).find_map(|k| {
        member_access(cx, k)
            .or_else(|| member_pointer_declarator(cx, k))
            .or_else(|| member_pointer_type_argument(cx, k))
            .or_else(|| member_address_argument(cx, k))
    })
}

/// `.*` and `->*` with the `*` at `k`.
fn member_access(cx: &Cx<'_>, k: usize) -> Option<Vec<Edit>> {
    if !cx.punct(k, b'*') || k == 0 {
        return None;
    }
    let a = if cx.punct(k - 1, b'.') {
        k - 1
    } else if cx.punct(k - 1, b'>') && k > 1 && cx.punct(k - 2, b'-') {
        k - 2
    } else {
        return None;
    };
    Some(vec![Edit { start: cx.toks[a].start, end: cx.toks[k].end, with: b"+" }])
}

/// `(T::*)` with the `(` at `k`.
fn member_pointer_declarator(cx: &Cx<'_>, k: usize) -> Option<Vec<Edit>> {
    let ok =
        cx.punct(k, b'(') && cx.ident(k + 1) && cx.punct(k + 2, b':') && cx.punct(k + 3, b':') && cx.punct(k + 4, b'*');
    ok.then(|| vec![Edit::blank(cx.toks[k + 1].start, cx.toks[k + 3].end)])
}

/// `S<R C::*>` with `C` at `k`: a member pointer type as a template
/// argument.
fn member_pointer_type_argument(cx: &Cx<'_>, k: usize) -> Option<Vec<Edit>> {
    let ok = cx.name(k)
        && k > 0
        && cx.name(k - 1)
        && cx.punct(k + 1, b':')
        && cx.punct(k + 2, b':')
        && cx.punct(k + 3, b'*')
        && (cx.punct(k + 4, b'>') || cx.punct(k + 4, b','));
    ok.then(|| vec![Edit::blank(cx.toks[k].start, cx.toks[k + 2].end)])
}

/// `<…, &T::m>` with the `&` at `k`.
fn member_address_argument(cx: &Cx<'_>, k: usize) -> Option<Vec<Edit>> {
    let ok = cx.punct(k, b'&')
        && k > 0
        && (cx.punct(k - 1, b',') || cx.punct(k - 1, b'<'))
        && cx.ident(k + 1)
        && cx.punct(k + 2, b':')
        && cx.punct(k + 3, b':');
    ok.then(|| vec![Edit::blank(cx.toks[k].start, cx.toks[k].end)])
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
/// the comma becomes `|`. `->` and `<<` in the operands are operators, not
/// brackets.
fn decltype_comma(cx: &Cx<'_>, i: usize, _: Site) -> Option<Vec<Edit>> {
    if !cx.punct(i, b',') {
        return None;
    }
    let adjacent = |a: usize, b: usize| cx.toks[a].end == cx.toks[b].start;
    let mut depth = 0i32;
    let mut p = i;
    let floor = i.saturating_sub(128);
    while p > floor {
        p -= 1;
        match cx.toks[p].kind {
            // `->`.
            Kind::Punct(b'>') if p > 0 && cx.punct(p - 1, b'-') && adjacent(p - 1, p) => p -= 1,
            // `<<`.
            Kind::Punct(b'<') if p > 0 && cx.punct(p - 1, b'<') && adjacent(p - 1, p) => p -= 1,
            Kind::Punct(b')' | b'>' | b']' | b'}') => depth += 1,
            Kind::Punct(b'(' | b'<' | b'[' | b'{') if depth > 0 => depth -= 1,
            Kind::Punct(b'(') => {
                return (p > 0 && cx.is(p - 1, "decltype"))
                    .then(|| vec![Edit { start: cx.toks[i].start, end: cx.toks[i].end, with: b"|" }]);
            }
            Kind::Punct(b'<' | b'[' | b';' | b'{') => return None,
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
    let eq = template_default_eq(cx, star)?;
    // `const T*` → `T`: the grammar takes neither the pointer nor the
    // qualifier there.
    let mut edits = vec![Edit::blank(cx.toks[star].start, cx.toks[star].end)];
    edits.extend(
        (eq + 1..star)
            .filter(|&q| cx.is(q, "const") || cx.is(q, "volatile"))
            .map(|q| Edit::blank(cx.toks[q].start, cx.toks[q].end)),
    );
    Some(edits)
}

/// The `=` of the template parameter containing token `star`, when that
/// parameter is in a `template <` head.
fn template_default_eq(cx: &Cx<'_>, star: usize) -> Option<usize> {
    let mut eq = None;
    let mut depth = 0i32;
    for p in (star.saturating_sub(64)..star).rev() {
        match cx.toks[p].kind {
            Kind::Punct(b'=') if depth == 0 && eq.is_none() => eq = Some(p),
            Kind::Punct(b',') if depth == 0 && eq.is_none() => return None,
            Kind::Punct(b'>' | b')') => depth += 1,
            Kind::Punct(b'(' | b'<') if depth > 0 => depth -= 1,
            Kind::Punct(b'<') => return eq.filter(|_| p > 0 && cx.is(p - 1, "template")),
            Kind::Punct(b';' | b'{' | b'}') => return None,
            _ => {}
        }
    }
    None
}

/// Qt `signals:` / `slots:` sections.
fn qt_section(cx: &Cx<'_>, i: usize, _: Site) -> Option<Vec<Edit>> {
    if i >= cx.toks.len() {
        return None;
    }
    let w = cx.text(i);
    if !cx.ident(i) || !cx.punct(i + 1, b':') || cx.punct(i + 2, b':') {
        return None;
    }
    match w {
        b"signals" | b"Q_SIGNALS" => Some(vec![Edit { start: cx.toks[i].start, end: cx.toks[i].end, with: b"public" }]),
        b"slots" | b"Q_SLOTS" if i > 0 && cx.word_in(i - 1, &["public", "private", "protected"]) => {
            Some(vec![Edit::blank(cx.toks[i].start, cx.toks[i].end)])
        }
        // An access macro (`JSON_PRIVATE_UNLESS_TESTED:`) at a statement
        // start in a class body.
        _ if cx.macro_like(i) && w.len() >= 6 && cx.starts_statement(i) && !site_is_case(cx, i) => {
            Some(vec![Edit { start: cx.toks[i].start, end: cx.toks[i].end, with: b"public" }])
        }
        _ => None,
    }
}

/// Whether token `i` is a `case` label's value.
fn site_is_case(cx: &Cx<'_>, i: usize) -> bool {
    i > 0 && (cx.is(i - 1, "case") || cx.is(i - 1, "default"))
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

/// `new char *[n]`, `new (std::nothrow) void *[n]`: an array of pointers,
/// which the grammar does not take after `new`; the `*` goes.
fn new_pointer_array(cx: &Cx<'_>, i: usize, site: Site) -> Option<Vec<Edit>> {
    if !site.in_body {
        return None;
    }
    let star = (i.saturating_sub(2)..=i + 1)
        .find(|&k| cx.punct(k, b'*') && (cx.punct(k + 1, b'[') || cx.punct(k + 1, b'*')))?;
    let mut last = star;
    while cx.punct(last + 1, b'*') {
        last += 1;
    }
    if !cx.punct(last + 1, b'[') {
        return None;
    }
    // Back over the type (names, `::`, template arguments) and a placement.
    let mut k = star;
    let mut depth = 0i32;
    while k > 0 && k + 32 > star {
        k -= 1;
        match cx.toks[k].kind {
            Kind::Punct(b'>' | b')') => depth += 1,
            Kind::Punct(b'<' | b'(') if depth > 0 => depth -= 1,
            Kind::Ident if depth == 0 && cx.is(k, "new") => {
                return Some(vec![Edit::blank(cx.toks[star].start, cx.toks[last].end)]);
            }
            Kind::Ident | Kind::Punct(b':' | b',') => {}
            _ if depth > 0 => {}
            _ => return None,
        }
    }
    None
}

/// `EXPECT_THROW(string s = format(…), E)`: a declaration as a macro
/// argument; the type goes, leaving an assignment.
fn declaration_argument(cx: &Cx<'_>, i: usize, site: Site) -> Option<Vec<Edit>> {
    if !site.in_body {
        return None;
    }
    for k in i.saturating_sub(4)..=(i + 1).min(cx.toks.len().saturating_sub(1)) {
        if k == 0 || !(cx.punct(k - 1, b'(') || cx.punct(k - 1, b',')) || !cx.name(k) {
            continue;
        }
        // `T name =`, `ns::T name =`.
        let mut e = k;
        while cx.punct(e + 1, b':') && cx.punct(e + 2, b':') && cx.name(e + 3) {
            e += 3;
        }
        if cx.name(e + 1) && cx.punct(e + 2, b'=') && !cx.punct(e + 3, b'=') {
            return Some(vec![Edit::blank(cx.toks[k].start, cx.toks[e].end)]);
        }
    }
    None
}

/// `BENCHMARK_CAPTURE(f, name / -, …)`: an argument ending in a binary
/// operator (a token-pasted label); the operator goes.
fn dangling_operator(cx: &Cx<'_>, i: usize, _: Site) -> Option<Vec<Edit>> {
    let op = |k: usize| {
        cx.toks
            .get(k)
            .is_some_and(|t| matches!(t.kind, Kind::Punct(b'+' | b'-' | b'/' | b'%' | b'|' | b'^' | b'=' | b'!')))
    };
    for k in i.saturating_sub(3)..=(i + 3).min(cx.toks.len().saturating_sub(1)) {
        if k == 0 || !op(k) || !matches!(cx.toks[k - 1].kind, Kind::Ident | Kind::Punct(b'0' | b')')) {
            continue;
        }
        let mut e = k;
        while op(e + 1) && e < k + 2 {
            e += 1;
        }
        let both = |a: u8| cx.punct(k, a) && cx.punct(e, a) && e == k + 1;
        if both(b'+') || both(b'-') {
            continue;
        }
        if cx.punct(e + 1, b',') || cx.punct(e + 1, b')') {
            return Some(vec![Edit::blank(cx.toks[k].start, cx.toks[e].end)]);
        }
    }
    None
}

/// `typename` where an expression stands (`f(typename T::x(), 0)`).
fn typename_expr(cx: &Cx<'_>, i: usize, _: Site) -> Option<Vec<Edit>> {
    if !cx.is(i, "typename") || i == 0 {
        return None;
    }
    let p = &cx.toks[i - 1];
    let expr = matches!(p.kind, Kind::Punct(b'(' | b'[' | b',' | b'=' | b'!' | b'?' | b'+' | b'-' | b'|' | b'&'))
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
    let mut edits = vec![Edit::blank(cx.toks[i].start, cx.toks[close].end)];
    // A calling convention later in the same head (`__declspec(dllimport)
    // int __stdcall f();`) goes with it: one alone does not parse either.
    for k in close + 1..(close + 8).min(cx.toks.len()) {
        if cx.punct(k, b'(') || cx.punct(k, b';') {
            break;
        }
        if cx.word_in(k, CALLING_CONVENTIONS) {
            edits.push(Edit::blank(cx.toks[k].start, cx.toks[k].end));
        }
    }
    Some(edits)
}

/// Microsoft calling conventions.
const CALLING_CONVENTIONS: &[&str] =
    &["APIENTRY", "CALLBACK", "WINAPI", "__cdecl", "__fastcall", "__stdcall", "__thiscall", "__vectorcall"];

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
    const QUALIFIERS: &[&str] = &["override", "final", "const", "noexcept"];
    if site.in_body || i == 0 || !cx.name(i) {
        return None;
    }
    if cx.word_in(i, &["override", "final", "const", "noexcept", "throw", "try", "requires"]) {
        return None;
    }
    // After the parameters' `)`, also past `const` / `final` / `override`
    // (`void grow(size_t) final FMT_OVERRIDE;`).
    let mut close = i - 1;
    while close > 0 && cx.word_in(close, QUALIFIERS) {
        close -= 1;
    }
    if !cx.punct(close, b')') || (close + 1 < i && !cx.macro_like(i)) {
        return None;
    }
    let end = if cx.punct(i + 1, b'(') { matching_paren(cx.toks, i + 1)? } else { i };
    let next = cx.toks.get(end + 1)?;
    // `;`, a body, a default / pure specifier, the next declarator, a
    // member initializer list (`text_style(…) FMT_NOEXCEPT : a(), b() {}`).
    let init_list = cx.punct(end + 1, b':') && !cx.punct(end + 2, b':');
    let ok =
        matches!(next.kind, Kind::Punct(b';' | b'{' | b'=' | b',')) || cx.word_in(end + 1, QUALIFIERS) || init_list;
    let i = close + 1;
    let first = (i..=end).find(|&k| !cx.word_in(k, QUALIFIERS)).unwrap_or(i);
    (ok && closes_parameter_list(cx, close)).then(|| vec![Edit::blank(cx.toks[first].start, cx.toks[end].end)])
}

/// Whether the `)` at `close` closes a parameter list: a name, `)` or
/// template arguments (`digits10<int128_t>() FMT_NOEXCEPT {`) before its
/// `(`.
fn closes_parameter_list(cx: &Cx<'_>, close: usize) -> bool {
    let open = (0..close).rev().try_fold(0i32, |d, p| match cx.toks[p].kind {
        Kind::Punct(b')') => Ok(d + 1),
        Kind::Punct(b'(') if d == 0 => Err(Some(p)),
        Kind::Punct(b'(') => Ok(d - 1),
        Kind::Punct(b';' | b'{' | b'}') => Err(None),
        _ => Ok(d),
    });
    match open {
        Err(Some(o)) => o > 0 && (cx.name(o - 1) || cx.punct(o - 1, b')') || cx.punct(o - 1, b'>')),
        _ => false,
    }
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
    // Brace initializers in the arguments (`CHECKED_IF(v == std::vector<int>{1, 2}) {`).
    let close = matching_paren_braced(cx, i + 1)?;
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
    // On the next line too when an `else` follows the statement
    // (`CHECKED_IF(flag)` + newline + `return true;` + `else`).
    let before_else = || {
        let mut depth = 0i32;
        for k in close + 1..(close + 64).min(cx.toks.len()) {
            match cx.toks[k].kind {
                Kind::Punct(b'(' | b'[' | b'{') => depth += 1,
                Kind::Punct(b')' | b']' | b'}') => depth -= 1,
                Kind::Punct(b';') if depth == 0 => return cx.is(k + 1, "else"),
                _ => {}
            }
            if depth < 0 {
                return false;
            }
        }
        false
    };
    let guarded = cx.name(close + 1) || cx.word_in(close + 1, &["return", "throw", "break", "continue"]);
    if !(same_line || before_else()) || !guarded || !expression_args(cx, i + 1, close) {
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
    if site.in_body || !cx.macro_like(i) || !cx.punct(i + 1, b'(') {
        return None;
    }
    // `static MP_DEFINE_CONST_FUN_OBJ_1(f_obj, f);`: storage specifiers
    // before the macro go with it.
    let start = cx.statement_start(i);
    let head = if start < i && (start..i).all(|k| cx.word_in(k, &["static", "const", "extern", "inline"])) {
        start
    } else if cx.starts_statement(i) {
        i
    } else {
        return None;
    };
    let close = matching_paren_braced(cx, i + 1)?;
    // Also without `;` when the next declaration starts on a later line
    // (`NLOHMANN_JSON_SERIALIZE_ENUM(T, { … })`).
    let next = cx.toks.get(close + 1);
    let alone = next.is_none_or(|n| {
        n.line > cx.toks[close].line && matches!(n.kind, Kind::Ident | Kind::Directive | Kind::Punct(b'}'))
    });
    (cx.punct(close + 1, b';') || alone).then(|| vec![Edit::blank(cx.toks[head].start, cx.toks[close].end)])
}

/// Attribute macros around a declaration (`__ALIGN_BEGIN static uint8_t
/// buf[N] __ALIGN_END;`, `size_t n JEMALLOC_CC_SILENCE_INIT(0);`, `const
/// T t[N] BSP_PLACE_IN_SECTION(".v") = {…}`): every one of the statement
/// goes in one edit, since blanking one alone may not lower the error
/// count. A leading macro must be followed by a keyword or by a type and
/// a name; a trailing one must follow the declared name (or its `[…]`)
/// and precede `;`, `=`, `{`, `,` or another macro.
fn declaration_attributes(cx: &Cx<'_>, i: usize, _: Site) -> Option<Vec<Edit>> {
    let start = cx.statement_start(i);
    let mut edits = Vec::new();
    let k = leading_attributes(cx, start, &mut edits);
    trailing_attributes(cx, start, k, &mut edits)?;
    (!edits.is_empty()).then_some(edits)
}

/// The last token of the macro at `k`: its `)` when it has arguments.
fn macro_span(cx: &Cx<'_>, k: usize) -> usize {
    if cx.punct(k + 1, b'(') { matching_paren(cx.toks, k + 1).unwrap_or(k) } else { k }
}

/// Blanks the leading attribute macros of the declaration at `start`;
/// returns the token after them.
fn leading_attributes(cx: &Cx<'_>, start: usize, edits: &mut Vec<Edit>) -> usize {
    let mut k = start;
    while cx.macro_like(k) {
        let e = macro_span(cx, k);
        let next_kw = cx.word_in(e + 1, DECL_WORDS) || cx.word_in(e + 1, TYPE_WORDS);
        let type_and_name = cx.name(e + 1) && (cx.name(e + 2) || cx.punct(e + 2, b'*'));
        if !(next_kw || type_and_name) {
            break;
        }
        edits.push(Edit::blank(cx.toks[k].start, cx.toks[e].end));
        k = e + 1;
    }
    k
}

/// Blanks the trailing attribute macros: after the declared name, up to
/// the end of the head. `None` on a function declarator (not this rule's
/// shape).
fn trailing_attributes(cx: &Cx<'_>, start: usize, mut k: usize, edits: &mut Vec<Edit>) -> Option<()> {
    let mut depth = 0i32;
    let mut names = 0;
    while k < cx.toks.len() && k < start + 64 {
        match cx.toks[k].kind {
            Kind::Punct(b';' | b'=' | b'{') if depth == 0 => break,
            Kind::Punct(b'[') => depth += 1,
            Kind::Punct(b']') => depth -= 1,
            Kind::Punct(b'(' | b')') if depth == 0 && !(k > 0 && cx.macro_like(k - 1)) => return None,
            Kind::Ident if depth == 0 && names >= 2 && cx.macro_like(k) => {
                if let Some(e) = trailing_attribute_end(cx, k) {
                    edits.push(Edit::blank(cx.toks[k].start, cx.toks[e].end));
                    k = e + 1;
                    continue;
                }
            }
            Kind::Ident if depth == 0 => names += 1,
            _ => {}
        }
        k += 1;
    }
    Some(())
}

/// The last token of the trailing attribute macro at `k`, when it follows
/// the declared name (or its `[…]`) and precedes `;`, `=`, `{`, `,` or
/// another macro.
fn trailing_attribute_end(cx: &Cx<'_>, k: usize) -> Option<usize> {
    let after_name = cx.name(k - 1) || cx.punct(k - 1, b']');
    let e = macro_span(cx, k);
    let follows = matches!(cx.toks.get(e + 1).map(|t| t.kind), Some(Kind::Punct(b';' | b'=' | b'{' | b',')))
        || cx.macro_like(e + 1);
    (after_name && follows).then_some(e)
}

/// `MP_STATE_PORT(h)[i] = c;` first in a block: the grammar reads a
/// macro type and an unnamed array declarator. A `*` in the space before
/// the call makes it an expression (the call and its arguments stay).
fn subscripted_call(cx: &Cx<'_>, i: usize, site: Site) -> Option<Vec<Edit>> {
    if !site.in_body {
        return None;
    }
    let k = (i.saturating_sub(1)..=i + 2).find(|&k| {
        cx.name(k)
            && cx.punct(k + 1, b'(')
            && matching_paren(cx.toks, k + 1).is_some_and(|c| cx.punct(c + 1, b'['))
            && cx.starts_statement(k)
    })?;
    let at = cx.toks[k].start;
    (at > 0 && matches!(cx.src[at - 1], b' ' | b'\t')).then(|| vec![Edit { start: at - 1, end: at, with: b"*" }])
}

/// An unnamed pointer parameter with a default: in a template head
/// (`template <typename R, R* = nullptr>`) the `*` goes (`R = nullptr`); in
/// a function's parameters (`typename enable_if<C>::type* = 0`, `const
/// void* /*hint*/ = 0`), which the grammar takes only with a declarator, the
/// space after the `*` becomes a name (`T*_= 0`).
fn unnamed_pointer_parameter(cx: &Cx<'_>, i: usize, _: Site) -> Option<Vec<Edit>> {
    let star = (i.saturating_sub(3)..=i + 1).find(|&k| {
        cx.punct(k, b'*')
            && cx.punct(k + 1, b'=')
            && !cx.punct(k + 2, b'=')
            && k > 0
            && (cx.name(k - 1) || cx.punct(k - 1, b'>') || cx.word_in(k - 1, TYPE_WORDS))
    })?;
    // Inside a template head (an unclosed `<` after `template`) or a
    // parameter list (an unclosed `(`) before it.
    let mut depth = 0i32;
    for p in (star.saturating_sub(128)..star).rev() {
        match cx.toks[p].kind {
            Kind::Punct(b'>' | b')') => depth += 1,
            Kind::Punct(b'<' | b'(') if depth > 0 => depth -= 1,
            Kind::Punct(b'<') => {
                return (p > 0 && cx.is(p - 1, "template"))
                    .then(|| vec![Edit::blank(cx.toks[star].start, cx.toks[star].end)]);
            }
            Kind::Punct(b'(') => {
                let x = cx.toks[star].end;
                let declarator = p > 0 && (cx.name(p - 1) || cx.punct(p - 1, b'>'));
                return (declarator && matches!(cx.src.get(x), Some(b' ' | b'\t')))
                    .then(|| vec![Edit { start: x, end: x + 1, with: b"_" }]);
            }
            Kind::Punct(b';' | b'{' | b'}') => return None,
            _ => {}
        }
    }
    None
}

/// `using testing::GMOCK_FLAG(verbose);`: a macro naming the declared
/// entity loses its name and parentheses.
fn using_macro(cx: &Cx<'_>, i: usize, _: Site) -> Option<Vec<Edit>> {
    let start = cx.statement_start(i);
    if !cx.is(start, "using") {
        return None;
    }
    let m = (start + 1..(start + 16).min(cx.toks.len())).find(|&k| cx.macro_like(k) && cx.punct(k + 1, b'('))?;
    let close = matching_paren(cx.toks, m + 1)?;
    (cx.punct(close + 1, b';') && close == m + 3).then(|| {
        vec![Edit::blank(cx.toks[m].start, cx.toks[m + 1].end), Edit::blank(cx.toks[close].start, cx.toks[close].end)]
    })
}

/// `BEST_TIME("x", f(), size, , repeat)`: an empty macro argument gets a
/// `0` in its space.
fn empty_argument(cx: &Cx<'_>, i: usize, _: Site) -> Option<Vec<Edit>> {
    let k = (i.saturating_sub(3)..=i + 3).find(|&k| {
        (cx.punct(k, b',') || cx.punct(k, b'('))
            && cx.punct(k + 1, b',')
            && cx.src[cx.toks[k].end..cx.toks[k + 1].start].iter().any(|c| matches!(c, b' ' | b'\t'))
    })?;
    let at =
        cx.toks[k].end + cx.src[cx.toks[k].end..cx.toks[k + 1].start].iter().position(|c| matches!(c, b' ' | b'\t'))?;
    Some(vec![Edit { start: at, end: at + 1, with: b"0" }])
}

/// `really_inline ~stream();`, `really_inline bool operator!=(…)`: a
/// lower-case specifier macro (with `_`, not a `…_t` type) before a
/// destructor or an operator goes.
fn specifier_before_operator(cx: &Cx<'_>, i: usize, site: Site) -> Option<Vec<Edit>> {
    if site.in_body {
        return None;
    }
    let s = cx.statement_start(i);
    let w = cx.toks.get(s).map(|t| &cx.src[t.start..t.end])?;
    let lower = w.iter().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_')
        && w.contains(&b'_')
        && !w.ends_with(b"_t");
    if !lower || !cx.name(s) {
        return None;
    }
    // `~name(`, `operator…(`, or a type (with template arguments,
    // qualifiers and `*` / `&`) then `operator` or a qualified name ending
    // in `operator`.
    if cx.punct(s + 1, b'~') || cx.is(s + 1, "operator") {
        return Some(vec![Edit::blank(cx.toks[s].start, cx.toks[s].end)]);
    }
    let mut depth = 0i32;
    for k in s + 1..(s + 48).min(cx.toks.len()) {
        match cx.toks[k].kind {
            Kind::Punct(b'<') => depth += 1,
            Kind::Punct(b'>') => depth -= 1,
            Kind::Punct(b'(' | b';' | b'{' | b'=') if depth == 0 => return None,
            Kind::Ident if depth == 0 && cx.is(k, "operator") => {
                return Some(vec![Edit::blank(cx.toks[s].start, cx.toks[s].end)]);
            }
            _ => {}
        }
    }
    None
}

/// `EXPECT_NO_FATAL_FAILURE(;)`: an empty statement as an argument goes.
fn empty_statement_argument(cx: &Cx<'_>, i: usize, _: Site) -> Option<Vec<Edit>> {
    let k = (i.saturating_sub(2)..=i + 2)
        .find(|&k| cx.punct(k, b';') && k > 0 && cx.punct(k - 1, b'(') && cx.punct(k + 1, b')'))?;
    Some(vec![Edit::blank(cx.toks[k].start, cx.toks[k].end)])
}

/// `register uint32_t r __asm__("r0") = v;`: a register variable's asm
/// label goes.
fn asm_label(cx: &Cx<'_>, i: usize, _: Site) -> Option<Vec<Edit>> {
    let k = (cx.statement_start(i)..=i + 1).find(|&k| {
        cx.ident(k)
            && matches!(cx.text(k), b"__asm__" | b"__asm" | b"asm" | b"__ASM")
            && cx.punct(k + 1, b'(')
            && k > 0
            && cx.name(k - 1)
    })?;
    let close = matching_paren(cx.toks, k + 1)?;
    (cx.punct(close + 1, b'=') || cx.punct(close + 1, b';'))
        .then(|| vec![Edit::blank(cx.toks[k].start, cx.toks[close].end)])
}

/// `__ASM volatile ("…")`, `__ASM("r0")`: a macro for the `asm` keyword.
fn asm_macro(cx: &Cx<'_>, i: usize, _: Site) -> Option<Vec<Edit>> {
    let k = (cx.statement_start(i)..=i + 1).find(|&k| {
        cx.ident(k)
            && matches!(cx.text(k), b"__ASM" | b"__asm_" | b"ASM" | b"__ASM__")
            && (cx.punct(k + 1, b'(') || cx.is(k + 1, "volatile"))
    })?;
    Some(vec![Edit { start: cx.toks[k].start, end: cx.toks[k].end, with: b"asm" }])
}

/// `LUAI_DDEC(const lu_byte luai_ctype_[N];)`, `LUAI_TRY(L, c, f(););`:
/// a macro wrapping whole declarations or statements loses its name and
/// parentheses (what remains is a declaration, or a comma expression and
/// an empty statement).
fn wrapper_macro(cx: &Cx<'_>, i: usize, _: Site) -> Option<Vec<Edit>> {
    let i = cx.statement_start(i);
    if !cx.macro_like(i) || !cx.punct(i + 1, b'(') {
        return None;
    }
    // The matching `)`, across the `;` inside (`matching_paren` stops there).
    let mut depth = 0i32;
    let close = (i + 1..cx.toks.len().min(i + 256)).find(|&k| {
        match cx.toks[k].kind {
            Kind::Punct(b'(') => depth += 1,
            Kind::Punct(b')') => depth -= 1,
            _ => {}
        }
        depth == 0
    })?;
    (close > i + 3 && cx.punct(close - 1, b';')).then(|| {
        vec![Edit::blank(cx.toks[i].start, cx.toks[i + 1].end), Edit::blank(cx.toks[close].start, cx.toks[close].end)]
    })
}

/// `LUA_API lua_State *(lua_newstate) (…);`: parentheses around a declared
/// name go (`(*fp)(…)` keeps them: a function pointer).
fn parenthesized_name(cx: &Cx<'_>, i: usize, site: Site) -> Option<Vec<Edit>> {
    if site.in_body {
        return None;
    }
    let open = (cx.statement_start(i)..=i + 1).find(|&k| {
        cx.punct(k, b'(')
            && cx.name(k + 1)
            && cx.punct(k + 2, b')')
            && cx.punct(k + 3, b'(')
            && k > 0
            && !cx.name(k - 1)
    })?;
    Some(vec![
        Edit::blank(cx.toks[open].start, cx.toks[open].end),
        Edit::blank(cx.toks[open + 2].start, cx.toks[open + 2].end),
    ])
}

/// `intop(+, a, b)`: an operator as a whole macro argument becomes `0`.
fn operator_argument(cx: &Cx<'_>, i: usize, _: Site) -> Option<Vec<Edit>> {
    let op = |k: usize| {
        cx.toks.get(k).is_some_and(|t| {
            matches!(
                t.kind,
                Kind::Punct(b'+' | b'-' | b'*' | b'/' | b'%' | b'&' | b'|' | b'^' | b'<' | b'>' | b'=' | b'!')
            )
        })
    };
    for k in i.saturating_sub(3)..=(i + 3).min(cx.toks.len().saturating_sub(1)) {
        if k == 0 || !(cx.punct(k - 1, b'(') || cx.punct(k - 1, b',')) || !op(k) {
            continue;
        }
        let mut e = k;
        while op(e + 1) && e < k + 2 {
            e += 1;
        }
        if cx.punct(e + 1, b',') || cx.punct(e + 1, b')') {
            return Some(vec![Edit { start: cx.toks[k].start, end: cx.toks[e].end, with: b"0" }]);
        }
    }
    None
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
    let tagged = cx.is(e, "template") && (cx.is(e + 1, "class") || cx.is(e + 1, "struct"));
    if tagged && cx.punct(e + 3, b'<') {
        return Some(vec![Edit::blank(cx.toks[e].start, cx.toks[e].end)]);
    }
    // `template struct detail::basic_data<void>;`: an explicit
    // instantiation of a qualified name (no forward declaration reads it)
    // defines nothing and goes, up to its `;`.
    let mut k = e + 2;
    while tagged && cx.name(k) && cx.punct(k + 1, b':') && cx.punct(k + 2, b':') {
        k += 3;
    }
    if tagged && k > e + 2 && cx.name(k) && cx.punct(k + 1, b'<') {
        let semi = (k + 2..(k + 64).min(cx.toks.len())).find(|&x| cx.punct(x, b';'))?;
        return Some(vec![Edit::blank(cx.toks[e].start, cx.toks[semi - 1].end)]);
    }
    None
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
    // Arguments that are no expressions (`FMT_MSC_WARNING(suppress : 6102)`,
    // a lone `:`) go with the call.
    let colon = (i + 2..close).any(|k| {
        cx.punct(k, b':') && !cx.punct(k + 1, b':') && !cx.punct(k - 1, b':') && !(i + 2..k).any(|q| cx.punct(q, b'?'))
    });
    if !colon && let Some(at) = cx.space_after(close) {
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
    // declared name; a template head is not part of it.
    let start = skip_template_heads(cx, head_start(cx, i))?;
    let (k, idents) = walk_head(cx, start)?;
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
    let trailing = constructor_attributes(cx, k);
    let mut blank = unknown_specifiers(cx, &idents[..idents.len() - 1], name, trailing.is_some());
    blank.retain(|&x| x != name);
    if blank.is_empty() {
        return None;
    }
    blank.extend(trailing.unwrap_or_default());
    // A macro with arguments goes with them.
    Some(blank.into_iter().map(|x| Edit::blank(cx.toks[x].start, cx.toks[macro_span(cx, x)].end)).collect())
}

/// The start of the declaration head containing token `i`: the statement
/// start or, inside a parameter list, the token after the nearest `(` /
/// `,` at depth 0.
fn head_start(cx: &Cx<'_>, i: usize) -> usize {
    let s = cx.statement_start(i);
    let mut depth = 0i32;
    for p in (s..i).rev() {
        match cx.toks[p].kind {
            Kind::Punct(b')') => depth += 1,
            Kind::Punct(b'(' | b',') if depth == 0 => return p + 1,
            Kind::Punct(b'(') => depth -= 1,
            _ => {}
        }
    }
    s
}

/// The index after the `template <…>` heads at `start`; `None` when one
/// is cut by a `;`, `{` or `}`.
fn skip_template_heads(cx: &Cx<'_>, mut start: usize) -> Option<usize> {
    while cx.is(start, "template") && cx.punct(start + 1, b'<') {
        start = angle_end(cx, start + 1, start + 256)?;
    }
    Some(start)
}

/// The index after the `>` closing the `<` at `open` (scanning below
/// `limit`); `None` at a `;`, `{` or `}`.
fn angle_end(cx: &Cx<'_>, open: usize, limit: usize) -> Option<usize> {
    let mut d = 0i32;
    let mut e = open;
    while e < cx.toks.len().min(limit) {
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
    Some(e)
}

/// Walks a declaration head from `start` (identifiers, `*`, `&`, `::`
/// chains, template arguments): where it stops and its identifiers.
/// `None` at a statement keyword or an `operator`.
fn walk_head(cx: &Cx<'_>, start: usize) -> Option<(usize, Vec<usize>)> {
    let mut k = start;
    let mut idents: Vec<usize> = Vec::new();
    while k < cx.toks.len() && k < start + 32 {
        match cx.toks[k].kind {
            Kind::Ident if cx.word_in(k, STATEMENT_WORDS) || cx.is(k, "operator") => return None,
            Kind::Ident => {
                idents.push(k);
                k += 1;
            }
            Kind::Punct(b'*' | b'&') => k += 1,
            // `extern "C"`.
            Kind::Literal if k > 0 && cx.is(k - 1, "extern") => k += 1,
            Kind::Punct(b':') if cx.punct(k + 1, b':') => k += 2,
            Kind::Punct(b'<') => k = angle_end(cx, k, usize::MAX)?,
            _ => break,
        }
    }
    Some((k, idents))
}

/// `MACRO Name(params) : init(…)` with the `(` at `k`: only a constructor
/// takes a member initializer list, so it has no return type. The
/// attribute macros between the parameters and the `:`
/// (`FMT_CONSTEXPR text_style(…) FMT_NOEXCEPT : a() {}`), which go too.
fn constructor_attributes(cx: &Cx<'_>, k: usize) -> Option<Vec<usize>> {
    if !cx.punct(k, b'(') {
        return None;
    }
    let mut e = matching_paren_braced(cx, k)? + 1;
    let mut trailing = Vec::new();
    while cx.word_in(e, &["noexcept", "const", "override"]) || cx.macro_like(e) {
        if cx.macro_like(e) {
            trailing.push(e);
        }
        e += 1;
    }
    (cx.punct(e, b':') && !cx.punct(e + 1, b':')).then_some(trailing)
}

/// The identifiers of the head before the declared `name` that are
/// unknown specifiers (`name` itself may be among them).
fn unknown_specifiers(cx: &Cx<'_>, head: &[usize], name: usize, constructor: bool) -> Vec<usize> {
    // The declarator's qualifier (`Type document::parser::name(`) is part
    // of the name, not a qualified type.
    let mut qualifier_start = name;
    while qualifier_start >= 3
        && cx.punct(qualifier_start - 1, b':')
        && cx.punct(qualifier_start - 2, b':')
        && cx.name(qualifier_start - 3)
    {
        qualifier_start -= 3;
    }
    let before: Vec<usize> = head.iter().copied().filter(|&x| x < qualifier_start).collect();
    let has_type_word = before.iter().any(|&x| cx.word_in(x, TYPE_WORDS));
    let explicit = before.iter().any(|&x| cx.is(x, "explicit"));
    // Tags: `struct X` names a type.
    let tag_names: Vec<usize> = before
        .iter()
        .filter(|&&x| cx.word_in(x, &["struct", "union", "enum", "class", "typename"]))
        .map(|&x| x + 1)
        .collect();
    let chained_type = before.iter().any(|&x| chain_member(cx, x));
    let candidates: Vec<usize> =
        before.iter().copied().filter(|&x| cx.name(x) && !chain_member(cx, x) && !tag_names.contains(&x)).collect();
    if has_type_word || !tag_names.is_empty() || explicit || constructor || chained_type {
        // `int CJSON_CDECL main`, `const void __unused *p` with a keyword
        // type, `explicit FMT_CONSTEXPR name(…)`, `MACRO ns::T f()`: no
        // other name is a type.
        return candidates;
    }
    // `static redisAtomic size_t n`, `CFSocketRef __unused s`: the type is
    // the last name that does not look like an attribute macro.
    let attribute_like = |x: usize| cx.text(x).starts_with(b"__") || cx.macro_like(x);
    let ty = candidates.iter().rev().find(|&&x| !attribute_like(x)).or(candidates.last()).copied();
    candidates.into_iter().filter(|&x| Some(x) != ty).collect()
}

/// Whether identifier `x` is a member of a qualified chain (`ns::Type`,
/// `parse_context<Char>::`): never an unknown specifier.
fn chain_member(cx: &Cx<'_>, x: usize) -> bool {
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
        || (i > 0 && cx.punct(i - 1, b',') && next_operand)
        // `FMT_POSIX_CALL(fileno FMT_ARGS(file_))`: a macro between a name
        // and its argument list inside parentheses.
        || (prev_operand && cx.punct(i + 1, b'(') && i >= 2 && cx.punct(i - 2, b'('));
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
    fn macro_wrapped_declarations_and_arguments() {
        // A storage class before a declaration macro goes with it.
        assert_eq!(
            fix("static MP_DEFINE_CONST_FUN_OBJ_1(f_obj, f);\n", "MP_", false),
            format!("{};\n", " ".repeat(42))
        );
        // A macro wrapping a declaration.
        assert_eq!(fix("LUAI_DDEC(const lu_byte t[N];)\n", "t[", false), "          const lu_byte t[N]; \n");
        // An operator as an argument.
        assert_eq!(fix("x = intop(+, a, 1);", "+", true), "x = intop(0, a, 1);");
        assert_eq!(fix("x = intop(>>, a, 1);", ">>", true), "x = intop(0 , a, 1);");
        // A qualified reference type as an argument.
        assert_eq!(
            fix("CHECK_THROWS_AS(f(), json::type_error&);", "&", true),
            "CHECK_THROWS_AS(f(), json::type_error );"
        );
        // Attribute macros around a declaration, all at once.
        assert_eq!(
            fix("__ALIGN_BEGIN static uint8_t buf[N] __ALIGN_END;", "static", false),
            "              static uint8_t buf[N]            ;"
        );
        let src = "  size_t n JEMALLOC_CC_SILENCE_INIT(0);";
        assert_eq!(fix(src, "JEM", true), blanked(src, "JEMALLOC_CC_SILENCE_INIT(0)"));
        assert_eq!(fix("  __ASM volatile (\"nop\");", "volatile", true), "  asm   volatile (\"nop\");");
        assert_eq!(fix("{\n    S(h)[i] = c;", "{", true), "{\n   *S(h)[i] = c;");
        assert_eq!(fix("template <typename R, R* = nullptr>", "*", false), "template <typename R, R  = nullptr>");
        assert_eq!(fix("  T(\"x\", f(), n, , r);", ", ,", true), "  T(\"x\", f(), n,0, r);");
        assert_eq!(fix("really_inline ~stream();", "~", false), "              ~stream();");
        assert_eq!(
            fix("really_inline simd8<T> operator|(T o) const;", "simd8", false),
            "              simd8<T> operator|(T o) const;"
        );
        assert_eq!(fix("CHECK_AS(s >> j, json::error const&);", "&", true), "CHECK_AS(s >> j, json::error       );");
        assert_eq!(fix("CHECK_AS(f({1, 2}), json::error&);", "&", true), "CHECK_AS(f({1, 2}), json::error );");
        assert_eq!(fix("using testing::GMOCK_FLAG(verbose);", "GMOCK", false), "using testing::           verbose ;");
        assert_eq!(fix("  X(;);", ";", true), "  X( );");
        let src = "  register int r __asm__ (\"r0\") = v;";
        assert_eq!(fix(src, "__asm__", true), blanked(src, "__asm__ (\"r0\")"));
        assert_eq!(fix("  SWAP(const byte *, a, b);", "*", true), "  SWAP(const byte  , a, b);");
        assert_eq!(fix("  TRY(L, c,\n    (*f)(L);\n  );", "(*f)", true), "      L, c,\n    (*f)(L);\n   ;");
        // An access macro in a class body.
        assert_eq!(
            fix("class A {\n  JSON_PRIVATE_UNLESS_TESTED:\n  int x;\n};", "JSON", false),
            "class A {\n  public                    :\n  int x;\n};"
        );
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

    #[test]
    fn library_and_test_shapes() {
        // An unnamed pointer parameter with a default gets a name.
        assert_eq!(
            fix("void f(const T&, typename E<T>::type* = 0) {}", "= 0", false),
            "void f(const T&, typename E<T>::type*_= 0) {}"
        );
        assert_eq!(fix("void f(const void* /*h*/ = 0) {}", "= 0", false), "void f(const void*_/*h*/ = 0) {}");
        // `->` and `<<` inside `decltype(a, b)`.
        assert_eq!(fix("auto f() -> decltype(p->g(), int());", ",", false), "auto f() -> decltype(p->g()| int());");
        assert_eq!(fix("auto f() -> decltype(s << v, T());", ",", false), "auto f() -> decltype(s << v| T());");
        // An array of pointers after `new`.
        assert_eq!(fix("  p = new char *[n];", "*", true), "  p = new char  [n];");
        assert_eq!(fix("  r(new (std::nothrow) void *[n]);", "*", true), "  r(new (std::nothrow) void  [n]);");
        // A declaration as a macro argument.
        assert_eq!(
            fix("  EXPECT_THROW(string s = format(\"{}\"), E);", "s =", true),
            "  EXPECT_THROW(       s = format(\"{}\"), E);"
        );
        // An argument ending in an operator.
        assert_eq!(fix("B(Dump, a / -, 1);", "-", false), "B(Dump, a    , 1);");
        // A trailing macro after a template-id declarator, a qualifier,
        // or before a member initializer list.
        assert_eq!(
            fix("template <> int d<int128_t>() FMT_NOEXCEPT { return 38; }", "FMT", false),
            "template <> int d<int128_t>()              { return 38; }"
        );
        assert_eq!(fix("void grow(size_t) final FMT_OVERRIDE;", "FMT", false), "void grow(size_t) final             ;");
        // A calling convention with `__declspec`.
        assert_eq!(
            fix("__declspec(dllimport) int __stdcall f();", "__declspec", false),
            "                      int           f();"
        );
        // A qualified explicit instantiation.
        assert_eq!(fix("template struct d::data<void>;", "template", false), format!("{};", " ".repeat(29)));
        // A member pointer type as a template argument.
        assert_eq!(fix("struct S<R C::*> {};", "C::", false), "struct S<R    *> {};");
        // A declaration macro without `;` before the next declaration.
        assert_eq!(fix("ENUM_MAP(T, {\n {a, 1}\n})\nint x;", "ENUM", false), "             \n       \n  \nint x;");
        // A statement macro whose arguments are no expression.
        assert_eq!(fix("  W(suppress : 6102)\n  return 1;", "W(", true), format!("  ;{}\n  return 1;", " ".repeat(17)));
        // An `if`-like macro on its own line before `else`.
        assert_eq!(
            fix("  CHECKED_IF(flag)\n    return true;\n  else\n    return false;", "CHECKED", true),
            "  if        (flag)\n    return true;\n  else\n    return false;"
        );
        // A macro between a function name and its arguments.
        assert_eq!(fix("  n = CALL(fileno ARGS(f));", "ARGS", true), "  n = CALL(fileno     (f));");
        // A lower-case specifier before a constructor with an attribute
        // macro before its initializer list.
        assert_eq!(
            fix("  really_inline S(int a) NOEXCEPT : a_(a) {}", "really", false),
            "                S(int a)          : a_(a) {}"
        );
    }
}
