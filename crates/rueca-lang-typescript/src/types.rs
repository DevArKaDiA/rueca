//! Receiver types from declarations and assignments (strategy 5,
//! `docs/how-it-works.md` §4.4).
//!
//! A pre-pass over the file records, per owner (the function-like node a
//! binding belongs to, as in [`crate::locals`]; the root for module-level
//! names) and per class, what each binding of a name says about its type:
//!
//! - **declared**: a parameter or variable annotation (`c: Context`,
//!   `let x: Foo = …`, `x?: Foo | undefined`), a class field annotation
//!   (`#node: Node`, `repo!: Repo`), a TS parameter property
//!   (`constructor(private repo: Repo)`);
//! - **inferred**: an initializer or assignment `new T(…)`, `v as T`,
//!   `<T>v` (`const app = new Hono()`, `this.cache = new Cache()`);
//! - **untyped**: any other binding (destructuring, `catch`, `for … of`,
//!   function and class names, imports) or assignment.
//!
//! A name has a type when its declarations agree on one declared type
//! (annotations hold whatever is assigned later), or, with no annotation,
//! when every binding and assignment infers the same type. An assignment is
//! recorded in its owner and in every enclosing one, since it may target a
//! binding of any of them; anything ambiguous leaves the name untyped. Type
//! parameters in scope are never types; predefined types (`string`, `any`)
//! and arrays are not either.

use std::collections::HashMap;

use rueca_extract::text_of;
use tree_sitter::Node;

/// Function-like nodes: their bindings belong to them (see `locals.rs`).
pub(crate) const OWNERS: &[&str] = &[
    "arrow_function",
    "function",
    "function_declaration",
    "function_expression",
    "generator_function",
    "generator_function_declaration",
    "method_definition",
];

/// Nodes whose `this` is a fresh object (not the enclosing class instance).
const THIS_BINDERS: &[&str] = &[
    "function",
    "function_declaration",
    "function_expression",
    "generator_function",
    "generator_function_declaration",
];

const CLASSES: &[&str] = &["class", "class_declaration", "abstract_class_declaration"];

#[derive(Debug, Default)]
struct Slot {
    declared: Vec<String>,
    inferred: Vec<String>,
    /// A binding (declaration) without a type.
    untyped_binding: bool,
    /// An assignment whose value has no known type.
    untyped_assignment: bool,
}

impl Slot {
    fn get(&self) -> Option<&str> {
        fn one(v: &[String]) -> Option<&str> {
            let first = v.first()?;
            v.iter().all(|t| t == first).then_some(first.as_str())
        }
        if self.untyped_binding {
            return None;
        }
        if !self.declared.is_empty() {
            return one(&self.declared);
        }
        if self.untyped_assignment {
            return None;
        }
        one(&self.inferred)
    }
}

/// Types of local, module-level and class-field bindings of one file.
#[derive(Debug, Default)]
pub(crate) struct TypeTable {
    vars: HashMap<(usize, String), Slot>,
    fields: HashMap<(usize, String), Slot>,
}

enum Fact {
    Declared(String),
    Inferred(String),
    UntypedBinding,
    UntypedAssignment,
}

impl Slot {
    fn add(&mut self, fact: Fact) {
        match fact {
            Fact::Declared(t) => self.declared.push(t),
            Fact::Inferred(t) => self.inferred.push(t),
            Fact::UntypedBinding => self.untyped_binding = true,
            Fact::UntypedAssignment => self.untyped_assignment = true,
        }
    }
}

impl TypeTable {
    /// The type of `name` as bound in `owner` (a function-like node id, or
    /// the root id for module-level names).
    pub(crate) fn var(&self, owner: usize, name: &str) -> Option<&str> {
        self.vars.get(&(owner, name.to_string())).and_then(Slot::get)
    }

    /// The type of field `name` of the class node `class`.
    pub(crate) fn field(&self, class: usize, name: &str) -> Option<&str> {
        self.fields.get(&(class, name.to_string())).and_then(Slot::get)
    }

    pub(crate) fn build(root: Node<'_>, src: &[u8], max_depth: usize) -> TypeTable {
        let mut b =
            Builder { src, max_depth, table: TypeTable::default(), owners: vec![root.id()], type_params: Vec::new() };
        b.walk(root, None, 0);
        b.table
    }
}

struct Builder<'s> {
    src: &'s [u8],
    max_depth: usize,
    table: TypeTable,
    owners: Vec<usize>,
    type_params: Vec<String>,
}

fn named_children<'t>(n: Node<'t>) -> Vec<Node<'t>> {
    let mut c = n.walk();
    n.named_children(&mut c).collect()
}

impl Builder<'_> {
    fn text(&self, n: Node<'_>) -> String {
        text_of(n, self.src).into_owned()
    }

    fn var(&mut self, owner: usize, name: String, fact: Fact) {
        self.table.vars.entry((owner, name)).or_default().add(fact);
    }

    fn field(&mut self, class: usize, name: String, fact: Fact) {
        self.table.fields.entry((class, name)).or_default().add(fact);
    }

    fn owner(&self) -> usize {
        *self.owners.last().expect("root owner")
    }

    /// The type a type node names: a type name or a dotted name, through
    /// generic arguments, parentheses and `| null` / `| undefined`.
    fn type_name(&self, t: Node<'_>) -> Option<String> {
        match t.kind() {
            "type_annotation" | "parenthesized_type" => self.type_name(t.named_child(0)?),
            "type_identifier" => {
                let name = self.text(t);
                (!self.type_params.contains(&name)).then_some(name)
            }
            "nested_type_identifier" => Some(self.text(t).split_whitespace().collect()),
            "generic_type" => self.type_name(t.child_by_field_name("name")?),
            "union_type" => {
                let mut members = Vec::new();
                self.union_members(t, &mut members, 0);
                let real: Vec<Node<'_>> = members
                    .into_iter()
                    .filter(|m| !matches!(self.text(*m).trim(), "null" | "undefined" | "void"))
                    .collect();
                match real[..] {
                    [one] => self.type_name(one),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn union_members<'t>(&self, t: Node<'t>, out: &mut Vec<Node<'t>>, depth: usize) {
        for c in named_children(t) {
            if c.kind() == "union_type" && depth < 16 {
                self.union_members(c, out, depth + 1);
            } else {
                out.push(c);
            }
        }
    }

    /// The type a value has by construction: `new T(…)`, `v as T`, `<T>v`.
    fn value_type(&self, v: Node<'_>) -> Option<String> {
        match v.kind() {
            "parenthesized_expression" | "non_null_expression" => self.value_type(v.named_child(0)?),
            "new_expression" => {
                let c = v.child_by_field_name("constructor")?;
                match c.kind() {
                    "identifier" => Some(self.text(c)),
                    "member_expression" => {
                        let t: String = self.text(c).split_whitespace().collect();
                        t.split('.')
                            .all(|s| !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '$'))
                            .then_some(t)
                    }
                    _ => None,
                }
            }
            // `v as T`: the type is the last child; `<T>v`: the first.
            "as_expression" => self.type_name(named_children(v).pop()?),
            "type_assertion" => {
                let args = named_children(v).into_iter().find(|c| c.kind() == "type_arguments")?;
                self.type_name(args.named_child(0)?)
            }
            _ => None,
        }
    }

    /// Records every identifier bound by a pattern as untyped.
    fn untyped_pattern(&mut self, p: Node<'_>, depth: usize) {
        if depth > 64 {
            return;
        }
        match p.kind() {
            "identifier" | "shorthand_property_identifier_pattern" => {
                let (o, n) = (self.owner(), self.text(p));
                self.var(o, n, Fact::UntypedBinding);
            }
            // Default values and computed keys bind nothing.
            "assignment_pattern" | "object_assignment_pattern" => {
                if let Some(l) = p.child_by_field_name("left") {
                    self.untyped_pattern(l, depth + 1);
                }
            }
            "pair_pattern" => {
                if let Some(v) = p.child_by_field_name("value") {
                    self.untyped_pattern(v, depth + 1);
                }
            }
            _ => {
                for c in named_children(p) {
                    self.untyped_pattern(c, depth + 1);
                }
            }
        }
    }

    /// A parameter: declared with its annotation, else untyped; TS
    /// parameter properties are also fields of `class`.
    fn parameter(&mut self, p: Node<'_>, class: Option<usize>) {
        match p.kind() {
            "required_parameter" | "optional_parameter" => {
                let Some(pat) = p.child_by_field_name("pattern") else { return };
                if pat.kind() != "identifier" {
                    return self.untyped_pattern(pat, 0);
                }
                let name = self.text(pat);
                let ty = p.child_by_field_name("type").and_then(|t| self.type_name(t));
                let property = named_children(p)
                    .iter()
                    .any(|c| matches!(c.kind(), "accessibility_modifier" | "override_modifier"))
                    || {
                        let mut cur = p.walk();
                        p.children(&mut cur).any(|c| c.kind() == "readonly")
                    };
                let o = self.owner();
                match &ty {
                    Some(t) => self.var(o, name.clone(), Fact::Declared(t.clone())),
                    None => self.var(o, name.clone(), Fact::UntypedBinding),
                }
                if property && let (Some(c), Some(t)) = (class, ty) {
                    self.field(c, name, Fact::Declared(t));
                }
            }
            _ => self.untyped_pattern(p, 0),
        }
    }

    /// An assignment to `left` with value `right`.
    fn assignment(&mut self, left: Node<'_>, right: Option<Node<'_>>, class: Option<usize>, plain: bool) {
        let ty = if plain { right.and_then(|r| self.value_type(r)) } else { None };
        let fact = || match &ty {
            Some(t) => Fact::Inferred(t.clone()),
            None => Fact::UntypedAssignment,
        };
        match left.kind() {
            "identifier" => {
                let name = self.text(left);
                for o in self.owners.clone() {
                    self.var(o, name.clone(), fact());
                }
            }
            "member_expression" => {
                let obj = left.child_by_field_name("object");
                let prop = left.child_by_field_name("property");
                if let (Some(o), Some(p), Some(c)) = (obj, prop, class)
                    && o.kind() == "this"
                {
                    let name = self.text(p);
                    self.field(c, name, fact());
                }
            }
            _ => {}
        }
    }

    /// Iterative pre-order walk (deep trees must not exhaust the stack).
    fn walk(&mut self, root: Node<'_>, class: Option<usize>, depth: usize) {
        enum Task<'t> {
            Visit(Node<'t>, Option<usize>, usize),
            Leave { owner: bool, mark: usize },
        }
        let mut tasks = vec![Task::Visit(root, class, depth)];
        while let Some(task) = tasks.pop() {
            match task {
                Task::Leave { owner, mark } => {
                    if owner {
                        self.owners.pop();
                    }
                    self.type_params.truncate(mark);
                }
                Task::Visit(n, class, depth) => {
                    if depth > self.max_depth {
                        continue;
                    }
                    let (owner, mark, class) = self.enter(n, class);
                    tasks.push(Task::Leave { owner, mark });
                    let params = if owner { n.child_by_field_name("parameters") } else { None };
                    let children: Vec<Node<'_>> = named_children(n);
                    for c in children.into_iter().rev() {
                        if Some(c) == params {
                            // Default values may still contain functions and assignments.
                            for p in named_children(c).into_iter().rev() {
                                if let Some(v) = p.child_by_field_name("value") {
                                    tasks.push(Task::Visit(v, class, depth + 2));
                                }
                            }
                            continue;
                        }
                        tasks.push(Task::Visit(c, class, depth + 1));
                    }
                }
            }
        }
    }

    /// Records the facts of one node; returns whether it is an owner (pushed
    /// on `owners`), the type-parameter mark to restore and the class its
    /// children see.
    fn enter(&mut self, n: Node<'_>, class: Option<usize>) -> (bool, usize, Option<usize>) {
        let kind = n.kind();
        let owner = OWNERS.contains(&kind);
        // `this` inside a non-arrow function is not the class instance.
        let class = if CLASSES.contains(&kind) {
            Some(n.id())
        } else if THIS_BINDERS.contains(&kind) {
            None
        } else {
            class
        };
        let mark = self.type_params.len();
        if let Some(tp) = n.child_by_field_name("type_parameters") {
            for p in named_children(tp) {
                if let Some(name) = p.child_by_field_name("name") {
                    let t = self.text(name);
                    self.type_params.push(t);
                }
            }
        }
        match kind {
            // Names of declarations bind in the enclosing owner.
            "function_declaration"
            | "generator_function_declaration"
            | "class_declaration"
            | "abstract_class_declaration" => {
                if let Some(name) = n.child_by_field_name("name") {
                    let (o, t) = (self.owner(), self.text(name));
                    self.var(o, t, Fact::UntypedBinding);
                }
            }
            "variable_declarator" => {
                if let Some(name) = n.child_by_field_name("name") {
                    if name.kind() == "identifier" {
                        let t = self.text(name);
                        let declared = n.child_by_field_name("type").and_then(|ty| self.type_name(ty));
                        let fact = match declared {
                            Some(d) => Fact::Declared(d),
                            None => match n.child_by_field_name("value").and_then(|v| self.value_type(v)) {
                                Some(i) => Fact::Inferred(i),
                                None => Fact::UntypedBinding,
                            },
                        };
                        let o = self.owner();
                        self.var(o, t, fact);
                    } else {
                        self.untyped_pattern(name, 0);
                    }
                }
            }
            "public_field_definition" | "field_definition" => {
                let name = n.child_by_field_name("name").or_else(|| n.child_by_field_name("property"));
                if let (Some(name), Some(c)) = (name, class) {
                    let t = self.text(name);
                    if let Some(d) = n.child_by_field_name("type").and_then(|ty| self.type_name(ty)) {
                        self.field(c, t, Fact::Declared(d));
                    } else if let Some(v) = n.child_by_field_name("value") {
                        let fact = self.value_type(v).map_or(Fact::UntypedAssignment, Fact::Inferred);
                        self.field(c, t, fact);
                    }
                }
            }
            "catch_clause" => {
                if let Some(p) = n.child_by_field_name("parameter") {
                    self.untyped_pattern(p, 0);
                }
            }
            "for_in_statement" => {
                if let Some(l) = n.child_by_field_name("left") {
                    let declares = {
                        let mut cur = n.walk();
                        n.children(&mut cur).any(|c| matches!(c.kind(), "const" | "let" | "var"))
                    };
                    if declares {
                        self.untyped_pattern(l, 0);
                    } else if l.kind() == "identifier" {
                        self.assignment(l, None, class, false);
                    }
                }
            }
            "import_clause" | "import_specifier" | "namespace_import" => {
                // Imports bind module-level names.
                for c in named_children(n) {
                    if c.kind() == "identifier" {
                        let t = self.text(c);
                        self.var(self.owners[0], t, Fact::UntypedBinding);
                    }
                }
                if n.kind() == "import_specifier"
                    && let Some(a) = n.child_by_field_name("alias").or_else(|| n.child_by_field_name("name"))
                {
                    let t = self.text(a);
                    self.var(self.owners[0], t, Fact::UntypedBinding);
                }
            }
            "assignment_expression" => {
                if let Some(l) = n.child_by_field_name("left") {
                    let r = n.child_by_field_name("right");
                    self.assignment(l, r, class, true);
                }
            }
            "augmented_assignment_expression" => {
                if let Some(l) = n.child_by_field_name("left") {
                    self.assignment(l, None, class, false);
                }
            }
            _ => {}
        }
        if owner {
            self.owners.push(n.id());
            // Parameters belong to the function.
            if let Some(ps) = n.child_by_field_name("parameters") {
                for p in named_children(ps) {
                    let c = if kind == "method_definition" { class } else { None };
                    self.parameter(p, c);
                }
            }
            if let Some(p) = n.child_by_field_name("parameter") {
                self.untyped_pattern(p, 0);
            }
        }
        (owner, mark, class)
    }
}
