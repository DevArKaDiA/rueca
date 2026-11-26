//! Ruby resolver rules: global constants looked up through the lexical
//! nesting and the ancestors, open classes, mixins and method lookup.
//!
//! The Ruby pack names every definition by its global constant path
//! (`A::B`, methods `A::B#m` / `A::B.m`, top-level `def`s by name; crate
//! docs of `rueca-lang-ruby`). A class may be reopened in many files: every
//! reopening is a node, and they share one table entry, so a method defined
//! in one file is found from a call in another.
//!
//! - **Constants** (`Foo`, `A::B`, the classes of receivers, superclasses
//!   and mixins) are looked up like Ruby does: the lexical nesting of the
//!   reference (`Module.nesting`: the enclosing class and module bodies,
//!   innermost first; a compact `class A::B` body is one level), then the
//!   ancestors of the innermost one, then the top level; further segments
//!   (`::B`) in the constant found, then in its ancestors (`import_resolved`
//!   0.95). A constant written `::A` is looked up from the top level only.
//!   A reopened class is one constant; the node a reference points to is
//!   the definition in the file Zeitwerk would load for it
//!   (`app/models/foo/bar.rb` for `Foo::Bar`), else the first with a
//!   superclass, else the first in path order.
//! - **Ancestors** (method resolution order): the prepended modules (last
//!   first), the class, its included modules (last first), then the
//!   superclass, recursively; singleton methods: the class's own, the
//!   modules it extends (`extend self` = its instance methods), then the
//!   superclass's. `module_function` methods are both. A class whose
//!   superclass or a mixin is outside the project is *open*: a method it
//!   does not define is external, never a miss.
//! - **Calls** by receiver:
//!
//! | Receiver | Lookup | Edge strategy | Confidence |
//! |---|---|---|---|
//! | none / `self` in an instance method | instance methods of the class, then its ancestors, then the top-level `def`s | `type_resolved` | 0.90 own / 0.85 inherited |
//! | none / `self` in a class body or a singleton method; `self.class` | singleton methods of the class and its ancestors | `type_resolved` | 0.90 / 0.85 |
//! | none at top level / in a test block | the top-level `def` of that name (the file's, else the only one) | `same_module` / `import_resolved` | 0.90 / 0.85 |
//! | `$super` | the same method in the ancestors after the enclosing class | `type_resolved` | 0.85 |
//! | a constant path (`Foo.bar`, `A::B.new`) | singleton methods of the constant; `new` without a singleton `new` is the class (and its `initialize`, × 0.95) | `import_resolved` | 0.85 |
//! | `$type:T` (a variable typed in the file) | instance methods of `T` (resolved at the reference) | `type_resolved` | 0.85 |
//! | `$ret:<receiver>#<name>` (a call's result) | the call is resolved, its declared return type (`#: -> T` in the callee's signature) resolved in the callee's namespace, then as `$type:T` | `type_resolved` | 0.85 |
//! | `?` / an untyped result | the only instance method of that name in the project (name ≥ 4 characters, not a core-class method such as `each` or `fetch`, not a test) | `heuristic` | 0.50 |
//!
//! `Write`s (`self.a = v`) find the accessor `a` like a call. `Read`,
//! `Type` and `Throw` references name constants. Superclasses and mixins
//! become `INHERITS` edges from each reopening that lists them.

use super::*;

/// Confidence of a constant resolved by Ruby's lookup.
const CONST_CONFIDENCE: f32 = 0.95;
/// Shortest method name the by-name rung binds.
const MIN_HEURISTIC_NAME: usize = 4;
/// Nesting bound of `$ret:` evaluations and ancestor walks.
const MAX_RUBY_DEPTH: usize = 12;

/// The prefix of a call-result receiver.
const RET_PREFIX: &str = "$ret:";

/// Methods of core classes (`String`, `Array`, `Hash`, `Enumerable`,
/// `Object`, `IO`, …) so common that a call on an untyped receiver is far
/// more likely to reach the core class than a project method of the same
/// name: the by-name rung never binds them.
const CORE_METHODS: &[&str] = &[
    "bytesize",
    "has_key?",
    "to_ary",
    "to_str",
    "string",
    "each",
    "map",
    "select",
    "reject",
    "find",
    "detect",
    "each_with_index",
    "each_with_object",
    "inject",
    "reduce",
    "sum",
    "count",
    "size",
    "length",
    "empty?",
    "any?",
    "all?",
    "none?",
    "first",
    "last",
    "push",
    "pop",
    "shift",
    "unshift",
    "include?",
    "key?",
    "keys",
    "values",
    "fetch",
    "store",
    "delete",
    "merge",
    "merge!",
    "update",
    "dup",
    "clone",
    "to_s",
    "to_a",
    "to_h",
    "to_i",
    "to_f",
    "to_sym",
    "to_proc",
    "join",
    "split",
    "strip",
    "gsub",
    "sub",
    "match",
    "match?",
    "start_with?",
    "end_with?",
    "upcase",
    "downcase",
    "capitalize",
    "chomp",
    "chars",
    "bytes",
    "lines",
    "sort",
    "sort_by",
    "min",
    "max",
    "min_by",
    "max_by",
    "group_by",
    "partition",
    "flat_map",
    "compact",
    "flatten",
    "uniq",
    "zip",
    "take",
    "drop",
    "slice",
    "concat",
    "replace",
    "clear",
    "freeze",
    "frozen?",
    "nil?",
    "is_a?",
    "kind_of?",
    "respond_to?",
    "send",
    "public_send",
    "call",
    "new",
    "class",
    "name",
    "hash",
    "eql?",
    "equal?",
    "inspect",
    "then",
    "tap",
    "read",
    "write",
    "close",
    "open",
    "puts",
    "print",
    "format",
    "encode",
    "force_encoding",
    "valid_encoding?",
    "filter",
    "filter_map",
    "each_pair",
    "each_key",
    "each_value",
    "transform_values",
    "transform_keys",
    "dig",
    "key",
    "value",
    "values_at",
    "sample",
    "shuffle",
    "reverse",
    "rotate",
    "index",
    "find_index",
    "insert",
    "append",
    "prepend",
    "start",
    "stop",
    "run",
    "wait",
    "join",
    "kill",
    "lock",
    "unlock",
    "synchronize",
    "set",
    "get",
    "add",
    "remove",
    "reset",
    "parse",
    "load",
    "dump",
    "path",
    "exist?",
    "exists?",
    "file?",
    "directory?",
    "basename",
    "dirname",
    "extname",
    "expand_path",
    "now",
    "error",
    "info",
    "debug",
    "warn",
    "message",
    "backtrace",
    "result",
    "status",
    "body",
    "headers",
    "params",
    "request",
    "response",
    "id",
    "type",
    "data",
    "options",
    "config",
    "env",
    "logger",
    "empty",
    "present?",
    "blank?",
    "presence",
    "try",
    "between?",
    "clamp",
];

fn is_core_method(name: &str) -> bool {
    CORE_METHODS.contains(&name)
}

/// Ruby tables of a snapshot (built by [`Resolver::ruby_build`]).
#[derive(Default)]
pub(crate) struct RubyTables {
    /// Class and module nodes by constant path, path order.
    namespaces: HashMap<String, Vec<NodeId>>,
    /// The node a reference to a namespace points to.
    canonical: HashMap<String, NodeId>,
    /// `Constant` nodes by path.
    constants: HashMap<String, Vec<NodeId>>,
    /// Instance and singleton members by namespace path and name.
    imembers: HashMap<String, BTreeMap<String, Vec<NodeId>>>,
    smembers: HashMap<String, BTreeMap<String, Vec<NodeId>>>,
    /// Top-level `def`s by name.
    functions: HashMap<String, Vec<NodeId>>,
    /// Resolved superclass of each class path.
    supers: HashMap<String, String>,
    /// Resolved mixins per namespace path, in body order.
    includes: HashMap<String, Vec<String>>,
    prepends: HashMap<String, Vec<String>>,
    extends: HashMap<String, Vec<String>>,
    /// Namespaces with a superclass or mixin outside the project.
    open: HashSet<String>,
    /// Non-test instance methods by name (the by-name rung).
    methods_by_name: HashMap<String, Vec<NodeId>>,
}

/// What `self` is in a piece of Ruby code.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SelfCtx {
    /// An instance of the class (an instance method).
    Instance(String),
    /// The class itself (a class body, a singleton method).
    Singleton(String),
    /// The top-level object (or an RSpec example).
    Top,
}

/// A method found in a namespace or its ancestors.
struct Found {
    node: NodeId,
    inherited: bool,
}

/// Splits a method's qualified name: (`A::B`, `m`, singleton).
fn split_member(qn: &str) -> Option<(&str, &str, bool)> {
    if let Some((ns, name)) = qn.rsplit_once('#') {
        return Some((ns, name, false));
    }
    let (ns, name) = qn.rsplit_once('.')?;
    Some((ns, name, true))
}

/// The original name an alias's signature names (`alias n o` → `o`,
/// `alias_method :n, :o` → `o`).
fn alias_target(sig: &str) -> Option<&str> {
    if let Some(rest) =
        sig.trim_start_matches("private ").trim_start_matches("protected ").strip_prefix("alias_method ")
    {
        return rest.split(',').nth(1).map(|o| o.trim().trim_start_matches(':').trim_matches('"'));
    }
    let rest = sig.trim_start_matches("private ").trim_start_matches("protected ").strip_prefix("alias ")?;
    rest.split_whitespace().nth(1).map(|o| o.trim_start_matches(':'))
}

/// A superclass entry of `Definition::bases` (not `include:M` & co.).
fn is_super_base(b: &str) -> bool {
    !["include:", "extend:", "prepend:"].iter().any(|p| b.starts_with(p))
}

/// `Foo::BarBaz` → `foo/bar_baz` (Zeitwerk's file name for a constant).
fn zeitwerk_path(path: &str) -> String {
    let mut out = String::new();
    for (k, seg) in path.split("::").enumerate() {
        if k > 0 {
            out.push('/');
        }
        for (i, c) in seg.chars().enumerate() {
            if c.is_ascii_uppercase() {
                let prev_lower =
                    i > 0 && seg.chars().nth(i - 1).is_some_and(|p| p.is_ascii_lowercase() || p.is_ascii_digit());
                if prev_lower {
                    out.push('_');
                }
                out.push(c.to_ascii_lowercase());
            } else {
                out.push(c);
            }
        }
    }
    out
}

/// The namespace paths of a qualified name's prefixes, innermost first
/// (`A::B::C` → `A::B::C`, `A::B`, `A`): the nesting assumed for a
/// definition seen only through its node.
fn prefix_nesting(path: &str) -> Vec<String> {
    let segs: Vec<&str> = path.split("::").collect();
    (1..=segs.len()).rev().map(|k| segs[..k].join("::")).collect()
}

impl<'a> Resolver<'a> {
    pub(crate) fn is_ruby(&self, f: FileId) -> bool {
        self.families[f as usize] == Some(LanguageFamily::Ruby)
    }

    // -----------------------------------------------------------------
    // Tables
    // -----------------------------------------------------------------

    /// Builds the Ruby tables and resolves superclasses and mixins
    /// (`INHERITS`). Runs before the generic base pass, which skips Ruby.
    pub(crate) fn ruby_build(&mut self) {
        let ruby_files: Vec<FileId> = (0..self.files.len() as FileId).filter(|&f| self.is_ruby(f)).collect();
        if ruby_files.is_empty() {
            return;
        }
        let mut t = RubyTables::default();
        let mut seen: HashSet<NodeId> = HashSet::new();
        for &f in &ruby_files {
            for j in 0..self.defs(f).len() {
                let n = self.node_of(f, j as LocalDefId);
                if seen.insert(n) {
                    self.ruby_index_def(&mut t, f, j);
                }
            }
        }
        self.ruby = t;
        self.ruby_pick_canonical();
        self.ruby_resolve_ancestors(&ruby_files);
    }

    fn ruby_index_def(&self, t: &mut RubyTables, f: FileId, j: usize) {
        let d = &self.defs(f)[j];
        let n = self.node_of(f, j as LocalDefId);
        match d.label {
            Label::Class | Label::Module => t.namespaces.entry(d.qualified_name.clone()).or_default().push(n),
            Label::Constant => t.constants.entry(d.qualified_name.clone()).or_default().push(n),
            Label::Function => t.functions.entry(d.name.clone()).or_default().push(n),
            Label::Method | Label::Field => self.ruby_index_member(t, d, n),
            _ => {}
        }
    }

    fn ruby_index_member(&self, t: &mut RubyTables, d: &Definition, n: NodeId) {
        let Some((ns, name, singleton)) = split_member(&d.qualified_name) else { return };
        let module_function = d.signature.as_deref().is_some_and(|s| s.starts_with("module_function "));
        if singleton {
            t.smembers.entry(ns.to_string()).or_default().entry(name.to_string()).or_default().push(n);
        }
        if !singleton || module_function {
            t.imembers.entry(ns.to_string()).or_default().entry(name.to_string()).or_default().push(n);
            if !d.is_test && d.label == Label::Method && !self.nodes[n as usize].is_test {
                t.methods_by_name.entry(name.to_string()).or_default().push(n);
            }
        }
    }

    /// The node each namespace path stands for (module docs).
    fn ruby_pick_canonical(&mut self) {
        let mut canonical = HashMap::new();
        for (path, nodes) in &self.ruby.namespaces {
            let want = format!("{}.rb", zeitwerk_path(path));
            let by_file = nodes.iter().copied().find(|&n| {
                self.nodes[n as usize].file.is_some_and(|f| {
                    let p = &self.files[f as usize].path;
                    p == &want || p.ends_with(&format!("/{want}"))
                })
            });
            let with_super = || {
                nodes
                    .iter()
                    .copied()
                    .find(|&n| self.ruby_def_of(n).is_some_and(|d| d.bases.iter().any(|b| is_super_base(b))))
            };
            let pick = by_file.or_else(with_super).unwrap_or(nodes[0]);
            canonical.insert(path.clone(), pick);
        }
        self.ruby.canonical = canonical;
    }

    /// The definition behind a node (its file's facts).
    fn ruby_def_of(&self, n: NodeId) -> Option<&'a Definition> {
        let f = self.nodes[n as usize].file?;
        let defs = self.defs(f);
        (0..defs.len()).find(|&j| self.node_of(f, j as LocalDefId) == n).map(|j| &defs[j])
    }

    /// Superclasses and mixins of every class and module, resolved in the
    /// scope around each definition; `INHERITS` edges.
    fn ruby_resolve_ancestors(&mut self, files: &[FileId]) {
        let mut edges: Vec<(NodeId, NodeId, FileId, Span)> = Vec::new();
        for &f in files {
            for j in 0..self.defs(f).len() {
                self.ruby_def_bases(f, j, &mut edges);
            }
        }
        for (src, dst, f, span) in edges {
            if src != dst {
                let meta = EdgeMeta {
                    confidence: CONST_CONFIDENCE,
                    strategy: Strategy::ImportResolved,
                    evidence: Some((f, span)),
                };
                self.add_edge(src, dst, EdgeKind::Inherits, meta);
            }
        }
    }

    /// The bases of definition `j` of file `f` (a class or module): the
    /// superclass is evaluated around the class, mixins in its body.
    fn ruby_def_bases(&mut self, f: FileId, j: usize, edges: &mut Vec<(NodeId, NodeId, FileId, Span)>) {
        let d = &self.defs(f)[j];
        if !matches!(d.label, Label::Class | Label::Module) || d.bases.is_empty() {
            return;
        }
        let node = self.node_of(f, j as LocalDefId);
        let outer = self.ruby_nesting(f, self.parents[f as usize][j]);
        let inner = self.ruby_nesting(f, Some(j as LocalDefId));
        for base in &d.bases {
            let nesting = if is_super_base(base) { &outer } else { &inner };
            if let Some(target) = self.ruby_record_base(&d.qualified_name, base, nesting, d.label) {
                edges.push((node, target, f, d.name_span));
            }
        }
    }

    /// Records one base of namespace `owner`; the node it points to.
    fn ruby_record_base(&mut self, owner: &str, base: &str, nesting: &[String], label: Label) -> Option<NodeId> {
        let (kind, text) = match base.split_once(':') {
            Some((k @ ("include" | "extend" | "prepend"), rest)) => (k, rest),
            _ => ("super", base),
        };
        if text == "self" {
            if kind == "extend" {
                self.ruby.extends.entry(owner.to_string()).or_default().push(owner.to_string());
            }
            return None;
        }
        let Some(path) = self.ruby_const(text, nesting) else {
            if !(kind == "super" && label == Label::Class && matches!(text, "Object" | "::Object" | "BasicObject")) {
                self.ruby.open.insert(owner.to_string());
            }
            return None;
        };
        if !self.ruby.namespaces.contains_key(&path) {
            self.ruby.open.insert(owner.to_string());
            return None;
        }
        let map = match kind {
            "include" => &mut self.ruby.includes,
            "extend" => &mut self.ruby.extends,
            "prepend" => &mut self.ruby.prepends,
            _ => {
                self.ruby.supers.entry(owner.to_string()).or_insert_with(|| path.clone());
                return self.ruby.canonical.get(&path).copied();
            }
        };
        let list = map.entry(owner.to_string()).or_default();
        if !list.contains(&path) {
            list.push(path.clone());
        }
        self.ruby.canonical.get(&path).copied()
    }

    // -----------------------------------------------------------------
    // Constants
    // -----------------------------------------------------------------

    /// `Module.nesting` at a definition scope of file `f`: the enclosing
    /// classes and modules, innermost first.
    fn ruby_nesting(&self, f: FileId, scope: Option<LocalDefId>) -> Vec<String> {
        let defs = self.defs(f);
        let mut out = Vec::new();
        let mut cur = scope;
        let mut guard = 0;
        while let Some(j) = cur {
            let d = &defs[j as usize];
            if matches!(d.label, Label::Class | Label::Module) {
                out.push(d.qualified_name.clone());
            }
            cur = self.parents[f as usize][j as usize];
            guard += 1;
            if guard > 256 {
                break;
            }
        }
        out
    }

    fn ruby_const_exists(&self, path: &str) -> bool {
        self.ruby.namespaces.contains_key(path) || self.ruby.constants.contains_key(path)
    }

    /// Resolves a constant path written at `nesting` to its global path.
    fn ruby_const(&self, written: &str, nesting: &[String]) -> Option<String> {
        let (absolute, rest) = match written.strip_prefix("::") {
            Some(r) => (true, r),
            None => (false, written),
        };
        let mut segs = rest.split("::").filter(|s| !s.is_empty());
        let head = segs.next()?;
        let mut cur = if absolute {
            self.ruby_const_exists(head).then(|| head.to_string())
        } else {
            self.ruby_head(head, nesting)
        }?;
        for seg in segs {
            cur = self.ruby_scoped(&cur, seg)?;
        }
        Some(cur)
    }

    /// The first segment of a relative constant path: lexical scopes, the
    /// ancestors of the innermost one, then the top level.
    fn ruby_head(&self, head: &str, nesting: &[String]) -> Option<String> {
        for scope in nesting {
            let p = format!("{scope}::{head}");
            if self.ruby_const_exists(&p) {
                return Some(p);
            }
        }
        if let Some(inner) = nesting.first() {
            for anc in self.ruby_ancestors(inner) {
                let p = format!("{anc}::{head}");
                if self.ruby_const_exists(&p) {
                    return Some(p);
                }
            }
        }
        self.ruby_const_exists(head).then(|| head.to_string())
    }

    /// `seg` inside the constant `scope`, then inside its ancestors.
    fn ruby_scoped(&self, scope: &str, seg: &str) -> Option<String> {
        let p = format!("{scope}::{seg}");
        if self.ruby_const_exists(&p) {
            return Some(p);
        }
        self.ruby_ancestors(scope)
            .into_iter()
            .skip(1)
            .map(|a| format!("{a}::{seg}"))
            .find(|p| self.ruby_const_exists(p))
    }

    /// The node a constant path stands for.
    fn ruby_const_node(&self, path: &str) -> Option<NodeId> {
        self.ruby
            .canonical
            .get(path)
            .copied()
            .or_else(|| self.ruby.constants.get(path).and_then(|v| v.first().copied()))
    }

    // -----------------------------------------------------------------
    // Ancestors and methods
    // -----------------------------------------------------------------

    /// Instance ancestors of a namespace, itself included (module docs).
    fn ruby_ancestors(&self, path: &str) -> Vec<String> {
        let mut out = Vec::new();
        self.ruby_ancestors_into(path, &mut out, 0);
        out
    }

    fn ruby_ancestors_into(&self, path: &str, out: &mut Vec<String>, depth: usize) {
        if depth > MAX_RUBY_DEPTH || out.iter().any(|p| p == path) {
            return;
        }
        for p in self.ruby.prepends.get(path).into_iter().flatten().rev() {
            self.ruby_ancestors_into(p, out, depth + 1);
        }
        out.push(path.to_string());
        for m in self.ruby.includes.get(path).into_iter().flatten().rev() {
            self.ruby_ancestors_into(m, out, depth + 1);
        }
        if let Some(s) = self.ruby.supers.get(path) {
            self.ruby_ancestors_into(s, out, depth + 1);
        }
    }

    /// Where singleton methods of a namespace are looked up: (namespace,
    /// singleton table?) pairs in order.
    fn ruby_singleton_chain(&self, path: &str) -> Vec<(String, bool)> {
        let mut out: Vec<(String, bool)> = Vec::new();
        let mut cur = Some(path.to_string());
        let mut guard = 0;
        while let Some(c) = cur {
            out.push((c.clone(), true));
            for e in self.ruby.extends.get(&c).into_iter().flatten().rev() {
                for a in self.ruby_ancestors(e) {
                    out.push((a, false));
                }
            }
            cur = self.ruby.supers.get(&c).cloned();
            guard += 1;
            if guard > MAX_RUBY_DEPTH {
                break;
            }
        }
        out
    }

    fn ruby_pick(&self, nodes: &[NodeId], want: Want) -> Option<NodeId> {
        let is_field = |n: &NodeId| self.nodes[*n as usize].label == Label::Field;
        let n = match want {
            Want::Value => nodes.iter().find(|n| is_field(n)).or_else(|| nodes.first()).copied(),
            _ => nodes.iter().find(|n| !is_field(n)).or_else(|| nodes.first()).copied(),
        }?;
        Some(self.ruby_unalias(n, 0))
    }

    /// An alias (`alias n o`, `alias_method :n, :o`) stands for the method
    /// it names: a call of `n` is a call of `o`, found from the alias's
    /// class (an alias of a method the project does not define stays).
    fn ruby_unalias(&self, n: NodeId, depth: usize) -> NodeId {
        let node = &self.nodes[n as usize];
        let Some(old) = node.signature.as_deref().and_then(alias_target) else { return n };
        let Some((ns, name, singleton)) = split_member(&node.qualified_name) else { return n };
        if depth > 4 || old == name {
            return n;
        }
        let found = if singleton {
            self.ruby_singleton_method(ns, old, Want::Call)
        } else {
            self.ruby_instance_method(ns, old, Want::Call)
        };
        found.map_or(n, |m| if m.node == n { n } else { self.ruby_unalias(m.node, depth + 1) })
    }

    /// An instance method `name` of `path` or its ancestors.
    fn ruby_instance_method(&self, path: &str, name: &str, want: Want) -> Option<Found> {
        for (k, anc) in self.ruby_ancestors(path).iter().enumerate() {
            if let Some(nodes) = self.ruby.imembers.get(anc).and_then(|m| m.get(name))
                && let Some(n) = self.ruby_pick(nodes, want)
            {
                return Some(Found { node: n, inherited: k > 0 && anc != path });
            }
        }
        None
    }

    /// A singleton method `name` of `path` (its own, extended, inherited).
    fn ruby_singleton_method(&self, path: &str, name: &str, want: Want) -> Option<Found> {
        for (k, (ns, singleton)) in self.ruby_singleton_chain(path).iter().enumerate() {
            let table = if *singleton { &self.ruby.smembers } else { &self.ruby.imembers };
            if let Some(nodes) = table.get(ns).and_then(|m| m.get(name))
                && let Some(n) = self.ruby_pick(nodes, want)
            {
                return Some(Found { node: n, inherited: k > 0 });
            }
        }
        None
    }

    /// Whether `path` or one of its ancestors is open (module docs).
    fn ruby_is_open(&self, path: &str, singleton: bool) -> bool {
        let chain: Vec<String> = if singleton {
            self.ruby_singleton_chain(path).into_iter().map(|(p, _)| p).collect()
        } else {
            self.ruby_ancestors(path)
        };
        chain.iter().any(|p| self.ruby.open.contains(p))
    }

    /// The top-level `def name` seen from file `f`: the file's own, else
    /// the only one.
    fn ruby_function(&self, f: FileId, name: &str) -> Option<(NodeId, Strategy, f32)> {
        let nodes = self.ruby.functions.get(name)?;
        if let Some(&n) = nodes.iter().find(|&&n| self.nodes[n as usize].file == Some(f)) {
            return Some((n, Strategy::SameModule, 0.9));
        }
        let live: Vec<NodeId> = nodes.iter().copied().filter(|&n| !self.nodes[n as usize].is_test).collect();
        (live.len() == 1).then(|| (live[0], Strategy::ImportResolved, 0.85))
    }

    /// The constructor (`initialize`) a `new` of class `class` runs.
    pub(crate) fn ruby_constructor(&self, class: NodeId) -> Option<(NodeId, bool)> {
        let path = &self.nodes[class as usize].qualified_name;
        self.ruby_instance_method(path, "initialize", Want::Call).map(|m| (m.node, m.inherited))
    }

    // -----------------------------------------------------------------
    // References
    // -----------------------------------------------------------------

    /// What `self` is at `scope` of file `f`.
    fn ruby_self(&self, f: FileId, scope: Option<LocalDefId>) -> SelfCtx {
        let defs = self.defs(f);
        let mut cur = scope;
        while let Some(j) = cur {
            let d = &defs[j as usize];
            match d.label {
                Label::Method | Label::Field => {
                    if let Some((ns, _, singleton)) = split_member(&d.qualified_name) {
                        return if singleton {
                            SelfCtx::Singleton(ns.to_string())
                        } else {
                            SelfCtx::Instance(ns.to_string())
                        };
                    }
                }
                Label::Class | Label::Module => return SelfCtx::Singleton(d.qualified_name.clone()),
                Label::Function | Label::Test => return SelfCtx::Top,
                _ => {}
            }
            cur = self.parents[f as usize][j as usize];
        }
        SelfCtx::Top
    }

    /// Resolves one reference of a Ruby file.
    pub(crate) fn resolve_ruby(&self, f: FileId, item: &RefItem<'_>, scope: Option<LocalDefId>) -> Outcome {
        match item.kind {
            RefKind::Read | RefKind::Type | RefKind::Throw => self.ruby_constant_ref(f, item, scope),
            RefKind::Call | RefKind::New => self.ruby_call(f, item, scope, Want::Call, 0),
            RefKind::Write => self.ruby_call(f, item, scope, Want::Value, 0),
            _ => Outcome::Miss,
        }
    }

    /// `Foo`, `A::B` (receiver `A`), `::Foo` (receiver `::`).
    fn ruby_constant_ref(&self, f: FileId, item: &RefItem<'_>, scope: Option<LocalDefId>) -> Outcome {
        let written = match item.receiver {
            Some("::") => format!("::{}", item.name),
            Some(q) => format!("{q}::{}", item.name),
            None => item.name.to_string(),
        };
        let nesting = self.ruby_nesting(f, scope);
        match self.ruby_const(&written, &nesting).and_then(|p| self.ruby_const_node(&p)) {
            Some(n) => Outcome::Hit(n, Strategy::ImportResolved, CONST_CONFIDENCE),
            None => Outcome::External,
        }
    }

    /// A call (or an attribute write, `want` = value) by its receiver.
    fn ruby_call(&self, f: FileId, item: &RefItem<'_>, scope: Option<LocalDefId>, want: Want, depth: usize) -> Outcome {
        let name = item.name;
        match item.receiver.unwrap_or("") {
            "" | "self" => self.ruby_implicit(f, scope, name, want),
            "$super" => self.ruby_super(f, scope, name, want),
            "self.class" => match self.ruby_self(f, scope) {
                SelfCtx::Instance(p) | SelfCtx::Singleton(p) => self.ruby_on_singleton(&p, name, want, 0.9),
                SelfCtx::Top => Outcome::External,
            },
            "?" => self.ruby_by_name(f, name, want),
            r if r.starts_with(TYPE_RECEIVER_PREFIX) => {
                let ty = &r[TYPE_RECEIVER_PREFIX.len()..];
                let nesting = self.ruby_nesting(f, scope);
                match self.ruby_const(ty, &nesting) {
                    Some(p) => self.ruby_on_instance(f, &p, name, want),
                    None => Outcome::External,
                }
            }
            r if r.starts_with(RET_PREFIX) => match self.ruby_ret_type(f, scope, &r[RET_PREFIX.len()..], depth) {
                RetType::Type(p) => self.ruby_on_instance(f, &p, name, want),
                RetType::External => Outcome::External,
                RetType::Unknown => self.ruby_by_name(f, name, want),
            },
            r => self.ruby_on_constant(f, scope, r, name, want),
        }
    }

    /// A method of the class an instance belongs to.
    fn ruby_on_instance(&self, f: FileId, path: &str, name: &str, want: Want) -> Outcome {
        if !self.ruby.namespaces.contains_key(path) {
            return Outcome::External;
        }
        match self.ruby_instance_method(path, name, want) {
            Some(m) => Outcome::Hit(m.node, Strategy::TypeResolved, 0.85),
            None if self.ruby_is_open(path, false) => Outcome::External,
            None => self.ruby_by_name(f, name, want),
        }
    }

    /// A singleton method of a class (`Foo.bar`, `self.class.bar`).
    fn ruby_on_singleton(&self, path: &str, name: &str, want: Want, confidence: f32) -> Outcome {
        match self.ruby_singleton_method(path, name, want) {
            Some(m) => {
                let c = if m.inherited { confidence.min(0.85) } else { confidence };
                Outcome::Hit(m.node, Strategy::TypeResolved, c)
            }
            None if name == "new" => self
                .ruby_const_node(path)
                .map_or(Outcome::External, |n| Outcome::Hit(n, Strategy::TypeResolved, confidence)),
            None => Outcome::External,
        }
    }

    /// `Foo.bar` / `A::B.new`: the constant's singleton methods.
    fn ruby_on_constant(&self, f: FileId, scope: Option<LocalDefId>, recv: &str, name: &str, want: Want) -> Outcome {
        let starts_const = recv.starts_with("::") || recv.chars().next().is_some_and(|c| c.is_ascii_uppercase());
        if !starts_const {
            return self.ruby_by_name(f, name, want);
        }
        let nesting = self.ruby_nesting(f, scope);
        let Some(path) = self.ruby_const(recv, &nesting) else { return Outcome::External };
        if !self.ruby.namespaces.contains_key(&path) {
            // A constant holding an object.
            return Outcome::External;
        }
        match self.ruby_on_singleton(&path, name, want, 0.85) {
            Outcome::Hit(n, _, c) => Outcome::Hit(n, Strategy::ImportResolved, c.min(0.85)),
            other => other,
        }
    }

    /// A call with no receiver or `self`.
    fn ruby_implicit(&self, f: FileId, scope: Option<LocalDefId>, name: &str, want: Want) -> Outcome {
        let found = match self.ruby_self(f, scope) {
            SelfCtx::Instance(p) => self.ruby_instance_method(&p, name, want).ok_or((p, false)),
            SelfCtx::Singleton(p) => match self.ruby_singleton_method(&p, name, want) {
                Some(m) => Ok(m),
                // `new` inside the class: the class (and its `initialize`).
                None if name == "new" && self.ruby.namespaces.contains_key(&p) => {
                    return self
                        .ruby_const_node(&p)
                        .map_or(Outcome::External, |n| Outcome::Hit(n, Strategy::TypeResolved, 0.9));
                }
                None => Err((p, true)),
            },
            SelfCtx::Top => {
                return self.ruby_function(f, name).map_or(Outcome::Miss, |(n, s, c)| Outcome::Hit(n, s, c));
            }
        };
        match found {
            Ok(m) => Outcome::Hit(m.node, Strategy::TypeResolved, if m.inherited { 0.85 } else { 0.9 }),
            Err((p, singleton)) => {
                if let Some((n, s, c)) = self.ruby_function(f, name) {
                    return Outcome::Hit(n, s, c);
                }
                if self.ruby_is_open(&p, singleton) { Outcome::External } else { Outcome::Miss }
            }
        }
    }

    /// `super`: the method of the same name after the enclosing class.
    fn ruby_super(&self, f: FileId, scope: Option<LocalDefId>, name: &str, want: Want) -> Outcome {
        let hit = match self.ruby_self(f, scope) {
            SelfCtx::Instance(p) => {
                let ancestors = self.ruby_ancestors(&p);
                let after = ancestors.iter().position(|a| *a == p).map_or(1, |k| k + 1);
                ancestors[after.min(ancestors.len())..].iter().find_map(|a| {
                    self.ruby.imembers.get(a).and_then(|m| m.get(name)).and_then(|v| self.ruby_pick(v, want))
                })
            }
            SelfCtx::Singleton(p) => {
                self.ruby.supers.get(&p).and_then(|s| self.ruby_singleton_method(s, name, want)).map(|m| m.node)
            }
            SelfCtx::Top => None,
        };
        hit.map_or(Outcome::External, |n| Outcome::Hit(n, Strategy::TypeResolved, 0.85))
    }

    /// The by-name rung: the only non-test instance method of that name.
    fn ruby_by_name(&self, f: FileId, name: &str, want: Want) -> Outcome {
        if want == Want::Value
            || name.chars().count() < MIN_HEURISTIC_NAME
            || is_core_method(name)
            || self.packs[f as usize].is_some_and(|p| p.is_builtin(name))
        {
            return Outcome::External;
        }
        match self.ruby.methods_by_name.get(name).map(Vec::as_slice) {
            Some([only]) => Outcome::Hit(*only, Strategy::Heuristic, 0.5),
            _ => Outcome::Miss,
        }
    }

    /// The class a `$ret:<receiver>#<name>` receiver evaluates to.
    fn ruby_ret_type(&self, f: FileId, scope: Option<LocalDefId>, spec: &str, depth: usize) -> RetType {
        if depth > MAX_RUBY_DEPTH {
            return RetType::Unknown;
        }
        let Some((recv, name)) = spec.rsplit_once('#') else { return RetType::Unknown };
        let inner = RefItem {
            kind: RefKind::Call,
            name,
            receiver: (!recv.is_empty()).then_some(recv),
            enclosing: scope,
            span: Span::default(),
            decorated: None,
            arity: None,
            arg_types: None,
        };
        let target = match self.ruby_call(f, &inner, scope, Want::Call, depth + 1) {
            Outcome::Hit(n, s, _) if s != Strategy::Heuristic => n,
            Outcome::External => return RetType::External,
            _ => return RetType::Unknown,
        };
        self.ruby_return_of(target)
    }

    /// The declared return type of a method (`#: -> T` in its signature),
    /// resolved in its namespace; a class for `new`.
    fn ruby_return_of(&self, target: NodeId) -> RetType {
        let node = &self.nodes[target as usize];
        if matches!(node.label, Label::Class | Label::Module) {
            // `Foo.new` reached the class: an instance of it.
            return RetType::Type(node.qualified_name.clone());
        }
        let Some(ret) = node.signature.as_deref().and_then(|s| s.rsplit_once(" #: -> ")).map(|(_, r)| r.trim()) else {
            return RetType::Unknown;
        };
        let owner = split_member(&node.qualified_name).map_or("", |(ns, _, _)| ns);
        let nesting = if owner.is_empty() { Vec::new() } else { prefix_nesting(owner) };
        match self.ruby_const(ret, &nesting) {
            Some(p) if self.ruby.namespaces.contains_key(&p) => RetType::Type(p),
            _ => RetType::External,
        }
    }
}

/// The type of a call's result.
enum RetType {
    Type(String),
    /// A library type, or a call outside the project.
    External,
    Unknown,
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn zeitwerk_names() {
        assert_eq!(zeitwerk_path("Foo::BarBaz"), "foo/bar_baz");
        assert_eq!(zeitwerk_path("HTTPClient"), "httpclient");
        assert_eq!(zeitwerk_path("V2::Api"), "v2/api");
        assert_eq!(prefix_nesting("A::B::C"), vec!["A::B::C", "A::B", "A"]);
        assert_eq!(split_member("A::B#m"), Some(("A::B", "m", false)));
        assert_eq!(split_member("A::B.m"), Some(("A::B", "m", true)));
        assert_eq!(split_member("m"), None);
        assert_eq!(alias_target("alias to_html render"), Some("render"));
        assert_eq!(alias_target("alias_method :to_html, :render"), Some("render"));
        assert_eq!(alias_target("private alias_method :a, :b"), Some("b"));
        assert_eq!(alias_target("def alias(x)"), None);
    }
}
