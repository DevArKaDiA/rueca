//! Building [`FileFacts`]: definitions, references and the positions they
//! come from.

use std::collections::HashMap;

use rueca_extract::{first_line, type_receiver};
use rueca_model::{Definition, FileFacts, Label, LocalDefId, RefKind, Reference, Span};

use crate::dialect::Dialect;
use crate::names::{self, Part};
use crate::text::Lines;

/// Signature length cap, in characters.
const MAX_SIGNATURE_CHARS: usize = 200;
/// Doc length cap, in characters.
pub const MAX_DOC_CHARS: usize = 500;

/// A Jinja placeholder identifier (only underscores).
pub fn is_placeholder(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b == b'_')
}

/// A qualified name as written: its parts and the byte range of each.
#[derive(Debug, Clone)]
pub struct Name {
    pub parts: Vec<Part>,
    pub ranges: Vec<(usize, usize)>,
}

impl Name {
    /// Byte range from the first part to the last.
    pub fn range(&self) -> (usize, usize) {
        let s = self.ranges.first().map_or(0, |r| r.0);
        let e = self.ranges.last().map_or(s, |r| r.1);
        (s, e.max(s))
    }

    /// Byte range of the last part (the object's own name).
    pub fn last_range(&self) -> (usize, usize) {
        self.ranges.last().copied().unwrap_or((0, 0))
    }
}

/// A definition to add.
pub struct NewDef {
    pub label: Label,
    pub name: String,
    pub qualified_name: String,
    pub parent: Option<LocalDefId>,
    pub range: (usize, usize),
    pub name_range: (usize, usize),
    pub signature: Option<String>,
    pub doc: Option<String>,
}

/// Accumulates the facts of one file.
pub struct Builder<'s> {
    pub source: &'s str,
    pub lines: Lines<'s>,
    pub dialect: Dialect,
    pub is_test: bool,
    pub facts: FileFacts,
    /// Tables and views defined in this file, by qualified name (for
    /// `ALTER TABLE … ADD COLUMN` members).
    pub relations: HashMap<String, LocalDefId>,
    /// Parameters and variables of the routine being read (normalized):
    /// never columns.
    pub locals: std::collections::HashSet<String>,
}

impl<'s> Builder<'s> {
    pub fn new(source: &'s str, dialect: Dialect, is_test: bool) -> Builder<'s> {
        Builder {
            source,
            lines: Lines::new(source),
            dialect,
            is_test,
            facts: FileFacts::default(),
            relations: HashMap::new(),
            locals: std::collections::HashSet::new(),
        }
    }

    pub fn span(&self, (s, e): (usize, usize)) -> Span {
        self.lines.span(s.min(self.source.len()), e.min(self.source.len()))
    }

    /// Normalized segments of a written name.
    pub fn segments(&self, name: &Name) -> Vec<String> {
        names::segments(self.dialect, &name.parts)
    }

    pub fn add_def(&mut self, d: NewDef) -> LocalDefId {
        let id = self.facts.definitions.len() as LocalDefId;
        let is_relation = matches!(d.label, Label::Struct) && d.parent.is_none();
        if is_relation {
            self.relations.entry(d.qualified_name.clone()).or_insert(id);
        }
        self.facts.definitions.push(Definition {
            label: d.label,
            name: d.name,
            qualified_name: d.qualified_name,
            parent: d.parent,
            span: self.span(d.range),
            name_span: self.span(d.name_range),
            signature: d.signature,
            doc: d.doc,
            decorators: Vec::new(),
            bases: Vec::new(),
            implements: Vec::new(),
            exported: true,
            is_test: self.is_test,
            is_declaration: false,
        });
        id
    }

    /// The qualified name of a definition of this file.
    pub fn qn(&self, id: LocalDefId) -> &str {
        &self.facts.definitions[id as usize].qualified_name
    }

    pub fn add_ref(
        &mut self,
        kind: RefKind,
        name: String,
        receiver: Option<String>,
        enclosing: Option<LocalDefId>,
        range: (usize, usize),
    ) {
        if name.is_empty() || is_placeholder(&name) {
            return;
        }
        let span = self.span(range);
        self.facts.references.push(Reference { kind, name, receiver, enclosing, span, arity: None });
    }

    /// A reference to a relation (table or view) by its written name.
    pub fn relation_ref(&mut self, kind: RefKind, name: &Name, enclosing: Option<LocalDefId>) {
        let mut segs = self.segments(name);
        let Some(last) = segs.pop() else { return };
        if last == "this" || is_placeholder(&last) || last.starts_with(['#', '@', ':', '$']) {
            return;
        }
        // A schema written through Jinja (`{{ target.schema }}.t`) is unknown.
        segs.retain(|s| !s.starts_with("__"));
        let receiver = (!segs.is_empty()).then(|| segs.join("."));
        self.add_ref(kind, last, receiver, enclosing, name.range());
    }

    /// A reference to column `column` of the relation named `table`
    /// (normalized qualified name): receiver `$type:<table>`.
    pub fn column_ref(
        &mut self,
        kind: RefKind,
        table: &str,
        column: &Part,
        enclosing: Option<LocalDefId>,
        range: (usize, usize),
    ) {
        let col = names::fold(self.dialect, column);
        if col.starts_with(['@', ':', '$', '#']) || self.locals.contains(&col) {
            return;
        }
        self.add_ref(kind, col, Some(type_receiver(table)), enclosing, range);
    }

    /// A call of a routine by its written name; built-ins are skipped.
    pub fn call_ref(&mut self, name: &Name, enclosing: Option<LocalDefId>) {
        let mut segs = self.segments(name);
        let Some(last) = segs.pop() else { return };
        if segs.is_empty() && crate::builtins::is_function(&last) {
            return;
        }
        if last.starts_with(['@', '#']) || last.starts_with("sp_executesql") {
            return;
        }
        let receiver = (!segs.is_empty()).then(|| segs.join("."));
        self.add_ref(RefKind::Call, last, receiver, enclosing, name.range());
    }

    /// One-line signature of the text at `range`, with the dialect for
    /// top-level definitions (`CREATE TABLE users ( -- postgres`).
    pub fn signature(&self, range: (usize, usize), with_dialect: bool) -> String {
        let text = &self.source[range.0.min(self.source.len())..range.1.min(self.source.len())];
        let line = first_line(text, MAX_SIGNATURE_CHARS);
        if with_dialect { format!("{line} -- {}", self.dialect.as_str()) } else { line }
    }

    /// The comment block at `range` as a doc: comment markers dropped,
    /// lines trimmed and joined.
    pub fn doc(&self, range: Option<(usize, usize)>) -> Option<String> {
        let (s, e) = range?;
        let text = self.source.get(s..e)?;
        let lines: Vec<&str> = text
            .lines()
            .map(|l| {
                l.trim()
                    .trim_start_matches("--")
                    .trim_start_matches("/*")
                    .trim_end_matches("*/")
                    .trim_start_matches('*')
                    .trim()
            })
            .filter(|l| !l.is_empty())
            .collect();
        let doc: String = lines.join(" ").chars().take(MAX_DOC_CHARS).collect();
        (!doc.is_empty()).then_some(doc)
    }

    /// The innermost definition whose range contains `range`, if any.
    pub fn enclosing_of(&self, range: (usize, usize)) -> Option<LocalDefId> {
        self.facts
            .definitions
            .iter()
            .enumerate()
            .filter(|(_, d)| {
                matches!(d.label, Label::Function | Label::Struct | Label::Test)
                    && (d.span.start_byte as usize) <= range.0
                    && range.1 <= d.span.end_byte as usize
            })
            .min_by_key(|(_, d)| d.span.end_byte - d.span.start_byte)
            .map(|(i, _)| i as LocalDefId)
    }
}
