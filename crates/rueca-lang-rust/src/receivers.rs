//! What the file says about types: a pre-pass over its items (return types
//! of its functions and methods, field types of its structs, `impl` ranges,
//! names it declares and binds), the inferred types of local bindings, and
//! the receiver texts of calls (`$self`, `$type:T`, `$member:a.b`, a path).
//!
//! Types are file-local. A binding is typed by its annotation (`x: T`,
//! `let x: T`), else by its value: `T { … }`, `T(…)`, `T::new()` /
//! `T::default()` / `T::with_*()` / `T::from*()` (constructor convention),
//! `Self::…`, a function or method of this file with a declared return type
//! (`make()`, `self.parts()`, `x.get()` for `x: T` with `impl T { fn get() }`
//! in the file), `?` / `.unwrap()` / `.expect()` of `Option<T>` /
//! `Result<T, _>`, `.borrow()` / `.lock()` of `RefCell<T>` / `Mutex<T>`,
//! fields of structs of this file (`self.store` → its declared type), the
//! element of `for x in xs` for `Vec<T>`, slices, sets and `Option`, and
//! `Some(x)` / `Ok(x)` patterns of a typed value. A generic parameter
//! stands for its first trait bound (`T: Tr`, `where T: Tr`, `impl Tr`,
//! `dyn Tr`), `Self` for the impl's self type, and smart pointers (`Box`,
//! `Rc`, `Arc`, …) for their pointee.

use std::collections::{HashMap, HashSet};

use rueca_extract::tree_sitter::Node;
use rueca_extract::{MEMBER_RECEIVER_PREFIX, SELF_RECEIVER, TYPE_RECEIVER_PREFIX};
use rueca_model::{Import, Span};

use crate::builtins::is_builtin;
use crate::extract::{Cx, Extractor, named_children};
use crate::locals::{DefKind, text};
use crate::types::{Ty, path_segments, ty_of};
use crate::uses::ModulePath;

/// Deepest expression nesting typed.
const MAX_TYPE_DEPTH: usize = 48;

/// What a pre-pass learns about the file's items.
#[derive(Debug, Default)]
pub(crate) struct FileInfo {
    /// Return type of each function of the file, by name.
    pub(crate) fns: HashMap<String, Ty>,
    /// Return type of each method / associated function, by (self type,
    /// name), `Self` replaced.
    pub(crate) methods: HashMap<(String, String), Ty>,
    /// Field types of each struct of the file.
    pub(crate) fields: HashMap<String, HashMap<String, Ty>>,
    /// Types of the file's `const` / `static` items.
    pub(crate) values: HashMap<String, Ty>,
    /// `impl` blocks: byte range and self type.
    pub(crate) impls: Vec<(u32, u32, Ty)>,
    /// `macro_rules!` names.
    pub(crate) macros: HashSet<String>,
    /// Names of the file's structs, unions and enums (a unit struct is also
    /// a value of its type).
    pub(crate) types: HashSet<String>,
    /// Names of the items the file declares (any depth).
    pub(crate) items: HashSet<String>,
    /// Names bound by `use`, `mod` and `extern crate`.
    pub(crate) bound: HashSet<String>,
    /// Names bound to `std::env`, and to `std::env::var` / `var_os`.
    pub(crate) env_mods: HashSet<String>,
    pub(crate) env_fns: HashSet<String>,
    /// `where T: Bound` clauses: (position of `T`, bound).
    where_bounds: Vec<(u32, String, Ty)>,
}

fn name_of<'s>(node: Node<'_>, src: &'s [u8]) -> Option<&'s str> {
    node.child_by_field_name("name").map(|n| text(n, src))
}

/// Names of a node's generic parameters.
fn generic_names(node: Node<'_>, src: &[u8]) -> Vec<String> {
    let Some(tp) = node.child_by_field_name("type_parameters") else { return Vec::new() };
    named_children(tp).into_iter().filter_map(|p| name_of(p, src).map(str::to_string)).collect()
}

/// The `impl_item` or `trait_item` a function is an item of.
fn owning_impl(node: Node<'_>) -> Option<Node<'_>> {
    let list = node.parent().filter(|p| p.kind() == "declaration_list")?;
    list.parent().filter(|p| matches!(p.kind(), "impl_item" | "trait_item"))
}

/// The self type of an `impl` block, or the trait itself for a trait's
/// default methods (`self.m()` there calls the trait's `m`).
fn owner_type(cx: Cx<'_>, owner: Node<'_>) -> Option<Ty> {
    match owner.kind() {
        "trait_item" => owner.child_by_field_name("name").map(|n| Ty::named(cx.text(n))),
        _ => owner.child_by_field_name("type").and_then(|t| ty_of(cx, t)),
    }
}

impl Extractor<'_> {
    // -- pre-pass ------------------------------------------------------------

    /// Items, imports and type facts of the whole file (see the module docs).
    pub(crate) fn prepass(&mut self, root: Node<'_>) {
        let mut stack: Vec<(Node<'_>, Vec<String>, usize)> = vec![(root, Vec::new(), 0)];
        while let Some((node, mods, depth)) = stack.pop() {
            if depth > self.max_depth {
                continue;
            }
            let child_mods = self.prepass_node(node, &mods);
            let mut cursor = node.walk();
            let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
            for c in children.into_iter().rev() {
                if c.kind() != "token_tree" {
                    stack.push((c, child_mods.clone().unwrap_or_else(|| mods.clone()), depth + 1));
                }
            }
        }
    }

    /// One node of the pre-pass; the inline-module chain of its children
    /// when it opens a module.
    fn prepass_node(&mut self, node: Node<'_>, mods: &[String]) -> Option<Vec<String>> {
        let cx = Cx::main(self.src);
        match node.kind() {
            "use_declaration" => self.use_declaration(node, mods),
            "extern_crate_declaration" => self.extern_crate(node),
            "mod_item" => return self.prepass_mod(node, mods),
            "function_item" | "function_signature_item" => self.prepass_function(cx, node),
            "struct_item" | "union_item" => self.prepass_struct(cx, node),
            "const_item" | "static_item" => self.prepass_value(cx, node),
            "enum_item" | "type_item" | "macro_definition" => self.prepass_named(node),
            "impl_item" | "trait_item" => self.prepass_impl(cx, node),
            "where_predicate" => self.prepass_where(cx, node),
            _ => {}
        }
        None
    }

    /// A `const` / `static` item: its name and declared type.
    fn prepass_value(&mut self, cx: Cx<'_>, node: Node<'_>) {
        let (Some(n), Some(t)) = (name_of(node, self.src), node.child_by_field_name("type")) else { return };
        self.file.items.insert(n.to_string());
        if let Some(ty) = ty_of(cx, t) {
            self.file.values.insert(n.to_string(), ty);
        }
    }

    /// An enum, type alias or `macro_rules!`: its name.
    fn prepass_named(&mut self, node: Node<'_>) {
        let Some(n) = name_of(node, self.src).map(str::to_string) else { return };
        match node.kind() {
            "enum_item" => {
                self.file.types.insert(n.clone());
            }
            "macro_definition" => {
                self.file.macros.insert(n.clone());
            }
            _ => {}
        }
        self.file.items.insert(n);
    }

    /// An `impl` block or trait: its range and self type (the trait itself
    /// for a trait's default methods).
    fn prepass_impl(&mut self, cx: Cx<'_>, node: Node<'_>) {
        if node.kind() == "trait_item"
            && let Some(n) = name_of(node, self.src)
        {
            self.file.items.insert(n.to_string());
        }
        if let Some(t) = owner_type(cx, node) {
            self.file.impls.push((node.start_byte() as u32, node.end_byte() as u32, t));
        }
    }

    fn prepass_mod(&mut self, node: Node<'_>, mods: &[String]) -> Option<Vec<String>> {
        let name = name_of(node, self.src)?.to_string();
        if node.child_by_field_name("body").is_none() {
            self.mod_declaration(node, mods);
            return None;
        }
        self.file.items.insert(name.clone());
        let mut inner = mods.to_vec();
        inner.push(name);
        Some(inner)
    }

    fn prepass_function(&mut self, cx: Cx<'_>, node: Node<'_>) {
        let Some(name) = name_of(node, self.src).map(str::to_string) else { return };
        let imp = owning_impl(node);
        if imp.is_none() {
            self.file.items.insert(name.clone());
        }
        let Some(ret) = node.child_by_field_name("return_type").and_then(|t| ty_of(cx, t)) else { return };
        let mut generic = generic_names(node, self.src);
        if let Some(i) = imp {
            generic.extend(generic_names(i, self.src));
        }
        if ret.path.len() == 1 && generic.contains(&ret.path[0]) {
            return;
        }
        match imp.and_then(|i| owner_type(cx, i)) {
            Some(self_ty) => {
                if let Some(r) = ret.with_self(Some(&self_ty)) {
                    self.file.methods.insert((self_ty.head().to_string(), name), r);
                }
            }
            None if ret.path.first().map(String::as_str) != Some("Self") => {
                self.file.fns.entry(name).or_insert(ret);
            }
            None => {}
        }
    }

    fn prepass_struct(&mut self, cx: Cx<'_>, node: Node<'_>) {
        let Some(name) = name_of(node, self.src).map(str::to_string) else { return };
        self.file.items.insert(name.clone());
        self.file.types.insert(name.clone());
        let generic = generic_names(node, self.src);
        let Some(body) = node.child_by_field_name("body").filter(|b| b.kind() == "field_declaration_list") else {
            return;
        };
        let mut fields = HashMap::new();
        for f in named_children(body) {
            let (Some(n), Some(t)) = (f.child_by_field_name("name"), f.child_by_field_name("type")) else { continue };
            if let Some(ty) = ty_of(cx, t).filter(|ty| !(ty.path.len() == 1 && generic.contains(&ty.path[0]))) {
                fields.insert(text(n, self.src).to_string(), ty);
            }
        }
        self.file.fields.insert(name, fields);
    }

    fn prepass_where(&mut self, cx: Cx<'_>, node: Node<'_>) {
        let (Some(left), Some(bounds)) = (node.child_by_field_name("left"), node.child_by_field_name("bounds")) else {
            return;
        };
        if left.kind() != "type_identifier" {
            return;
        }
        if let Some(b) = ty_of(cx, bounds) {
            self.file.where_bounds.push((left.start_byte() as u32, text(left, self.src).to_string(), b));
        }
    }

    // -- binding types -------------------------------------------------------

    /// Types every local binding, in visibility order (a binding's value
    /// may use earlier bindings).
    pub(crate) fn infer_types(&mut self, root: Node<'_>) {
        // Generic parameters first (their bounds, then `where` clauses), so
        // values of their types see the bound.
        let (params, values): (Vec<usize>, Vec<usize>) = self
            .locals
            .inference_order()
            .into_iter()
            .partition(|&i| self.locals.bindings[i].kind == DefKind::TypeParameter);
        for i in params {
            let ty = self.binding_type(root, i);
            self.locals.bindings[i].ty = ty;
        }
        let bounds = std::mem::take(&mut self.file.where_bounds);
        for (pos, name, bound) in bounds {
            if let Some(i) = self.locals.lookup_index(&name, pos)
                && self.locals.bindings[i].kind == DefKind::TypeParameter
                && self.locals.bindings[i].ty.is_none()
            {
                self.locals.bindings[i].ty = Some(bound);
            }
        }
        for i in values {
            let ty = self.binding_type(root, i);
            self.locals.bindings[i].ty = ty;
        }
    }

    fn binding_type(&self, root: Node<'_>, i: usize) -> Option<Ty> {
        let b = &self.locals.bindings[i];
        let pos = b.pos;
        let ident = root.descendant_for_byte_range(pos as usize, pos as usize + 1)?;
        let parent = ident.parent()?;
        let cx = Cx::main(self.src);
        if b.kind == DefKind::TypeParameter {
            return parent.child_by_field_name("bounds").and_then(|bd| ty_of(cx, bd));
        }
        match parent.kind() {
            "parameter" => self.declared(cx, parent.child_by_field_name("type")?, pos),
            "let_declaration" => match parent.child_by_field_name("type") {
                Some(t) => self.declared(cx, t, pos),
                None => self.expr_type(cx, parent.child_by_field_name("value")?, 0),
            },
            "for_expression" => self.element_type(cx, parent.child_by_field_name("value")?),
            "tuple_struct_pattern" => self.unwrapped_pattern(cx, parent),
            _ => None,
        }
    }

    /// A declared type at `pos`: `Self` replaced, a generic parameter by its
    /// bound.
    fn declared(&self, cx: Cx<'_>, node: Node<'_>, pos: u32) -> Option<Ty> {
        let ty = ty_of(cx, node)?.with_self(self.impl_at(pos))?;
        if ty.path.len() == 1
            && let Some(b) = self.locals.lookup(&ty.path[0], pos).filter(|b| b.kind == DefKind::TypeParameter)
        {
            return b.ty.clone();
        }
        Some(ty)
    }

    /// The element type of what a `for` loop iterates: `xs`, `&xs`,
    /// `xs.iter()`, `xs.iter_mut()`, `xs.into_iter()`.
    fn element_type(&self, cx: Cx<'_>, value: Node<'_>) -> Option<Ty> {
        let mut v = value;
        if v.kind() == "reference_expression" {
            v = v.child_by_field_name("value")?;
        }
        if v.kind() == "call_expression"
            && let Some(f) = v.child_by_field_name("function").filter(|f| f.kind() == "field_expression")
            && matches!(f.child_by_field_name("field").map(|n| cx.text(n)), Some("iter" | "iter_mut" | "into_iter"))
        {
            v = f.child_by_field_name("value")?;
        }
        self.expr_type(cx, v, 0)?.element()
    }

    /// `Some(x)` / `Ok(x)` / `Err(e)` in `if let`, `while let` or a `match`
    /// arm on a typed value.
    fn unwrapped_pattern(&self, cx: Cx<'_>, pattern: Node<'_>) -> Option<Ty> {
        let ctor = cx.text(pattern.child_by_field_name("type")?);
        let mut cur = pattern.parent();
        let value = loop {
            let n = cur?;
            match n.kind() {
                "let_condition" => break n.child_by_field_name("value")?,
                "match_arm" => break n.parent()?.parent()?.child_by_field_name("value")?,
                "match_pattern" | "or_pattern" => cur = n.parent(),
                _ => return None,
            }
        };
        let ty = self.expr_type(cx, value, 0)?.deref();
        match (ctor, ty.head()) {
            ("Some", "Option") | ("Ok", "Result") => ty.args.into_iter().next(),
            ("Err", "Result") => ty.args.into_iter().nth(1),
            _ => None,
        }
    }

    /// Self type of the innermost `impl` block containing `pos` (the trait
    /// inside a trait).
    pub(crate) fn impl_at(&self, pos: u32) -> Option<&Ty> {
        self.file.impls.iter().filter(|(s, e, _)| *s <= pos && pos < *e).max_by_key(|(s, _, _)| *s).map(|(_, _, t)| t)
    }

    // -- expression types ----------------------------------------------------

    /// The type of an expression, from this file (see the module docs).
    pub(crate) fn expr_type(&self, cx: Cx<'_>, expr: Node<'_>, depth: usize) -> Option<Ty> {
        if depth > MAX_TYPE_DEPTH {
            return None;
        }
        let inner = || named_children(expr).into_iter().next_back();
        match expr.kind() {
            "parenthesized_expression" | "await_expression" => {
                self.expr_type(cx, *named_children(expr).first()?, depth + 1)
            }
            "reference_expression" => self.expr_type(cx, expr.child_by_field_name("value")?, depth + 1),
            "unary_expression" => self.expr_type(cx, inner()?, depth + 1).map(Ty::deref),
            "try_expression" => self.expr_type(cx, *named_children(expr).first()?, depth + 1)?.unwrapped(),
            "self" => self.impl_at(cx.pos(expr)).cloned(),
            "identifier" => self.identifier_type(cx, expr),
            "struct_expression" => self.struct_type(cx, expr),
            "call_expression" => self.call_type(cx, expr, depth),
            "field_expression" => self.field_type(cx, expr, depth),
            "index_expression" => self.expr_type(cx, *named_children(expr).first()?, depth + 1)?.element(),
            "type_cast_expression" => self.declared(cx, expr.child_by_field_name("type")?, cx.pos(expr)),
            _ => None,
        }
    }

    fn identifier_type(&self, cx: Cx<'_>, ident: Node<'_>) -> Option<Ty> {
        let name = cx.text(ident);
        match self.binding(cx, ident, name) {
            Some(b) if b.kind != DefKind::TypeParameter => b.ty.clone(),
            Some(_) => None,
            None => self.file.values.get(name).cloned().or_else(|| {
                // A unit struct used as a value (`Local.value()`).
                self.file.types.contains(name).then(|| Ty::named(name))
            }),
        }
    }

    fn struct_type(&self, cx: Cx<'_>, expr: Node<'_>) -> Option<Ty> {
        let name = expr.child_by_field_name("name")?;
        let segs = path_segments(cx, name)?;
        Ty::from_path(segs).with_self(self.impl_at(cx.pos(expr)))
    }

    fn field_type(&self, cx: Cx<'_>, expr: Node<'_>, depth: usize) -> Option<Ty> {
        let owner = self.expr_type(cx, expr.child_by_field_name("value")?, depth + 1)?.deref();
        let field = cx.text(expr.child_by_field_name("field")?);
        self.file.fields.get(owner.head())?.get(field).cloned()
    }

    fn call_type(&self, cx: Cx<'_>, call: Node<'_>, depth: usize) -> Option<Ty> {
        let mut func = call.child_by_field_name("function")?;
        if func.kind() == "generic_function" {
            func = func.child_by_field_name("function")?;
        }
        match func.kind() {
            "identifier" => {
                let name = cx.text(func);
                if self.binding(cx, func, name).is_some_and(|b| b.kind != DefKind::TypeParameter) {
                    return None;
                }
                if name.starts_with(|c: char| c.is_uppercase()) {
                    return (!is_builtin(name)).then(|| Ty::named(name));
                }
                self.file.fns.get(name).cloned()
            }
            "scoped_identifier" => self.path_call_type(cx, func),
            "field_expression" => {
                let recv = self.expr_type(cx, func.child_by_field_name("value")?, depth + 1)?;
                self.method_result(recv, cx.text(func.child_by_field_name("field")?))
            }
            _ => None,
        }
    }

    /// `T::f(…)`: the declared return type of `T::f` in this file, else the
    /// constructor convention; `E::V(…)` is an `E`.
    fn path_call_type(&self, cx: Cx<'_>, func: Node<'_>) -> Option<Ty> {
        let mut segs = path_segments(cx, func)?;
        let f = segs.pop()?;
        let owner = Ty::from_path(segs).with_self(self.impl_at(cx.pos(func)))?;
        if !owner.head().starts_with(|c: char| c.is_uppercase()) {
            return None;
        }
        if f.starts_with(|c: char| c.is_uppercase()) {
            return Some(owner);
        }
        if let Some(r) = self.file.methods.get(&(owner.head().to_string(), f.clone())) {
            return Some(r.clone());
        }
        is_constructor_name(&f).then_some(owner)
    }

    /// The result of method `m` called on a `recv`.
    fn method_result(&self, recv: Ty, m: &str) -> Option<Ty> {
        let recv = recv.deref();
        match m {
            "unwrap" | "expect" | "unwrap_or" | "unwrap_or_default" | "unwrap_or_else" | "unwrap_unchecked" => {
                return recv.unwrapped();
            }
            "clone" | "to_owned" | "as_ref" | "as_mut" | "by_ref" => return Some(recv),
            _ => {}
        }
        match (recv.head(), m) {
            ("RefCell", "borrow" | "borrow_mut") | ("Mutex" | "RwLock", "get_mut" | "into_inner") => {
                recv.args.into_iter().next()
            }
            ("Mutex", "lock") | ("RwLock", "read" | "write") => {
                Some(Ty { path: vec!["Result".into()], args: recv.args.into_iter().take(1).collect() })
            }
            ("Vec" | "[]" | "VecDeque", "iter" | "iter_mut" | "into_iter") => {
                Some(Ty { path: vec!["Iter".into()], args: recv.args.into_iter().take(1).collect() })
            }
            ("Vec" | "[]" | "VecDeque" | "Iter", "first" | "last" | "get" | "get_mut" | "pop" | "next") => {
                Some(Ty { path: vec!["Option".into()], args: recv.args.into_iter().take(1).collect() })
            }
            (head, _) => self.file.methods.get(&(head.to_string(), m.to_string())).cloned(),
        }
    }

    // -- receivers -----------------------------------------------------------

    /// Registers a synthetic import of a path anchor (see the crate docs).
    fn anchor(&mut self, alias: &str, path: ModulePath, span: Span) {
        self.anchors.entry(alias.to_string()).or_insert_with(|| Import {
            module: path.module,
            relative_level: path.level,
            names: Vec::new(),
            module_alias: Some(alias.to_string()),
            type_only: true,
            reexport: false,
            span,
        });
    }

    /// Normalizes the segments of a path written at `node` (see the crate
    /// docs, *Paths*): `self` / `super` inside the file become the inline
    /// module chain, other anchors (`crate`, `super` beyond the file, a
    /// crate name) are bound by synthetic imports.
    pub(crate) fn normalize_path(&mut self, cx: Cx<'_>, segs: &[String], node: Node<'_>) -> Vec<String> {
        let Some(first) = segs.first() else { return Vec::new() };
        let span = self.span(cx, node);
        let mods = self.container.mods.clone();
        match first.as_str() {
            "" if segs.len() > 1 => {
                let name = segs[1].clone();
                self.anchor(&name, ModulePath { level: 0, module: format!("::{name}") }, span);
                segs[1..].to_vec()
            }
            "crate" | "$crate" => {
                self.anchor("crate", ModulePath { level: 0, module: "crate".into() }, span);
                let mut out = vec!["crate".to_string()];
                out.extend_from_slice(&segs[1..]);
                out
            }
            "self" => mods.into_iter().chain(segs[1..].iter().cloned()).collect(),
            "super" => self.super_path(segs, &mods, span),
            name if self.is_extern_anchor(cx, name, node) => {
                self.anchor(name, ModulePath { level: 0, module: name.to_string() }, span);
                segs.to_vec()
            }
            _ => segs.to_vec(),
        }
    }

    /// `super::…` paths: inside the file, the enclosing inline modules;
    /// beyond it, a synthetic import of the ancestor module (`super$1`,
    /// `super$2`, …).
    fn super_path(&mut self, segs: &[String], mods: &[String], span: Span) -> Vec<String> {
        let k = segs.iter().take_while(|s| *s == "super").count();
        let rest = &segs[k..];
        if k <= mods.len() {
            return mods[..mods.len() - k].iter().chain(rest).cloned().collect();
        }
        let up = k - mods.len();
        // `super` alone would read as the `super` receiver of other languages.
        let alias = format!("super${up}");
        self.anchor(&alias, ModulePath { level: 1 + up as u32, module: String::new() }, span);
        std::iter::once(alias).chain(rest.iter().cloned()).collect()
    }

    /// Whether the first segment of a path is a crate name: lower-case, not
    /// bound or declared in the file, not a local, generic or builtin.
    fn is_extern_anchor(&self, cx: Cx<'_>, name: &str, node: Node<'_>) -> bool {
        name.starts_with(|c: char| c.is_lowercase() || c == '_')
            && !self.file.bound.contains(name)
            && !self.file.items.contains(name)
            && !is_builtin(name)
            && self.binding(cx, node, name).is_none()
    }

    /// Receiver text of a path qualifier (`a::b` in `a::b::f()`): `None`
    /// when it names the current module.
    pub(crate) fn path_receiver(&mut self, cx: Cx<'_>, segs: &[String], node: Node<'_>) -> Option<String> {
        if segs.first().map(String::as_str) == Some("Self") {
            return Some(SELF_RECEIVER.to_string());
        }
        if segs.len() == 1 && self.is_type_param(cx, node, &segs[0]) {
            let bound = self.binding(cx, node, &segs[0]).and_then(|b| b.ty.clone());
            return Some(match bound {
                Some(t) => self.type_receiver(cx, &t, node),
                None => format!("({})", segs[0]),
            });
        }
        let norm = self.normalize_path(cx, segs, node);
        (!norm.is_empty()).then(|| norm.join("."))
    }

    /// `$type:` receiver text of a type: a generic parameter stands for its
    /// bound, the element and iterator types this module invents for
    /// slices are the standard library's (external).
    pub(crate) fn type_receiver(&mut self, cx: Cx<'_>, t: &Ty, node: Node<'_>) -> String {
        if t.path.len() == 1 {
            let name = t.path[0].as_str();
            match name {
                "[]" => return format!("{TYPE_RECEIVER_PREFIX}std.slice"),
                "Iter" if !self.file.items.contains("Iter") => return format!("{TYPE_RECEIVER_PREFIX}std.slice.Iter"),
                _ => {}
            }
            if self.is_type_param(cx, node, name) {
                let bound = self.binding(cx, node, name).and_then(|b| b.ty.clone());
                return match bound {
                    Some(b) if !(b.path.len() == 1 && self.is_type_param(cx, node, &b.path[0])) => {
                        let norm = self.normalize_path(cx, &b.path, node);
                        format!("{TYPE_RECEIVER_PREFIX}{}", norm.join("."))
                    }
                    _ => format!("({name})"),
                };
            }
        }
        let norm = self.normalize_path(cx, &t.path, node);
        if norm.is_empty() { "(?)".to_string() } else { format!("{TYPE_RECEIVER_PREFIX}{}", norm.join(".")) }
    }

    /// Receiver text of a method call's value (see the crate docs,
    /// *Receivers*).
    pub(crate) fn receiver_of(&mut self, cx: Cx<'_>, value: Node<'_>) -> String {
        if value.kind() == "self" {
            return SELF_RECEIVER.to_string();
        }
        if let Some(t) = self.expr_type(cx, value, 0) {
            return self.type_receiver(cx, &t.deref(), value);
        }
        match value.kind() {
            "field_expression" => match self_chain(cx, value) {
                Some(chain) => format!("{MEMBER_RECEIVER_PREFIX}{}", chain.join(".")),
                None => "(…)".to_string(),
            },
            "identifier" => {
                let name = cx.text(value);
                if self.binding(cx, value, name).is_some() { format!("({name})") } else { name.to_string() }
            }
            _ => "(…)".to_string(),
        }
    }
}

/// `self.a.b` → `["a", "b"]` (fields of `self`).
fn self_chain(cx: Cx<'_>, expr: Node<'_>) -> Option<Vec<String>> {
    let mut chain = Vec::new();
    let mut cur = expr;
    for _ in 0..16 {
        match cur.kind() {
            "field_expression" => {
                chain.push(cx.text(cur.child_by_field_name("field")?).to_string());
                cur = cur.child_by_field_name("value")?;
            }
            "self" => {
                chain.reverse();
                return Some(chain);
            }
            _ => return None,
        }
    }
    None
}

/// Constructor convention: `new`, `default`, `with_*`, `from`, `from_*`.
fn is_constructor_name(f: &str) -> bool {
    matches!(f, "new" | "default" | "from" | "create" | "build")
        || f.starts_with("with_")
        || f.starts_with("from_")
        || f.starts_with("new_")
}
