//! Code-aware tokenizer shared by symbol indexing and symbol queries.
//!
//! Rules, applied to every maximal run of identifier characters (alphanumerics,
//! `_` and `$`); everything else (whitespace, `.`, `/`, `:`, `-`, …) separates
//! runs:
//!
//! - split at `_` / `$`, lower→upper (`getUser` → get|User), acronym ends
//!   (`HTTPServer` → HTTP|Server) and letter↔digit boundaries (`utf8` → utf|8);
//! - every part is lowercased and emitted;
//! - if a run splits into more than one part, the whole run lowercased is
//!   emitted too (`getuserbyid`), so exact identifier queries rank first.
//!
//! Tokens longer than [`MAX_TOKEN_BYTES`] are dropped.

use tantivy::tokenizer::{Token, TokenStream, Tokenizer};

/// Name under which the tokenizer is registered in tantivy.
pub const CODE_TOKENIZER: &str = "rueca_code";
pub const MAX_TOKEN_BYTES: usize = 128;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Lower,
    Upper,
    Digit,
    Other,
}

fn class(c: char) -> Class {
    if c.is_numeric() {
        Class::Digit
    } else if c.is_uppercase() {
        Class::Upper
    } else if c.is_lowercase() {
        Class::Lower
    } else {
        Class::Other // alphabetic without case (CJK, …)
    }
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// Calls `emit(text, start, end)` for every token of `text`, in order.
pub fn for_each_token(text: &str, mut emit: impl FnMut(String, usize, usize)) {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let end_of = |i: usize| chars.get(i).map(|(b, _)| *b).unwrap_or(text.len());
    let mut i = 0;
    while i < chars.len() {
        if !is_ident(chars[i].1) {
            i += 1;
            continue;
        }
        let run_start = i;
        while i < chars.len() && is_ident(chars[i].1) {
            i += 1;
        }
        let run_end = i;
        // Split the run into parts.
        let mut parts: Vec<(usize, usize)> = Vec::new(); // char index ranges
        let mut p = run_start;
        let mut cur: Option<usize> = None;
        while p < run_end {
            let c = chars[p].1;
            if c == '_' || c == '$' {
                if let Some(s) = cur.take() {
                    parts.push((s, p));
                }
                p += 1;
                continue;
            }
            match cur {
                None => cur = Some(p),
                Some(s) => {
                    let prev = class(chars[p - 1].1);
                    let this = class(c);
                    let next = chars.get(p + 1).filter(|_| p + 1 < run_end).map(|(_, n)| class(*n));
                    let boundary = match (prev, this) {
                        (Class::Lower, Class::Upper) => true,
                        (Class::Upper, Class::Upper) => next == Some(Class::Lower),
                        (Class::Digit, Class::Digit) => false,
                        (Class::Digit, _) | (_, Class::Digit) => true,
                        (Class::Other, Class::Other) => false,
                        (Class::Other, _) | (_, Class::Other) => true,
                        _ => false,
                    };
                    if boundary {
                        parts.push((s, p));
                        cur = Some(p);
                    }
                }
            }
            p += 1;
        }
        if let Some(s) = cur {
            parts.push((s, run_end));
        }
        for &(s, e) in &parts {
            let (bs, be) = (chars[s].0, end_of(e));
            let tok = text[bs..be].to_lowercase();
            if !tok.is_empty() && tok.len() <= MAX_TOKEN_BYTES {
                emit(tok, bs, be);
            }
        }
        let whole_differs = parts.len() > 1 || parts.first().is_some_and(|&(s, e)| s != run_start || e != run_end);
        if whole_differs {
            let (bs, be) = (chars[run_start].0, end_of(run_end));
            let tok = text[bs..be].to_lowercase();
            if tok.len() <= MAX_TOKEN_BYTES && tok.chars().any(|c| c.is_alphanumeric()) {
                emit(tok, bs, be);
            }
        }
    }
}

/// All tokens of `text`, in order (with duplicates).
pub fn tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for_each_token(text, |t, _, _| out.push(t));
    out
}

/// tantivy adapter.
#[derive(Clone, Default)]
pub struct CodeTokenizer;

pub struct CodeTokenStream {
    tokens: Vec<Token>,
    idx: usize,
}

impl Tokenizer for CodeTokenizer {
    type TokenStream<'a> = CodeTokenStream;
    fn token_stream<'a>(&'a mut self, text: &'a str) -> CodeTokenStream {
        let mut tokens = Vec::new();
        for_each_token(text, |t, from, to| {
            let position = tokens.len();
            tokens.push(Token { offset_from: from, offset_to: to, position, text: t, position_length: 1 });
        });
        CodeTokenStream { tokens, idx: 0 }
    }
}

impl TokenStream for CodeTokenStream {
    fn advance(&mut self) -> bool {
        if self.idx < self.tokens.len() {
            self.idx += 1;
            true
        } else {
            false
        }
    }
    fn token(&self) -> &Token {
        &self.tokens[self.idx - 1]
    }
    fn token_mut(&mut self) -> &mut Token {
        &mut self.tokens[self.idx - 1]
    }
}

#[cfg(test)]
mod tests {
    use super::tokens;

    #[test]
    fn camel_and_acronyms() {
        assert_eq!(tokens("getUserById"), vec!["get", "user", "by", "id", "getuserbyid"]);
        assert_eq!(tokens("HTTPServer"), vec!["http", "server", "httpserver"]);
        assert_eq!(tokens("parseHTTPResponse2"), vec!["parse", "http", "response", "2", "parsehttpresponse2"]);
    }

    #[test]
    fn snake_paths_and_dots() {
        assert_eq!(tokens("get_user_by_id"), vec!["get", "user", "by", "id", "get_user_by_id"]);
        assert_eq!(tokens("src/api/users.py"), vec!["src", "api", "users", "py"]);
        assert_eq!(tokens("pkg.mod.Class.run"), vec!["pkg", "mod", "class", "run"]);
        assert_eq!(tokens("utf8Decode"), vec!["utf", "8", "decode", "utf8decode"]);
        assert_eq!(tokens("__init__"), vec!["init", "__init__"]);
        assert_eq!(tokens("  "), Vec::<String>::new());
    }

    #[test]
    fn unicode() {
        assert_eq!(tokens("ÉtatMachine"), vec!["état", "machine", "étatmachine"]);
        assert_eq!(tokens("日本語"), vec!["日本語"]);
    }
}
