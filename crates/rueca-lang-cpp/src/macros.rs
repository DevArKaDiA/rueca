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
        let c = b[i];
        match c {
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i < b.len() && !(b[i] == b'*' && b.get(i + 1) == Some(&b'/')) {
                    i += 1;
                }
                i += 2;
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'"' | b'\'' => {
                i += 1;
                while i < b.len() && b[i] != c && b[i] != b'\n' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                i += 1;
                out.push((T::Other, i, i));
            }
            _ if c.is_ascii_alphabetic() || c == b'_' => {
                let s = i;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                    i += 1;
                }
                out.push((T::Ident, s, i));
            }
            _ if c.is_ascii_digit() => {
                // Numbers, with suffixes and exponents (`1e-3f`, `0x1p3`).
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'.') {
                    i += 1;
                }
                out.push((T::Other, i, i));
            }
            b':' if b.get(i + 1) == Some(&b':') => {
                out.push((T::Scope, i, i + 2));
                i += 2;
            }
            b'-' if b.get(i + 1) == Some(&b'>') => {
                out.push((T::Member, i, i + 2));
                i += 2;
            }
            b'.' => {
                out.push((T::Member, i, i + 1));
                i += 1;
            }
            b'#' => {
                let s = i;
                i += if b.get(i + 1) == Some(&b'#') { 2 } else { 1 };
                out.push((T::Hash, s, i));
            }
            b'(' => {
                out.push((T::Open, i, i + 1));
                i += 1;
            }
            b')' => {
                out.push((T::Close, i, i + 1));
                i += 1;
            }
            b',' => {
                out.push((T::Comma, i, i + 1));
                i += 1;
            }
            _ if c.is_ascii_whitespace() || c == b'\\' => i += 1,
            _ => {
                out.push((T::Other, i, i + 1));
                i += 1;
            }
        }
    }
    out
}

/// Calls in `body` (the replacement list of a macro whose parameters are
/// `params`).
pub(crate) fn body_calls(body: &[u8], params: &[String]) -> Vec<BodyCall> {
    let toks = tokens(body);
    let word = |k: usize| std::str::from_utf8(&body[toks[k].1..toks[k].2]).unwrap_or("");
    let mut out = Vec::new();
    let mut k = 0;
    while k < toks.len() && out.len() < MAX_CALLS {
        // The start of a name chain: `a`, `::a`, `a::b::c`.
        let start = k;
        let global = toks[k].0 == T::Scope && toks.get(k + 1).is_some_and(|t| t.0 == T::Ident);
        let mut j = if global { k + 1 } else { k };
        if toks[j].0 != T::Ident {
            k += 1;
            continue;
        }
        let mut segs = vec![j];
        while toks.get(j + 1).is_some_and(|t| t.0 == T::Scope) && toks.get(j + 2).is_some_and(|t| t.0 == T::Ident) {
            j += 2;
            segs.push(j);
        }
        let next = j + 1;
        k = next;
        if toks.get(next).is_none_or(|t| t.0 != T::Open) {
            continue;
        }
        let prev = start.checked_sub(1).map(|p| toks[p].0);
        if matches!(prev, Some(T::Member | T::Hash)) || toks.get(next + 1).is_some_and(|t| t.0 == T::Hash) {
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
        // Arguments: top-level commas up to the matching `)`.
        let (mut depth, mut commas, mut any) = (0i32, 0u32, false);
        for t in &toks[next..] {
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
        let qualifier = segs[..segs.len() - 1].iter().map(|&s| word(s)).collect::<Vec<_>>().join("::");
        out.push(BodyCall {
            start: toks[j].1,
            end: toks[j].2,
            name: name.to_string(),
            qualifier,
            global,
            arity: if any || commas > 0 { commas + 1 } else { 0 },
        });
    }
    out
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
