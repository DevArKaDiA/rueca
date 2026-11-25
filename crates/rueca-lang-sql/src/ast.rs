//! Facts from a statement sqlparser parsed: definitions by statement kind,
//! then every relation, column and call in it through sqlparser's visitor
//! (crate docs, *Definitions*, *References*).

use std::collections::HashSet;
use std::ops::ControlFlow;

use rueca_model::{Label, LocalDefId, RefKind};
use sqlparser::ast::{
    AlterTableOperation, AssignmentTarget, ColumnDef, ColumnOption, CreateFunctionBody, CreateIndex, CreateTable,
    CreateTrigger, Expr, ForeignKeyConstraint, FromTable, Ident, ObjectName, ObjectNamePart, ObjectType, Query, Select,
    Spanned, Statement, TableConstraint, TableFactor, TableObject, TableWithJoins, UserDefinedTypeRepresentation,
    Value, Visit, Visitor,
};
use sqlparser::tokenizer::Span as SqlSpan;

use crate::facts::{Builder, Name, NewDef};
use crate::names::{self, Part};
use crate::text::Lines;

/// Converts sqlparser locations of one parsed chunk to file bytes.
pub struct Pos<'c> {
    pub lines: Lines<'c>,
    /// File offset of the chunk's first byte.
    pub base: usize,
}

impl Pos<'_> {
    pub fn range(&self, span: SqlSpan) -> Option<(usize, usize)> {
        if span.start.line == 0 {
            return None;
        }
        let s = self.base + self.lines.byte_at(span.start.line, span.start.column);
        let e = self.base + self.lines.byte_at(span.end.line, span.end.column);
        Some((s, e.max(s)))
    }

    pub fn name(&self, obj: &ObjectName) -> Option<Name> {
        let mut parts = Vec::new();
        let mut ranges = Vec::new();
        for p in &obj.0 {
            let ObjectNamePart::Identifier(id) = p else { return None };
            parts.push(part(id));
            ranges.push(self.range(id.span)?);
        }
        (!parts.is_empty()).then_some(Name { parts, ranges })
    }

    fn ident(&self, id: &Ident) -> Option<Name> {
        Some(Name { parts: vec![part(id)], ranges: vec![self.range(id.span)?] })
    }
}

fn part(id: &Ident) -> Part {
    Part { text: id.value.clone(), quoted: id.quote_style.is_some() }
}

/// Where a statement sits in the file.
pub struct Ctx {
    /// The definition the statement belongs to (dbt model, named query,
    /// snapshot), if any.
    pub enclosing: Option<LocalDefId>,
    /// Byte range of the statement.
    pub range: (usize, usize),
    /// Doc from the comments before the statement.
    pub doc: Option<String>,
}

/// A routine body sqlparser keeps as a string (PostgreSQL `$$ … $$`): its
/// byte range and the routine, scanned by [`crate::scan`].
pub struct OpaqueBody {
    pub range: (usize, usize),
    pub enclosing: LocalDefId,
}

/// Adds the facts of one parsed statement; returns routine bodies left
/// to the token scanner.
pub fn statement(b: &mut Builder<'_>, pos: &Pos<'_>, stmt: &Statement, ctx: &Ctx) -> Vec<OpaqueBody> {
    let mut bodies = Vec::new();
    let def = define(b, pos, stmt, ctx, &mut bodies);
    let enclosing = def.map(|(d, _)| d).or(ctx.enclosing);
    let skip: HashSet<usize> = def.and_then(|(_, s)| s).into_iter().collect();
    let mut w = walker(b, pos, enclosing, skip);
    let _ = stmt.visit(&mut w);
    bodies
}

/// Adds the references of one parsed statement inside `enclosing` (a
/// statement of a routine body: it defines nothing of its own).
pub fn references(b: &mut Builder<'_>, pos: &Pos<'_>, stmt: &Statement, enclosing: Option<LocalDefId>) {
    let mut w = walker(b, pos, enclosing, HashSet::new());
    let _ = stmt.visit(&mut w);
}

/// Adds the references of one parsed expression (a condition or a value
/// of a procedural statement).
pub fn expression(b: &mut Builder<'_>, pos: &Pos<'_>, expr: &Expr, enclosing: Option<LocalDefId>) {
    let mut w = walker(b, pos, enclosing, HashSet::new());
    let _ = expr.visit(&mut w);
}

fn walker<'a, 'b, 'p>(
    b: &'a mut Builder<'b>,
    pos: &'a Pos<'p>,
    enclosing: Option<LocalDefId>,
    skip: HashSet<usize>,
) -> Walker<'a, 'b, 'p> {
    Walker { b, pos, enclosing, skip, writes: HashSet::new(), scopes: Vec::new(), ctes: Vec::new(), pushed: Vec::new() }
}

// ---------------------------------------------------------------------------
// Definitions
// ---------------------------------------------------------------------------

/// The statement's own definition and the byte where its name starts (the
/// visitor sees that name as a relation and must skip it).
fn define(
    b: &mut Builder<'_>,
    pos: &Pos<'_>,
    stmt: &Statement,
    ctx: &Ctx,
    bodies: &mut Vec<OpaqueBody>,
) -> Option<(LocalDefId, Option<usize>)> {
    match stmt {
        Statement::CreateTable(t) => table(b, pos, t, ctx),
        Statement::CreateView(v) => relation(b, pos, &v.name, ctx),
        Statement::CreateVirtualTable { name, .. } => relation(b, pos, name, ctx),
        Statement::CreateFunction(f) => {
            let d = top_def(b, pos, Label::Function, &f.name, ctx)?;
            opaque_body(b, pos, f.function_body.as_ref(), d, bodies);
            Some((d, None))
        }
        Statement::CreateProcedure { name, .. } | Statement::CreateMacro { name, .. } => {
            top_def(b, pos, Label::Function, name, ctx).map(|d| (d, None))
        }
        Statement::CreateTrigger(t) => trigger(b, pos, t, ctx),
        Statement::CreateIndex(i) => index(b, pos, i, ctx),
        Statement::CreateSequence { name, .. } => top_def(b, pos, Label::Variable, name, ctx).map(|d| (d, None)),
        Statement::CreateType { name, representation } => user_type(b, pos, name, representation.as_ref(), ctx),
        Statement::CreateDomain(d) => top_def(b, pos, Label::TypeAlias, &d.name, ctx).map(|d| (d, None)),
        Statement::AlterTable(a) => {
            alter_table(b, pos, &a.name, &a.operations, ctx);
            None
        }
        _ => None,
    }
}

/// A top-level definition named by `obj`, spanning the statement.
fn top_def(b: &mut Builder<'_>, pos: &Pos<'_>, label: Label, obj: &ObjectName, ctx: &Ctx) -> Option<LocalDefId> {
    let name = pos.name(obj)?;
    let segs = b.segments(&name);
    let short = segs.last()?.clone();
    let def = NewDef {
        label,
        name: short,
        qualified_name: segs.join("."),
        parent: None,
        range: ctx.range,
        name_range: name.last_range(),
        signature: Some(b.signature(ctx.range, true)),
        doc: ctx.doc.clone(),
    };
    Some(b.add_def(def))
}

fn relation(b: &mut Builder<'_>, pos: &Pos<'_>, obj: &ObjectName, ctx: &Ctx) -> Option<(LocalDefId, Option<usize>)> {
    let d = top_def(b, pos, Label::Struct, obj, ctx)?;
    let start = pos.name(obj).map(|n| n.range().0);
    Some((d, start))
}

fn table(b: &mut Builder<'_>, pos: &Pos<'_>, t: &CreateTable, ctx: &Ctx) -> Option<(LocalDefId, Option<usize>)> {
    let (d, start) = relation(b, pos, &t.name, ctx)?;
    let qn = b.qn(d).to_string();
    // PostgreSQL `INHERITS (parent)` / `PARTITION OF parent`.
    let parents = t.inherits.iter().flatten().chain(t.partition_of.iter());
    for p in parents.filter_map(|p| pos.name(p)) {
        b.relation_ref(RefKind::Read, &p, Some(d));
    }
    for c in &t.columns {
        column(b, pos, c, &qn, Some(d));
        for o in &c.options {
            if let ColumnOption::ForeignKey(fk) = &o.option {
                foreign_key(b, pos, fk, Some(d));
            }
        }
    }
    for c in &t.constraints {
        if let TableConstraint::ForeignKey(fk) = c {
            foreign_key(b, pos, fk, Some(d));
        }
    }
    Some((d, start))
}

/// A column (`Field`) of relation `table`, child of `parent` when the
/// relation is defined in this file.
fn column(b: &mut Builder<'_>, pos: &Pos<'_>, c: &ColumnDef, table: &str, parent: Option<LocalDefId>) {
    let Some(name_range) = pos.range(c.name.span) else { return };
    let name = names::fold(b.dialect, &part(&c.name));
    let end = column_end(b.source, name_range.0);
    let def = NewDef {
        label: Label::Field,
        qualified_name: format!("{table}.{name}"),
        name,
        parent,
        range: (name_range.0, end),
        name_range,
        signature: Some(b.signature((name_range.0, end), false)),
        doc: None,
    };
    b.add_def(def);
}

/// End of a column definition starting at `start`: the next `,` or `)` at
/// depth 0, or the end of the line.
fn column_end(source: &str, start: usize) -> usize {
    let mut depth = 0i32;
    for (i, c) in source[start..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' if depth == 0 => return start + i,
            ')' => depth -= 1,
            ',' if depth == 0 => return start + i,
            '\n' => return start + i,
            _ => {}
        }
    }
    source.len()
}

fn foreign_key(b: &mut Builder<'_>, pos: &Pos<'_>, fk: &ForeignKeyConstraint, enclosing: Option<LocalDefId>) {
    let Some(target) = pos.name(&fk.foreign_table) else { return };
    b.relation_ref(RefKind::Read, &target, enclosing);
    let qn = b.segments(&target).join(".");
    for c in &fk.referred_columns {
        if let Some(r) = pos.range(c.span) {
            b.column_ref(RefKind::Read, &qn, &part(c), enclosing, r);
        }
    }
}

/// PostgreSQL bodies in a string (`AS $$ … $$`, `AS '…'`).
fn opaque_body(
    b: &Builder<'_>,
    pos: &Pos<'_>,
    body: Option<&CreateFunctionBody>,
    d: LocalDefId,
    out: &mut Vec<OpaqueBody>,
) {
    let expr = match body {
        Some(CreateFunctionBody::AsBeforeOptions { body, .. } | CreateFunctionBody::AsAfterOptions(body)) => body,
        _ => return,
    };
    let Expr::Value(v) = expr else { return };
    let Some((s, e)) = pos.range(v.span) else { return };
    let inner = match &v.value {
        Value::DollarQuotedString(q) => {
            let tag = q.tag.as_ref().map_or(0, String::len) + 2;
            (s + tag, e.saturating_sub(tag))
        }
        Value::SingleQuotedString(_) => (s + 1, e.saturating_sub(1)),
        _ => return,
    };
    if inner.1 > inner.0 && inner.1 <= b.source.len() {
        out.push(OpaqueBody { range: inner, enclosing: d });
    }
}

/// A trigger is qualified by its table (`actor.last_updated`): PostgreSQL
/// names triggers per table.
fn trigger(b: &mut Builder<'_>, pos: &Pos<'_>, t: &CreateTrigger, ctx: &Ctx) -> Option<(LocalDefId, Option<usize>)> {
    let d = top_def(b, pos, Label::Function, &t.name, ctx)?;
    if let Some(table) = pos.name(&t.table_name) {
        let qn = format!("{}.{}", b.segments(&table).join("."), b.facts.definitions[d as usize].name);
        b.facts.definitions[d as usize].qualified_name = qn;
        b.relation_ref(RefKind::Read, &table, Some(d));
    }
    if let Some(f) = t.exec_body.as_ref().and_then(|e| pos.name(&e.func_desc.name)) {
        b.call_ref(&f, Some(d));
    }
    Some((d, None))
}

fn index(b: &mut Builder<'_>, pos: &Pos<'_>, i: &CreateIndex, ctx: &Ctx) -> Option<(LocalDefId, Option<usize>)> {
    let d = i.name.as_ref().and_then(|n| top_def(b, pos, Label::Constant, n, ctx));
    let enclosing = d.or(ctx.enclosing);
    if let Some(table) = pos.name(&i.table_name) {
        let qn = b.segments(&table).join(".");
        for c in &i.columns {
            if let Expr::Identifier(id) = &c.column.expr
                && let Some(r) = pos.range(id.span)
            {
                b.column_ref(RefKind::Read, &qn, &part(id), enclosing, r);
            }
        }
    }
    d.map(|d| (d, None))
}

fn user_type(
    b: &mut Builder<'_>,
    pos: &Pos<'_>,
    name: &ObjectName,
    repr: Option<&UserDefinedTypeRepresentation>,
    ctx: &Ctx,
) -> Option<(LocalDefId, Option<usize>)> {
    let Some(UserDefinedTypeRepresentation::Enum { labels }) = repr else {
        return top_def(b, pos, Label::TypeAlias, name, ctx).map(|d| (d, None));
    };
    let d = top_def(b, pos, Label::Enum, name, ctx)?;
    let qn = b.qn(d).to_string();
    for l in labels {
        let Some(r) = pos.range(l.span) else { continue };
        let def = NewDef {
            label: Label::Constant,
            name: l.value.clone(),
            qualified_name: format!("{qn}.{}", l.value),
            parent: Some(d),
            range: r,
            name_range: r,
            signature: Some(b.signature(r, false)),
            doc: None,
        };
        b.add_def(def);
    }
    Some((d, None))
}

/// `ALTER TABLE t ADD COLUMN c …` adds a member of `t` (a child when `t`
/// is defined in this file, else a top-level `t.c`); added foreign keys
/// are references.
fn alter_table(b: &mut Builder<'_>, pos: &Pos<'_>, obj: &ObjectName, ops: &[AlterTableOperation], ctx: &Ctx) {
    let Some(name) = pos.name(obj) else { return };
    let qn = b.segments(&name).join(".");
    let parent = b.relations.get(&qn).copied();
    for op in ops {
        match op {
            AlterTableOperation::AddColumn { column_def, .. } => column(b, pos, column_def, &qn, parent),
            AlterTableOperation::AddConstraint { constraint: TableConstraint::ForeignKey(fk), .. } => {
                foreign_key(b, pos, fk, ctx.enclosing)
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// References
// ---------------------------------------------------------------------------

/// Relations visible to column references in one `SELECT` (or the target
/// of an `UPDATE` / `DELETE`).
#[derive(Default)]
struct Scope {
    /// (alias or table name, the table's qualified name; `None` for a CTE
    /// or a derived table).
    entries: Vec<(String, Option<String>)>,
    /// A source that is not a named relation (subquery, table function).
    opaque: bool,
}

struct Walker<'a, 'b, 'p> {
    b: &'a mut Builder<'b>,
    pos: &'a Pos<'p>,
    enclosing: Option<LocalDefId>,
    /// First bytes of names that are the statement's own definition.
    skip: HashSet<usize>,
    /// First bytes of relation names written to (`INSERT INTO`, `UPDATE`,
    /// `DELETE`, `MERGE INTO`, `ALTER`, `TRUNCATE`).
    writes: HashSet<usize>,
    scopes: Vec<Scope>,
    /// CTE names visible, per query.
    ctes: Vec<Vec<String>>,
    /// Per visited statement: whether it pushed a scope.
    pushed: Vec<bool>,
}

impl Visitor for Walker<'_, '_, '_> {
    type Break = ();

    fn pre_visit_statement(&mut self, stmt: &Statement) -> ControlFlow<()> {
        self.unvisited_refs(stmt);
        let scope = self.statement_targets(stmt);
        self.pushed.push(scope.is_some());
        if let Some(s) = scope {
            self.scopes.push(s);
        }
        ControlFlow::Continue(())
    }

    fn post_visit_statement(&mut self, _stmt: &Statement) -> ControlFlow<()> {
        if self.pushed.pop() == Some(true) {
            self.scopes.pop();
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_query(&mut self, q: &Query) -> ControlFlow<()> {
        let names: Vec<String> = q
            .with
            .iter()
            .flat_map(|w| &w.cte_tables)
            .map(|c| names::fold(self.b.dialect, &part(&c.alias.name)))
            .collect();
        self.define_ctes(q);
        self.ctes.push(names);
        ControlFlow::Continue(())
    }

    fn post_visit_query(&mut self, _q: &Query) -> ControlFlow<()> {
        self.ctes.pop();
        ControlFlow::Continue(())
    }

    fn pre_visit_select(&mut self, s: &Select) -> ControlFlow<()> {
        let scope = self.scope_of(&s.from);
        self.scopes.push(scope);
        ControlFlow::Continue(())
    }

    fn post_visit_select(&mut self, _s: &Select) -> ControlFlow<()> {
        self.scopes.pop();
        ControlFlow::Continue(())
    }

    /// `FROM f(x)`: a table-valued function call, not a relation.
    fn pre_visit_table_factor(&mut self, f: &TableFactor) -> ControlFlow<()> {
        if let TableFactor::Table { name, args: Some(_), .. } = f
            && let Some(n) = self.pos.name(name)
        {
            self.skip.insert(n.range().0);
            self.b.call_ref(&n, self.enclosing);
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_relation(&mut self, obj: &ObjectName) -> ControlFlow<()> {
        let Some(name) = self.pos.name(obj) else { return ControlFlow::Continue(()) };
        let start = name.range().0;
        if self.skip.contains(&start) {
            return ControlFlow::Continue(());
        }
        if let Some(cte) = self.cte_name(&name) {
            if self.enclosing.is_some() {
                self.b.add_ref(RefKind::Read, cte, None, self.enclosing, name.range());
            }
            return ControlFlow::Continue(());
        }
        let kind = if self.writes.contains(&start) { RefKind::Write } else { RefKind::Read };
        self.b.relation_ref(kind, &name, self.enclosing);
        ControlFlow::Continue(())
    }

    fn pre_visit_expr(&mut self, e: &Expr) -> ControlFlow<()> {
        match e {
            Expr::Function(f) => {
                if let Some(n) = self.pos.name(&f.name) {
                    self.b.call_ref(&n, self.enclosing);
                }
            }
            Expr::Identifier(id) => self.unqualified_column(id),
            Expr::CompoundIdentifier(ids) => self.qualified_column(ids),
            _ => {}
        }
        ControlFlow::Continue(())
    }
}

impl Walker<'_, '_, '_> {
    /// References the visitor does not reach as relations or expressions:
    /// `DROP TABLE` / `VIEW` (writes), `CALL p()`, `EXEC p` (calls).
    fn unvisited_refs(&mut self, stmt: &Statement) {
        let names: Vec<&ObjectName> = match stmt {
            Statement::Drop {
                object_type: ObjectType::Table | ObjectType::View | ObjectType::MaterializedView,
                names,
                ..
            } => {
                for n in names.iter().filter_map(|n| self.pos.name(n)) {
                    self.b.relation_ref(RefKind::Write, &n, self.enclosing);
                }
                return;
            }
            Statement::Call(f) => vec![&f.name],
            Statement::Execute { name: Some(name), .. } => vec![name],
            _ => return,
        };
        for n in names.into_iter().filter_map(|n| self.pos.name(n)) {
            self.b.call_ref(&n, self.enclosing);
        }
    }

    /// Write targets of `stmt` (recorded) and, for `UPDATE` / `DELETE`, the
    /// scope its columns are read in; `INSERT` / `UPDATE` column lists are
    /// column writes.
    fn statement_targets(&mut self, stmt: &Statement) -> Option<Scope> {
        match stmt {
            Statement::Insert(i) => {
                self.insert_targets(&i.table, &i.columns);
                None
            }
            Statement::Update(u) => {
                self.write_factor(&u.table.relation);
                let table = self.factor_table(&u.table.relation);
                for a in &u.assignments {
                    self.assignment(&a.target, table.as_deref());
                }
                Some(self.scope_of(std::slice::from_ref(&u.table)))
            }
            Statement::Delete(d) => Some(self.delete_targets(&d.tables, &d.from)),
            Statement::Merge(m) => {
                self.write_factor(&m.table);
                None
            }
            Statement::AlterTable(a) => {
                self.write_name(&a.name);
                None
            }
            Statement::Truncate(t) => {
                for n in &t.table_names {
                    self.write_name(&n.name);
                }
                None
            }
            _ => None,
        }
    }

    fn insert_targets(&mut self, table: &TableObject, columns: &[ObjectName]) {
        let TableObject::TableName(obj) = table else { return };
        self.write_name(obj);
        let Some(name) = self.pos.name(obj) else { return };
        let qn = self.b.segments(&name).join(".");
        for c in columns.iter().filter_map(|c| self.pos.name(c)) {
            if let [p] = c.parts.as_slice() {
                self.b.column_ref(RefKind::Write, &qn, p, self.enclosing, c.range());
            }
        }
    }

    fn assignment(&mut self, target: &AssignmentTarget, table: Option<&str>) {
        let (AssignmentTarget::ColumnName(obj), Some(table)) = (target, table) else { return };
        let Some(c) = self.pos.name(obj) else { return };
        if let Some(p) = c.parts.last() {
            self.b.column_ref(RefKind::Write, table, p, self.enclosing, c.last_range());
        }
    }

    fn delete_targets(&mut self, tables: &[ObjectName], from: &FromTable) -> Scope {
        let from = match from {
            FromTable::WithFromKeyword(v) | FromTable::WithoutKeyword(v) => v,
        };
        for t in tables {
            self.write_name(t);
        }
        if tables.is_empty()
            && let Some(first) = from.first()
        {
            self.write_factor(&first.relation);
        }
        self.scope_of(from)
    }

    fn write_name(&mut self, obj: &ObjectName) {
        if let Some(n) = self.pos.name(obj) {
            self.writes.insert(n.range().0);
        }
    }

    fn write_factor(&mut self, f: &TableFactor) {
        if let TableFactor::Table { name, .. } = f {
            self.write_name(name);
        }
    }

    /// The qualified name of the table a factor names (not a CTE).
    fn factor_table(&self, f: &TableFactor) -> Option<String> {
        let TableFactor::Table { name, .. } = f else { return None };
        let n = self.pos.name(name)?;
        if self.cte_name(&n).is_some() {
            return None;
        }
        Some(self.b.segments(&n).join("."))
    }

    fn scope_of(&self, from: &[TableWithJoins]) -> Scope {
        let mut scope = Scope::default();
        let factors = from.iter().flat_map(|t| std::iter::once(&t.relation).chain(t.joins.iter().map(|j| &j.relation)));
        for f in factors {
            self.add_factor(&mut scope, f);
        }
        scope
    }

    fn add_factor(&self, scope: &mut Scope, f: &TableFactor) {
        let (TableFactor::Table { name, alias, args: None, .. }, Some(n)) = (f, self.factor_name(f)) else {
            scope.opaque = true;
            if let Some(a) = factor_alias(f) {
                scope.entries.push((names::fold(self.b.dialect, &part(a)), None));
            }
            return;
        };
        let _ = name;
        let table = self.factor_table(f);
        let key = match alias {
            Some(a) => names::fold(self.b.dialect, &part(&a.name)),
            None => self.b.segments(&n).pop().unwrap_or_default(),
        };
        scope.entries.push((key, table));
    }

    fn factor_name(&self, f: &TableFactor) -> Option<Name> {
        match f {
            TableFactor::Table { name, .. } => self.pos.name(name),
            _ => None,
        }
    }

    /// The CTE a one-part relation name refers to, if any.
    fn cte_name(&self, name: &Name) -> Option<String> {
        let [p] = name.parts.as_slice() else { return None };
        let n = names::fold(self.b.dialect, p);
        self.ctes.iter().rev().any(|c| c.contains(&n)).then_some(n)
    }

    /// CTEs of a query directly in a definition are its local definitions
    /// (`Variable`, child of the definition).
    fn define_ctes(&mut self, q: &Query) {
        let (Some(with), Some(parent)) = (&q.with, self.enclosing) else { return };
        let parent_qn = self.b.qn(parent).to_string();
        for c in &with.cte_tables {
            let Some(name) = self.pos.ident(&c.alias.name) else { continue };
            let Some(range) = self.pos.range(c.span()) else { continue };
            let short = names::fold(self.b.dialect, &name.parts[0]);
            let def = NewDef {
                label: Label::Variable,
                qualified_name: format!("{parent_qn}.{short}"),
                name: short,
                parent: Some(parent),
                range: (name.range().0, range.1),
                name_range: name.range(),
                signature: Some(self.b.signature(name.range(), false)),
                doc: None,
            };
            self.b.add_def(def);
        }
    }

    /// `col` with exactly one named table in the innermost scope.
    fn unqualified_column(&mut self, id: &Ident) {
        let Some(scope) = self.scopes.last() else { return };
        let [(_, Some(table))] = scope.entries.as_slice() else { return };
        if scope.opaque || id.quote_style == Some('\'') {
            return;
        }
        let table = table.clone();
        if let Some(r) = self.pos.range(id.span) {
            self.b.column_ref(RefKind::Read, &table, &part(id), self.enclosing, r);
        }
    }

    /// `alias.col` through the scopes (innermost first), `schema.table.col`
    /// by name.
    fn qualified_column(&mut self, ids: &[Ident]) {
        let (table, col) = match ids {
            [q, c] => {
                let key = names::fold(self.b.dialect, &part(q));
                let hit = self.scopes.iter().rev().find_map(|s| s.entries.iter().find(|(k, _)| *k == key));
                match hit {
                    Some((_, Some(t))) => (t.clone(), c),
                    _ => return,
                }
            }
            [s, t, c] => (names::segments(self.b.dialect, &[part(s), part(t)]).join("."), c),
            _ => return,
        };
        if let Some(r) = self.pos.range(col.span) {
            self.b.column_ref(RefKind::Read, &table, &part(col), self.enclosing, r);
        }
    }
}

fn factor_alias(f: &TableFactor) -> Option<&Ident> {
    match f {
        TableFactor::Table { alias, .. } | TableFactor::Derived { alias, .. } | TableFactor::Function { alias, .. } => {
            alias.as_ref().map(|a| &a.name)
        }
        _ => None,
    }
}
