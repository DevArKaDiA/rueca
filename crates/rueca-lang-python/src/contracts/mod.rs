//! Contract hints of a Python file (RFC 0006 phase 4; keys and matching in
//! `rueca_model::contract`). One pass over the syntax tree after the
//! definitions are known, so every hint can name its owner definition.
//!
//! - **HTTP providers** (`routes.rs`): Flask / Quart / FastAPI / aiohttp /
//!   Sanic / Litestar route decorators (`@app.route("/x", methods=[…])`,
//!   `@router.get("/x")`), composed with a `Blueprint(…, url_prefix=…)` or
//!   `APIRouter(prefix=…)` bound in the same file; Flask
//!   `add_url_rule`; Django `path()` / `re_path()` / `url()` (method `*`);
//!   aiohttp `router.add_get()` / `add_route()` / `web.get()`; Starlette
//!   `Route("/x", h, methods=[…])`. Owner: the handler function when the
//!   file defines it, else the enclosing definition, else the file.
//! - **HTTP consumers** (`clients.rs`): a call whose callee is an HTTP verb
//!   (`get`, `_post`, `get_api_list`, …) or `request` / `_request` /
//!   `urlopen`, with a path-like argument (`/…`, `http(s)://…`, an
//!   interpolated base followed by `/…`); the method from the callee name, a
//!   literal method argument or `method=` (`method=self._conn.get` counts),
//!   `*` when unknown. Owner: the enclosing definition.
//! - **gRPC** (`grpc.rs`): consumers `stub.Method(…)` / `self._stub.Method`
//!   where the holder name is bound in the file to `<mod>.XStub(…)` (an
//!   assignment, an annotation, a parameter or a function's return type);
//!   providers: the public PascalCase methods of a class deriving from
//!   `<mod>.XServicer`. Key `X/Method`.
//! - **Topics**: kafka-python / aiokafka producers' `send("topic")`
//!   (consumes) and consumers' topic arguments / `subscribe([…])`
//!   (provides), with the receiver bound to the client class in the file.

mod clients;
mod grpc;
mod render;
mod routes;

use std::collections::{HashMap, HashSet};

use rueca_extract::tree_sitter::Node;
use rueca_extract::{span_of, text_of};
use rueca_model::contract::truncate_raw;
use rueca_model::{ContractHint, ContractKind, ContractSide, Definition, Label, LocalDefId};

/// Contract hints of a parsed file; `defs` are the file's definitions.
pub(crate) fn extract(root: Node<'_>, src: &[u8], defs: &[Definition], max_depth: usize) -> Vec<ContractHint> {
    let mut cx = Ctx::new(root, src, defs, max_depth);
    let mut stack = vec![(root, 0usize)];
    while let Some((node, depth)) = stack.pop() {
        if depth > max_depth {
            continue;
        }
        if node.kind() == "decorated_definition" {
            routes::decorated(&mut cx, node);
            // Decorators declare routes; they never call one.
            if let Some(def) = node.child_by_field_name("definition") {
                stack.push((def, depth + 1));
            }
            continue;
        }
        cx.visit(node);
        let mut c = node.walk();
        stack.extend(node.named_children(&mut c).map(|ch| (ch, depth + 1)));
    }
    cx.hints.sort_by_key(|h| h.span.start_byte);
    cx.hints
}

/// What the file says about names, plus the hints found so far.
pub(super) struct Ctx<'t, 's> {
    src: &'s [u8],
    defs: &'s [Definition],
    max_depth: usize,
    /// Local name → dotted target it was imported as (`np` → `numpy`,
    /// `get` → `requests.get`).
    imports: HashMap<String, String>,
    /// Classes defined in the file (a `FakeStub` is not a generated stub).
    local_classes: HashSet<String>,
    /// Route prefixes of names bound to `Blueprint(…, url_prefix=…)` /
    /// `APIRouter(prefix=…)`.
    prefixes: HashMap<String, String>,
    /// Holder name (leading `_` dropped) → gRPC service of the stub it
    /// holds; `None` when the file binds it to several services.
    stubs: HashMap<String, Option<String>>,
    /// Name → kafka client class (`KafkaProducer`, `AIOKafkaConsumer`).
    kafka: HashMap<String, String>,
    /// Single-assignment tables of functions, by function node id.
    assigned: HashMap<usize, HashMap<String, Option<Node<'t>>>>,
    /// `(line, method)` of RPC calls already recorded.
    rpc_seen: HashSet<(u32, String)>,
    hints: Vec<ContractHint>,
}

impl<'t, 's> Ctx<'t, 's> {
    fn new(root: Node<'t>, src: &'s [u8], defs: &'s [Definition], max_depth: usize) -> Self {
        let mut cx = Ctx {
            src,
            defs,
            max_depth,
            imports: HashMap::new(),
            local_classes: HashSet::new(),
            prefixes: HashMap::new(),
            stubs: HashMap::new(),
            kafka: HashMap::new(),
            assigned: HashMap::new(),
            rpc_seen: HashSet::new(),
            hints: Vec::new(),
        };
        cx.scan(root);
        cx
    }

    /// Imports, local classes and the bindings later passes look up.
    fn scan(&mut self, root: Node<'t>) {
        let mut stack = vec![(root, 0usize)];
        let mut bindings = Vec::new();
        while let Some((n, depth)) = stack.pop() {
            match n.kind() {
                "import_statement" => self.scan_import(n),
                "import_from_statement" => self.scan_import_from(n),
                "class_definition" => {
                    if let Some(name) = n.child_by_field_name("name") {
                        self.local_classes.insert(self.text(name));
                    }
                }
                "assignment" | "function_definition" | "typed_parameter" | "typed_default_parameter" => {
                    bindings.push(n)
                }
                _ => {}
            }
            if depth < self.max_depth {
                let mut c = n.walk();
                stack.extend(n.named_children(&mut c).map(|ch| (ch, depth + 1)));
            }
        }
        // Bindings need every import first.
        for n in bindings {
            self.scan_binding(n);
        }
    }

    /// `import a.b` (binds `a`), `import a.b as x`.
    fn scan_import(&mut self, n: Node<'t>) {
        let mut c = n.walk();
        for name in n.children_by_field_name("name", &mut c) {
            match name.kind() {
                "dotted_name" => {
                    let target = self.text(name);
                    let local = target.split('.').next().unwrap_or_default().to_string();
                    self.imports.insert(local.clone(), local);
                }
                "aliased_import" => {
                    if let (Some(m), Some(a)) = (name.child_by_field_name("name"), name.child_by_field_name("alias")) {
                        self.imports.insert(self.text(a), self.text(m));
                    }
                }
                _ => {}
            }
        }
    }

    /// `from m import n as k` binds `k` to `m.n` (relative modules keep
    /// their dots).
    fn scan_import_from(&mut self, n: Node<'t>) {
        let Some(module) = n.child_by_field_name("module_name").map(|m| self.text(m)) else { return };
        let mut c = n.walk();
        for name in n.children_by_field_name("name", &mut c) {
            let (orig, local) = match name.kind() {
                "dotted_name" => (self.text(name), self.text(name)),
                "aliased_import" => match (name.child_by_field_name("name"), name.child_by_field_name("alias")) {
                    (Some(m), Some(a)) => (self.text(m), self.text(a)),
                    _ => continue,
                },
                _ => continue,
            };
            let sep = if module.ends_with('.') { "" } else { "." };
            self.imports.insert(local, format!("{module}{sep}{orig}"));
        }
    }

    /// Route prefixes, stub holders and kafka clients bound by `n`.
    fn scan_binding(&mut self, n: Node<'t>) {
        match n.kind() {
            "assignment" => {
                routes::scan_prefix(self, n);
                grpc::scan_assignment(self, n);
                self.scan_kafka(n);
            }
            "function_definition" => grpc::scan_return(self, n),
            _ => grpc::scan_parameter(self, n),
        }
    }

    /// `p = KafkaProducer(…)` / `self.c = AIOKafkaConsumer(…)`.
    fn scan_kafka(&mut self, n: Node<'t>) {
        let (Some(left), Some(right)) = (n.child_by_field_name("left"), n.child_by_field_name("right")) else {
            return;
        };
        let Some(class) = self.called_class(right) else { return };
        let imported = self.imports.get(&class).is_some_and(|t| t.starts_with("kafka") || t.starts_with("aiokafka"));
        if imported
            && class.contains("Kafka")
            && let Some(name) = last_name(left, self.src)
        {
            self.kafka.insert(name, class);
        }
    }

    /// The class name called by `X(…)` / `mod.X(…)`, if `n` is such a call.
    fn called_class(&self, n: Node<'_>) -> Option<String> {
        if n.kind() != "call" {
            return None;
        }
        let func = n.child_by_field_name("function")?;
        match func.kind() {
            "identifier" => Some(self.text(func)),
            "attribute" => Some(self.text(func.child_by_field_name("attribute")?)),
            _ => None,
        }
    }

    fn visit(&mut self, node: Node<'t>) {
        match node.kind() {
            "call" => {
                if !routes::registration(self, node) && !self.topic_call(node) {
                    clients::http_call(self, node);
                }
            }
            "attribute" => grpc::stub_call(self, node),
            "class_definition" => grpc::servicer(self, node),
            _ => {}
        }
    }

    // ---------------------------------------------------------------------
    // Topics
    // ---------------------------------------------------------------------

    /// kafka `producer.send("t", …)` (consumes), `KafkaConsumer("t", …)` and
    /// `consumer.subscribe(["t"])` (provides). Whether it matched.
    fn topic_call(&mut self, call: Node<'t>) -> bool {
        if self.kafka.is_empty() && !self.imports_root("kafka") && !self.imports_root("aiokafka") {
            return false;
        }
        self.kafka_consumer(call) || self.kafka_method(call)
    }

    /// `KafkaConsumer("a", "b", …)`: the topics it subscribes to.
    fn kafka_consumer(&mut self, call: Node<'t>) -> bool {
        let Some(class) = self.called_class(call) else { return false };
        let kafka = self.imports.get(&class).is_some_and(|t| t.starts_with("kafka") || t.starts_with("aiokafka"));
        if !kafka || !class.ends_with("Consumer") {
            return false;
        }
        let topics: Vec<String> = positional(call).iter().filter_map(|a| render::plain_string(*a, self.src)).collect();
        self.topics(call, ContractSide::Provides, topics)
    }

    /// `producer.send("t", …)` / `send_and_wait`, `consumer.subscribe([…])`
    /// on a name bound to a kafka client.
    fn kafka_method(&mut self, call: Node<'t>) -> bool {
        let Some(func) = call.child_by_field_name("function").filter(|f| f.kind() == "attribute") else {
            return false;
        };
        let (Some(obj), Some(attr)) = (func.child_by_field_name("object"), func.child_by_field_name("attribute"))
        else {
            return false;
        };
        let Some(class) = last_name(obj, self.src).and_then(|h| self.kafka.get(&h).cloned()) else { return false };
        let first = positional(call).first().copied();
        match (self.text(attr).as_str(), class.ends_with("Producer")) {
            ("send" | "send_and_wait", true) => {
                let topic = first.and_then(|a| render::plain_string(a, self.src));
                self.topics(call, ContractSide::Consumes, topic.into_iter().collect())
            }
            ("subscribe", false) => {
                let topics = first.map(|a| render::string_list(a, self.src)).unwrap_or_default();
                self.topics(call, ContractSide::Provides, topics)
            }
            _ => false,
        }
    }

    fn topics(&mut self, call: Node<'t>, side: ContractSide, topics: Vec<String>) -> bool {
        let found = !topics.is_empty();
        let owner = self.enclosing(call);
        for t in topics {
            self.push(side, ContractKind::Topic, t, call, owner, "kafka");
        }
        found
    }

    // ---------------------------------------------------------------------
    // Shared helpers
    // ---------------------------------------------------------------------

    fn text(&self, n: Node<'_>) -> String {
        text_of(n, self.src).into_owned()
    }

    /// Records a hint whose evidence is `node` (its text, one line).
    fn push(
        &mut self,
        side: ContractSide,
        kind: ContractKind,
        key: String,
        node: Node<'_>,
        owner: Option<LocalDefId>,
        framework: &str,
    ) {
        let raw = text_of(node, self.src).split_whitespace().collect::<Vec<_>>().join(" ");
        self.push_raw(side, kind, key, node, owner, framework, raw);
    }

    /// [`Ctx::push`] with the evidence text given.
    #[allow(clippy::too_many_arguments)]
    fn push_raw(
        &mut self,
        side: ContractSide,
        kind: ContractKind,
        key: String,
        node: Node<'_>,
        owner: Option<LocalDefId>,
        framework: &str,
        raw: String,
    ) {
        self.hints.push(ContractHint {
            side,
            kind,
            key,
            raw: truncate_raw(&raw),
            span: span_of(node),
            owner,
            framework: framework.to_string(),
        });
    }

    /// The definition whose name is the identifier `name`.
    fn def_named_at(&self, name: Node<'_>) -> Option<LocalDefId> {
        let (s, e) = (name.start_byte() as u32, name.end_byte() as u32);
        self.defs.iter().position(|d| d.name_span.start_byte == s && d.name_span.end_byte == e).map(|i| i as LocalDefId)
    }

    /// The innermost function, method or class around `node`.
    fn enclosing(&self, node: Node<'_>) -> Option<LocalDefId> {
        let span = span_of(node);
        self.defs
            .iter()
            .enumerate()
            .filter(|(_, d)| is_scope_label(d.label) && d.span.contains(&span))
            .max_by_key(|(_, d)| d.span.start_byte)
            .map(|(i, _)| i as LocalDefId)
    }

    /// The handler named by `handler` (a bare name of a function the file
    /// defines), else the enclosing definition of `site`.
    fn handler_owner(&self, handler: Option<Node<'_>>, site: Node<'_>) -> Option<LocalDefId> {
        let named = handler.filter(|h| h.kind() == "identifier").and_then(|h| {
            let name = self.text(h);
            self.defs.iter().position(|d| {
                d.parent.is_none() && matches!(d.label, Label::Function | Label::Class) && d.qualified_name == name
            })
        });
        named.map(|i| i as LocalDefId).or_else(|| self.enclosing(site))
    }

    /// Root module the file imported `local` from (`requests` for
    /// `import requests` or `from requests import get`).
    fn import_root(&self, local: &str) -> Option<&str> {
        self.imports.get(local).map(|t| t.trim_start_matches('.').split('.').next().unwrap_or_default())
    }

    /// Whether the file imports a module whose root is `root`.
    fn imports_root(&self, root: &str) -> bool {
        self.imports.values().any(|t| t.split('.').next() == Some(root))
    }
}

fn is_scope_label(label: Label) -> bool {
    matches!(label, Label::Function | Label::Method | Label::Class | Label::Test)
}

/// Positional arguments of a call (keywords and splats excluded).
fn positional<'t>(call: Node<'t>) -> Vec<Node<'t>> {
    let Some(args) = call.child_by_field_name("arguments") else { return Vec::new() };
    let mut c = args.walk();
    args.named_children(&mut c)
        .filter(|a| !matches!(a.kind(), "keyword_argument" | "list_splat" | "dictionary_splat" | "comment"))
        .collect()
}

/// The value of keyword argument `name` of a call.
fn keyword<'t>(call: Node<'t>, name: &str, src: &[u8]) -> Option<Node<'t>> {
    let args = call.child_by_field_name("arguments")?;
    let mut c = args.walk();
    let kw = args
        .named_children(&mut c)
        .filter(|a| a.kind() == "keyword_argument")
        .find(|a| a.child_by_field_name("name").is_some_and(|n| text_of(n, src) == name))?;
    kw.child_by_field_name("value")
}

/// Last identifier of an identifier / attribute chain, leading `_` kept.
fn last_name(n: Node<'_>, src: &[u8]) -> Option<String> {
    match n.kind() {
        "identifier" => Some(text_of(n, src).into_owned()),
        "attribute" => Some(text_of(n.child_by_field_name("attribute")?, src).into_owned()),
        _ => None,
    }
}

/// Root identifier of an identifier / attribute chain.
fn root_name(n: Node<'_>, src: &[u8]) -> Option<String> {
    let mut cur = n;
    while cur.kind() == "attribute" {
        cur = cur.child_by_field_name("object")?;
    }
    (cur.kind() == "identifier").then(|| text_of(cur, src).into_owned())
}

#[cfg(test)]
mod tests;
