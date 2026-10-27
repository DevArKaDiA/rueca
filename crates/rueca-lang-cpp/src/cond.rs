//! Conditional compilation without a preprocessor: which `#if` branches the
//! parser sees.
//!
//! The grammars accept `#if` / `#else` groups where a declaration, a
//! statement, a member or an enumerator may stand, with every branch a
//! complete piece of code. Real code also splits expressions, statements,
//! initializer lists and function headers across branches, which no
//! grammar can parse. This module reads every group of a file and decides,
//! per group:
//!
//! - **Keep every branch** (as before): the branches are balanced (braces,
//!   parentheses, brackets), end at a statement boundary and the group
//!   stands where the grammar allows one. Branches whose condition is known
//!   to be false are blanked (dead code: `#if 0`, Windows-only code,
//!   `#ifdef __cplusplus` in C).
//! - **Collapse** to one branch: otherwise the parser would fail, so the
//!   group's directives and every other branch are blanked and the code of
//!   one branch stays, as a preprocessor with one configuration would leave
//!   it. The branch is the first whose condition is not known to be false
//!   (`#else` counts as true): unknown conditions take the first, "likely"
//!   branch.
//!
//! Conditions are evaluated in three-valued logic over `defined`, `!`,
//! `&&`, `||`, parentheses and integer literals, with a small fixed define
//! set ([`known`]): `__cplusplus` per language, Objective-C (`__OBJC__`)
//! and C++/CLI (`__cplusplus_cli`, `_MANAGED`) undefined (the grammars parse
//! neither), GCC / Clang defined, Windows targets (`_WIN32`, `_MSC_VER`, …)
//! undefined; every other name is unknown. Project `-D` flags are not used: extraction sees one file.
//!
//! The same group table serves the error-driven repair (`preprocess.rs`):
//! a group next to a parse error is collapsed even when it looked
//! well-formed.

use std::collections::HashMap;

use rueca_model::Language;

use crate::mask::{Kind, Tok, directive_kind};

/// Three-valued truth of a condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tri {
    True,
    False,
    Unknown,
}

impl Tri {
    fn not(self) -> Tri {
        match self {
            Tri::True => Tri::False,
            Tri::False => Tri::True,
            Tri::Unknown => Tri::Unknown,
        }
    }
    fn and(self, o: Tri) -> Tri {
        match (self, o) {
            (Tri::False, _) | (_, Tri::False) => Tri::False,
            (Tri::True, Tri::True) => Tri::True,
            _ => Tri::Unknown,
        }
    }
    fn or(self, o: Tri) -> Tri {
        match (self, o) {
            (Tri::True, _) | (_, Tri::True) => Tri::True,
            (Tri::False, Tri::False) => Tri::False,
            _ => Tri::Unknown,
        }
    }
}

/// One `#if` … `#endif` group.
#[derive(Debug)]
pub(crate) struct Group {
    /// Token index of each branch directive (`#if…`, `#elif…`, `#else`).
    pub(crate) branches: Vec<usize>,
    /// Token index of the `#endif` (the end of the file when missing).
    pub(crate) end: usize,
    /// Truth of each branch's own condition (`#else` is true), assuming
    /// the target: chooses the branch of a collapsed group.
    pub(crate) truth: Vec<Tri>,
    /// Whether each branch is dead whatever the target (`#if 0`,
    /// `#ifdef __cplusplus` in C): blanked even when the group is kept.
    pub(crate) dead: Vec<bool>,
    /// A branch condition on one macro (`#ifdef X`, `#if !defined(X)`, `#if
    /// X`): the macro and whether it must be defined.
    pub(crate) on: Vec<Option<(Vec<u8>, bool)>>,
}

impl Group {
    /// Token range of branch `k`'s code (exclusive of its directive).
    fn body(&self, k: usize) -> (usize, usize) {
        let next = self.branches.get(k + 1).copied().unwrap_or(self.end);
        (self.branches[k] + 1, next)
    }

    /// The branch kept when the group collapses: the first one not known
    /// to be false.
    fn chosen(&self) -> Option<usize> {
        self.truth.iter().position(|t| *t != Tri::False)
    }

    /// [`Group::chosen`] under the assumptions earlier collapses made
    /// (`#ifdef X` chosen once means `X` is defined for later groups).
    fn chosen_with(&self, assumed: &HashMap<Vec<u8>, bool>) -> Option<usize> {
        (0..self.branches.len()).find(|&k| match &self.on[k] {
            Some((name, want)) if assumed.contains_key(name) => assumed[name] == *want,
            _ => self.truth[k] != Tri::False,
        })
    }

    /// Byte ranges to blank so that only `keep`'s code remains.
    fn collapse(&self, toks: &[Tok], keep: Option<usize>) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        for k in 0..self.branches.len() {
            let d = &toks[self.branches[k]];
            out.push((d.start, d.end));
            if Some(k) != keep {
                let (a, b) = self.body(k);
                if a < b {
                    out.push((toks[a].start, toks[b - 1].end));
                }
            }
        }
        if let Some(e) = toks.get(self.end).filter(|t| t.kind == Kind::Directive) {
            out.push((e.start, e.end));
        }
        out
    }
}

/// What the evaluator assumes: the language always; the target (GCC or
/// Clang, not Windows) only to choose the branch a collapsed group keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Assume {
    pub(crate) language: Language,
    pub(crate) target: bool,
}

/// Names the evaluator knows: `Some(true)` defined (non-zero), `Some(false)`
/// undefined, `None` unknown.
fn known(name: &[u8], assume: Assume) -> Option<bool> {
    match name {
        b"__cplusplus" => Some(assume.language == Language::Cpp),
        b"__STDC__" => Some(true),
        // Other languages the grammars do not parse: Objective-C(++) and
        // C++/CLI code is dead for both.
        b"__OBJC__" | b"__OBJC2__" | b"__cplusplus_cli" | b"__cplusplus_winrt" | b"_MANAGED" | b"__CLR_VER" => {
            Some(false)
        }
        b"__GNUC__" | b"__clang__" if assume.target => Some(true),
        b"_WIN32" | b"_WIN64" | b"WIN32" | b"_WINDOWS" | b"_MSC_VER" | b"__MINGW32__" | b"__MINGW64__"
        | b"__CYGWIN__" | b"_MSC_FULL_VER"
            if assume.target =>
        {
            Some(false)
        }
        _ => None,
    }
}

fn defined(name: &[u8], assume: Assume) -> Tri {
    match known(name, assume) {
        Some(true) => Tri::True,
        Some(false) => Tri::False,
        None => Tri::Unknown,
    }
}

/// Words and operators of a condition (comments and continuations dropped).
fn cond_tokens(text: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < text.len() {
        let c = text[i];
        if c.is_ascii_whitespace() || c == b'\\' {
            i += 1;
        } else if text[i..].starts_with(b"//") {
            break;
        } else if text[i..].starts_with(b"/*") {
            i = text[i + 2..].windows(2).position(|w| w == b"*/").map_or(text.len(), |p| i + 2 + p + 2);
        } else {
            let end = cond_token_end(text, i);
            out.push(&text[i..end]);
            i = end;
        }
    }
    out
}

/// The end of the condition token at `i`: a word or number, `&&` / `||`,
/// or one byte.
fn cond_token_end(text: &[u8], i: usize) -> usize {
    let c = text[i];
    if c.is_ascii_alphanumeric() || c == b'_' {
        return i + text[i..]
            .iter()
            .take_while(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'\'' | b'.'))
            .count();
    }
    if text[i..].starts_with(b"&&") || text[i..].starts_with(b"||") { i + 2 } else { i + 1 }
}

/// Three-valued evaluation of a condition's tokens.
struct Eval<'a> {
    toks: Vec<&'a [u8]>,
    at: usize,
    assume: Assume,
    depth: usize,
}

impl<'a> Eval<'a> {
    fn peek(&self) -> Option<&'a [u8]> {
        self.toks.get(self.at).copied()
    }
    fn or(&mut self) -> Tri {
        let mut v = self.and();
        while self.peek() == Some(b"||") {
            self.at += 1;
            v = v.or(self.and());
        }
        v
    }
    fn and(&mut self) -> Tri {
        let mut v = self.compare();
        while self.peek() == Some(b"&&") {
            self.at += 1;
            v = v.and(self.compare());
        }
        v
    }
    /// A unary term, possibly compared or combined arithmetically with
    /// others (any such operator makes the result unknown).
    fn compare(&mut self) -> Tri {
        let v = self.unary();
        let mut unknown = false;
        while let Some(op) = self.peek() {
            let is_op =
                matches!(op, b"=" | b"<" | b">" | b"+" | b"-" | b"*" | b"/" | b"%" | b"&" | b"|" | b"^" | b"?" | b":");
            if !is_op {
                break;
            }
            self.at += 1;
            // `==`, `>=`, `<<`: the second byte is its own token here.
            if matches!(self.peek(), Some(b"=" | b"<" | b">")) {
                self.at += 1;
            }
            self.unary();
            unknown = true;
        }
        if unknown { Tri::Unknown } else { v }
    }
    fn unary(&mut self) -> Tri {
        self.depth += 1;
        if self.depth > 64 {
            return Tri::Unknown;
        }
        let v = match self.peek() {
            Some(b"!") => {
                self.at += 1;
                self.unary().not()
            }
            Some(b"(") => self.parenthesized(),
            Some(b"defined") => self.defined_operator(),
            Some(w) if w[0].is_ascii_digit() => {
                self.at += 1;
                number_truth(w)
            }
            Some(w) if w[0].is_ascii_alphabetic() || w[0] == b'_' => {
                self.at += 1;
                // A function-like test (`__has_include(<x>)`): unknown.
                if self.peek() == Some(b"(") {
                    self.skip_parens();
                    Tri::Unknown
                } else {
                    defined(w, self.assume)
                }
            }
            _ => {
                self.at += 1;
                Tri::Unknown
            }
        };
        self.depth -= 1;
        v
    }

    /// `( … )`, the `(` next.
    fn parenthesized(&mut self) -> Tri {
        self.at += 1;
        let v = self.or();
        if self.peek() == Some(b")") {
            self.at += 1;
        }
        v
    }

    /// `defined X` / `defined(X)`, `defined` next.
    fn defined_operator(&mut self) -> Tri {
        self.at += 1;
        let paren = self.peek() == Some(b"(");
        if paren {
            self.at += 1;
        }
        let v = match self.peek() {
            Some(n) => {
                self.at += 1;
                defined(n, self.assume)
            }
            None => Tri::Unknown,
        };
        if paren && self.peek() == Some(b")") {
            self.at += 1;
        }
        v
    }

    /// Skips a parenthesized argument list, the `(` next.
    fn skip_parens(&mut self) {
        let mut depth = 0;
        while let Some(t) = self.peek() {
            self.at += 1;
            match t {
                b"(" => depth += 1,
                b")" => {
                    depth -= 1;
                    if depth == 0 {
                        return;
                    }
                }
                _ => {}
            }
        }
    }
}

/// Truth of an integer literal (`0`, `1L`); unknown for anything else.
fn number_truth(w: &[u8]) -> Tri {
    let digits = w.iter().take_while(|c| c.is_ascii_digit()).count();
    let suffix_ok = w[digits..].iter().all(|c| matches!(c, b'u' | b'U' | b'l' | b'L'));
    if digits == 0 || !suffix_ok {
        Tri::Unknown
    } else if w[..digits].iter().all(|c| *c == b'0') {
        Tri::False
    } else {
        Tri::True
    }
}

/// Truth of a branch directive's own condition.
pub(crate) fn condition(line: &[u8], assume: Assume) -> Tri {
    let rest = &line[1..];
    let start = rest.iter().position(|b| !matches!(b, b' ' | b'\t')).unwrap_or(rest.len());
    let rest = &rest[start..];
    let word_len = rest.iter().take_while(|b| b.is_ascii_alphabetic()).count();
    let (word, cond) = rest.split_at(word_len);
    let name = || cond_tokens(cond).first().copied().unwrap_or(b"");
    let eval = |cond: &[u8]| {
        let mut e = Eval { toks: cond_tokens(cond), at: 0, assume, depth: 0 };
        let v = e.or();
        if e.at < e.toks.len() { Tri::Unknown } else { v }
    };
    match word {
        b"ifdef" | b"elifdef" => defined(name(), assume),
        b"ifndef" | b"elifndef" => defined(name(), assume).not(),
        b"if" | b"elif" => eval(cond),
        b"else" => Tri::True,
        _ => Tri::Unknown,
    }
}

/// The macro a branch condition tests alone and whether it must be
/// defined: `#ifdef X` / `#if defined(X)` / `#if X` → `(X, true)`, `#ifndef
/// X` / `#if !defined(X)` → `(X, false)`.
fn single_macro(line: &[u8]) -> Option<(Vec<u8>, bool)> {
    let rest = line.get(1..)?;
    let start = rest.iter().position(|b| !matches!(b, b' ' | b'\t'))?;
    let rest = &rest[start..];
    let word_len = rest.iter().take_while(|b| b.is_ascii_alphabetic()).count();
    let (word, cond) = rest.split_at(word_len);
    let toks = cond_tokens(cond);
    let name = |t: &[u8]| (t[0].is_ascii_alphabetic() || t[0] == b'_').then(|| t.to_vec());
    match (word, toks.as_slice()) {
        (b"ifdef" | b"elifdef", [n]) => Some((name(n)?, true)),
        (b"ifndef" | b"elifndef", [n]) => Some((name(n)?, false)),
        (b"if" | b"elif", [b"defined", n]) | (b"if" | b"elif", [b"defined", b"(", n, b")"]) => Some((name(n)?, true)),
        (b"if" | b"elif", [b"!", b"defined", n]) | (b"if" | b"elif", [b"!", b"defined", b"(", n, b")"]) => {
            Some((name(n)?, false))
        }
        (b"if" | b"elif", [n]) if n != b"defined" => Some((name(n)?, true)),
        _ => None,
    }
}

/// Every `#if` group of the file, in the order of their `#if`.
pub(crate) fn groups(toks: &[Tok], src: &[u8], language: Language) -> Vec<Group> {
    let mut out: Vec<Group> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    // Per group: an earlier branch is known taken whatever the target
    // (`#ifndef __OBJC__` … `#else`), so the later ones are dead.
    let mut taken: Vec<bool> = Vec::new();
    for (i, t) in toks.iter().enumerate() {
        if t.kind != Kind::Directive {
            continue;
        }
        let line = &src[t.start..t.end];
        match directive_kind(line) {
            Some("if") => {
                stack.push(out.len());
                let neutral = condition(line, Assume { language, target: false });
                taken.push(neutral == Tri::True);
                out.push(Group {
                    branches: vec![i],
                    end: toks.len(),
                    truth: vec![condition(line, Assume { language, target: true })],
                    dead: vec![neutral == Tri::False],
                    on: vec![single_macro(line)],
                });
            }
            Some("elif" | "else") => {
                if let Some(&g) = stack.last() {
                    let neutral = condition(line, Assume { language, target: false });
                    out[g].branches.push(i);
                    out[g].truth.push(condition(line, Assume { language, target: true }));
                    out[g].dead.push(neutral == Tri::False || taken[g]);
                    out[g].on.push(single_macro(line));
                    taken[g] |= neutral == Tri::True;
                }
            }
            Some("endif") => {
                if let Some(g) = stack.pop() {
                    out[g].end = i;
                }
            }
            _ => {}
        }
    }
    out
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Brace {
    /// An initializer list (`= {`, `, {`, `{ {`, `( {`, `return {`).
    Init,
    /// An `enum` body.
    Enum,
    /// Any other block: function, class, namespace, statement.
    Other,
}

/// What surrounds a group's `#if`.
struct Context {
    /// Inside parentheses or brackets.
    in_parens: bool,
    brace: Option<Brace>,
}

/// The context of every directive token: open parentheses and the kind of
/// the innermost brace.
fn contexts(toks: &[Tok], src: &[u8]) -> Vec<Option<Context>> {
    let mut stack: Vec<Option<Brace>> = Vec::new(); // None = paren / bracket
    let mut out = Vec::with_capacity(toks.len());
    for (i, t) in toks.iter().enumerate() {
        if t.kind == Kind::Directive {
            out.push(Some(Context {
                in_parens: stack.last().is_some_and(|b| b.is_none()),
                brace: stack.iter().rev().find_map(|b| *b),
            }));
            continue;
        }
        out.push(None);
        match t.kind {
            Kind::Punct(b'(' | b'[') => stack.push(None),
            Kind::Punct(b')' | b']') => {
                if stack.last().is_some_and(|b| b.is_none()) {
                    stack.pop();
                }
            }
            Kind::Punct(b'{') => stack.push(Some(brace_kind(toks, i, src))),
            Kind::Punct(b'}') => {
                // Close any parentheses left open inside the block.
                while stack.last().is_some_and(|b| b.is_none()) {
                    stack.pop();
                }
                stack.pop();
            }
            _ => {}
        }
    }
    out
}

/// What the `{` at token `i` opens.
fn brace_kind(toks: &[Tok], i: usize, src: &[u8]) -> Brace {
    let text = |t: &Tok| &src[t.start..t.end];
    let prev = toks[..i].iter().rev().find(|p| p.kind != Kind::Directive);
    let init = prev.is_some_and(|p| {
        matches!(p.kind, Kind::Punct(b'=' | b',' | b'{' | b'(')) || (p.kind == Kind::Ident && text(p) == b"return")
    });
    if init {
        return Brace::Init;
    }
    let is_enum = toks[..i]
        .iter()
        .rev()
        .take(16)
        .take_while(|p| matches!(p.kind, Kind::Ident | Kind::Punct(b':')))
        .any(|p| text(p) == b"enum");
    if is_enum { Brace::Enum } else { Brace::Other }
}

/// The pre-parse decisions: byte ranges to blank (collapsed groups and
/// dead branches) and which groups were collapsed.
pub(crate) struct Plan {
    pub(crate) blanks: Vec<(usize, usize)>,
    pub(crate) collapsed: Vec<bool>,
}

/// Balance of braces, parentheses and brackets of each branch of every
/// group, and whether the running balance of a branch ever dips below zero
/// (`}` `else {` closes a block the branch did not open); a nested group
/// counts with its first branch.
fn balances(groups: &[Group], toks: &[Tok]) -> Vec<Vec<[i32; 4]>> {
    let mut out: Vec<Vec<[i32; 4]>> = groups.iter().map(|g| vec![[0; 4]; g.branches.len()]).collect();
    // Innermost groups first: a group starts after its parent.
    let mut order: Vec<usize> = (0..groups.len()).collect();
    order.sort_by_key(|&g| std::cmp::Reverse(groups[g].branches[0]));
    let starts: HashMap<usize, usize> = groups.iter().enumerate().map(|(g, group)| (group.branches[0], g)).collect();
    for g in order {
        for k in 0..groups[g].branches.len() {
            let bal = branch_balance(groups, toks, &starts, &out, (g, k));
            out[g][k] = bal;
        }
    }
    out
}

/// The [`balances`] entry of branch `k` of group `g`, nested groups'
/// first branches (in `done`) already computed.
fn branch_balance(
    groups: &[Group],
    toks: &[Tok],
    starts: &HashMap<usize, usize>,
    done: &[Vec<[i32; 4]>],
    (g, k): (usize, usize),
) -> [i32; 4] {
    let (a, b) = groups[g].body(k);
    let mut bal = [0i32; 4];
    let mut i = a;
    while i < b {
        // A nested group: add its first branch, skip the rest.
        if let Some(&n) = starts.get(&i).filter(|&&n| n != g) {
            for (x, y) in bal.iter_mut().zip(done[n][0]) {
                *x += y;
            }
            i = groups[n].end + 1;
            continue;
        }
        if let Some((slot, delta)) = bracket_delta(toks[i].kind) {
            bal[slot] += delta;
        }
        if bal[..3].iter().any(|x| *x < 0) {
            bal[3] = 1;
        }
        i += 1;
    }
    bal
}

/// The balance slot a bracket token counts in (0: braces, 1: parentheses,
/// 2: brackets) and its sign.
fn bracket_delta(kind: Kind) -> Option<(usize, i32)> {
    match kind {
        Kind::Punct(b'{') => Some((0, 1)),
        Kind::Punct(b'}') => Some((0, -1)),
        Kind::Punct(b'(') => Some((1, 1)),
        Kind::Punct(b')') => Some((1, -1)),
        Kind::Punct(b'[') => Some((2, 1)),
        Kind::Punct(b']') => Some((2, -1)),
        _ => None,
    }
}

/// Pre-parse plan for every group (module docs).
pub(crate) fn plan(groups: &[Group], toks: &[Tok], src: &[u8]) -> Plan {
    let ctx = contexts(toks, src);
    let bal = balances(groups, toks);
    let mut blanks = Vec::new();
    let mut collapsed = vec![false; groups.len()];
    // Token ranges already blanked (a group inside is skipped).
    let mut dead: Vec<(usize, usize)> = Vec::new();
    // What collapses chose so far: one configuration for the whole file.
    let mut assumed: HashMap<Vec<u8>, bool> = HashMap::new();
    for (g, group) in groups.iter().enumerate() {
        let first = group.branches[0];
        if dead.iter().any(|&(a, b)| first >= a && first < b) {
            continue;
        }
        if !must_collapse(group, &bal[g], toks, ctx[first].as_ref()) {
            blank_dead_branches(group, toks, &mut blanks, &mut dead);
            continue;
        }
        let keep = group.chosen_with(&assumed);
        if let Some((name, want)) = keep.and_then(|k| group.on[k].clone()) {
            assumed.entry(name).or_insert(want);
        }
        blanks.extend(group.collapse(toks, keep));
        collapsed[g] = true;
        dead.extend((0..group.branches.len()).filter(|&k| Some(k) != keep).map(|k| group.body(k)));
    }
    blanks.extend(stray_directive_blanks(toks, src, &ctx));
    blanks.extend(prose_blanks(groups, toks));
    Plan { blanks, collapsed }
}

/// Blanks around directives the grammars reject where they sit (see
/// [`misplaced_directive`] and [`label_before_directive`]), in token order;
/// `ctx` holds the [`contexts`] of `toks`.
fn stray_directive_blanks(toks: &[Tok], src: &[u8], ctx: &[Option<Context>]) -> Vec<(usize, usize)> {
    let mut blanks = Vec::new();
    for (i, c) in ctx.iter().enumerate() {
        let Some(c) = c.as_ref() else { continue };
        blanks.extend(misplaced_directive(toks, src, i, c));
        blanks.extend(label_before_directive(toks, src, i, c));
    }
    blanks
}

/// Blanks for the branches that are prose, not code (`#if A + B != 1` /
/// `Exactly one of A, B should be defined.` / `#endif`, a deliberate
/// compile error).
fn prose_blanks(groups: &[Group], toks: &[Tok]) -> Vec<(usize, usize)> {
    let mut blanks = Vec::new();
    for group in groups {
        for k in 0..group.branches.len() {
            let (a, b) = group.body(k);
            if a < b && is_prose(toks, a, b) {
                blanks.push((toks[a].start, toks[b - 1].end));
            }
        }
    }
    blanks
}

/// Whether a group must collapse to one branch before parsing: a branch
/// is unbalanced or does not end a statement, or the group sits where a
/// directive cannot (in parentheses, an initializer list, mid-statement).
/// `bal` is the group's [`balances`], `c` the context of its `#if`.
fn must_collapse(group: &Group, bal: &[[i32; 4]], toks: &[Tok], c: Option<&Context>) -> bool {
    let first = group.branches[0];
    let brace = c.and_then(|c| c.brace);
    let in_enum = brace == Some(Brace::Enum);
    let prev = toks[..first].iter().rev().find(|t| t.kind != Kind::Directive);
    let boundary = prev.is_none_or(|t| {
        matches!(t.kind, Kind::Punct(b';' | b'{' | b'}' | b':')) || (in_enum && matches!(t.kind, Kind::Punct(b',')))
    });
    let unbalanced = bal.iter().any(|b| *b != [0; 4]);
    let misplaced = c.is_some_and(|c| c.in_parens) || brace == Some(Brace::Init) || !boundary;
    let unfinished = (0..group.branches.len()).any(|k| {
        let (a, b) = group.body(k);
        let last = toks[a..b].iter().rev().find(|t| t.kind != Kind::Directive);
        last.is_some_and(|t| {
            !(matches!(t.kind, Kind::Punct(b';' | b'}' | b':'))
                || (in_enum && matches!(t.kind, Kind::Punct(b',' | b'0') | Kind::Ident)))
        })
    });
    unbalanced || misplaced || unfinished
}

/// Blanks the branches of `group` known dead whatever the target.
fn blank_dead_branches(group: &Group, toks: &[Tok], blanks: &mut Vec<(usize, usize)>, dead: &mut Vec<(usize, usize)>) {
    for k in (0..group.branches.len()).filter(|&k| group.dead[k]) {
        let (a, b) = group.body(k);
        if a < b {
            blanks.push((toks[a].start, toks[b - 1].end));
            dead.push((a, b));
        }
    }
}

/// An `#include` / `#define` line (token `i`, context `c`) inside an
/// initializer list, an enum body or parentheses (X-macro tables), also
/// after an `=` (`static const char s[] =` + `#include "x.inc"`): the
/// grammars take none there, and nothing in them is extracted (includes
/// are read lexically).
fn misplaced_directive(toks: &[Tok], src: &[u8], i: usize, c: &Context) -> Option<(usize, usize)> {
    let t = &toks[i];
    let prev = toks[..i].iter().rev().find(|t| t.kind != Kind::Directive);
    let after_operator = prev.is_some_and(|p| matches!(p.kind, Kind::Punct(b'=' | b',' | b'(')));
    let misplaced = c.in_parens || after_operator || matches!(c.brace, Some(Brace::Init | Brace::Enum));
    (misplaced && t.kind == Kind::Directive && directive_kind(&src[t.start..t.end]).is_none())
        .then_some((t.start, t.end))
}

/// A goto label right before the directive at token `i` (`ovfl:` +
/// `#ifdef X`): a label needs a statement, which a directive is not to
/// the grammar. Labels carry nothing to extract; `goto ovfl;` stays valid.
fn label_before_directive(toks: &[Tok], src: &[u8], i: usize, c: &Context) -> Option<(usize, usize)> {
    if toks[i].kind != Kind::Directive || i < 2 || c.brace != Some(Brace::Other) || c.in_parens {
        return None;
    }
    let (l, colon) = (&toks[i - 2], &toks[i - 1]);
    let before = i.checked_sub(3).map(|k| &toks[k]);
    let label = l.kind == Kind::Ident
        && colon.kind == Kind::Punct(b':')
        && !matches!(&src[l.start..l.end], b"default" | b"public" | b"private" | b"protected")
        && before.is_none_or(|b| matches!(b.kind, Kind::Punct(b';' | b'{' | b'}') | Kind::Directive));
    label.then_some((l.start, colon.end))
}

/// Whether tokens `a..b` hold a line of prose: five or more words (and
/// commas) ending with `.`.
fn is_prose(toks: &[Tok], a: usize, b: usize) -> bool {
    let mut i = a;
    while i < b {
        let line = toks[i].line;
        let end = (i..b).find(|&j| toks[j].line != line).unwrap_or(b);
        let words = (i..end).filter(|&j| toks[j].kind == Kind::Ident).count();
        let shape = end > i
            && toks[end - 1].kind == Kind::Punct(b'.')
            && (i..end - 1).all(|j| matches!(toks[j].kind, Kind::Ident | Kind::Punct(b',')));
        if shape && words >= 5 {
            return true;
        }
        i = end;
    }
    false
}

/// Error-driven collapse: the innermost group not yet collapsed that
/// contains byte `at` or has a directive on lines `lines`, collapsed to the
/// branch containing `at`, else its chosen branch, else its first. Marks it
/// collapsed.
pub(crate) fn collapse_near(
    groups: &[Group],
    collapsed: &mut [bool],
    toks: &[Tok],
    at: usize,
    lines: (u32, u32),
) -> Option<Vec<(usize, usize)>> {
    let mut best: Option<(usize, usize)> = None; // (size in tokens, group)
    for (g, group) in groups.iter().enumerate() {
        if collapsed[g] {
            continue;
        }
        let first = &toks[group.branches[0]];
        let end = toks.get(group.end).map_or(usize::MAX, |t| t.end);
        let directive_near = group
            .branches
            .iter()
            .chain(std::iter::once(&group.end))
            .filter_map(|&i| toks.get(i))
            .any(|t| t.line >= lines.0 && t.line <= lines.1);
        let contains = first.start <= at && at < end;
        if !(directive_near || contains) {
            continue;
        }
        let size = group.end.saturating_sub(group.branches[0]);
        // An include guard or a group spanning most of the file is not the
        // cause of a local error unless one of its directives is.
        if !directive_near && size > 2000 {
            continue;
        }
        if best.is_none_or(|(s, _)| size < s) {
            best = Some((size, g));
        }
    }
    let (_, g) = best?;
    collapsed[g] = true;
    let group = &groups[g];
    // Keep the branch with the error (removing it would only hide the
    // error), else the chosen one; never drop every branch.
    let containing = (0..group.branches.len()).find(|&k| {
        let (a, b) = group.body(k);
        a < b && toks[a].start <= at && at < toks[b - 1].end
    });
    Some(group.collapse(toks, containing.or(group.chosen()).or(Some(0))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mask::lex;

    fn truth(line: &str, language: Language) -> Tri {
        condition(line.as_bytes(), Assume { language, target: true })
    }

    #[test]
    fn conditions() {
        use Tri::*;
        assert_eq!(truth("#if 0", Language::C), False);
        assert_eq!(truth("#if 1", Language::C), True);
        assert_eq!(truth("#ifdef __cplusplus", Language::C), False);
        assert_eq!(truth("#ifdef __cplusplus", Language::Cpp), True);
        assert_eq!(truth("#ifndef __cplusplus", Language::Cpp), False);
        assert_eq!(truth("#if defined(_WIN32) || defined(_WIN64)", Language::C), False);
        assert_eq!(truth("#if !defined(_MSC_VER) && defined(HAVE_X)", Language::C), Unknown);
        assert_eq!(truth("#if defined(_WIN32) && FOO", Language::C), False);
        assert_eq!(truth("#if defined __GNUC__ || FOO", Language::C), True);
        assert_eq!(truth("#if FMT_CLANG_VERSION >= 307", Language::Cpp), Unknown);
        assert_eq!(truth("#if __has_include(<x.h>) // c", Language::C), Unknown);
        assert_eq!(truth("#ifdef HAVE_ALLOCA_H", Language::C), Unknown);
        assert_eq!(truth("#ifndef FOO_H", Language::C), Unknown);
        assert_eq!(truth("#else", Language::C), True);
        assert_eq!(truth("#elif 0 /* off */", Language::C), False);
        assert_eq!(truth("#if 0L", Language::C), False);
        assert_eq!(truth("#if (0)", Language::C), False);
        assert_eq!(truth("#if 1 \\\n && defined(_WIN32)", Language::C), False);
        // Without the target assumption only the language and literals count.
        let neutral = |l: &str| condition(l.as_bytes(), Assume { language: Language::C, target: false });
        assert_eq!(neutral("#ifdef _WIN32"), Unknown);
        assert_eq!(neutral("#if defined(__cplusplus) || 0"), False);
    }

    fn blanked(src: &str, language: Language) -> String {
        let toks = lex(src.as_bytes());
        let gs = groups(&toks, src.as_bytes(), language);
        let p = plan(&gs, &toks, src.as_bytes());
        let mut out = src.as_bytes().to_vec();
        for (a, b) in p.blanks {
            for c in &mut out[a..b] {
                if *c != b'\n' {
                    *c = b' ';
                }
            }
        }
        String::from_utf8(out)
            .unwrap()
            .lines()
            .map(str::trim_end)
            .filter(|l| !l.trim().is_empty())
            .collect::<Vec<_>>()
            .join("|")
    }

    #[test]
    fn well_formed_groups_keep_every_live_branch() {
        let src = "#ifdef X\nint a;\n#else\nint b;\n#endif\n";
        assert_eq!(blanked(src, Language::C), "#ifdef X|int a;|#else|int b;|#endif");
        // Dead branches go; platform branches stay (they are code of the
        // repository).
        let src = "#if 0\nint a;\n#else\nint b;\n#endif\n#ifdef _WIN32\nint w;\n#endif\n";
        assert_eq!(blanked(src, Language::C), "#if 0|#else|int b;|#endif|#ifdef _WIN32|int w;|#endif");
        // Objective-C and C++/CLI branches are dead, so is every branch
        // after one known to be taken.
        let src = "#ifndef __OBJC__\nint a;\n#else\n[pool drain];\n#endif\n#if defined(_MANAGED)\nT^ r;\n#endif\n";
        assert_eq!(blanked(src, Language::Cpp), "#ifndef __OBJC__|int a;|#else|#endif|#if defined(_MANAGED)|#endif");
        let src = "#ifdef __cplusplus\nint a;\n#elif X\nint b;\n#else\nint c;\n#endif\n";
        assert_eq!(blanked(src, Language::Cpp), "#ifdef __cplusplus|int a;|#elif X|#else|#endif");
        // Enumerators may end without a comma.
        let src = "enum E {\n  A,\n#ifdef X\n  B,\n#endif\n  C\n};\n";
        assert_eq!(blanked(src, Language::C), "enum E {|  A,|#ifdef X|  B,|#endif|  C|};");
        // X-macro tables: includes and defines inside an initializer or an
        // enum body go.
        let src =
            "enum op {\n#define OP(n) n,\n#include \"ops.h\"\n#undef OP\n};\nint t[] = {\n#include \"t.inc\"\n};\n";
        assert_eq!(blanked(src, Language::C), "enum op {|};|int t[] = {|};");
    }

    #[test]
    fn labels_before_directives_and_prose_branches() {
        let src = "int f() {\n ovfl:\n#ifdef X\n  a();\n#endif\n  return 0;\n}\n";
        assert_eq!(blanked(src, Language::C), "int f() {|#ifdef X|  a();|#endif|  return 0;|}");
        let src = "#if defined(A) + defined(B) != 1\nExactly one of A, B or C should be defined.\n#endif\nint x;\n";
        assert_eq!(blanked(src, Language::C), "int x;");
        // `default:` and access specifiers stay.
        let src = "void f(int c) {\n switch (c) {\n default:\n#ifdef X\n  a();\n#endif\n  break;\n }\n}\n";
        assert!(blanked(src, Language::C).contains("default:"));
    }

    #[test]
    fn malformed_groups_collapse_to_one_branch() {
        // A function header written once per branch.
        let src = "#if OLD\nvoid f(int a) {\n#else\nvoid f() {\n#endif\n  g();\n}\n";
        assert_eq!(blanked(src, Language::C), "void f(int a) {|  g();|}");
        // Inside an initializer list, an argument list, after `if (…)`.
        let src = "int n[] = {\n 1,\n#ifdef X\n 2,\n#endif\n};\n";
        assert_eq!(blanked(src, Language::C), "int n[] = {| 1,| 2,|};");
        let src = "void f() {\n g(a,\n#ifdef X\n b\n#else\n c\n#endif\n );\n}\n";
        assert_eq!(blanked(src, Language::C), "void f() {| g(a,| b| );|}");
        let src = "void f() {\n if (x)\n#if defined(_WIN32)\n  w();\n#else\n  p();\n#endif\n}\n";
        assert_eq!(blanked(src, Language::C), "void f() {| if (x)|  p();|}");
        // A branch that ends mid-declaration.
        let src = "#ifdef X\nstatic\n#endif\nint f(void);\n";
        assert_eq!(blanked(src, Language::C), "static|int f(void);");
        // A nested group inside a dropped branch goes with it.
        let src = "#if A\nint f() {\n#ifdef B\nint x;\n#endif\n#else\nint g() {\n#endif\n return 0;\n}\n";
        assert_eq!(blanked(src, Language::C), "int f() {|#ifdef B|int x;|#endif| return 0;|}");
        // One configuration: a macro assumed defined by a collapse stays so.
        let src = "int f(int x) {\n#ifdef SU\n i = 1;\n#else\n if (x) {\n#endif\n  g();\n#ifndef SU\n }\n else {\n  h();\n }\n#endif\n return 0;\n}\n";
        assert_eq!(blanked(src, Language::C), "int f(int x) {| i = 1;|  g();| return 0;|}");
        // Balanced overall, but closing a block it did not open.
        let src = "void f(int x) {\n if (x) {\n#ifdef A\n  a();\n }\n else {\n#endif\n  b();\n }\n}\n";
        assert_eq!(blanked(src, Language::C), "void f(int x) {| if (x) {|  a();| }| else {|  b();| }|}");
    }

    #[test]
    fn error_driven_collapse() {
        let src = "int f() {\n#ifdef X\n  a();\n#endif\n  b();\n}\n";
        let toks = lex(src.as_bytes());
        let gs = groups(&toks, src.as_bytes(), Language::C);
        let mut collapsed = vec![false; gs.len()];
        let ranges = collapse_near(&gs, &mut collapsed, &toks, 20, (2, 2)).unwrap();
        assert_eq!(ranges.len(), 2);
        assert!(collapsed[0]);
        assert!(collapse_near(&gs, &mut collapsed, &toks, 20, (2, 2)).is_none());
    }
}
