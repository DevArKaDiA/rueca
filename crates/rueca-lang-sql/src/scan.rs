//! Token-level recovery (crate docs, *Statements and recovery*): for a
//! statement no dialect parses, and for routine bodies sqlparser keeps as
//! text (PL/pgSQL `$$ … $$`), definitions and references are read from the
//! tokens:
//!
//! - `CREATE [OR REPLACE | OR ALTER] [TEMP …] TABLE | VIEW | MATERIALIZED
//!   VIEW | FUNCTION | PROCEDURE | PROC | TRIGGER | [UNIQUE] INDEX |
//!   SEQUENCE name` defines `name` (a table's parenthesized column list
//!   gives its columns);
//! - a name after `FROM`, `JOIN`, `REFERENCES` is read, after `INTO`,
//!   `UPDATE`, `DELETE FROM`, `ALTER TABLE`, `DROP TABLE` / `VIEW`,
//!   `TRUNCATE [TABLE]` written; after `ON` in `CREATE INDEX` /
//!   `CREATE TRIGGER` read;
//! - `CALL` / `EXEC` / `EXECUTE` / `PERFORM name` and `name(` (not a type,
//!   not a built-in, not after a definition keyword) are calls.
//!
//! No column references come from here.

use rueca_model::{Label, LocalDefId, RefKind};
use sqlparser::keywords::Keyword;
use sqlparser::tokenizer::{Token, Word};

use crate::builtins;
use crate::dialect::Dialect;
use crate::facts::{Builder, Name, NewDef};
use crate::names::{self, Part};
use crate::split::{token_range, tokenize};
use crate::text::Lines;

/// One significant token: the token and its file byte range.
struct Tok {
    token: Token,
    range: (usize, usize),
}

/// Significant tokens of `text[start..end]` (file offsets).
fn tokens(text: &str, start: usize, end: usize, dialect: Dialect) -> Vec<Tok> {
    let slice = &text[start..end];
    let Some(raw) = tokenize(slice, dialect) else { return Vec::new() };
    let lines = Lines::new(slice);
    raw.iter()
        .filter(|t| !matches!(t.token, Token::Whitespace(_)))
        .map(|t| {
            let (s, e) = token_range(&lines, t);
            Tok { token: t.token.clone(), range: (s + start, e + start) }
        })
        .collect()
}

/// Facts of an unparsed statement: its definition (if it creates one) and
/// its references; returns the definition.
pub fn statement(
    b: &mut Builder<'_>,
    text: &str,
    range: (usize, usize),
    enclosing: Option<LocalDefId>,
    doc: Option<String>,
) -> Option<LocalDefId> {
    let toks = tokens(text, range.0, range.1, b.dialect);
    let def = create(b, &toks, range, doc);
    let skip = def.as_ref().map_or(0, |(_, after)| *after);
    let enclosing = def.as_ref().map(|(d, _)| *d).or(enclosing);
    Scanner { b, toks: &toks, enclosing }.refs(skip);
    def.map(|(d, _)| d)
}

/// References in a routine body kept as text (`$$ … $$`).
pub fn body(b: &mut Builder<'_>, text: &str, range: (usize, usize), enclosing: LocalDefId) {
    let toks = tokens(text, range.0, range.1, b.dialect);
    Scanner { b, toks: &toks, enclosing: Some(enclosing) }.refs(0);
}

/// Kind of object a `CREATE` names.
#[derive(Clone, Copy, PartialEq)]
enum Object {
    Table,
    View,
    Routine,
    Trigger,
    Index,
    Sequence,
}

/// `CREATE … <object> [IF NOT EXISTS] name`: the definition and the index
/// of the first token after its name.
fn create(
    b: &mut Builder<'_>,
    toks: &[Tok],
    range: (usize, usize),
    doc: Option<String>,
) -> Option<(LocalDefId, usize)> {
    if !toks.first().is_some_and(|t| is_kw(t, Keyword::CREATE) || is_kw(t, Keyword::ALTER)) {
        return None;
    }
    let (object, at) = object_keyword(toks)?;
    let mut i = at + 1;
    if toks.get(i).is_some_and(|t| is_kw(t, Keyword::IF)) {
        i += 3;
    }
    let (name, after) = read_name(toks, i, b.dialect)?;
    let label = match object {
        Object::Table | Object::View => Label::Struct,
        Object::Routine | Object::Trigger => Label::Function,
        Object::Index => Label::Constant,
        Object::Sequence => Label::Variable,
    };
    let segs = b.segments(&name);
    let short = segs.last()?.clone();
    let qn = segs.join(".");
    let def = NewDef {
        label,
        name: short,
        qualified_name: qn.clone(),
        parent: None,
        range,
        name_range: name.last_range(),
        signature: Some(b.signature(range, true)),
        doc,
    };
    let d = b.add_def(def);
    if object == Object::Table && toks.get(after).is_some_and(|t| t.token == Token::LParen) {
        columns(b, toks, after, &qn, d);
    }
    let after = if matches!(object, Object::Index | Object::Trigger) { on_target(b, toks, after, d) } else { after };
    if object == Object::Trigger
        && let Some(table) = b.facts.references.last().filter(|r| r.enclosing == Some(d)).map(|r| match &r.receiver {
            Some(s) => format!("{s}.{}", r.name),
            None => r.name.clone(),
        })
    {
        // Qualified by its table, as for parsed triggers.
        let name = b.facts.definitions[d as usize].name.clone();
        b.facts.definitions[d as usize].qualified_name = format!("{table}.{name}");
    }
    Some((d, after))
}

/// The object keyword within the first tokens (after `OR REPLACE`, `TEMP`,
/// MySQL `DEFINER = …`, `ALGORITHM = …`) and its index.
fn object_keyword(toks: &[Tok]) -> Option<(Object, usize)> {
    toks.iter().enumerate().skip(1).take(14).find_map(|(i, t)| {
        let Token::Word(w) = &t.token else { return None };
        let o = match w.keyword {
            Keyword::TABLE => Object::Table,
            Keyword::VIEW => Object::View,
            Keyword::FUNCTION | Keyword::PROCEDURE | Keyword::PROC | Keyword::MACRO => Object::Routine,
            Keyword::TRIGGER => Object::Trigger,
            Keyword::INDEX => Object::Index,
            Keyword::SEQUENCE => Object::Sequence,
            Keyword::AS | Keyword::SELECT => return Some(None),
            _ => return None,
        };
        Some(Some((o, i)))
    })?
}

/// `ON table` after an index or trigger name (within the next tokens):
/// a read reference; returns the index after it.
fn on_target(b: &mut Builder<'_>, toks: &[Tok], from: usize, d: LocalDefId) -> usize {
    let Some(on) = (from..toks.len().min(from + 12)).find(|&i| is_kw(&toks[i], Keyword::ON)) else { return from };
    match read_name(toks, on + 1, b.dialect) {
        Some((name, after)) => {
            b.relation_ref(RefKind::Read, &name, Some(d));
            after
        }
        None => from,
    }
}

/// Columns of `CREATE TABLE name ( … )`: each top-level item whose first
/// token is a name and not a constraint keyword.
fn columns(b: &mut Builder<'_>, toks: &[Tok], open: usize, table: &str, parent: LocalDefId) {
    let mut depth = 0;
    let mut item_start = true;
    for (i, t) in toks.iter().enumerate().skip(open) {
        match t.token {
            Token::LParen => depth += 1,
            Token::RParen if depth == 1 => return,
            Token::RParen => depth -= 1,
            Token::Comma if depth == 1 => {
                item_start = true;
                continue;
            }
            _ => {}
        }
        if item_start && depth == 1 && i > open {
            item_start = false;
            column(b, toks, i, table, parent);
        }
    }
}

const CONSTRAINT_WORDS: &[Keyword] = &[
    Keyword::CONSTRAINT,
    Keyword::PRIMARY,
    Keyword::FOREIGN,
    Keyword::UNIQUE,
    Keyword::CHECK,
    Keyword::KEY,
    Keyword::INDEX,
    Keyword::FULLTEXT,
    Keyword::SPATIAL,
    Keyword::EXCLUDE,
    Keyword::PERIOD,
    Keyword::LIKE,
];

fn column(b: &mut Builder<'_>, toks: &[Tok], i: usize, table: &str, parent: LocalDefId) {
    let Token::Word(w) = &toks[i].token else { return };
    if w.quote_style.is_none() && CONSTRAINT_WORDS.contains(&w.keyword) {
        return;
    }
    let name = names::fold(b.dialect, &word_part(w));
    let range = toks[i].range;
    let end = item_end(toks, i);
    let def = NewDef {
        label: Label::Field,
        qualified_name: format!("{table}.{name}"),
        name,
        parent: Some(parent),
        range: (range.0, end),
        name_range: range,
        signature: Some(b.signature((range.0, end), false)),
        doc: None,
    };
    b.add_def(def);
}

/// End byte of the column item starting at token `i`.
fn item_end(toks: &[Tok], i: usize) -> usize {
    let mut depth = 0;
    let mut end = toks[i].range.1;
    for t in &toks[i..] {
        match t.token {
            Token::LParen => depth += 1,
            Token::RParen if depth == 0 => break,
            Token::RParen => depth -= 1,
            Token::Comma if depth == 0 => break,
            _ => {}
        }
        end = t.range.1;
    }
    end
}

/// Reads `a.b.c` at token `i`: the name and the index after it.
fn read_name(toks: &[Tok], mut i: usize, _dialect: Dialect) -> Option<(Name, usize)> {
    let mut name = Name { parts: Vec::new(), ranges: Vec::new() };
    loop {
        let t = toks.get(i)?;
        let Token::Word(w) = &t.token else { return None };
        name.parts.push(word_part(w));
        name.ranges.push(t.range);
        i += 1;
        if toks.get(i).is_some_and(|t| t.token == Token::Period)
            && toks.get(i + 1).is_some_and(|t| matches!(t.token, Token::Word(_)))
        {
            i += 1;
            continue;
        }
        return Some((name, i));
    }
}

fn word_part(w: &Word) -> Part {
    Part { text: w.value.clone(), quoted: w.quote_style.is_some() }
}

fn is_kw(t: &Tok, k: Keyword) -> bool {
    matches!(&t.token, Token::Word(w) if w.quote_style.is_none() && w.keyword == k)
}

fn keyword(t: Option<&Tok>) -> Option<Keyword> {
    match t.map(|t| &t.token) {
        Some(Token::Word(w)) if w.quote_style.is_none() => Some(w.keyword),
        _ => None,
    }
}

/// Keywords after which `name(` defines or types rather than calls.
const NOT_CALL_AFTER: &[Keyword] = &[
    Keyword::FUNCTION,
    Keyword::PROCEDURE,
    Keyword::PROC,
    Keyword::TABLE,
    Keyword::INTO,
    Keyword::VIEW,
    Keyword::INDEX,
    Keyword::ON,
    Keyword::REFERENCES,
    Keyword::TYPE,
    Keyword::KEY,
    Keyword::TRIGGER,
    Keyword::RETURNS,
    Keyword::EXISTS,
    Keyword::USING,
];

struct Scanner<'a, 'b> {
    b: &'a mut Builder<'b>,
    toks: &'a [Tok],
    enclosing: Option<LocalDefId>,
}

impl Scanner<'_, '_> {
    fn refs(&mut self, from: usize) {
        let mut i = from;
        while i < self.toks.len() {
            i = self.step(i);
        }
    }

    /// Handles token `i`; returns the next index.
    fn step(&mut self, i: usize) -> usize {
        let Some(k) = keyword(self.toks.get(i)) else { return i + 1 };
        match k {
            Keyword::FROM | Keyword::JOIN | Keyword::REFERENCES => self.relation(i, self.source_kind(i)),
            Keyword::INTO | Keyword::UPDATE => self.relation(i, RefKind::Write),
            Keyword::TABLE if self.after_alter_drop(i) => self.relation(i, RefKind::Write),
            Keyword::VIEW if keyword(i.checked_sub(1).and_then(|p| self.toks.get(p))) == Some(Keyword::DROP) => {
                self.relation(i, RefKind::Write)
            }
            Keyword::TRUNCATE if keyword(self.toks.get(i + 1)) != Some(Keyword::TABLE) => {
                self.relation(i, RefKind::Write)
            }
            Keyword::CALL | Keyword::EXEC | Keyword::EXECUTE => self.exec(i),
            Keyword::NoKeyword if self.is_word(i, "PERFORM") => self.exec(i),
            _ => self.maybe_call(i),
        }
    }

    fn is_word(&self, i: usize, upper: &str) -> bool {
        matches!(&self.toks[i].token, Token::Word(w) if w.quote_style.is_none() && w.value.eq_ignore_ascii_case(upper))
    }

    /// `DELETE FROM t` writes `t`; other `FROM` / `JOIN` read.
    fn source_kind(&self, i: usize) -> RefKind {
        let prev = i.checked_sub(1).and_then(|p| keyword(self.toks.get(p)));
        if prev == Some(Keyword::DELETE) { RefKind::Write } else { RefKind::Read }
    }

    fn after_alter_drop(&self, i: usize) -> bool {
        let prev = i.checked_sub(1).and_then(|p| keyword(self.toks.get(p)));
        matches!(prev, Some(Keyword::ALTER | Keyword::DROP | Keyword::TRUNCATE | Keyword::LOCK))
    }

    /// A relation name after keyword `i` (`IF [NOT] EXISTS`, `ONLY`,
    /// `LATERAL` skipped); `FROM a, b` reads both; `FROM f(…)` is a call.
    fn relation(&mut self, i: usize, kind: RefKind) -> usize {
        let mut j = i + 1;
        while matches!(
            keyword(self.toks.get(j)),
            Some(Keyword::IF | Keyword::NOT | Keyword::EXISTS | Keyword::ONLY | Keyword::LATERAL | Keyword::TABLE)
        ) {
            j += 1;
        }
        let Some((name, after)) = self.relation_name(j) else { return i + 1 };
        if self.toks.get(after).is_some_and(|t| t.token == Token::LParen)
            && kind == RefKind::Read
            && !is_kw(&self.toks[i], Keyword::REFERENCES)
        {
            self.b.call_ref(&name, self.enclosing);
            return after;
        }
        self.b.relation_ref(kind, &name, self.enclosing);
        let after = self.skip_alias(after);
        if is_kw(&self.toks[i], Keyword::FROM) && self.toks.get(after).is_some_and(|t| t.token == Token::Comma) {
            return self.relation(after, kind);
        }
        after
    }

    /// A name that is not a reserved word of the statement grammar.
    fn relation_name(&self, j: usize) -> Option<(Name, usize)> {
        let t = self.toks.get(j)?;
        let Token::Word(w) = &t.token else { return None };
        if w.quote_style.is_none() && is_clause_keyword(w.keyword) {
            return None;
        }
        read_name(self.toks, j, self.b.dialect)
    }

    /// `[AS] alias` after a relation.
    fn skip_alias(&self, mut j: usize) -> usize {
        if is_kw_at(self.toks, j, Keyword::AS) {
            j += 1;
        }
        match self.toks.get(j).map(|t| &t.token) {
            Some(Token::Word(w)) if w.quote_style.is_some() || !is_clause_keyword(w.keyword) => j + 1,
            _ => j,
        }
    }

    /// `CALL p(…)`, `EXEC [@r =] p …`, `EXECUTE FUNCTION f()`, `PERFORM f()`.
    fn exec(&mut self, i: usize) -> usize {
        let mut j = i + 1;
        if matches!(keyword(self.toks.get(j)), Some(Keyword::FUNCTION | Keyword::PROCEDURE)) {
            j += 1;
        }
        if let Some(Token::Word(w)) = self.toks.get(j).map(|t| &t.token)
            && w.value.starts_with('@')
            && self.toks.get(j + 1).is_some_and(|t| t.token == Token::Eq)
        {
            j += 2;
        }
        if matches!(keyword(self.toks.get(j)), Some(Keyword::IMMEDIATE | Keyword::SELECT)) {
            return j;
        }
        match self.relation_name(j) {
            Some((name, after)) => {
                self.b.call_ref(&name, self.enclosing);
                after
            }
            None => i + 1,
        }
    }

    /// `name(` that calls a routine.
    fn maybe_call(&mut self, i: usize) -> usize {
        let Some((name, after)) = read_name(self.toks, i, self.b.dialect) else { return i + 1 };
        if !self.toks.get(after).is_some_and(|t| t.token == Token::LParen) {
            return after;
        }
        let prev = i.checked_sub(1).and_then(|p| keyword(self.toks.get(p)));
        let after_period = i.checked_sub(1).is_some_and(|p| self.toks[p].token == Token::Period);
        let last =
            names::fold(self.b.dialect, name.parts.last().unwrap_or(&Part { text: String::new(), quoted: false }));
        // A one-part keyword followed by `(` is syntax (`PRIMARY KEY (`,
        // `VALUES (`), not a routine.
        let reserved = name.parts.len() == 1 && keyword(self.toks.get(i)).is_some_and(|k| k != Keyword::NoKeyword);
        if after_period || reserved || prev.is_some_and(|p| NOT_CALL_AFTER.contains(&p)) || builtins::is_type(&last) {
            return after;
        }
        self.b.call_ref(&name, self.enclosing);
        after
    }
}

fn is_kw_at(toks: &[Tok], j: usize, k: Keyword) -> bool {
    toks.get(j).is_some_and(|t| is_kw(t, k))
}

/// Keywords that start or continue a clause: never a relation or alias
/// name, never a call when followed by `(`.
fn is_clause_keyword(k: Keyword) -> bool {
    matches!(
        k,
        Keyword::SELECT
            | Keyword::FROM
            | Keyword::WHERE
            | Keyword::GROUP
            | Keyword::ORDER
            | Keyword::HAVING
            | Keyword::LIMIT
            | Keyword::OFFSET
            | Keyword::UNION
            | Keyword::EXCEPT
            | Keyword::INTERSECT
            | Keyword::JOIN
            | Keyword::INNER
            | Keyword::LEFT
            | Keyword::RIGHT
            | Keyword::FULL
            | Keyword::OUTER
            | Keyword::CROSS
            | Keyword::NATURAL
            | Keyword::ON
            | Keyword::USING
            | Keyword::SET
            | Keyword::VALUES
            | Keyword::WITH
            | Keyword::AS
            | Keyword::AND
            | Keyword::OR
            | Keyword::NOT
            | Keyword::IN
            | Keyword::EXISTS
            | Keyword::CASE
            | Keyword::WHEN
            | Keyword::THEN
            | Keyword::ELSE
            | Keyword::END
            | Keyword::BEGIN
            | Keyword::IF
            | Keyword::RETURN
            | Keyword::RETURNING
            | Keyword::WINDOW
            | Keyword::QUALIFY
            | Keyword::FOR
            | Keyword::WHILE
            | Keyword::INTO
            | Keyword::DECLARE
            | Keyword::CREATE
            | Keyword::INSERT
            | Keyword::UPDATE
            | Keyword::DELETE
            | Keyword::MERGE
            | Keyword::TABLE
            | Keyword::NULL
            | Keyword::DEFAULT
            | Keyword::PRIMARY
            | Keyword::FOREIGN
            | Keyword::CHECK
            | Keyword::UNIQUE
            | Keyword::CONSTRAINT
            | Keyword::REFERENCES
            | Keyword::OVER
            | Keyword::PARTITION
            | Keyword::FILTER
            | Keyword::WITHIN
            | Keyword::LATERAL
            | Keyword::ANY
            | Keyword::ALL
            | Keyword::SOME
            | Keyword::BETWEEN
            | Keyword::LIKE
            | Keyword::IS
            | Keyword::CAST
            | Keyword::RAISE
            | Keyword::RAISERROR
            | Keyword::PRINT
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::Dialect;

    fn scan(src: &str, d: Dialect) -> Builder<'_> {
        let mut b = Builder::new(src, d, false);
        statement(&mut b, src, (0, src.len()), None, None);
        b
    }

    #[test]
    fn recovers_definitions_and_references() {
        let src = "CREATE PROCEDURE insert_data(a int) BEGIN INSERT INTO tbl VALUES (a); SELECT x FROM other o, third; CALL helper(1); END";
        let b = scan(src, Dialect::MySql);
        let d = &b.facts.definitions;
        assert_eq!((d[0].label, d[0].name.as_str()), (Label::Function, "insert_data"));
        let refs: Vec<(RefKind, &str)> = b.facts.references.iter().map(|r| (r.kind, r.name.as_str())).collect();
        assert_eq!(
            refs,
            [(RefKind::Write, "tbl"), (RefKind::Read, "other"), (RefKind::Read, "third"), (RefKind::Call, "helper")]
        );
        assert!(b.facts.references.iter().all(|r| r.enclosing == Some(0)));
    }

    #[test]
    fn table_columns_and_foreign_keys() {
        let src = "CREATE TABLE IF NOT EXISTS app.orders (id INT, customer_id INT REFERENCES customers(id), PRIMARY KEY (id), total DECIMAL(10, 2))";
        let b = scan(src, Dialect::Generic);
        let names: Vec<&str> = b.facts.definitions.iter().map(|d| d.qualified_name.as_str()).collect();
        assert_eq!(names, ["app.orders", "app.orders.id", "app.orders.customer_id", "app.orders.total"]);
        let refs: Vec<&str> = b.facts.references.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(refs, ["customers"]);
    }

    #[test]
    fn trigger_and_index_targets() {
        let b = scan(
            "CREATE TRIGGER trg AFTER INSERT ON film FOR EACH ROW EXECUTE PROCEDURE film_fulltext()",
            Dialect::Postgres,
        );
        let refs: Vec<(RefKind, &str)> = b.facts.references.iter().map(|r| (r.kind, r.name.as_str())).collect();
        assert_eq!(refs, [(RefKind::Read, "film"), (RefKind::Call, "film_fulltext")]);
        let b = scan("CREATE UNIQUE INDEX idx ON dbo.t (a)", Dialect::MsSql);
        assert_eq!(b.facts.definitions[0].label, Label::Constant);
        assert_eq!(b.facts.references[0].name, "t");
    }
}
