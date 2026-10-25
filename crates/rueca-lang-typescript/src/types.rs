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
//! binding of any of them; anything ambiguous leaves the name untyped. A
//! type parameter in scope stands for its bound (`D extends Dog` → `Dog`),
//! and is no type without one; predefined types (`string`, `any`) and arrays
//! are not types either.
//!
//! Array annotations (`xs: T[]`, `Array<T>`, `readonly T[]`) record the
//! element type, which types the unannotated first parameter of a callback
//! passed to an array method (`xs.map((x) => x.m())`) and a `for … of`
//! variable (`for (const x of xs)`).
//!
//! A first pass records the **return types** of the file's module-level
//! functions and of its classes' methods: the return annotation (`this` is
//! the class itself), or, for a function without one whose every `return`
//! is an object literal (`return { next() {…} }`), the function itself (its
//! object-literal members are its `Method` children, see `extract.rs`). A
//! value `f()` / `C.create()` (static) then has that type, and so does a
//! receiver `x.m()` whose `x` has a class type of the file
//! ([`TypeTable::method_return`]).

use std::collections::{HashMap, HashSet};

use rueca_extract::text_of;
use tree_sitter::Node;

use crate::builtins::is_builtin;

/// Array methods whose first argument is a callback receiving an element.
const ARRAY_CALLBACKS: &[&str] =
    &["every", "filter", "find", "findIndex", "findLast", "findLastIndex", "flatMap", "forEach", "map", "some"];

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
    /// Element types of array bindings, keyed like `vars`.
    elements: HashMap<(usize, String), Slot>,
    /// Return types of module-level functions, by name.
    functions: HashMap<String, String>,
    /// Return types of class methods, by (class, method).
    methods: HashMap<(String, String), String>,
    /// Names of the file's classes.
    classes: HashSet<String>,
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

    /// Declared return type of the module-level function `name`.
    pub(crate) fn function_return(&self, name: &str) -> Option<&str> {
        self.functions.get(name).map(String::as_str)
    }

    /// Declared return type of method `method` of the file's class `class`.
    pub(crate) fn method_return(&self, class: &str, method: &str) -> Option<&str> {
        self.methods.get(&(class.to_string(), method.to_string())).map(String::as_str)
    }

    /// Whether the file declares a class named `name`.
    pub(crate) fn is_class(&self, name: &str) -> bool {
        self.classes.contains(name)
    }

    pub(crate) fn build(root: Node<'_>, src: &[u8], max_depth: usize) -> TypeTable {
        let mut b = Builder {
            src,
            max_depth,
            table: TypeTable::default(),
            owners: vec![root.id()],
            type_params: Vec::new(),
            callbacks: HashMap::new(),
        };
        b.returns(root);
        b.walk(root, None, 0);
        b.table
    }
}

struct Builder<'s> {
    src: &'s [u8],
    max_depth: usize,
    table: TypeTable,
    owners: Vec<usize>,
    /// Type parameters in scope with their bound's type, if any.
    type_params: Vec<(String, Option<String>)>,
    /// Callbacks passed to an array method → the element type their first
    /// parameter receives.
    callbacks: HashMap<usize, String>,
}

/// Bound on nodes visited by the return-type pass.
const MAX_RETURN_SCAN: usize = 200_000;

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
                match self.type_params.iter().rev().find(|(p, _)| *p == name) {
                    Some((_, bound)) => bound.clone(),
                    None => Some(name),
                }
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

    /// The element type of an array type: `T[]`, `readonly T[]`,
    /// `Array<T>`, `ReadonlyArray<T>`.
    fn element_type(&self, t: Node<'_>) -> Option<String> {
        match t.kind() {
            "type_annotation" | "parenthesized_type" | "readonly_type" => self.element_type(t.named_child(0)?),
            "array_type" => self.type_name(t.named_child(0)?),
            "generic_type" => {
                let name = self.text(t.child_by_field_name("name")?);
                if name != "Array" && name != "ReadonlyArray" {
                    return None;
                }
                self.type_name(t.child_by_field_name("type_arguments")?.named_child(0)?)
            }
            _ => None,
        }
    }

    /// Records the element type of an array annotation of `name` in `owner`.
    fn element(&mut self, owner: usize, name: &str, ty: Option<Node<'_>>) {
        if let Some(e) = ty.and_then(|t| self.element_type(t)) {
            self.table.elements.entry((owner, name.to_string())).or_default().add(Fact::Declared(e));
        }
    }

    /// The element type of the array binding `name` visible here: the
    /// innermost owner that binds `name` decides.
    fn element_of(&self, name: &str) -> Option<String> {
        let key = self.owners.iter().rev().map(|&o| (o, name.to_string())).find(|k| self.table.vars.contains_key(k))?;
        self.table.elements.get(&key).and_then(Slot::get).map(str::to_string)
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
            // `make()` of a module-level function, `C.create()` of a class
            // of the file, with a known return type.
            "call_expression" => {
                let f = v.child_by_field_name("function")?;
                match f.kind() {
                    "identifier" => self.table.function_return(&self.text(f)).map(str::to_string),
                    "member_expression" => {
                        let o = f.child_by_field_name("object")?;
                        let p = f.child_by_field_name("property")?;
                        let class = self.text(o);
                        if o.kind() != "identifier" || !self.table.is_class(&class) {
                            return None;
                        }
                        self.table.method_return(&class, &self.text(p)).map(str::to_string)
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// Pushes the type parameters `n` declares (with their bounds); returns
    /// the previous stack length.
    fn push_type_params(&mut self, n: Node<'_>) -> usize {
        let mark = self.type_params.len();
        if let Some(tp) = n.child_by_field_name("type_parameters") {
            for p in named_children(tp) {
                if let Some(name) = p.child_by_field_name("name") {
                    let bound = p
                        .child_by_field_name("constraint")
                        .and_then(|c| c.named_child(0))
                        .and_then(|t| self.type_name(t))
                        .filter(|t| !is_builtin(t.split('.').next().unwrap_or(t)));
                    let t = self.text(name);
                    self.type_params.push((t, bound));
                }
            }
        }
        mark
    }

    /// A return annotation's type (`this` → `class`), without builtins.
    fn return_type(&mut self, f: Node<'_>, class: Option<&str>) -> Option<String> {
        let mark = self.push_type_params(f);
        let rt = f.child_by_field_name("return_type");
        let t = match rt.and_then(|r| r.named_child(0)) {
            Some(t) if t.kind() == "this_type" => class.map(str::to_string),
            Some(_) => rt.and_then(|r| self.type_name(r)),
            None => None,
        };
        self.type_params.truncate(mark);
        t.filter(|t| !is_builtin(t.split('.').next().unwrap_or(t)))
    }

    /// Whether every `return` of function `f` (not of nested functions)
    /// yields an object literal; at least one must.
    fn returns_object(&self, f: Node<'_>) -> bool {
        let Some(body) = f.child_by_field_name("body") else { return false };
        let is_object = |mut v: Node<'_>| {
            for _ in 0..8 {
                match v.kind() {
                    "parenthesized_expression" | "as_expression" | "satisfies_expression" => match v.named_child(0) {
                        Some(i) => v = i,
                        None => return false,
                    },
                    k => return k == "object",
                }
            }
            false
        };
        if body.kind() != "statement_block" {
            return is_object(body);
        }
        let mut stack = vec![body];
        let mut seen = 0usize;
        let mut any = false;
        while let Some(n) = stack.pop() {
            seen += 1;
            if seen > 10_000 {
                return false;
            }
            for c in named_children(n) {
                match c.kind() {
                    "return_statement" => match c.named_child(0) {
                        Some(v) if is_object(v) => any = true,
                        _ => return false,
                    },
                    k if OWNERS.contains(&k) || CLASSES.contains(&k) => {}
                    _ => stack.push(c),
                }
            }
        }
        any
    }

    /// First pass: return types of module-level functions and of class
    /// methods (see the module docs).
    fn returns(&mut self, root: Node<'_>) {
        let mut stack = vec![(root, 0usize)];
        let mut seen = 0usize;
        while let Some((n, depth)) = stack.pop() {
            seen += 1;
            if seen > MAX_RETURN_SCAN || depth > self.max_depth {
                break;
            }
            let module_level = || {
                let mut p = n.parent();
                while let Some(q) = p
                    .filter(|q| matches!(q.kind(), "export_statement" | "lexical_declaration" | "variable_declaration"))
                {
                    p = q.parent();
                }
                p.is_some_and(|q| q.kind() == "program")
            };
            match n.kind() {
                "function_declaration" | "generator_function_declaration" if module_level() => {
                    if let Some(name) = n.child_by_field_name("name") {
                        self.function_returns(self.text(name), n);
                    }
                }
                "variable_declarator" if module_level() => {
                    if let (Some(name), Some(v)) = (n.child_by_field_name("name"), n.child_by_field_name("value"))
                        && name.kind() == "identifier"
                        && matches!(v.kind(), "arrow_function" | "function_expression" | "function")
                    {
                        self.function_returns(self.text(name), v);
                    }
                }
                "class_declaration" | "abstract_class_declaration" => {
                    if let (Some(name), Some(body)) = (n.child_by_field_name("name"), n.child_by_field_name("body")) {
                        let class = self.text(name);
                        self.table.classes.insert(class.clone());
                        let mark = self.push_type_params(n);
                        for m in named_children(body) {
                            if m.kind() != "method_definition" {
                                continue;
                            }
                            let Some(mn) = m.child_by_field_name("name") else { continue };
                            if let Some(t) = self.return_type(m, Some(&class)) {
                                let key = (class.clone(), self.text(mn));
                                self.table.methods.entry(key).or_insert(t);
                            }
                        }
                        self.type_params.truncate(mark);
                    }
                }
                _ => {}
            }
            for c in named_children(n) {
                stack.push((c, depth + 1));
            }
        }
    }

    fn function_returns(&mut self, name: String, f: Node<'_>) {
        let t = if f.child_by_field_name("return_type").is_some() {
            self.return_type(f, None)
        } else if self.returns_object(f) {
            Some(name.clone())
        } else {
            None
        };
        if let Some(t) = t {
            self.table.functions.entry(name).or_insert(t);
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
    /// `hint`: the element type a callback's first parameter receives.
    fn parameter(&mut self, p: Node<'_>, class: Option<usize>, hint: Option<String>) {
        match p.kind() {
            "identifier" if hint.is_some() => {
                let (o, n) = (self.owner(), self.text(p));
                self.var(o, n, Fact::Declared(hint.unwrap_or_default()));
            }
            "required_parameter" | "optional_parameter" => {
                let Some(pat) = p.child_by_field_name("pattern") else { return };
                if pat.kind() != "identifier" {
                    return self.untyped_pattern(pat, 0);
                }
                let name = self.text(pat);
                let annotation = p.child_by_field_name("type");
                let o = self.owner();
                self.element(o, &name, annotation);
                let ty = match annotation {
                    Some(t) => self.type_name(t),
                    None => hint,
                };
                let property = named_children(p)
                    .iter()
                    .any(|c| matches!(c.kind(), "accessibility_modifier" | "override_modifier"))
                    || {
                        let mut cur = p.walk();
                        p.children(&mut cur).any(|c| c.kind() == "readonly")
                    };
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
        let mark = self.push_type_params(n);
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
                        let annotation = n.child_by_field_name("type");
                        let o = self.owner();
                        self.element(o, &t, annotation);
                        let declared = annotation.and_then(|ty| self.type_name(ty));
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
                    // `for (const x of xs)` with `xs: T[]`: `x` is a `T`.
                    let element = n
                        .child_by_field_name("right")
                        .filter(|r| r.kind() == "identifier" && l.kind() == "identifier")
                        .filter(|_| {
                            let mut cur = n.walk();
                            n.children(&mut cur).any(|c| c.kind() == "of")
                        })
                        .and_then(|r| self.element_of(&self.text(r)));
                    if declares && let Some(e) = element {
                        let (o, t) = (self.owner(), self.text(l));
                        self.var(o, t, Fact::Declared(e));
                    } else if declares {
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
            "call_expression" => self.callback(n),
            "augmented_assignment_expression" => {
                if let Some(l) = n.child_by_field_name("left") {
                    self.assignment(l, None, class, false);
                }
            }
            _ => {}
        }
        if owner {
            self.owners.push(n.id());
            let mut hint = self.callbacks.remove(&n.id());
            // Parameters belong to the function.
            if let Some(ps) = n.child_by_field_name("parameters") {
                for p in named_children(ps) {
                    let c = if kind == "method_definition" { class } else { None };
                    self.parameter(p, c, hint.take());
                }
            }
            if let Some(p) = n.child_by_field_name("parameter") {
                match hint.take() {
                    Some(h) if p.kind() == "identifier" => self.parameter(p, None, Some(h)),
                    _ => self.untyped_pattern(p, 0),
                }
            }
        }
        (owner, mark, class)
    }

    /// `xs.map((x) => …)` with `xs` an array of `T`: the callback's first
    /// parameter is a `T`.
    fn callback(&mut self, call: Node<'_>) {
        let Some(f) = call.child_by_field_name("function").filter(|f| f.kind() == "member_expression") else {
            return;
        };
        let (Some(o), Some(p)) = (f.child_by_field_name("object"), f.child_by_field_name("property")) else { return };
        if o.kind() != "identifier" || !ARRAY_CALLBACKS.contains(&self.text(p).as_str()) {
            return;
        }
        let Some(cb) = call.child_by_field_name("arguments").and_then(|a| a.named_child(0)) else { return };
        if !matches!(cb.kind(), "arrow_function" | "function_expression" | "function") {
            return;
        }
        if let Some(e) = self.element_of(&self.text(o)) {
            self.callbacks.insert(cb.id(), e);
        }
    }
}
