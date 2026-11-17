//! Types as written in a file, reduced to what receiver typing needs: a
//! path and its type arguments (`fmt::Formatter`, `Vec<Item>`), read from
//! type nodes; references, pointers, `dyn` / `impl` traits and bounded types
//! are seen through.

use rueca_extract::tree_sitter::Node;

use crate::builtins::is_deref_wrapper;
use crate::extract::{Cx, named_children};

/// A named type: path segments as written and type arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Ty {
    pub(crate) path: Vec<String>,
    pub(crate) args: Vec<Ty>,
}

impl Ty {
    pub(crate) fn named(name: &str) -> Ty {
        Ty { path: vec![name.to_string()], args: Vec::new() }
    }

    pub(crate) fn from_path(path: Vec<String>) -> Ty {
        Ty { path, args: Vec::new() }
    }

    /// Last path segment (`Formatter` for `fmt::Formatter`).
    pub(crate) fn head(&self) -> &str {
        self.path.last().map_or("", String::as_str)
    }

    /// Through smart pointers (`Box<T>`, `Arc<T>`, `Rc<T>`, …), which
    /// dereference to `T`; at most 8 levels.
    pub(crate) fn deref(mut self) -> Ty {
        for _ in 0..8 {
            if !is_deref_wrapper(self.head()) || self.args.is_empty() {
                break;
            }
            self = self.args.swap_remove(0);
        }
        self
    }

    /// `T` of `Option<T>` / `Result<T, E>` (the value `?`, `unwrap()` and
    /// `expect()` give).
    pub(crate) fn unwrapped(self) -> Option<Ty> {
        let t = self.deref();
        match t.head() {
            "Option" | "Result" | "Poll" => t.args.into_iter().next(),
            _ => None,
        }
    }

    /// Element type of a collection iterated by `for` (`Vec<T>`, `[T]`,
    /// `HashSet<T>`, `Option<T>`, …).
    pub(crate) fn element(self) -> Option<Ty> {
        let t = self.deref();
        match t.head() {
            "Vec" | "[]" | "VecDeque" | "HashSet" | "BTreeSet" | "BinaryHeap" | "LinkedList" | "Option"
            | "IntoIter" | "Iter" => t.args.into_iter().next(),
            _ => None,
        }
    }

    /// Replaces `Self` by `self_ty` (the impl's self type), in the path and
    /// the arguments.
    pub(crate) fn with_self(mut self, self_ty: Option<&Ty>) -> Option<Ty> {
        if self.path.first().map(String::as_str) == Some("Self") {
            let st = self_ty?;
            let mut path = st.path.clone();
            path.extend(self.path.drain(1..));
            self.path = path;
            if self.args.is_empty() && self.path.len() == st.path.len() {
                self.args = st.args.clone();
            }
        }
        self.args = self.args.into_iter().filter_map(|a| a.with_self(self_ty)).collect();
        Some(self)
    }
}

/// Path segments of a path node (`identifier`, `scoped_identifier`,
/// `scoped_type_identifier`, `crate`, `super`, `self`, generic types in
/// paths), as written. `None` for paths with a bracketed or qualified head.
pub(crate) fn path_segments(cx: Cx<'_>, node: Node<'_>) -> Option<Vec<String>> {
    let mut segs = Vec::new();
    let mut cur = node;
    for _ in 0..64 {
        match cur.kind() {
            "identifier" | "type_identifier" | "crate" | "super" | "self" | "metavariable" => {
                segs.push(cx.text(cur).to_string());
                break;
            }
            "scoped_identifier" | "scoped_type_identifier" => {
                let name = cur.child_by_field_name("name")?;
                segs.push(cx.text(name).to_string());
                match cur.child_by_field_name("path") {
                    Some(p) => cur = p,
                    // `::name`: an extern crate.
                    None => {
                        segs.push(String::new());
                        break;
                    }
                }
            }
            "generic_type" => cur = cur.child_by_field_name("type")?,
            _ => return None,
        }
    }
    segs.reverse();
    Some(segs)
}

/// The type a type node names (see the module docs); `None` for tuples,
/// function types, arrays of unnamed types and the never type.
pub(crate) fn ty_of(cx: Cx<'_>, node: Node<'_>) -> Option<Ty> {
    let mut cur = node;
    for _ in 0..64 {
        match cur.kind() {
            "reference_type" | "pointer_type" | "parenthesized_type" => cur = type_child(cur)?,
            "dynamic_type" | "abstract_type" => cur = cur.child_by_field_name("trait")?,
            "bounded_type" | "trait_bounds" => cur = first_bound(cur)?,
            "qualified_type" => cur = cur.child_by_field_name("alias")?,
            "bracketed_type" => cur = *named_children(cur).first()?,
            "higher_ranked_trait_bound" => cur = cur.child_by_field_name("type")?,
            "array_type" => {
                let elem = ty_of(cx, cur.child_by_field_name("element")?)?;
                return Some(Ty { path: vec!["[]".into()], args: vec![elem] });
            }
            "generic_type" => return generic(cx, cur),
            "type_identifier" | "identifier" | "scoped_type_identifier" | "scoped_identifier" | "primitive_type" => {
                let path = if cur.kind() == "primitive_type" {
                    vec![cx.text(cur).to_string()]
                } else {
                    path_segments(cx, cur)?
                };
                return Some(Ty::from_path(path));
            }
            _ => return None,
        }
    }
    None
}

/// The `type` child of a reference, pointer or parenthesized type.
fn type_child(node: Node<'_>) -> Option<Node<'_>> {
    node.child_by_field_name("type").or_else(|| named_children(node).into_iter().find(|c| c.kind() != "lifetime"))
}

/// The first bound of a bounded type or bound list that is a type (not a
/// lifetime, not `?Sized`).
fn first_bound(node: Node<'_>) -> Option<Node<'_>> {
    named_children(node).into_iter().find(|c| !matches!(c.kind(), "lifetime" | "removed_trait_bound" | "use_bounds"))
}

fn generic(cx: Cx<'_>, node: Node<'_>) -> Option<Ty> {
    let mut ty = Ty::from_path(path_segments(cx, node.child_by_field_name("type")?)?);
    if let Some(args) = node.child_by_field_name("type_arguments") {
        for a in named_children(args) {
            if let Some(t) = ty_of(cx, a) {
                ty.args.push(t);
            }
        }
    }
    Some(ty)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrappers() {
        let inner = Ty::named("Store");
        let t = Ty { path: vec!["Arc".into()], args: vec![Ty { path: vec!["Box".into()], args: vec![inner.clone()] }] };
        assert_eq!(t.deref(), inner);
        let opt = Ty { path: vec!["Option".into()], args: vec![inner.clone()] };
        assert_eq!(opt.clone().unwrapped(), Some(inner.clone()));
        assert_eq!(opt.element(), Some(inner.clone()));
        assert_eq!(Ty::named("Store").unwrapped(), None);
        let s = Ty::from_path(vec!["Self".into()]).with_self(Some(&Ty::from_path(vec!["a".into(), "S".into()])));
        assert_eq!(s.unwrap().path.join("."), "a.S");
        assert_eq!(Ty::named("Self").with_self(None), None);
    }
}
