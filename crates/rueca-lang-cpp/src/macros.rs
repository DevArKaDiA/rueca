//! Calls written in a macro's replacement list, read lexically.
//!
//! The grammar keeps a `#define` body as raw text (`preproc_arg`), so the
//! calls a macro expands to are found by a small lexer: an identifier, or a
//! `::`-qualified chain of them, followed by `(`, that is not a parameter of
//! the macro, a keyword, a member call (`x.f(`, `p->f(`), a stringized or
//! pasted token (`#x`, `a ## b(`). The pack reports them as calls made by
//! the macro's definition; the resolver links each caller of the macro to
//! them (`rueca-resolve`, *Macro expansion*), which is what a compiler sees
//! after preprocessing: `#define lua_pop(L,n) lua_settop(L, -(n)-1)` makes
//! every `lua_pop(L, 1)` a call of `lua_settop`.

/// A call in a macro body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BodyCall {
    /// Byte range of the called name in the body.
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) name: String,
    /// Qualifier segments of `a::b::f(` (`a::b`), empty when unqualified.
    pub(crate) qualifier: String,
    /// Written with a leading `::`.
    pub(crate) global: bool,
    /// Number of top-level arguments.
    pub(crate) arity: u32,
}

/// At most this many calls per macro body.
const MAX_CALLS: usize = 64;

/// Words followed by `(` that are not calls of a definition.
const NOT_CALLS: &[&str] = &[
    "_Alignof",
    "_Generic",
    "_Pragma",
    "_Static_assert",
    "__attribute__",
    "__builtin_expect",
    "__declspec",
    "__extension__",
    "__typeof__",
    "alignas",
    "alignof",
    "case",
    "catch",
    "const_cast",
    "decltype",
    "defined",
    "delete",
    "do",
    "dynamic_cast",
    "else",
    "for",
    "if",
    "new",
    "noexcept",
    "operator",
    "reinterpret_cast",
    "return",
    "sizeof",
    "static_assert",
    "static_cast",
    "switch",
    "throw",
    "typeid",
    "typeof",
    "while",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum T {
    Ident,
    /// `::`
    Scope,
    /// `.` or `->`
    Member,
    /// `#` or `##`
    Hash,
    Open,
    Close,
    Comma,
    Other,
}

/// Tokens of a macro body: (kind, start, end). Comments, string and
/// character literals and line continuations are skipped.
fn tokens(b: &[u8]) -> Vec<(T, usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let (tok, next) = token_at(b, i);
        out.extend(tok);
        i = next;
    }
    out
}

/// The token at `i` of a macro body (`None` for a comment, whitespace or
/// a continuation) and where the next one starts. A literal or a number
/// is an empty [`T::Other`] at its end.
fn token_at(b: &[u8], i: usize) -> (Option<(T, usize, usize)>, usize) {
    let c = b[i];
    match (c, b.get(i + 1)) {
        (b'/', Some(b'*')) => (None, block_comment_end(b, i)),
        (b'/', Some(b'/')) => (None, run_end(b, i, |x| x != b'\n')),
        (b'"' | b'\'', _) => {
            let end = literal_end(b, i);
            (Some((T::Other, end, end)), end)
        }
        _ if c.is_ascii_alphabetic() || c == b'_' => {
            let end = run_end(b, i, |x| x.is_ascii_alphanumeric() || x == b'_');
            (Some((T::Ident, i, end)), end)
        }
        // Numbers, with suffixes and exponents (`1e-3f`, `0x1p3`).
        _ if c.is_ascii_digit() => {
            let end = run_end(b, i, |x| x.is_ascii_alphanumeric() || x == b'_' || x == b'.');
            (Some((T::Other, end, end)), end)
        }
        (b':', Some(b':')) => (Some((T::Scope, i, i + 2)), i + 2),
        (b'-', Some(b'>')) => (Some((T::Member, i, i + 2)), i + 2),
        (b'#', Some(b'#')) => (Some((T::Hash, i, i + 2)), i + 2),
        _ if c.is_ascii_whitespace() || c == b'\\' => (None, i + 1),
        _ => (Some((single_token(c), i, i + 1)), i + 1),
    }
}

/// The kind of a one-byte token.
fn single_token(c: u8) -> T {
    match c {
        b'.' => T::Member,
        b'#' => T::Hash,
        b'(' => T::Open,
        b')' => T::Close,
        b',' => T::Comma,
        _ => T::Other,
    }
}

/// The end of the run of bytes from `i` that satisfy `keep`.
fn run_end(b: &[u8], i: usize, keep: impl Fn(u8) -> bool) -> usize {
    i + b[i..].iter().take_while(|x| keep(**x)).count()
}

/// The index after the block comment opening at `i` (may pass the end of
/// an unterminated one).
fn block_comment_end(b: &[u8], mut i: usize) -> usize {
    i += 2;
    while i < b.len() && !(b[i] == b'*' && b.get(i + 1) == Some(&b'/')) {
        i += 1;
    }
    i + 2
}

/// The index after the string or character literal opening at `i` (at
/// the end of the line for an unterminated one).
fn literal_end(b: &[u8], mut i: usize) -> usize {
    let quote = b[i];
    i += 1;
    while i < b.len() && b[i] != quote && b[i] != b'\n' {
        i += if b[i] == b'\\' { 2 } else { 1 };
    }
    i + 1
}

/// Calls in `body` (the replacement list of a macro whose parameters are
/// `params`).
pub(crate) fn body_calls(body: &[u8], params: &[String]) -> Vec<BodyCall> {
    let toks = tokens(body);
    let word = |k: usize| std::str::from_utf8(&body[toks[k].1..toks[k].2]).unwrap_or("");
    let mut out = Vec::new();
    let mut k = 0;
    while k < toks.len() && out.len() < MAX_CALLS {
        let start = k;
        let Some((global, segs)) = name_chain(&toks, k) else {
            k += 1;
            continue;
        };
        let j = segs[segs.len() - 1];
        let next = j + 1;
        k = next;
        if !called(&toks, start, next) {
            continue;
        }
        let name = word(j);
        let first = word(segs[0]);
        if NOT_CALLS.contains(&name)
            || params.iter().any(|p| p == first)
            || first == "__VA_ARGS__"
            || name.starts_with("__builtin_")
        {
            continue;
        }
        let qualifier = segs[..segs.len() - 1].iter().map(|&s| word(s)).collect::<Vec<_>>().join("::");
        out.push(BodyCall {
            start: toks[j].1,
            end: toks[j].2,
            name: name.to_string(),
            qualifier,
            global,
            arity: arity(&toks[next..]),
        });
    }
    out
}

/// The name chain starting at token `k` (`a`, `::a`, `a::b::c`): whether
/// it is global and the indexes of its names.
fn name_chain(toks: &[(T, usize, usize)], k: usize) -> Option<(bool, Vec<usize>)> {
    let global = toks[k].0 == T::Scope && toks.get(k + 1).is_some_and(|t| t.0 == T::Ident);
    let mut j = if global { k + 1 } else { k };
    if toks[j].0 != T::Ident {
        return None;
    }
    let mut segs = vec![j];
    while toks.get(j + 1).is_some_and(|t| t.0 == T::Scope) && toks.get(j + 2).is_some_and(|t| t.0 == T::Ident) {
        j += 2;
        segs.push(j);
    }
    Some((global, segs))
}

/// Whether the name chain `start..next` is called: followed by `(`, and
/// not a member call (`x.f(`, `p->f(`) or a stringized or pasted token
/// (`#x`, `a ## b(`).
fn called(toks: &[(T, usize, usize)], start: usize, next: usize) -> bool {
    if toks.get(next).is_none_or(|t| t.0 != T::Open) {
        return false;
    }
    let prev = start.checked_sub(1).map(|p| toks[p].0);
    !matches!(prev, Some(T::Member | T::Hash)) && toks.get(next + 1).is_none_or(|t| t.0 != T::Hash)
}

/// Number of top-level arguments of the call whose `(` starts `toks`
/// (commas up to the matching `)`).
fn arity(toks: &[(T, usize, usize)]) -> u32 {
    let (mut depth, mut commas, mut any) = (0i32, 0u32, false);
    for t in toks {
        match t.0 {
            T::Open => depth += 1,
            T::Close => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            T::Comma if depth == 1 => commas += 1,
            _ if depth >= 1 => any = true,
            _ => {}
        }
    }
    if any || commas > 0 { commas + 1 } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(body: &str, params: &[&str]) -> Vec<(String, String, bool, u32)> {
        let params: Vec<String> = params.iter().map(|p| p.to_string()).collect();
        body_calls(body.as_bytes(), &params).into_iter().map(|c| (c.qualifier, c.name, c.global, c.arity)).collect()
    }

    #[test]
    fn calls_in_bodies() {
        let n = |q: &str, f: &str, g: bool, a: u32| (q.to_string(), f.to_string(), g, a);
        assert_eq!(names("((int)luaL_checkinteger(L, (n)))", &["L", "n"]), vec![n("", "luaL_checkinteger", false, 2)]);
        assert_eq!(names("lua_settop(L, -(n)-1)", &["L", "n"]), vec![n("", "lua_settop", false, 2)]);
        assert_eq!(
            names("((c) ? (void)0 : ::fmt::assert_fail(__FILE__, __LINE__, (m)))", &["c", "m"]),
            vec![n("fmt", "assert_fail", true, 3)]
        );
        assert_eq!(
            names("fmt::detail::ignore_unused((c), (m))", &["c", "m"]),
            vec![n("fmt::detail", "ignore_unused", false, 2)]
        );
        // Parameters, members, keywords, pasting, strings and comments.
        assert!(names("f(x) + x->g(1) + sizeof(int) + a##b(1) + #s", &["f", "x", "s", "a", "b"]).is_empty());
        assert!(names("\"call(me)\" /* h(1) */ // k(2)", &[]).is_empty());
        assert_eq!(names("{ lua_unlock(L); \\\n  lua_lock(L); }", &["L"]).len(), 2);
        assert_eq!(names("g()", &[]), vec![n("", "g", false, 0)]);
    }
}
