//! Facts builder ([`Out`]) and the generic tree → facts walk (RFC 0003
//! §4.4–§4.6): every mapping entry becomes a `Key` with its key path, every
//! mapping element of an array an `[N]` (or identity-keyed) `Key`; scalars
//! become previews. YAML anchors, aliases and merge keys and `$ref`
//! pointers become `DataRef` facts and bases. File kinds (`kinds`) refine
//! the result afterwards through [`Ctx`].

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use rueca_extract::DATA_REF_QN;
use rueca_model::{Definition, FileFacts, Import, Label, LocalDefId, RefKind, Reference};

use crate::keypath::{self, Segment};
use crate::kinds::Kind;
use crate::text::{self, CommentStyle, LineIndex, MASK};
use crate::tree::{Entry, MAX_DEPTH, Node, Tree, Value};

/// At most this many `Key` definitions per file (RFC 0003 §4.5).
pub const MAX_KEYS: usize = 5_000;
/// Mapping elements indexed per array.
pub const MAX_ELEMENTS: usize = 64;
/// All definitions per file (kind nodes included).
pub const MAX_DEFS: usize = 20_000;
/// References per file.
pub const MAX_REFS: usize = 20_000;
/// Imports per file.
pub const MAX_IMPORTS: usize = 2_000;
/// YAML anchors remembered per file.
pub const MAX_ANCHORS: usize = 1_000;

/// A definition to add.
pub(crate) struct Def {
    pub label: Label,
    pub name: String,
    pub qn: String,
    pub parent: Option<LocalDefId>,
    pub span: Range<usize>,
    pub name_span: Range<usize>,
    pub signature: Option<String>,
    pub doc: Option<String>,
}

/// Facts under construction for one file, with the caps of §4.5.
pub(crate) struct Out<'s> {
    pub src: &'s str,
    pub lines: LineIndex,
    pub style: CommentStyle,
    pub facts: FileFacts,
    /// `Key` definitions emitted.
    pub keys: usize,
    /// Array-element definitions emitted (`[N]`, `["id"]`).
    pub elements: usize,
    /// A cap cut the output (the file is `partial`).
    pub truncated: bool,
    imports: HashSet<String>,
    refs: HashSet<(Option<LocalDefId>, u8, String, Option<String>)>,
}

impl<'s> Out<'s> {
    pub(crate) fn new(src: &'s str, style: CommentStyle) -> Out<'s> {
        Out {
            src,
            lines: LineIndex::new(src),
            style,
            facts: FileFacts::default(),
            keys: 0,
            elements: 0,
            truncated: false,
            imports: HashSet::new(),
            refs: HashSet::new(),
        }
    }

    pub(crate) fn span(&self, r: &Range<usize>) -> rueca_model::Span {
        let end = r.end.min(self.src.len());
        self.lines.span(r.start.min(end), end)
    }

    /// Source text of a range (empty when out of bounds).
    pub(crate) fn slice(&self, r: &Range<usize>) -> &'s str {
        self.src.get(r.start..r.end.min(self.src.len())).unwrap_or("")
    }

    /// Comment doc for a definition whose key starts at `start`; a trailing
    /// comment is looked for after byte `after` on the same line.
    pub(crate) fn doc(&self, start: usize, after: Option<usize>) -> Option<String> {
        let line = self.lines.line_of(start.min(self.src.len()));
        let after = after.filter(|a| self.lines.line_of((*a).min(self.src.len())) == line);
        text::doc_comment(self.src, &self.lines, self.style, line, after)
    }

    /// Adds a definition; `None` when a cap is reached.
    pub(crate) fn def(&mut self, d: Def) -> Option<LocalDefId> {
        if self.facts.definitions.len() >= MAX_DEFS || (d.label == Label::Key && self.keys >= MAX_KEYS) {
            self.truncated = true;
            return None;
        }
        if d.label == Label::Key {
            self.keys += 1;
        }
        let id = self.facts.definitions.len() as LocalDefId;
        let span = self.span(&(d.span.start..d.span.end.max(d.name_span.end)));
        let name_span = self.span(&d.name_span);
        self.facts.definitions.push(Definition {
            label: d.label,
            name: d.name,
            qualified_name: d.qn,
            parent: d.parent,
            span,
            name_span,
            signature: d.signature,
            doc: d.doc,
            decorators: Vec::new(),
            bases: Vec::new(),
            implements: Vec::new(),
            exported: true,
            is_test: false,
            is_declaration: false,
        });
        Some(id)
    }

    /// Changes the label of an emitted definition (kinds), keeping the key
    /// count right.
    pub(crate) fn relabel(&mut self, id: LocalDefId, label: Label) {
        let d = &mut self.facts.definitions[id as usize];
        if d.label == Label::Key && label != Label::Key {
            self.keys -= 1;
        } else if d.label != Label::Key && label == Label::Key {
            self.keys += 1;
        }
        d.label = label;
    }

    pub(crate) fn get(&mut self, id: LocalDefId) -> &mut Definition {
        &mut self.facts.definitions[id as usize]
    }

    /// Adds a reference, deduplicated per (enclosing, kind, name, receiver).
    pub(crate) fn reference(
        &mut self,
        kind: RefKind,
        name: &str,
        receiver: Option<&str>,
        enclosing: Option<LocalDefId>,
        range: &Range<usize>,
    ) {
        if name.is_empty() || self.facts.references.len() >= MAX_REFS {
            return;
        }
        let key = (enclosing, kind as u8, name.to_string(), receiver.map(str::to_string));
        if !self.refs.insert(key) {
            return;
        }
        let span = self.span(range);
        self.facts.references.push(Reference {
            kind,
            name: name.to_string(),
            receiver: receiver.map(str::to_string),
            enclosing,
            span,
            arity: None,
        });
    }

    /// Adds an import of a file or directory, once per specifier.
    pub(crate) fn import(&mut self, module: &str, range: &Range<usize>) {
        let module = module.trim();
        if module.is_empty() || self.imports.len() >= MAX_IMPORTS || !self.imports.insert(module.to_string()) {
            return;
        }
        let span = self.span(range);
        self.facts.imports.push(Import {
            module: module.to_string(),
            relative_level: 0,
            names: Vec::new(),
            module_alias: None,
            type_only: false,
            reexport: false,
            span,
        });
    }

    /// Env var reads of `${NAME}` interpolations in the value at `range`.
    pub(crate) fn interpolation_reads(&mut self, range: &Range<usize>, enclosing: Option<LocalDefId>) {
        let raw = self.slice(range);
        for (name, r) in text::interpolations(raw) {
            let abs = range.start + r.start..range.start + r.end;
            self.reference(RefKind::EnvVar, &name, None, enclosing, &abs);
        }
    }
}

/// A scalar of the tree and the definition it belongs to.
pub(crate) struct Site<'t> {
    pub node: &'t Node,
    pub enclosing: Option<LocalDefId>,
}

/// What the generic walk recorded for the kind pass.
pub(crate) struct Ctx<'t> {
    /// Definition of each map entry / array element, by value-node address.
    pub defs: HashMap<usize, LocalDefId>,
    /// Every scalar (map values, array items) with its owner.
    pub scalars: Vec<Site<'t>>,
    /// Root definition and key path of each document (by index).
    pub roots: Vec<(Option<LocalDefId>, String)>,
}

impl Ctx<'_> {
    pub(crate) fn def_of(&self, node: &Node) -> Option<LocalDefId> {
        self.defs.get(&node.addr()).copied()
    }
}

/// Identity of a whole document (Kubernetes `Kind/name`).
pub(crate) struct DocIdentity {
    pub segment: String,
    pub label: Label,
    pub name: String,
    pub signature: String,
}

struct Walk<'t> {
    kind: Kind,
    split_dotted: bool,
    anchors: HashMap<usize, String>,
    ctx: Ctx<'t>,
    /// Current document root (for `#/…` pointers) and its key-path prefix.
    doc: Option<(&'t Node, String)>,
}

/// Walks every document of `tree` and emits the generic definitions.
pub(crate) fn emit_tree<'t>(out: &mut Out<'_>, tree: &'t Tree, kind: Kind) -> Ctx<'t> {
    let mut w = Walk {
        kind,
        split_dotted: kind.splits_dotted_keys(),
        anchors: HashMap::new(),
        ctx: Ctx { defs: HashMap::new(), scalars: Vec::new(), roots: Vec::new() },
        doc: None,
    };
    let multi = tree.docs.iter().filter(|d| !d.is_null()).count() >= 2;
    for (i, doc) in tree.docs.iter().enumerate() {
        if doc.is_null() {
            w.ctx.roots.push((None, String::new()));
            continue;
        }
        let (root, prefix) = match kind.doc_identity(doc) {
            Some(id) => {
                let d = out.def(Def {
                    label: id.label,
                    name: id.name,
                    qn: id.segment.clone(),
                    parent: None,
                    span: doc.span.clone(),
                    name_span: identity_span(doc),
                    signature: Some(id.signature),
                    doc: out.doc(doc.span.start, None),
                });
                (d, id.segment)
            }
            None if multi => {
                let seg = keypath::format(&[Segment::Doc(i)]);
                let d = out.def(Def {
                    label: Label::Document,
                    name: seg.clone(),
                    qn: seg.clone(),
                    parent: None,
                    span: doc.span.clone(),
                    name_span: doc.span.start..doc.span.start,
                    signature: Some(format!("document {i}: {}", value_preview(out, doc))),
                    doc: out.doc(doc.span.start, None),
                });
                (d, seg)
            }
            None => (None, String::new()),
        };
        if let Some(r) = root {
            w.ctx.defs.insert(doc.addr(), r);
        }
        w.ctx.roots.push((root, prefix.clone()));
        w.doc = Some((doc, prefix.clone()));
        w.value(out, doc, root, &prefix, "", 0);
    }
    w.ctx
}

/// Span of the `name:` value of a Kubernetes-like document, else its start.
fn identity_span(doc: &Node) -> Range<usize> {
    doc.get("metadata").and_then(|m| m.get("name")).map(|n| n.span.clone()).unwrap_or(doc.span.start..doc.span.start)
}

impl<'t> Walk<'t> {
    fn value(
        &mut self,
        out: &mut Out<'_>,
        node: &'t Node,
        owner: Option<LocalDefId>,
        qn: &str,
        key: &str,
        depth: usize,
    ) {
        match &node.value {
            Value::Map(entries) => self.map(out, entries, owner, qn, depth),
            Value::Seq(items) => self.seq(out, items, owner, qn, key, depth),
            Value::Alias(id) => self.alias(out, *id, owner, &node.span),
            Value::Scalar(_) => self.ctx.scalars.push(Site { node, enclosing: owner }),
            Value::Folded => {}
        }
    }

    fn alias(&mut self, out: &mut Out<'_>, id: usize, owner: Option<LocalDefId>, span: &Range<usize>) {
        if let Some(target) = self.anchors.get(&id) {
            out.reference(RefKind::DataRef, &target.clone(), None, owner, span);
        }
    }

    fn remember_anchor(&mut self, node: &Node, qn: &str) {
        if let Some(a) = node.anchor
            && self.anchors.len() < MAX_ANCHORS
        {
            self.anchors.entry(a).or_insert_with(|| qn.to_string());
        }
    }

    fn map(&mut self, out: &mut Out<'_>, entries: &'t [Entry], owner: Option<LocalDefId>, qn: &str, depth: usize) {
        for e in entries {
            if self.reference_entry(out, e, owner) {
                continue;
            }
            let segs: Vec<&str> = if e.split || (self.split_dotted && e.key.contains('.')) {
                e.key.split('.').collect()
            } else {
                vec![e.key.as_str()]
            };
            let depth = depth + segs.len();
            if depth > MAX_DEPTH {
                continue;
            }
            let path = segs.iter().fold(qn.to_string(), |p, s| keypath::join(&p, &keypath::key_segment(s)));
            let name = segs.last().copied().unwrap_or("").to_string();
            let Some(id) = out.def(key_def(out, e, name, path.clone(), owner)) else { return };
            self.ctx.defs.insert(e.value.addr(), id);
            self.remember_anchor(&e.value, &path);
            self.value(out, &e.value, Some(id), &path, &e.key, depth);
        }
    }

    /// YAML merge keys (`<<`, bases of the owning mapping) and `$ref`
    /// pointers: references, not keys. False for an ordinary entry.
    fn reference_entry(&mut self, out: &mut Out<'_>, e: &Entry, owner: Option<LocalDefId>) -> bool {
        if e.key == "<<" && self.merge(out, &e.value, owner) {
            return true;
        }
        if e.key == "$ref"
            && let Some(target) = e.value.as_str()
        {
            self.pointer(out, target, owner, &e.value.span);
            return true;
        }
        false
    }

    fn seq(
        &mut self,
        out: &mut Out<'_>,
        items: &'t [Node],
        owner: Option<LocalDefId>,
        qn: &str,
        key: &str,
        depth: usize,
    ) {
        let mut mappings = 0;
        let mut ids: HashSet<String> = HashSet::new();
        for (i, item) in items.iter().enumerate() {
            match &item.value {
                Value::Map(entries) => {
                    if mappings >= MAX_ELEMENTS || depth + 1 > MAX_DEPTH {
                        out.truncated |= mappings >= MAX_ELEMENTS;
                        continue;
                    }
                    mappings += 1;
                    let id = self.kind.element_identity(key, item).filter(|id| ids.insert(id.clone()));
                    let (seg, name) = match &id {
                        Some(id) => (keypath::id_segment(id), id.clone()),
                        None => (keypath::index_segment(i), keypath::index_segment(i)),
                    };
                    let path = keypath::join(qn, &seg);
                    let def = Def {
                        label: Label::Key,
                        name: name.clone(),
                        qn: path.clone(),
                        parent: owner,
                        span: item.span.clone(),
                        name_span: entries.first().map_or(item.span.start..item.span.start, |e| e.key_span.clone()),
                        signature: Some(format!("{name}: {}", value_preview(out, item))),
                        doc: out.doc(item.span.start, None),
                    };
                    let Some(d) = out.def(def) else { return };
                    out.elements += 1;
                    self.ctx.defs.insert(item.addr(), d);
                    self.remember_anchor(item, &path);
                    self.map(out, entries, Some(d), &path, depth + 1);
                }
                Value::Alias(a) => self.alias(out, *a, owner, &item.span),
                Value::Scalar(_) => self.ctx.scalars.push(Site { node: item, enclosing: owner }),
                Value::Seq(_) | Value::Folded => {}
            }
        }
    }

    /// `<<: *a` / `<<: [*a, *b]`: bases of the owner. False when the value
    /// is not made of aliases (then `<<` is an ordinary key).
    fn merge(&mut self, out: &mut Out<'_>, value: &Node, owner: Option<LocalDefId>) -> bool {
        let ids: Vec<usize> = match &value.value {
            Value::Alias(id) => vec![*id],
            Value::Seq(items) if items.iter().all(|i| matches!(i.value, Value::Alias(_))) => {
                items.iter().filter_map(|i| if let Value::Alias(a) = i.value { Some(a) } else { None }).collect()
            }
            _ => return false,
        };
        if let Some(owner) = owner {
            for id in ids {
                if let Some(target) = self.anchors.get(&id).cloned() {
                    let bases = &mut out.get(owner).bases;
                    if !bases.contains(&target) {
                        bases.push(target);
                    }
                }
            }
        }
        true
    }

    /// `$ref` (OpenAPI, JSON Schema, AsyncAPI): `#/a/b` → `DataRef` in this
    /// file; `file#/a/b` → import + `DataRef` with the file as receiver;
    /// `file` → import. URLs are external and recorded nowhere.
    fn pointer(&mut self, out: &mut Out<'_>, target: &str, owner: Option<LocalDefId>, span: &Range<usize>) {
        let target = target.trim();
        let (file, fragment) = match target.split_once('#') {
            Some((f, frag)) => (f, Some(frag)),
            None => (target, None),
        };
        if file.contains("://") || file.starts_with("urn:") {
            return;
        }
        let tokens: Vec<String> = match fragment {
            Some(frag) if frag.starts_with('/') => frag[1..].split('/').map(keypath::pointer_token).collect(),
            _ => Vec::new(),
        };
        if file.is_empty() {
            if tokens.is_empty() {
                return;
            }
            let Some((doc, prefix)) = &self.doc else { return };
            let path = local_pointer(doc, prefix, &tokens, self.kind);
            out.reference(RefKind::DataRef, &path, None, owner, span);
        } else {
            out.import(file, span);
            if !tokens.is_empty() {
                let segs: Vec<Segment> = tokens.into_iter().map(Segment::Key).collect();
                out.reference(RefKind::DataRef, &keypath::format(&segs), Some(file), owner, span);
            }
        }
    }
}

/// The `Key` definition of a mapping entry.
fn key_def(out: &Out<'_>, e: &Entry, name: String, qn: String, owner: Option<LocalDefId>) -> Def {
    let scalar = matches!(e.value.value, Value::Scalar(_) | Value::Alias(_));
    let after = if scalar { e.value.span.end } else { e.key_span.end };
    Def {
        label: Label::Key,
        name,
        qn,
        parent: owner,
        span: e.key_span.start..e.value.span.end.max(e.key_span.end),
        name_span: e.key_span.clone(),
        signature: Some(signature(out, &e.key, &e.value)),
        doc: out.doc(e.key_span.start, Some(after)),
    }
}

/// Key path of a JSON Pointer into the current document: array indexes
/// become `[N]` (or the element's identity), keys stay keys.
fn local_pointer(doc: &Node, prefix: &str, tokens: &[String], kind: Kind) -> String {
    let mut path = prefix.to_string();
    let mut node = Some(doc);
    let mut key = "";
    for t in tokens {
        let seg = match node.map(|n| &n.value) {
            Some(Value::Seq(items)) => match t.parse::<usize>().ok().and_then(|i| items.get(i).map(|n| (i, n))) {
                Some((i, n)) => {
                    let seg = kind
                        .element_identity(key, n)
                        .map(|id| keypath::id_segment(&id))
                        .unwrap_or_else(|| keypath::index_segment(i));
                    node = Some(n);
                    seg
                }
                None => {
                    node = None;
                    keypath::key_segment(t)
                }
            },
            Some(Value::Map(entries)) => {
                node = entries.iter().find(|e| &e.key == t).map(|e| &e.value);
                keypath::key_segment(t)
            }
            _ => {
                node = None;
                keypath::key_segment(t)
            }
        };
        key = t;
        path = keypath::join(&path, &seg);
    }
    path
}

/// `key: <preview>`, masked for secret-looking names.
pub(crate) fn signature(out: &Out<'_>, key: &str, value: &Node) -> String {
    if text::is_secret_name(key) && matches!(value.value, Value::Scalar(_)) && !value.is_null() {
        return format!("{key}: {MASK}");
    }
    format!("{key}: {}", value_preview(out, value))
}

/// Preview of a value: scalars as written (first line, ≤ 80 chars), maps
/// `{a, b, c, … N more}`, arrays `[N items]`.
pub(crate) fn value_preview(out: &Out<'_>, value: &Node) -> String {
    match &value.value {
        Value::Scalar(_) | Value::Alias(_) => text::preview(out.slice(&value.span)),
        Value::Map(entries) => {
            let names: Vec<&str> = entries.iter().take(3).map(|e| e.key.as_str()).collect();
            let more = entries.len().saturating_sub(3);
            let mut s = format!("{{{}", names.join(", "));
            if more > 0 {
                s.push_str(&format!(", … {more} more"));
            }
            s.push('}');
            text::truncate(&s, 120)
        }
        Value::Seq(items) => match items.len() {
            1 => "[1 item]".to_string(),
            n => format!("[{n} items]"),
        },
        Value::Folded => "{…}".to_string(),
    }
}

/// `DataRef` to a global qualified name (`pyproject` scripts).
pub(crate) fn qn_ref(out: &mut Out<'_>, qn: &str, owner: Option<LocalDefId>, span: &Range<usize>) {
    out.reference(RefKind::DataRef, qn, Some(DATA_REF_QN), owner, span);
}
