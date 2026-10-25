//! Receiver types from annotations and assignments (strategy 5,
//! `docs/how-it-works.md` §4.4).
//!
//! A pre-pass over the file records, per owner (a function or lambda node;
//! the module root for module-level names) and per class, what each binding
//! of a name says about its type:
//!
//! - **declared**: a parameter or variable annotation (`req: Request`,
//!   `x: Foo = …`, `Optional[Foo]`, `Foo | None`, `"Foo"`), a class-body
//!   annotation (`session: Session`) or `self.x: Foo = …`;
//! - **inferred**: an assignment whose value is a call of a class-like name
//!   (`x = Foo(…)`, `x = models.Foo(…)`: the last segment is capitalized),
//!   or of a function or method of this file with a return annotation
//!   (`ctx = self.request_context(env)` with `-> RequestContext`,
//!   `x = make()`, `x = Foo.create()`); `self.x = …` the same way;
//! - **untyped**: any other binding (parameters without annotation, `for`
//!   and `with` targets, `except … as`, `:=`, tuple targets, imports,
//!   `global` / `nonlocal`, nested `def` / `class`) or assignment.
//!
//! A name has a type when its annotations agree on one type, or, without
//! annotations, when every binding and assignment infers the same type.
//! Comprehension targets count as bindings of the enclosing owner (which
//! only makes it more conservative).

use std::collections::HashMap;

use rueca_extract::text_of;
use tree_sitter::Node;

use crate::is_builtin_name;

#[derive(Debug, Default)]
struct Slot {
    declared: Vec<String>,
    inferred: Vec<String>,
    untyped_binding: bool,
    untyped_assignment: bool,
}

impl Slot {
    fn get(&self) -> Option<&str> {
        fn one(v: &[String]) -> Option<&str> {
            let first = v.first()?;
            v.iter().all(|t| t == first).then_some(first.as_str())
        }
        if !self.declared.is_empty() {
            return if self.untyped_binding { None } else { one(&self.declared) };
        }
        if self.untyped_binding || self.untyped_assignment {
            return None;
        }
        one(&self.inferred)
    }
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

/// Types of the bindings, instance attributes and return annotations of
/// one file.
#[derive(Debug, Default)]
pub(crate) struct TypeTable {
    vars: HashMap<(usize, String), Slot>,
    /// (class node, attribute) → type of `self.attribute`.
    attrs: HashMap<(usize, String), Slot>,
    /// (class node, method) → return annotation.
    methods: HashMap<(usize, String), Option<String>>,
    /// Module-level class name → class node.
    classes: HashMap<String, usize>,
    /// Module-level function → return annotation.
    functions: HashMap<String, Option<String>>,
}

impl TypeTable {
    /// The type of `name` as bound in `owner` (a function / lambda node id,
    /// or the root id for module-level names).
    pub(crate) fn var(&self, owner: usize, name: &str) -> Option<&str> {
        self.vars.get(&(owner, name.to_string())).and_then(Slot::get)
    }

    /// Whether `owner` binds `name` at all.
    pub(crate) fn binds(&self, owner: usize, name: &str) -> bool {
        self.vars.contains_key(&(owner, name.to_string()))
    }

    /// The type of `self.<name>` in the class node `class`.
    pub(crate) fn attr(&self, class: usize, name: &str) -> Option<&str> {
        self.attrs.get(&(class, name.to_string())).and_then(Slot::get)
    }

    /// Return annotation of method `name` of the class node `class`.
    pub(crate) fn method_return(&self, class: usize, name: &str) -> Option<&str> {
        self.methods.get(&(class, name.to_string()))?.as_deref()
    }

    /// Return annotation of a module-level function, or of `Cls.method` for
    /// a module-level class of this file.
    pub(crate) fn call_return(&self, callee: &[&str]) -> Option<&str> {
        match callee {
            [f] => self.functions.get(*f)?.as_deref(),
            [c, m] => self.method_return(*self.classes.get(*c)?, m),
            _ => None,
        }
    }

    pub(crate) fn build(root: Node<'_>, src: &[u8], max_depth: usize) -> TypeTable {
        let mut b = Builder { src, table: TypeTable::default() };
        b.signatures(root, max_depth);
        b.walk(root, max_depth);
        b.table
    }
}

/// Where a node of the walk is.
struct Frame {
    owner: usize,
    /// Class whose methods are being walked, and the method's first
    /// parameter name (`self`).
    class: Option<usize>,
    self_name: Option<String>,
    /// Directly in a class body (assignments are class attributes).
    class_body: Option<usize>,
}

struct Builder<'s> {
    src: &'s [u8],
    table: TypeTable,
}

fn named_children<'t>(n: Node<'t>) -> Vec<Node<'t>> {
    let mut c = n.walk();
    n.named_children(&mut c).collect()
}

/// A dotted name made of identifiers only (`a`, `a.b.C`).
fn is_dotted(t: &str) -> bool {
    !t.is_empty() && t.split('.').all(|s| !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_'))
}

/// The function definition of a (possibly decorated) definition node.
fn function_of(n: Node<'_>) -> Option<Node<'_>> {
    match n.kind() {
        "function_definition" => Some(n),
        "decorated_definition" => n.child_by_field_name("definition").filter(|d| d.kind() == "function_definition"),
        _ => None,
    }
}

fn class_of(n: Node<'_>) -> Option<Node<'_>> {
    match n.kind() {
        "class_definition" => Some(n),
        "decorated_definition" => n.child_by_field_name("definition").filter(|d| d.kind() == "class_definition"),
        _ => None,
    }
}

impl Builder<'_> {
    fn text(&self, n: Node<'_>) -> String {
        text_of(n, self.src).into_owned()
    }

    fn dotted(&self, n: Node<'_>) -> Option<String> {
        let t: String = self.text(n).split_whitespace().collect();
        is_dotted(&t).then_some(t)
    }

    /// The class an annotation names: a name or dotted name, through
    /// `Optional[…]`, `Union[…, None]`, `Annotated[…]`, `… | None` and
    /// string forward references. `None` for anything else.
    fn annotation(&self, t: Node<'_>, depth: usize) -> Option<String> {
        if depth > 16 {
            return None;
        }
        match t.kind() {
            "type" | "parenthesized_expression" => self.annotation(t.named_child(0)?, depth + 1),
            "identifier" | "attribute" => {
                let name = self.dotted(t)?;
                let last = name.rsplit('.').next().unwrap_or(&name);
                (!matches!(last, "Any" | "Self" | "None" | "object")).then_some(name)
            }
            "string" => {
                let raw = self.text(t);
                let mut inner = raw.trim_matches(|c| c == '"' || c == '\'').trim();
                // `"Foo | None"`, `"Optional[Foo]"`.
                if let Some(x) = inner.strip_prefix("Optional[").and_then(|x| x.strip_suffix(']')) {
                    inner = x.trim();
                }
                let parts: Vec<&str> = inner.split('|').map(str::trim).filter(|p| *p != "None").collect();
                let [one] = parts[..] else { return None };
                let last = one.rsplit('.').next().unwrap_or(one);
                (is_dotted(one) && !matches!(last, "Any" | "Self")).then(|| one.to_string())
            }
            "binary_operator" => {
                let l = t.child_by_field_name("left")?;
                let r = t.child_by_field_name("right")?;
                let none = |n: Node<'_>| self.text(n).trim() == "None";
                match (none(l), none(r)) {
                    (false, true) => self.annotation(l, depth + 1),
                    (true, false) => self.annotation(r, depth + 1),
                    _ => None,
                }
            }
            "subscript" | "generic_type" => {
                let value = t.child_by_field_name("value").or_else(|| t.named_child(0))?;
                let head = self.dotted(value)?;
                let head = head.rsplit('.').next().unwrap_or(&head).to_string();
                let args: Vec<Node<'_>> = match t.kind() {
                    "subscript" => {
                        let mut c = t.walk();
                        t.children_by_field_name("subscript", &mut c).collect()
                    }
                    _ => named_children(t).into_iter().skip(1).collect(),
                };
                let args: Vec<Node<'_>> = args
                    .into_iter()
                    .flat_map(|a| if a.kind() == "type_parameter" || a.kind() == "tuple" { named_children(a) } else { vec![a] })
                    .collect();
                let real: Vec<Node<'_>> = args.into_iter().filter(|a| self.text(*a).trim() != "None").collect();
                match (head.as_str(), &real[..]) {
                    ("Optional" | "Union", [one]) => self.annotation(*one, depth + 1),
                    ("Annotated", [first, ..]) => self.annotation(*first, depth + 1),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// Return annotations of module-level functions and of the methods of
    /// every class, and the module-level classes.
    fn signatures(&mut self, root: Node<'_>, max_depth: usize) {
        let mut stack = vec![(root, 0usize)];
        while let Some((n, depth)) = stack.pop() {
            if depth > max_depth {
                continue;
            }
            for c in named_children(n) {
                if let Some(f) = function_of(c) {
                    let name = f.child_by_field_name("name").map(|x| self.text(x));
                    let ret = f.child_by_field_name("return_type").and_then(|r| self.annotation(r, 0));
                    if let Some(name) = name {
                        if n.kind() == "module" {
                            self.table.functions.insert(name, ret);
                        } else if let Some(class) = enclosing_class_body(n) {
                            // Several definitions (properties, overloads): unknown unless equal.
                            let e = self.table.methods.entry((class, name)).or_insert(ret.clone());
                            if *e != ret {
                                *e = None;
                            }
                        }
                    }
                } else if let Some(k) = class_of(c) {
                    if n.kind() == "module"
                        && let Some(name) = k.child_by_field_name("name")
                    {
                        self.table.classes.insert(self.text(name), k.id());
                    }
                    if let Some(body) = k.child_by_field_name("body") {
                        stack.push((body, depth + 1));
                    }
                } else if matches!(c.kind(), "if_statement" | "try_statement" | "block" | "else_clause" | "elif_clause")
                    || c.kind().ends_with("_clause")
                {
                    stack.push((c, depth + 1));
                }
            }
        }
    }

    /// The type a value has: a call of a class-like name, or of a function
    /// / method of this file with a return annotation.
    fn value_type(&self, v: Node<'_>, class: Option<usize>, self_name: Option<&str>) -> Option<String> {
        // Through parentheses and chained assignments, iteratively.
        let mut v = v;
        for _ in 0..64 {
            v = match v.kind() {
                "parenthesized_expression" => v.named_child(0)?,
                "assignment" => v.child_by_field_name("right")?,
                _ => break,
            };
        }
        match v.kind() {
            "call" => {
                let callee = self.dotted(v.child_by_field_name("function")?)?;
                let segs: Vec<&str> = callee.split('.').collect();
                if let [s, m] = segs[..]
                    && Some(s) == self_name
                {
                    return class.and_then(|c| self.table.method_return(c, m)).map(str::to_string);
                }
                if let Some(r) = self.table.call_return(&segs) {
                    return Some(r.to_string());
                }
                let last = segs.last()?;
                let class_like = last.starts_with(|c: char| c.is_ascii_uppercase())
                    && last.chars().any(|c| c.is_ascii_lowercase())
                    && !is_builtin_name(last);
                class_like.then_some(callee)
            }
            _ => None,
        }
    }

    fn var(&mut self, owner: usize, name: String, fact: Fact) {
        self.table.vars.entry((owner, name)).or_default().add(fact);
    }

    fn attr(&mut self, class: usize, name: String, fact: Fact) {
        self.table.attrs.entry((class, name)).or_default().add(fact);
    }

    /// Records every identifier of a target as an untyped binding.
    fn untyped_target(&mut self, owner: usize, t: Node<'_>, depth: usize) {
        if depth > 64 {
            return;
        }
        match t.kind() {
            "identifier" => {
                let n = self.text(t);
                self.var(owner, n, Fact::UntypedBinding);
            }
            // `obj.x = …` / `obj[i] = …` bind no name.
            "attribute" | "subscript" => {}
            _ => {
                for c in named_children(t) {
                    self.untyped_target(owner, c, depth + 1);
                }
            }
        }
    }

    fn parameters(&mut self, owner: usize, params: Node<'_>) {
        for p in named_children(params) {
            match p.kind() {
                "identifier" => {
                    let n = self.text(p);
                    self.var(owner, n, Fact::UntypedBinding);
                }
                "typed_parameter" | "typed_default_parameter" => {
                    let name = p
                        .child_by_field_name("name")
                        .or_else(|| named_children(p).into_iter().find(|c| c.kind() == "identifier"));
                    let Some(name) = name else { continue };
                    let ty = p.child_by_field_name("type").and_then(|t| self.annotation(t, 0));
                    let n = self.text(name);
                    self.var(owner, n, ty.map_or(Fact::UntypedBinding, Fact::Declared));
                }
                _ => self.untyped_target(owner, p, 0),
            }
        }
    }

    /// Iterative walk: owners are functions and lambdas (module root at the
    /// bottom); classes give `self.x` assignments their class.
    fn walk(&mut self, root: Node<'_>, max_depth: usize) {
        let mut stack: Vec<(Node<'_>, usize, std::rc::Rc<Frame>)> = vec![(
            root,
            0,
            std::rc::Rc::new(Frame { owner: root.id(), class: None, self_name: None, class_body: None }),
        )];
        while let Some((n, depth, frame)) = stack.pop() {
            if depth > max_depth {
                continue;
            }
            let mut child_frame = frame.clone();
            match n.kind() {
                "function_definition" => {
                    if let Some(name) = n.child_by_field_name("name") {
                        let t = self.text(name);
                        self.var(frame.owner, t, Fact::UntypedBinding);
                    }
                    let in_class = frame.class_body.is_some();
                    let params = n.child_by_field_name("parameters");
                    let self_name = if in_class {
                        params.and_then(|p| named_children(p).into_iter().next()).map(|p| match p.kind() {
                            "identifier" => self.text(p),
                            _ => p.child_by_field_name("name").map(|x| self.text(x)).unwrap_or_default(),
                        })
                    } else {
                        frame.self_name.clone()
                    };
                    if let Some(p) = params {
                        self.parameters(n.id(), p);
                    }
                    child_frame = std::rc::Rc::new(Frame {
                        owner: n.id(),
                        class: if in_class { frame.class_body } else { frame.class },
                        self_name,
                        class_body: None,
                    });
                    // Only the body runs in the new owner (defaults and
                    // annotations are evaluated outside).
                    if let Some(b) = n.child_by_field_name("body") {
                        stack.push((b, depth + 1, child_frame));
                    }
                    continue;
                }
                "lambda" => {
                    if let Some(p) = n.child_by_field_name("parameters") {
                        self.parameters(n.id(), p);
                    }
                    child_frame = std::rc::Rc::new(Frame {
                        owner: n.id(),
                        class: frame.class,
                        self_name: frame.self_name.clone(),
                        class_body: None,
                    });
                }
                "class_definition" => {
                    if let Some(name) = n.child_by_field_name("name") {
                        let t = self.text(name);
                        self.var(frame.owner, t, Fact::UntypedBinding);
                    }
                    if let Some(b) = n.child_by_field_name("body") {
                        let f = std::rc::Rc::new(Frame {
                            owner: frame.owner,
                            class: None,
                            self_name: None,
                            class_body: Some(n.id()),
                        });
                        stack.push((b, depth + 1, f));
                    }
                    continue;
                }
                "assignment" => self.assignment(n, &frame),
                "augmented_assignment" => {
                    if let Some(l) = n.child_by_field_name("left") {
                        self.assign_untyped(l, &frame);
                    }
                }
                "for_statement" | "for_in_clause" => {
                    if let Some(l) = n.child_by_field_name("left") {
                        self.untyped_target(frame.owner, l, 0);
                    }
                }
                "as_pattern_target" | "named_expression" => {
                    let t = if n.kind() == "named_expression" { n.child_by_field_name("name") } else { Some(n) };
                    if let Some(t) = t {
                        self.untyped_target(frame.owner, t, 0);
                    }
                }
                "except_clause" => {
                    // `except E as e`: the alias.
                    if let Some(a) = n.child_by_field_name("alias") {
                        self.untyped_target(frame.owner, a, 0);
                    }
                }
                "import_statement" | "import_from_statement" | "global_statement" | "nonlocal_statement" => {
                    for c in named_children(n) {
                        let name = match c.kind() {
                            "aliased_import" => c.child_by_field_name("alias"),
                            "dotted_name" => c.named_child(0),
                            "identifier" => Some(c),
                            _ => None,
                        };
                        if let Some(name) = name
                            && Some(c) != n.child_by_field_name("module_name")
                        {
                            let t = self.text(name);
                            self.var(frame.owner, t, Fact::UntypedBinding);
                        }
                    }
                }
                _ => {}
            }
            for c in named_children(n).into_iter().rev() {
                stack.push((c, depth + 1, child_frame.clone()));
            }
        }
    }

    fn assignment(&mut self, n: Node<'_>, frame: &Frame) {
        let Some(left) = n.child_by_field_name("left") else { return };
        let declared = n.child_by_field_name("type").and_then(|t| self.annotation(t, 0));
        let right = n.child_by_field_name("right");
        let inferred = right.and_then(|r| self.value_type(r, frame.class, frame.self_name.as_deref()));
        let fact = || match (&declared, &inferred) {
            (Some(d), _) => Fact::Declared(d.clone()),
            (None, Some(i)) => Fact::Inferred(i.clone()),
            (None, None) => Fact::UntypedAssignment,
        };
        match left.kind() {
            "identifier" => {
                let name = self.text(left);
                match frame.class_body {
                    // Class attributes: `session: Session`, `x = Foo()`.
                    Some(c) => self.attr(c, name, fact()),
                    None => self.var(frame.owner, name, fact()),
                }
            }
            "attribute" => {
                let obj = left.child_by_field_name("object");
                let attr = left.child_by_field_name("attribute");
                if let (Some(o), Some(a), Some(c)) = (obj, attr, frame.class)
                    && o.kind() == "identifier"
                    && Some(self.text(o)) == frame.self_name
                {
                    let name = self.text(a);
                    self.attr(c, name, fact());
                }
            }
            _ => self.untyped_target(frame.owner, left, 0),
        }
    }

    fn assign_untyped(&mut self, left: Node<'_>, frame: &Frame) {
        match left.kind() {
            "identifier" => {
                let name = self.text(left);
                self.var(frame.owner, name, Fact::UntypedAssignment);
            }
            "attribute" => {
                let obj = left.child_by_field_name("object");
                let attr = left.child_by_field_name("attribute");
                if let (Some(o), Some(a), Some(c)) = (obj, attr, frame.class)
                    && o.kind() == "identifier"
                    && Some(self.text(o)) == frame.self_name
                {
                    let name = self.text(a);
                    self.attr(c, name, Fact::UntypedAssignment);
                }
            }
            _ => {}
        }
    }
}

/// The class node whose body is `block` (directly, or through `if` / `try`
/// blocks in the class body).
fn enclosing_class_body(block: Node<'_>) -> Option<usize> {
    let mut cur = Some(block);
    while let Some(n) = cur {
        match n.kind() {
            "class_definition" => return Some(n.id()),
            "function_definition" | "module" | "lambda" => return None,
            _ => cur = n.parent(),
        }
    }
    None
}
