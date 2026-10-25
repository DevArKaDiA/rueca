use std::collections::{BTreeMap, BTreeSet};

use rueca_extract::{Budget, ExtractError, LanguagePack, ProjectFiles};
use rueca_model::{Definition, FileFacts, Import, ImportedName, Label, RefKind, Reference};

use crate::TypeScriptPack;
use crate::locals::{self, Locals};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn lang_of(path: &str) -> rueca_model::Language {
    rueca_model::Language::from_path(path).expect("ecmascript path")
}

fn facts(path: &str, src: &str) -> FileFacts {
    TypeScriptPack.extract(path, lang_of(path), src.as_bytes(), &Budget::default()).expect("extract")
}

fn def<'f>(f: &'f FileFacts, qn: &str) -> &'f Definition {
    f.definitions.iter().find(|d| d.qualified_name == qn).unwrap_or_else(|| {
        let all: Vec<_> = f.definitions.iter().map(|d| &d.qualified_name).collect();
        panic!("no definition {qn}; have {all:?}")
    })
}

fn has_def(f: &FileFacts, qn: &str) -> bool {
    f.definitions.iter().any(|d| d.qualified_name == qn)
}

fn id(f: &FileFacts, qn: &str) -> u32 {
    f.definitions.iter().position(|d| d.qualified_name == qn).unwrap_or_else(|| panic!("no definition {qn}")) as u32
}

fn refs<'f>(f: &'f FileFacts, kind: RefKind, name: &str) -> Vec<&'f Reference> {
    f.references.iter().filter(|r| r.kind == kind && r.name == name).collect()
}

fn has_ref(f: &FileFacts, kind: RefKind, name: &str, receiver: Option<&str>) -> bool {
    refs(f, kind, name).iter().any(|r| r.receiver.as_deref() == receiver)
}

fn names(i: &Import) -> Vec<(String, Option<String>)> {
    i.names.iter().map(|n| (n.name.clone(), n.alias.clone())).collect()
}

fn n(name: &str, alias: Option<&str>) -> (String, Option<String>) {
    (name.to_string(), alias.map(str::to_string))
}

fn project(paths: &[&str], configs: &[(&str, &str)]) -> ProjectFiles {
    let mut p: BTreeSet<String> = paths.iter().map(|s| s.to_string()).collect();
    let mut c = BTreeMap::new();
    for (k, v) in configs {
        p.insert(k.to_string());
        c.insert(k.to_string(), v.to_string());
    }
    ProjectFiles { paths: p, configs: c }
}

fn resolve(project: &ProjectFiles, from: &str, module: &str) -> Option<String> {
    let imp = Import {
        module: module.into(),
        relative_level: 0,
        names: vec![],
        module_alias: None,
        type_only: false,
        reexport: false,
        span: Default::default(),
    };
    TypeScriptPack.resolve_import(from, &imp, project)
}

const SERVICE: &str = include_str!("../tests/fixtures/users.service.ts");
const CONTROLLER: &str = include_str!("../tests/fixtures/users.controller.ts");
const BUTTON: &str = include_str!("../tests/fixtures/Button.tsx");
const LEGACY: &str = include_str!("../tests/fixtures/legacy.js");
const SPEC: &str = include_str!("../tests/fixtures/user.spec.ts");
const BARREL: &str = include_str!("../tests/fixtures/index.ts");

// ---------------------------------------------------------------------------
// Vendored queries
// ---------------------------------------------------------------------------

#[test]
fn locals_queries_compile_against_pinned_grammars() {
    use rueca_model::Language::*;
    for lang in [JavaScript, TypeScript, Tsx] {
        let q = locals::locals_query(lang).unwrap_or_else(|e| panic!("{lang}: {e}"));
        let _ = q;
        let grammar = locals::grammar(lang).unwrap();
        let query = tree_sitter::Query::new(&grammar, &locals::query_source(lang)).unwrap();
        let caps: BTreeSet<&str> = query.capture_names().iter().copied().collect();
        assert!(caps.contains("local.scope"), "{lang}: {caps:?}");
        assert!(caps.contains("local.definition.var"), "{lang}: {caps:?}");
        assert!(!caps.iter().any(|c| c.starts_with("local.reference")), "{lang}: references must be stripped");
        assert_eq!(caps.contains("rueca.namespace_body"), lang != JavaScript, "{lang}");
        let parents = (0..query.pattern_count())
            .filter(|i| query.property_settings(*i).iter().any(|p| &*p.key == "definition.var.scope"))
            .count();
        assert_eq!(parents, 2, "{lang}: function / generator declaration names bind in the parent scope");
    }
    assert!(matches!(locals::locals_query(Python), Err(ExtractError::Unsupported(_))));
}

#[test]
fn vendored_queries_carry_provenance() {
    for (name, text) in [
        ("ecma", locals::ECMA_LOCALS),
        ("javascript", locals::JAVASCRIPT_LOCALS),
        ("typescript", locals::TYPESCRIPT_LOCALS),
        ("tsx", locals::TSX_LOCALS),
    ] {
        assert!(text.starts_with("; SPDX-License-Identifier: "), "{name}");
        assert!(text.contains("29212d826054bb2948fa9d3a99eca9bf6a050eb2"), "{name}: pinned nvim-treesitter commit");
        let patterns = text.lines().filter(|l| !l.trim_start().starts_with(';'));
        assert!(!patterns.into_iter().any(|l| l.contains("@local.reference")), "{name}");
    }
    assert!(locals::ECMA_LOCALS.contains("44c892e0be055ac465d5eeddae6d3e194424e7de"));
}

fn locals_of(path: &str, src: &str) -> (Locals, tree_sitter::Tree) {
    let lang = lang_of(path);
    let tree = rueca_extract::parse(&locals::grammar(lang).unwrap(), src.as_bytes(), &Budget::default()).unwrap();
    let q = locals::locals_query(lang).unwrap();
    let l = Locals::compute(
        q,
        tree.root_node(),
        src.as_bytes(),
        std::time::Instant::now() + std::time::Duration::from_secs(5),
    );
    (l, tree)
}

fn first_of_kind<'t>(n: tree_sitter::Node<'t>, kind: &str) -> Option<tree_sitter::Node<'t>> {
    if n.kind() == kind {
        return Some(n);
    }
    let mut c = n.walk();
    let children: Vec<_> = n.named_children(&mut c).collect();
    children.into_iter().find_map(|ch| first_of_kind(ch, kind))
}

#[test]
fn locals_are_attributed_to_owning_functions() {
    let src = r#"
const top = 1;
function outer(a, { b, c: [d, ...e] }, f = 2) {
  const { g: { h } } = a;
  let i;
  for (const j of xs) {}
  for (k in ys) {}
  try {} catch (err) {}
  function inner(z) {}
  class Local {}
  const fe = function named(q) {};
}
if (top) { const blockLocal = 1; }
for (const loopVar of top) {}
"#;
    let (l, tree) = locals_of("a.js", src);
    let root = tree.root_node();
    let outer = first_of_kind(root, "function_declaration").unwrap();
    let got = l.names(outer.id());
    for want in ["a", "b", "d", "e", "f", "h", "i", "j", "k", "err", "inner", "Local", "fe"] {
        assert!(got.contains(&want.to_string()), "missing {want} in {got:?}");
    }
    for not in ["z", "q", "named", "outer", "top", "g", "c"] {
        assert!(!got.contains(&not.to_string()), "unexpected {not} in {got:?}");
    }
    let module = l.names(root.id());
    assert_eq!(module, vec!["blockLocal".to_string(), "loopVar".to_string()]);
}

#[test]
fn typescript_locals_respect_namespaces_and_signatures() {
    let src = r#"
namespace NS {
  export const member = 1;
  function nsFn(p: string) { const q = p; }
}
function f(x: number, cb: (y: string) => void) { const z = x; }
declare function sig(s: string): void;
"#;
    let (l, tree) = locals_of("a.ts", src);
    let root = tree.root_node();
    assert!(l.names(root.id()).is_empty(), "namespace members are not locals: {:?}", l.names(root.id()));
    let fns: Vec<_> = {
        let mut out = Vec::new();
        let mut stack = vec![root];
        while let Some(n) = stack.pop() {
            if n.kind() == "function_declaration" {
                out.push(n);
            }
            let mut c = n.walk();
            stack.extend(n.named_children(&mut c));
        }
        out.sort_by_key(|n| n.start_byte());
        out
    };
    assert_eq!(l.names(fns[0].id()), vec!["p", "q"]);
    assert_eq!(l.names(fns[1].id()), vec!["cb", "x", "z"], "`y` belongs to the function type");
}

// ---------------------------------------------------------------------------
// Definitions
// ---------------------------------------------------------------------------

#[test]
fn nest_service_definitions() {
    let f = facts("src/users/users.service.ts", SERVICE);
    assert_eq!(f.parse_errors, 0);
    assert_eq!(f.line_count, SERVICE.lines().count() as u32);

    assert_eq!(def(&f, "DEFAULT_PAGE_SIZE").label, Label::Constant);
    let page = def(&f, "Page");
    assert_eq!(page.label, Label::Interface);
    assert_eq!(page.doc.as_deref(), Some("Paginated result."));
    assert_eq!(def(&f, "Page.items").label, Label::Field);
    assert_eq!(def(&f, "Page.next").label, Label::Method);
    assert_eq!(def(&f, "UserId").label, Label::TypeAlias);
    assert_eq!(def(&f, "Role").label, Label::Enum);
    assert_eq!(def(&f, "Role.Admin").label, Label::Constant);
    assert_eq!(def(&f, "BaseService").label, Label::Class);
    assert_eq!(def(&f, "BaseService.findById").label, Label::Method);

    let svc = def(&f, "UserService");
    assert_eq!(svc.label, Label::Class);
    assert!(svc.exported);
    assert_eq!(svc.decorators, vec!["Injectable()"]);
    assert_eq!(svc.bases, vec!["BaseService"]);
    assert_eq!(svc.implements, vec!["Searchable", "Auditable"]);
    assert_eq!(svc.doc.as_deref(), Some("Users domain service.\nHandles persistence."));
    assert_eq!(
        svc.signature.as_deref(),
        Some("class UserService extends BaseService<User> implements Searchable<User>, Auditable")
    );

    let find = def(&f, "UserService.findById");
    assert_eq!(find.label, Label::Method);
    assert_eq!(find.parent, Some(id(&f, "UserService")));
    assert!(find.exported);
    assert_eq!(find.doc.as_deref(), Some("Finds a user or throws."));
    assert_eq!(find.signature.as_deref(), Some("async findById(id: UserId): Promise<User | null>"));

    let ctor = def(&f, "UserService.constructor");
    assert_eq!(
        ctor.signature.as_deref(),
        Some("constructor(private readonly repo: Repository<User>, private readonly audit: AuditLog)")
    );
    let repo = def(&f, "UserService.repo");
    assert_eq!(repo.label, Label::Field);
    assert_eq!(repo.decorators, vec!["InjectRepository(User)"]);
    assert_eq!(def(&f, "UserService.cache").label, Label::Field);
    // get/set pair → one Method
    assert_eq!(f.definitions.iter().filter(|d| d.qualified_name == "UserService.size").count(), 1);
    assert!(!has_def(&f, "cached") && !has_def(&f, "UserService.findById.user"));
}

#[test]
fn nest_service_references() {
    let f = facts("src/users/users.service.ts", SERVICE);
    let find = id(&f, "UserService.findById");
    let create = id(&f, "UserService.create");
    assert!(has_ref(&f, RefKind::Decorator, "Injectable", None));
    assert!(has_ref(&f, RefKind::Decorator, "InjectRepository", None));
    assert!(
        refs(&f, RefKind::Call, "findOne")
            .iter()
            .any(|r| r.receiver.as_deref() == Some("$type:Repository") && r.enclosing == Some(find))
    );
    // `private readonly audit: AuditLog` is a typed field (optional call too).
    assert!(has_ref(&f, RefKind::Call, "record", Some("$type:AuditLog")), "optional call");
    assert!(has_ref(&f, RefKind::Throw, "NotFoundException", None));
    assert!(has_ref(&f, RefKind::New, "NotFoundException", None));
    assert!(has_ref(&f, RefKind::Write, "lastCreated", Some("this")));
    assert!(has_ref(&f, RefKind::Call, "randomUUID", Some("crypto")));
    assert!(refs(&f, RefKind::Read, "DEFAULT_PAGE_SIZE").iter().any(|r| r.enclosing == Some(create)));
    for t in ["User", "UserId", "Repository", "AuditLog", "CreateUserDto", "Searchable", "Auditable", "BaseService"] {
        assert!(!refs(&f, RefKind::Type, t).is_empty(), "type ref {t}");
    }
    // Builtin types, type parameters and the declared name itself are not references.
    for t in ["Promise", "Partial", "Map", "T", "string", "number"] {
        assert!(refs(&f, RefKind::Type, t).is_empty(), "unexpected type ref {t}");
    }
    // `Page` only where it is used (`Promise<Page<T>>`), not at its declaration.
    assert!(refs(&f, RefKind::Type, "Page").iter().all(|r| r.span.start_line == 13));
    // Locals (params, destructuring, for-of, catch) are not reads.
    for local in ["id", "email", "displayName", "cached", "user", "err", "hook", "dto", "v"] {
        assert!(refs(&f, RefKind::Read, local).is_empty(), "unexpected read of local {local}");
    }
    // Builtin globals are not calls.
    assert!(refs(&f, RefKind::New, "Map").is_empty());
    assert!(f.references.iter().all(|r| r.receiver.as_deref().is_none_or(|x| x.chars().count() <= 201)));
}

#[test]
fn nest_service_imports() {
    let f = facts("src/users/users.service.ts", SERVICE);
    let by_module = |m: &str| f.imports.iter().filter(|i| i.module == m).collect::<Vec<_>>();
    let common = by_module("@nestjs/common");
    assert_eq!(names(common[0]), vec![n("Injectable", None), n("NotFoundException", None)]);
    let typeorm = by_module("typeorm");
    assert!(typeorm[0].type_only);
    let dto = by_module("./dto");
    assert_eq!(dto.len(), 2, "mixed value/type import is split");
    assert!(!dto[0].type_only && dto[1].type_only);
    assert_eq!(names(dto[1]), vec![n("UpdateUserDto", None)]);
    let crypto = by_module("crypto");
    assert_eq!(crypto[0].module_alias.as_deref(), Some("crypto"));
    assert!(f.imports.iter().all(|i| i.relative_level == 0 && !i.reexport));
}

#[test]
fn nest_controller_decorators() {
    let f = facts("src/users/users.controller.ts", CONTROLLER);
    let c = def(&f, "UsersController");
    assert_eq!(c.decorators, vec!["Controller('users')"]);
    let find = def(&f, "UsersController.findOne");
    assert_eq!(find.decorators, vec!["Get(':id')"]);
    assert_eq!(find.signature.as_deref(), Some("findOne(id: string)"));
    assert_eq!(def(&f, "UsersController.users").label, Label::Field);
    let find_id = id(&f, "UsersController.findOne");
    assert!(refs(&f, RefKind::Decorator, "Param").iter().any(|r| r.enclosing == Some(find_id)));
    assert!(
        refs(&f, RefKind::Call, "findById")
            .iter()
            .any(|r| r.receiver.as_deref() == Some("$type:UserService") && r.enclosing == Some(find_id))
    );
    assert!(has_ref(&f, RefKind::Type, "UserService", None));
}

#[test]
fn react_component() {
    let f = facts("src/Button.tsx", BUTTON);
    assert_eq!(f.parse_errors, 0);
    let b = def(&f, "Button");
    assert_eq!(b.label, Label::Function);
    assert!(b.exported);
    assert_eq!(b.doc.as_deref(), Some("A button."));
    assert_eq!(def(&f, "IconButton").label, Label::Function);
    assert_eq!(def(&f, "ButtonProps.onClick").label, Label::Field);
    let bid = id(&f, "Button");
    assert!(refs(&f, RefKind::Call, "Icon").iter().any(|r| r.enclosing == Some(bid)), "JSX tag is a call");
    assert!(has_ref(&f, RefKind::Call, "Content", Some("Tooltip")));
    assert!(refs(&f, RefKind::Call, "span").is_empty() && refs(&f, RefKind::Call, "button").is_empty());
    assert!(has_ref(&f, RefKind::Call, "useState", None));
    assert!(has_ref(&f, RefKind::Read, "root", Some("styles")));
    for local in ["label", "count", "handle", "onClick", "props"] {
        assert!(refs(&f, RefKind::Read, local).is_empty(), "unexpected read of local {local}");
    }
    assert!(refs(&f, RefKind::Call, "Button").iter().any(|r| r.enclosing == Some(id(&f, "IconButton"))));
    let react = f.imports.iter().find(|i| i.module == "react").unwrap();
    assert_eq!(names(react)[0], n("default", Some("React")));
}

#[test]
fn commonjs_module() {
    let f = facts("lib/router.js", LEGACY);
    assert_eq!(f.parse_errors, 0);
    let router = def(&f, "Router");
    assert!(router.exported, "module.exports = Router");
    let route = def(&f, "Router.route");
    assert_eq!(route.label, Label::Method);
    assert!(route.exported, "members inherit the late export");
    assert_eq!(route.doc.as_deref(), Some("Adds a route."));
    assert_eq!(def(&f, "Router.create").label, Label::Method);
    let helper = def(&f, "helper");
    assert!(helper.exported);
    let imports: Vec<_> = f.imports.iter().map(|i| (i.module.as_str(), names(i), i.module_alias.clone())).collect();
    assert!(imports.contains(&("path", vec![], Some("path".into()))));
    assert!(imports.contains(&("fs/promises", vec![n("readFile", None), n("writeFile", Some("write"))], None)));
    assert!(imports.contains(&("./config", vec![n("settings", Some("config"))], None)));
    assert!(imports.contains(&("./polyfills", vec![], None)));
    assert!(imports.contains(&("./lazy.js", vec![], Some("mod".into()))), "dynamic import");
    assert!(imports.contains(&("debug", vec![], None)));
    assert!(has_ref(&f, RefKind::Write, "stack", Some("this")));
    assert!(has_ref(&f, RefKind::New, "Layer", None));
    assert!(has_ref(&f, RefKind::Call, "join", Some("path")));
    assert!(!has_def(&f, "path") && !has_def(&f, "readFile"), "require bindings are imports, not variables");
}

#[test]
fn test_blocks() {
    let f = facts("src/user.spec.ts", SPEC);
    let outer = def(&f, "UserService");
    assert_eq!(outer.label, Label::Test);
    assert!(outer.is_test);
    let leaf = def(&f, "UserService > findById > returns the cached user");
    assert!(leaf.is_test);
    assert_eq!(leaf.parent, Some(id(&f, "UserService > findById")));
    assert!(has_def(&f, "UserService > findById > handles %s"), "it.each");
    let dotted = def(&f, "UserService > v1_2 works");
    assert_eq!(dotted.name, "v1.2 works");
    let leaf_id = id(&f, "UserService > findById > returns the cached user");
    assert!(refs(&f, RefKind::New, "UserService").iter().any(|r| r.enclosing == Some(leaf_id)));
    assert!(
        refs(&f, RefKind::Call, "findById")
            .iter()
            .any(|r| r.enclosing == Some(leaf_id) && r.receiver.as_deref() == Some("$type:UserService"))
    );
    assert!(refs(&f, RefKind::Call, "expect").is_empty());
    assert!(refs(&f, RefKind::Read, "repo").is_empty(), "describe-callback locals");
}

#[test]
fn barrel_reexports() {
    let f = facts("src/index.ts", BARREL);
    assert!(f.definitions.is_empty());
    let all: Vec<_> =
        f.imports.iter().map(|i| (i.module.as_str(), names(i), i.module_alias.clone(), i.type_only)).collect();
    assert!(f.imports.iter().all(|i| i.reexport));
    assert_eq!(all[0], ("./users.service", vec![n("*", None)], None, false));
    assert_eq!(all[1], ("./users.controller", vec![n("UsersController", Some("Controller"))], None, false));
    assert_eq!(all[2], ("./dto", vec![], Some("dto".into()), false));
    assert_eq!(all[3], ("./users.service", vec![n("Page", None)], None, true));
    assert_eq!(all[4], ("./Button", vec![n("default", Some("Button"))], None, false));
    assert_eq!(all[5], ("./Button", vec![n("ButtonProps", None)], None, true));
}

#[test]
fn exports_and_default() {
    let f = facts(
        "a.ts",
        r#"
const a = 1, b = 2;
function hidden() {}
export { a, b as c };
export default function main() {}
"#,
    );
    assert!(def(&f, "a").exported && def(&f, "b").exported);
    assert!(!def(&f, "hidden").exported);
    assert!(def(&f, "main").exported);
    let anon = facts("b.ts", "export default class { run() {} }\n");
    assert_eq!(def(&anon, "default").label, Label::Class);
    assert!(def(&anon, "default.run").exported);
    let anon_fn = facts("c.js", "export default () => 1;\n");
    assert_eq!(def(&anon_fn, "default").label, Label::Function);
}

#[test]
fn namespaces_overloads_and_object_methods() {
    let f = facts(
        "a.ts",
        r#"
export namespace Geometry {
  export function area(r: number): number { return r * PI; }
  const PI = 3.14;
}
declare module "virtual" {
  function v(): void;
}
export function over(a: string): string;
export function over(a: number): number;
export function over(a: any) { return a; }
export const api = {
  get(id: string) { return fetchIt(id); },
  post: async (body: Body) => send(body),
  version: 2,
};
let counter = 0;
function* gen() { yield 1; }
"#,
    );
    assert_eq!(def(&f, "Geometry").label, Label::Module);
    assert_eq!(def(&f, "Geometry.area").label, Label::Function);
    assert_eq!(def(&f, "Geometry.PI").label, Label::Constant);
    assert!(refs(&f, RefKind::Read, "PI").len() == 1, "namespace members are not locals");
    assert!(def(&f, "virtual.v").exported, "ambient module members are exported");
    assert_eq!(f.definitions.iter().filter(|d| d.qualified_name == "over").count(), 1);
    assert_eq!(def(&f, "api.get").label, Label::Method);
    assert_eq!(def(&f, "api.post").label, Label::Method);
    assert!(def(&f, "api.get").exported);
    assert!(has_ref(&f, RefKind::Call, "fetchIt", None));
    assert_eq!(def(&f, "counter").label, Label::Variable);
    assert_eq!(def(&f, "gen").label, Label::Function);
}

#[test]
fn type_references_and_type_parameters() {
    let f = facts(
        "a.ts",
        r#"
export function pick<T extends Base>(xs: T[], key: keyof T): Wrapper<T> {
  const v = load() as Model;
  const w = cfg satisfies Config;
  return wrap<Item>(v);
}
type Mapped<K> = { [P in keyof K]: Other<P> };
interface Ext extends Parent<string>, ns.Other {}
"#,
    );
    for t in ["Base", "Wrapper", "Model", "Config", "Item", "Other", "Parent"] {
        assert!(!refs(&f, RefKind::Type, t).is_empty(), "type ref {t}");
    }
    for t in ["T", "K", "P", "string"] {
        assert!(refs(&f, RefKind::Type, t).is_empty(), "unexpected type ref {t}");
    }
    assert!(has_ref(&f, RefKind::Type, "Other", Some("ns")));
    assert_eq!(def(&f, "Ext").bases, vec!["Parent", "ns.Other"]);
}

#[test]
fn optional_calls_and_reads() {
    let f = facts(
        "a.js",
        r#"
import { store } from "./store";
export function run(opts) {
  store?.load?.(opts);
  const out = cache.get(opts.key);
  console.log(out);
  return compute(globalConfig.level, out);
}
"#,
    );
    assert!(has_ref(&f, RefKind::Call, "load", Some("store")));
    assert!(has_ref(&f, RefKind::Call, "get", Some("cache")));
    assert!(refs(&f, RefKind::Call, "log").is_empty(), "console is a builtin");
    assert!(has_ref(&f, RefKind::Read, "level", Some("globalConfig")));
    assert!(refs(&f, RefKind::Read, "out").is_empty());
    assert!(refs(&f, RefKind::Read, "opts").is_empty());
}

#[test]
fn signatures_are_one_line_and_bounded() {
    let long: String = (0..60).map(|i| format!("p{i}: number")).collect::<Vec<_>>().join(", ");
    let src = format!("/**\n * Doc.\n */\nexport function big(\n  {long}\n) {{\n  return 1;\n}}\n");
    let f = facts("a.ts", &src);
    let sig = def(&f, "big").signature.clone().unwrap();
    assert!(!sig.contains('\n'));
    assert!(sig.chars().count() <= 201, "{}", sig.chars().count());
    assert_eq!(def(&f, "big").doc.as_deref(), Some("Doc."));
}

#[test]
fn declaration_files() {
    let f = facts(
        "types/global.d.ts",
        "declare global { interface Window { app: App } }\nexport declare function boot(): void;\n",
    );
    assert!(has_def(&f, "Window"));
    assert!(def(&f, "boot").exported);
}

// ---------------------------------------------------------------------------
// Robustness and determinism
// ---------------------------------------------------------------------------

#[test]
fn deterministic() {
    for (path, src) in
        [("a.ts", SERVICE), ("b.tsx", BUTTON), ("c.js", LEGACY), ("d.ts", SPEC), ("e.ts", CONTROLLER), ("f.ts", BARREL)]
    {
        assert_eq!(facts(path, src), facts(path, src), "{path}");
    }
}

#[test]
fn never_panics_on_garbage() {
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let alphabet = b"{}()[]<>;:,.=+-*/!?@#'\"`\\ \n\tabcxyzABC0123 class function const let var import export from require new this async await => ...";
    for round in 0..300 {
        let len = (next() % 600) as usize;
        let bytes: Vec<u8> = (0..len)
            .map(|_| {
                let r = next();
                if round % 3 == 0 { r as u8 } else { alphabet[(r as usize) % alphabet.len()] }
            })
            .collect();
        for path in ["x.ts", "x.tsx", "x.js"] {
            let _ = TypeScriptPack.extract(path, lang_of(path), &bytes, &Budget::default());
        }
    }
}

#[test]
fn deep_nesting_is_bounded() {
    let depth = 3000;
    let src = format!("{}1{}", "(".repeat(depth), ")".repeat(depth));
    let f = facts("a.js", &src);
    assert_eq!(f.parse_errors, 0);
    let nested = format!("{}{}", "function f() {".repeat(800), "}".repeat(800));
    let budget = Budget { max_depth: 64, ..Budget::default() };
    let small = TypeScriptPack.extract("a.js", lang_of("a.js"), nested.as_bytes(), &budget).unwrap();
    assert!(small.definitions.len() < 64);
    let big = Budget { max_bytes: 10, ..Budget::default() };
    assert!(matches!(
        TypeScriptPack.extract("a.ts", lang_of("a.ts"), SERVICE.as_bytes(), &big),
        Err(ExtractError::TooLarge { .. })
    ));
    assert_eq!(facts("empty.ts", ""), FileFacts::default());
}

/// Every recursive shape at the default depth budget must fit the default
/// 2 MiB test-thread stack (debug build, the worst case).
#[test]
fn deep_shapes_fit_default_stack() {
    let n = 3000;
    let rep = |open: &str, mid: &str, close: &str| format!("{}{}{}", open.repeat(n), mid, close.repeat(n));
    let shapes = [
        ("parens.ts", rep("(", "1", ")")),
        ("calls.ts", rep("f(", "1", ")")),
        ("arrays.ts", format!("const x = {};", rep("[", "1", "]"))),
        ("objects.ts", format!("export const x = {};", rep("{a:", "1", "}"))),
        ("functions.ts", rep("function f(a: T) {", "g();", "}")),
        ("arrows.ts", format!("const f = {}1;", "() => ".repeat(n))),
        ("blocks.ts", rep("if (a) {", "b();", "}")),
        ("classes.ts", rep("class A extends B { m() {", "this.x = 1;", "}}")),
        ("namespaces.ts", rep("namespace N {", "export const x = 1;", "}")),
        ("types.ts", format!("type T = {};", rep("A<", "1", ">"))),
        ("objtypes.ts", format!("type T = {};", rep("{ a: ", "1", " }"))),
        ("ternary.ts", format!("x = {}1;", "a ? b : ".repeat(n))),
        ("binary.ts", format!("x = {}1;", "a + ".repeat(n))),
        ("members.ts", format!("x = a{};", ".b".repeat(n))),
        ("chain.ts", format!("x = a{};", ".b()".repeat(n))),
        ("tests.ts", rep("describe('x', () => {", "it('y', () => {});", "});")),
        ("jsx.tsx", format!("const x = {};", rep("<A>", "1", "</A>"))),
        ("decorators.ts", rep("@d(", "1", ")\nclass C {}")),
        ("destructure.ts", format!("function f({}) {{}}", rep("{a:", "b", "}"))),
    ];
    for (path, src) in shapes {
        let f = TypeScriptPack.extract(path, lang_of(path), src.as_bytes(), &Budget::default());
        assert!(f.is_ok(), "{path}");
    }
}

#[test]
fn reference_cap() {
    let src: String = (0..25_000).map(|i| format!("f{i}();\n")).collect();
    let f = facts("a.js", &src);
    assert_eq!(f.references.len(), crate::MAX_REFS);
}

#[test]
fn parse_errors_are_counted() {
    let f = facts("a.ts", "class A { m( { }\nfunction ok() {}\n");
    assert!(f.parse_errors > 0);
}

// ---------------------------------------------------------------------------
// Extractor version 2
// ---------------------------------------------------------------------------

#[test]
fn named_function_expressions_are_definitions() {
    let src = r#"
/**
 * Parse the query string.
 */
defineGetter(req, 'query', function query() {
  return parse(this).query;
});

app.use(function handler(req, res) { next(); });

module.exports.x = function x() { return 1 };

export default isSupported && function httpAdapter(config) {
  return new Promise(function dispatchHttpRequest(resolve, reject) {
    function handleResponse(res) { resolve(res); }
    stream.on('data', function onData(chunk) { handleResponse(chunk); });
  });
}

const timing = (config) => {
  return async function timing(c, next) { await next(); };
};
const anon = run(function () {});
"#;
    let f = facts("lib/request.js", src);
    let q = def(&f, "query");
    assert_eq!((q.label, q.parent), (Label::Function, None));
    assert_eq!(q.signature.as_deref(), Some("function query()"));
    assert_eq!(q.doc.as_deref(), Some("Parse the query string."));
    assert_eq!(q.span.start_line, 4, "span starts at `function`, not at the call");
    assert!(refs(&f, RefKind::Call, "parse").iter().all(|r| r.enclosing == Some(id(&f, "query"))));
    assert!(has_def(&f, "handler") && has_def(&f, "x") && has_def(&f, "httpAdapter"));
    let d = def(&f, "httpAdapter.dispatchHttpRequest");
    assert_eq!(d.parent, Some(id(&f, "httpAdapter")));
    assert!(has_def(&f, "httpAdapter.dispatchHttpRequest.handleResponse"));
    assert!(has_def(&f, "httpAdapter.dispatchHttpRequest.onData"));
    let call = refs(&f, RefKind::Call, "handleResponse");
    assert_eq!(call[0].enclosing, Some(id(&f, "httpAdapter.dispatchHttpRequest.onData")));
    assert_eq!(def(&f, "timing.timing").parent, Some(id(&f, "timing")));
    // Anonymous function expressions stay anonymous.
    assert_eq!(f.definitions.iter().filter(|d| d.label == Label::Function).count(), 9);
}

#[test]
fn type_parameters_of_signatures_and_function_types_are_not_references() {
    let src = r#"
interface Handler<E extends Env> {
  <P extends string, E2 extends Env = E>(path: P, h: H<E2, P>): Hono<E2>
  new <T>(value: T): Box<T>
}
type Mapper = <In, Out>(x: In) => Out
type Ctor = new <C>(c: C) => Wrapped<C>
type Keys<T> = { [K in keyof T]: T[K] }
"#;
    let f = facts("src/types.ts", src);
    for tp in ["E", "P", "E2", "T", "In", "Out", "C", "K"] {
        assert!(refs(&f, RefKind::Type, tp).is_empty(), "type parameter {tp} reported as a type reference");
    }
    for real in ["Env", "H", "Hono", "Box", "Wrapped"] {
        assert!(!refs(&f, RefKind::Type, real).is_empty(), "missing type reference {real}");
    }
}

/// tree-sitter-typescript 0.23 splits an interface whose generic call
/// signatures start on new lines into `ERROR` fragments; the type parameters
/// of the interface and of each fragment stay in scope there.
#[test]
fn type_parameters_survive_signature_parse_errors() {
    let src = r#"
export interface HandlerInterface<E extends Env = Env, BasePath extends string = '/'> {
  <P extends string = BasePath, E2 extends Env = E>(
    handler: H<E2, P>
  ): HonoBase<E, BasePath>

  // two handlers
  <
    P extends string = BasePath,
    E2 extends Env = E,
    E3 extends Env = Merge<E, E2>,
  >(
    ...handlers: [H<E2, P>, H<E3, P>]
  ): HonoBase<E3, BasePath>
}
"#;
    let f = facts("src/types.ts", src);
    assert!(f.parse_errors > 0, "the grammar gap this test covers is gone; simplify error_region");
    for tp in ["E", "BasePath", "P", "E2", "E3"] {
        assert!(refs(&f, RefKind::Type, tp).is_empty(), "type parameter {tp} reported as a type reference");
    }
    let h = refs(&f, RefKind::Type, "H");
    assert!(h.len() >= 3);
    assert!(h.iter().all(|r| r.enclosing == Some(id(&f, "HandlerInterface"))), "{h:?}");
    assert!(!refs(&f, RefKind::Type, "Merge").is_empty());
}

#[test]
fn runtime_globals_are_builtins_for_the_resolver_only() {
    let p = TypeScriptPack;
    for g in ["Request", "Response", "Headers", "FormData", "Blob", "ReadableStream", "WebSocket", "Event", "crypto"] {
        assert!(p.is_builtin(g), "{g}");
    }
    for g in ["URL", "URLSearchParams", "fetch", "AbortController", "TextEncoder", "structuredClone", "Buffer"] {
        assert!(p.is_builtin(g), "{g}");
    }
    // References are still emitted, so an imported or local `Request` resolves.
    let f = facts(
        "src/a.ts",
        "import { Request } from './request'\nexport function f(r: Request) { return new Response(r) }\n",
    );
    assert!(has_ref(&f, RefKind::Type, "Request", None));
    assert!(has_ref(&f, RefKind::New, "Response", None));
    // Hard builtins are dropped at the source as before.
    let f = facts("src/b.ts", "export function g() { return fetch(new URL('x')) }\n");
    assert!(refs(&f, RefKind::Call, "fetch").is_empty() && refs(&f, RefKind::New, "URL").is_empty());
}

#[test]
fn local_exports_are_self_reexports() {
    let src = "function main() {}\nconst helper = 1;\nexport { helper as util, main };\nexport default main;\n";
    let f = facts("src/app.ts", src);
    let own: Vec<_> = f.imports.iter().filter(|i| i.reexport).collect();
    assert_eq!(own.len(), 2, "{:?}", f.imports);
    assert!(own.iter().all(|i| i.module == "./app.ts" && !i.type_only));
    assert_eq!(names(own[0]), vec![n("helper", Some("util"))]);
    assert_eq!(names(own[1]), vec![n("main", Some("default"))]);
    assert!(def(&f, "main").exported && def(&f, "helper").exported);
    // CommonJS `module.exports = x` is the ESM-interop default.
    let f = facts("lib/express.js", "function createApp() {}\nmodule.exports = createApp;\n");
    let own: Vec<_> = f.imports.iter().filter(|i| i.reexport).collect();
    assert_eq!(own.len(), 1);
    assert_eq!((own[0].module.as_str(), names(own[0])), ("./express.js", vec![n("createApp", Some("default"))]));
}

// ---------------------------------------------------------------------------
// Pack metadata
// ---------------------------------------------------------------------------

#[test]
fn pack_metadata() {
    let p = TypeScriptPack;
    assert_eq!(p.extractor_version(), 5);
    assert_eq!(p.self_receivers(), &["this"]);
    assert!(p.is_builtin("console") && p.is_builtin("Promise") && p.is_builtin("Record"));
    assert!(!p.is_builtin("UserService"));
    let proj = ProjectFiles::default();
    assert_eq!(p.module_name("src/util/index.ts", &proj), "src/util/index");
    assert_eq!(p.module_name("types/x.d.ts", &proj), "types/x");
    assert_eq!(p.module_name("x.d.mts", &proj), "x");
    assert_eq!(p.module_name("a/b.c/Component.test.tsx", &proj), "a/b.c/Component.test");
    assert_eq!(p.module_name("Makefile", &proj), "Makefile");
    assert_eq!(p.module_name(".eslintrc.js", &proj), ".eslintrc");
}

#[test]
fn imported_name_shape() {
    let f = facts("a.ts", "import def, * as ns from './x';\nimport './side';\nconst m = import('./dyn');\n");
    let x = &f.imports[0];
    assert_eq!(x.names, vec![ImportedName { name: "default".into(), alias: Some("def".into()) }]);
    assert_eq!(x.module_alias.as_deref(), Some("ns"));
    assert!(f.imports.iter().any(|i| i.module == "./side" && i.names.is_empty() && i.module_alias.is_none()));
    assert!(f.imports.iter().any(|i| i.module == "./dyn"));
}

// ---------------------------------------------------------------------------
// Import resolution (oxc_resolver over the virtual file system)
// ---------------------------------------------------------------------------

#[test]
fn resolves_relative_with_extension_probing() {
    let p = project(
        &[
            "src/app.ts",
            "src/util.ts",
            "src/comp.tsx",
            "src/legacy.js",
            "src/data.json",
            "src/lib/index.ts",
            "src/esm.mts",
            "src/types.d.ts",
            "src/deep/x.ts",
            "README.md",
        ],
        &[],
    );
    assert_eq!(resolve(&p, "src/app.ts", "./util").as_deref(), Some("src/util.ts"));
    assert_eq!(resolve(&p, "src/app.ts", "./comp").as_deref(), Some("src/comp.tsx"));
    assert_eq!(resolve(&p, "src/app.ts", "./legacy").as_deref(), Some("src/legacy.js"));
    assert_eq!(resolve(&p, "src/app.ts", "./data.json").as_deref(), Some("src/data.json"));
    assert_eq!(resolve(&p, "src/app.ts", "./lib").as_deref(), Some("src/lib/index.ts"));
    assert_eq!(resolve(&p, "src/app.ts", "./util.js").as_deref(), Some("src/util.ts"), ".js → .ts");
    assert_eq!(resolve(&p, "src/app.ts", "./comp.js").as_deref(), Some("src/comp.tsx"), ".js → .tsx");
    assert_eq!(resolve(&p, "src/app.ts", "./esm.mjs").as_deref(), Some("src/esm.mts"), ".mjs → .mts");
    assert_eq!(resolve(&p, "src/app.ts", "./types").as_deref(), Some("src/types.d.ts"));
    assert_eq!(resolve(&p, "src/deep/x.ts", "../util").as_deref(), Some("src/util.ts"));
    assert_eq!(resolve(&p, "src/deep/x.ts", "../../README.md").as_deref(), Some("README.md"));
    assert_eq!(resolve(&p, "src/app.ts", "./missing"), None);
    assert_eq!(resolve(&p, "src/app.ts", "../../outside"), None);
    assert_eq!(resolve(&p, "src/app.ts", ""), None);
}

#[test]
fn third_party_and_builtins_are_none() {
    let p = project(
        &["src/a.ts", "node_modules/react/index.js"],
        &[
            ("package.json", r#"{"name":"app","dependencies":{"react":"18"}}"#),
            ("node_modules/react/package.json", r#"{"name":"react","main":"index.js"}"#),
        ],
    );
    assert_eq!(resolve(&p, "src/a.ts", "react"), None, "vendored node_modules are outside the project");
    assert_eq!(resolve(&p, "src/a.ts", "lodash"), None);
    assert_eq!(resolve(&p, "src/a.ts", "fs"), None);
    assert_eq!(resolve(&p, "src/a.ts", "node:path"), None);
    assert_eq!(resolve(&p, "src/a.ts", "https://example.com/x.js"), None);
}

#[test]
fn tsconfig_paths_base_url_and_extends() {
    let base = r#"{
      // comments and trailing commas are fine
      "compilerOptions": {
        "baseUrl": ".",
        "paths": { "@app/*": ["src/app/*"], "@shared": ["src/shared/index.ts"], },
      },
    }"#;
    let p = project(
        &["src/app/users/service.ts", "src/shared/index.ts", "src/main.ts", "src/config.ts"],
        &[
            ("tsconfig.base.json", base),
            ("tsconfig.json", r#"{ "extends": "./tsconfig.base.json", "include": ["src"] }"#),
        ],
    );
    assert_eq!(resolve(&p, "src/main.ts", "@app/users/service").as_deref(), Some("src/app/users/service.ts"));
    assert_eq!(resolve(&p, "src/main.ts", "@shared").as_deref(), Some("src/shared/index.ts"));
    assert_eq!(resolve(&p, "src/main.ts", "src/config").as_deref(), Some("src/config.ts"), "baseUrl");
    assert_eq!(resolve(&p, "src/main.ts", "./config").as_deref(), Some("src/config.ts"));
}

#[test]
fn nested_tsconfig_and_jsconfig() {
    let p = project(
        &["apps/web/src/pages/home.tsx", "apps/web/src/components/Nav.tsx", "tools/a.js", "tools/lib/b.js"],
        &[
            ("apps/web/tsconfig.json", r#"{"compilerOptions":{"paths":{"@/*":["./src/*"]}}}"#),
            ("tools/jsconfig.json", r#"{"compilerOptions":{"baseUrl":".","paths":{"~lib/*":["lib/*"]}}}"#),
        ],
    );
    assert_eq!(
        resolve(&p, "apps/web/src/pages/home.tsx", "@/components/Nav").as_deref(),
        Some("apps/web/src/components/Nav.tsx")
    );
    assert_eq!(resolve(&p, "tools/a.js", "~lib/b").as_deref(), Some("tools/lib/b.js"));
    assert_eq!(resolve(&p, "tools/a.js", "lib/b").as_deref(), Some("tools/lib/b.js"));
}

#[test]
fn tsconfig_extending_missing_package_still_resolves() {
    let p = project(
        &["src/a.ts", "src/b.ts"],
        &[(
            "tsconfig.json",
            r#"{ "extends": "@tsconfig/node20/tsconfig.json", "compilerOptions": {"baseUrl": "src"} }"#,
        )],
    );
    assert_eq!(resolve(&p, "src/a.ts", "./b").as_deref(), Some("src/b.ts"));
}

#[test]
fn workspace_packages() {
    let p = project(
        &[
            "packages/ui/src/index.ts",
            "packages/ui/src/button.tsx",
            "packages/core/lib/main.js",
            "packages/core/lib/util.js",
            "packages/built/src/index.ts",
            "apps/web/src/main.ts",
        ],
        &[
            ("package.json", r#"{"name":"monorepo","private":true,"workspaces":["packages/*","apps/*"]}"#),
            (
                "packages/ui/package.json",
                r#"{"name":"@acme/ui","exports":{".":{"types":"./src/index.ts","default":"./dist/index.js"},"./button":"./src/button.tsx"}}"#,
            ),
            ("packages/core/package.json", r#"{"name":"core","main":"lib/main.js"}"#),
            (
                "packages/built/package.json",
                r#"{"name":"@acme/built","main":"dist/index.js","types":"dist/index.d.ts"}"#,
            ),
            ("apps/web/package.json", r#"{"name":"web","dependencies":{"@acme/ui":"workspace:*"}}"#),
        ],
    );
    let from = "apps/web/src/main.ts";
    assert_eq!(resolve(&p, from, "@acme/ui").as_deref(), Some("packages/ui/src/index.ts"), "exports + types condition");
    assert_eq!(resolve(&p, from, "@acme/ui/button").as_deref(), Some("packages/ui/src/button.tsx"), "exports subpath");
    assert_eq!(resolve(&p, from, "core").as_deref(), Some("packages/core/lib/main.js"), "main field");
    assert_eq!(resolve(&p, from, "core/lib/util").as_deref(), Some("packages/core/lib/util.js"), "deep import");
    assert_eq!(resolve(&p, from, "@acme/built").as_deref(), Some("packages/built/src/index.ts"), "src fallback");
    assert_eq!(resolve(&p, from, "@acme/missing"), None);
}

#[test]
fn resolver_cache_tracks_project_identity() {
    let a = project(&["src/a.ts", "src/b.ts"], &[]);
    let b = project(&["src/a.ts", "src/b.tsx"], &[]);
    assert_eq!(resolve(&a, "src/a.ts", "./b").as_deref(), Some("src/b.ts"));
    assert_eq!(resolve(&b, "src/a.ts", "./b").as_deref(), Some("src/b.tsx"));
    assert_eq!(resolve(&a, "src/a.ts", "./b").as_deref(), Some("src/b.ts"));
    for _ in 0..3 {
        let c = project(&["x.ts", "y.js"], &[]);
        assert_eq!(resolve(&c, "x.ts", "./y").as_deref(), Some("y.js"));
        drop(c);
        let d = project(&["x.ts", "y.ts"], &[]);
        assert_eq!(resolve(&d, "x.ts", "./y").as_deref(), Some("y.ts"));
    }
}

#[test]
fn resolves_extracted_imports_end_to_end() {
    let p = project(
        &[
            "src/users/users.service.ts",
            "src/users/users.controller.ts",
            "src/users/dto/index.ts",
            "src/users/user.entity.ts",
            "src/users/index.ts",
            "src/users/Button.tsx",
        ],
        &[],
    );
    let f = facts("src/users/index.ts", BARREL);
    let got: Vec<_> = f.imports.iter().map(|i| TypeScriptPack.resolve_import("src/users/index.ts", i, &p)).collect();
    assert_eq!(
        got,
        vec![
            Some("src/users/users.service.ts".to_string()),
            Some("src/users/users.controller.ts".to_string()),
            Some("src/users/dto/index.ts".to_string()),
            Some("src/users/users.service.ts".to_string()),
            Some("src/users/Button.tsx".to_string()),
            Some("src/users/Button.tsx".to_string()),
        ]
    );
}

// ---------------------------------------------------------------------------
// Real repositories (manual): RUECA_TS_REPOS=/path/a:/path/b cargo test -p
// rueca-lang-typescript --release -- --ignored real_repos --nocapture
// ---------------------------------------------------------------------------

#[test]
#[ignore]
fn real_repos() {
    let Ok(roots) = std::env::var("RUECA_TS_REPOS") else {
        eprintln!("RUECA_TS_REPOS not set; skipping");
        return;
    };
    for root in roots.split(':').filter(|r| !r.is_empty()) {
        real_repo(std::path::Path::new(root));
    }
}

fn walk(dir: &std::path::Path, root: &std::path::Path, out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name().to_string_lossy().to_string();
        let path = e.path();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            if name != ".git" && name != "node_modules" {
                walk(&path, root, out);
            }
        } else if ft.is_file() {
            out.push(path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/"));
        }
    }
}

fn real_repo(root: &std::path::Path) {
    use std::time::Instant;
    let mut paths = Vec::new();
    walk(root, root, &mut paths);
    let mut configs = BTreeMap::new();
    for p in &paths {
        if ProjectFiles::is_config_file(p)
            && let Ok(text) = std::fs::read_to_string(root.join(p))
        {
            configs.insert(p.clone(), text);
        }
    }
    let project = ProjectFiles { paths: paths.iter().cloned().collect(), configs };
    let mut files = 0usize;
    let mut bytes = 0usize;
    let (mut defs, mut nrefs, mut imports) = (0usize, 0usize, 0usize);
    let mut error_files = Vec::new();
    let mut all_imports = Vec::new();
    let t0 = Instant::now();
    for p in &paths {
        let Some(lang) = rueca_model::Language::from_path(p) else { continue };
        if lang == rueca_model::Language::Python {
            continue;
        }
        let src = std::fs::read(root.join(p)).unwrap();
        match TypeScriptPack.extract(p, lang, &src, &Budget::default()) {
            Ok(f) => {
                files += 1;
                bytes += src.len();
                defs += f.definitions.len();
                nrefs += f.references.len();
                imports += f.imports.len();
                if f.parse_errors > 0 {
                    error_files.push(format!("{p} ({})", f.parse_errors));
                }
                all_imports.extend(f.imports.into_iter().map(|i| (p.clone(), i)));
            }
            Err(e) => error_files.push(format!("{p}: {e}")),
        }
    }
    let extract_time = t0.elapsed();
    let t1 = Instant::now();
    let (mut rel_total, mut rel_ok, mut bare_total, mut bare_ok) = (0usize, 0usize, 0usize, 0usize);
    let mut unresolved_rel = Vec::new();
    let mut unresolved_bare: BTreeMap<String, usize> = BTreeMap::new();
    let mut resolved_bare: BTreeMap<String, usize> = BTreeMap::new();
    for (from, imp) in &all_imports {
        let r = TypeScriptPack.resolve_import(from, imp, &project);
        if imp.module.starts_with('.') {
            rel_total += 1;
            if r.is_some() {
                rel_ok += 1;
            } else if unresolved_rel.len() < 15 {
                unresolved_rel.push(format!("{from} -> {}", imp.module));
            }
        } else {
            bare_total += 1;
            bare_ok += usize::from(r.is_some());
            let pkg = crate::resolve::split_package(&imp.module).map_or(imp.module.clone(), |(p, _)| p.to_string());
            *if r.is_some() { &mut resolved_bare } else { &mut unresolved_bare }.entry(pkg).or_default() += 1;
        }
    }
    let resolve_time = t1.elapsed();
    let pct = |a: usize, b: usize| if b == 0 { 100.0 } else { 100.0 * a as f64 / b as f64 };
    println!("== {}", root.display());
    println!("files {files} ({:.1} MB), definitions {defs}, references {nrefs}, imports {imports}", bytes as f64 / 1e6);
    println!(
        "extract {:.2?} ({:.0} files/s), resolve {:.2?} ({} imports)",
        extract_time,
        files as f64 / extract_time.as_secs_f64(),
        resolve_time,
        all_imports.len()
    );
    println!(
        "resolved: relative {rel_ok}/{rel_total} ({:.1}%), bare {bare_ok}/{bare_total} ({:.1}%), overall {:.1}%",
        pct(rel_ok, rel_total),
        pct(bare_ok, bare_total),
        pct(rel_ok + bare_ok, rel_total + bare_total)
    );
    println!("files with parse errors: {} {:?}", error_files.len(), error_files.iter().take(20).collect::<Vec<_>>());
    println!("unresolved relative (first 15): {unresolved_rel:#?}");
    let top = |m: &BTreeMap<String, usize>| {
        let mut v: Vec<_> = m.iter().map(|(k, c)| (*c, k.clone())).collect();
        v.sort_by(|a, b| b.cmp(a));
        v.into_iter().take(15).map(|(c, k)| format!("{k} ({c})")).collect::<Vec<_>>()
    };
    println!("resolved bare packages: {:?}", top(&resolved_bare));
    println!("unresolved bare packages (top 15): {:?}", top(&unresolved_bare));
}

// ---------------------------------------------------------------------------
// Env var and config key reads (the code ↔ config bridge, RFC 0003 §7.2)
// ---------------------------------------------------------------------------

#[test]
fn env_and_config_reads() {
    let f = facts(
        "src/env.ts",
        r#"import config from "config";
const TOP = process.env.TOP_LEVEL;

export function start(): string {
  const url = process.env.DATABASE_URL ?? "";
  const level = process.env["LOG_LEVEL"];
  const { REDIS_URL, QUEUE: queue, MODE = "dev" } = process.env;
  const vite = import.meta.env.VITE_API;
  const deno = Deno.env.get("DENO_X");
  const port = config.get("server.port");
  const has = config.has("db.host");
  process.env.SET_ONLY = "1";
  delete process.env.GONE;
  const dynamic = process.env[url];
  return `${url}${level}${REDIS_URL}${queue}${MODE}${vite}${deno}${port}${has}${dynamic}${TOP}`;
}

function shadow(process: { env: Record<string, string> }) {
  return process.env.NOT_ENV;
}
"#,
    );
    let mut env: Vec<(String, Option<u32>)> =
        f.references.iter().filter(|r| r.kind == RefKind::EnvVar).map(|r| (r.name.clone(), r.enclosing)).collect();
    env.sort();
    let start = id(&f, "start");
    let top = id(&f, "TOP");
    let expect: Vec<(String, Option<u32>)> = [
        ("DATABASE_URL", Some(start)),
        ("DENO_X", Some(start)),
        ("LOG_LEVEL", Some(start)),
        ("MODE", Some(start)),
        ("QUEUE", Some(start)),
        ("REDIS_URL", Some(start)),
        ("TOP_LEVEL", Some(top)),
        ("VITE_API", Some(start)),
    ]
    .iter()
    .map(|(n, e)| (n.to_string(), *e))
    .collect();
    assert_eq!(env, expect);
    let cfg: Vec<(&str, Option<&str>)> = f
        .references
        .iter()
        .filter(|r| r.kind == RefKind::ConfigKey)
        .map(|r| (r.name.as_str(), r.receiver.as_deref()))
        .collect();
    assert_eq!(cfg, [("server.port", Some("node-config")), ("db.host", Some("node-config"))]);
    // Ordinary facts are unchanged.
    assert!(has_ref(&f, RefKind::Call, "get", Some("config")));

    // `config.get` without the `config` package import is not a config read.
    let f = facts("src/a.js", "const config = load();\nexport const p = config.get(\"server.port\");\n");
    assert!(f.references.iter().all(|r| r.kind != RefKind::ConfigKey));
    let f = facts("src/b.js", "const config = require(\"config\");\nexport const p = config.get(\"a.b\");\n");
    assert!(has_ref(&f, RefKind::ConfigKey, "a.b", Some("node-config")));
}

// ---------------------------------------------------------------------------
// Receiver types from declarations and assignments (`types.rs`)
// ---------------------------------------------------------------------------

fn call_receiver(f: &FileFacts, name: &str) -> Vec<Option<String>> {
    refs(f, RefKind::Call, name).iter().map(|r| r.receiver.clone()).collect()
}

#[test]
fn receivers_typed_from_annotations_and_constructions() {
    let f = facts(
        "src/app.ts",
        r#"import { Hono } from "./hono";
import type { Context } from "./context";
const app = new Hono();
let maybe = new Hono();
maybe = makeApp();
export function handler(c: Context, opt?: Store | undefined, list: Store[], u: Store | Cache) {
  c.header("x");
  opt.get();
  list.at(0);
  u.get();
  app.get("/", () => {});
  maybe.route();
  const s = new Store();
  s.put();
  const t = other() as Store;
  t.flush();
  let r: Repo = make();
  r = something();
  r.save();
  let k = new Store();
  if (x) { k = new Cache(); }
  k.drop();
  (new Store()).open();
  new Store().close();
}
function shadow(app: Other) {
  app.get();
}
export class Router<T> {
  #node: Node = new Node();
  private cache = new Cache();
  plain;
  constructor(private repo: Repo, readonly name: string, t: T) {
    this.plain = new Store();
    t.go();
  }
  add() {
    this.#node.insert();
    this.repo.save();
    this.cache.read();
    this.plain.write();
    const f = () => this.cache.evict();
    function g() { this.cache.nope(); }
  }
}
"#,
    );
    let ty = |s: &str| Some(format!("$type:{s}"));
    assert_eq!(call_receiver(&f, "header"), vec![ty("Context")]);
    // `opt?: Store | undefined` → Store; arrays and real unions are untyped.
    assert_eq!(call_receiver(&f, "at"), vec![Some("list".into())]);
    assert!(call_receiver(&f, "get").contains(&ty("Store")));
    assert!(call_receiver(&f, "get").contains(&Some("u".into())));
    // Module-level `const app = new Hono()`, shadowed by a parameter in `shadow`.
    assert!(call_receiver(&f, "get").contains(&ty("Hono")));
    assert!(call_receiver(&f, "get").contains(&ty("Other")));
    // Reassigned to an untyped value: untyped.
    assert_eq!(call_receiver(&f, "route"), vec![Some("maybe".into())]);
    assert_eq!(call_receiver(&f, "put"), vec![ty("Store")]);
    assert_eq!(call_receiver(&f, "flush"), vec![ty("Store")]);
    // An annotation holds whatever is assigned later.
    assert_eq!(call_receiver(&f, "save"), vec![ty("Repo"), ty("Repo")]);
    // Two different constructed types: untyped.
    assert_eq!(call_receiver(&f, "drop"), vec![Some("k".into())]);
    assert_eq!(call_receiver(&f, "open"), vec![ty("Store")]);
    assert_eq!(call_receiver(&f, "close"), vec![ty("Store")]);
    // Class fields: annotated, constructed, parameter properties, `this.x = new T()`.
    assert_eq!(call_receiver(&f, "insert"), vec![ty("Node")]);
    assert_eq!(call_receiver(&f, "read"), vec![ty("Cache")]);
    assert_eq!(call_receiver(&f, "write"), vec![ty("Store")]);
    assert_eq!(call_receiver(&f, "evict"), vec![ty("Cache")]);
    // `this` in a plain function is not the instance; type parameters are not types.
    assert_eq!(call_receiver(&f, "nope"), vec![Some("this.cache".into())]);
    assert_eq!(call_receiver(&f, "go"), vec![Some("t".into())]);
}

#[test]
fn receivers_typed_in_javascript() {
    let f = facts(
        "lib/app.js",
        r#"var View = require('./view');
function render(name, options) {
  var view = new View(name, options);
  view.render(options);
  var other = new View(name);
  other = options.view;
  other.render();
}
"#,
    );
    assert_eq!(call_receiver(&f, "render"), vec![Some("$type:View".into()), Some("other".into())]);
}

#[test]
fn receivers_typed_from_bounds_elements_and_returns() {
    let f = facts(
        "src/zoo.ts",
        r#"export class Animal { speak(): string { return ""; } }
export class Dog extends Animal {
  fetch(): string { return ""; }
  self(): this { return this; }
  static make(): Dog { return new Dog(); }
}
export class Box<T> {
  map<U>(f: (v: T) => U): Box<U> { return new Box(); }
  get(): T { return this.v; }
}
export function chorus<T extends Animal>(xs: T[], ys: Array<Dog>) {
  xs.map((x) => x.speak());
  ys.forEach(function (y) { y.fetch(); });
  for (const z of ys) { z.fetch(); }
  for (const w of xs) { w.speak(); }
}
export function walk<D extends Dog, R extends Record<string, D>>(d: D, r: R) {
  d.fetch();
  r.keys();
}
function makeBox(): Box<number> { return new Box(); }
function makeCounter() { return { next: () => 1 }; }
export function use() {
  makeBox().map(f).get();
  const c = makeCounter();
  c.next();
  Dog.make().self().fetch();
  const d = Dog.make();
  d.speak();
}
"#,
    );
    let ty = |s: &str| Some(format!("$type:{s}"));
    assert_eq!(call_receiver(&f, "speak"), vec![ty("Animal"), ty("Animal"), ty("Dog")]);
    assert_eq!(call_receiver(&f, "fetch"), vec![ty("Dog"), ty("Dog"), ty("Dog"), ty("Dog")]);
    // A bound that is a builtin type is no type.
    assert_eq!(call_receiver(&f, "keys"), vec![Some("r".into())]);
    assert_eq!(call_receiver(&f, "map"), vec![Some("xs".into()), ty("Box")]);
    assert_eq!(call_receiver(&f, "get"), vec![ty("Box")]);
    // A function returning an object literal types its result as itself.
    assert_eq!(call_receiver(&f, "next"), vec![ty("makeCounter")]);
    assert_eq!(call_receiver(&f, "self"), vec![ty("Dog")]);
}

#[test]
fn object_literal_methods_and_assigned_functions_are_definitions() {
    let f = facts(
        "src/counter.ts",
        r#"export function makeCounter() {
  const step = () => 1;
  return {
    next: () => step(),
    peek() { return 0; },
    get size() { return 1; },
    set size(v: number) {},
  };
}
export default {
  start() { return 1; },
};
export function logged(tag: string) {
  return function (_t: object, key: string, desc: PropertyDescriptor) {
    desc.value = function () { trace(tag); };
  };
}
export class Timer {
  constructor() {
    this.onTick = () => { tick(); };
  }
}
"#,
    );
    let next = def(&f, "makeCounter.next");
    assert_eq!(next.label, Label::Method);
    assert_eq!(next.parent, Some(id(&f, "makeCounter")));
    assert_eq!(def(&f, "makeCounter.peek").label, Label::Method);
    assert_eq!(f.definitions.iter().filter(|d| d.qualified_name == "makeCounter.size").count(), 1);
    let step = refs(&f, RefKind::Call, "step");
    assert_eq!(step[0].enclosing, Some(id(&f, "makeCounter.next")));
    // `export default { … }`: a `default` variable owning the methods.
    assert_eq!(def(&f, "default").label, Label::Variable);
    assert_eq!(def(&f, "default.start").label, Label::Method);
    // Functions assigned to members are named by the member.
    let value = def(&f, "logged.value");
    assert_eq!(value.label, Label::Function);
    assert_eq!(refs(&f, RefKind::Call, "trace")[0].enclosing, Some(id(&f, "logged.value")));
    assert_eq!(refs(&f, RefKind::Call, "tick")[0].enclosing, Some(id(&f, "Timer.constructor.onTick")));
    assert!(has_ref(&f, RefKind::Write, "onTick", Some("this")));
}

#[test]
fn commonjs_object_exports_and_own_export_calls() {
    let f = facts(
        "lib/service.js",
        r#"function helper(x) { return x; }
module.exports = {
  start() { return helper(1); },
  stop: () => helper(0),
  helper,
};
module.exports.read = function (p) { return exports.decode(p) + module.exports.stop(); };
exports.decode = (raw) => raw;
"#,
    );
    for name in ["start", "stop", "read", "decode"] {
        let d = def(&f, name);
        assert!(d.exported && d.parent.is_none(), "{name}");
    }
    assert!(has_ref(&f, RefKind::Call, "decode", None));
    assert!(has_ref(&f, RefKind::Call, "stop", None));
    let log = facts("lib/log.js", "module.exports = function log(m) { return m; };\n");
    let reexport = log.imports.iter().find(|i| i.reexport).expect("default re-export");
    assert_eq!(names(reexport), vec![n("log", Some("default"))]);
    let loader = facts("src/loader.ts", "export function cache() {}\nexport default function load() { cache(); }\n");
    let reexport = loader.imports.iter().find(|i| i.reexport).expect("default re-export");
    assert_eq!(names(reexport), vec![n("load", Some("default"))]);
}

#[test]
fn super_calls_local_callees_and_shadowed_builtins() {
    let f = facts(
        "src/app.ts",
        r#"import { fetch } from "./http";
import { expect } from "vitest";
export function fn(): number { return 1; }
export class User extends Model {
  constructor(id: string) {
    super(id);
  }
}
export function run(fn: () => number, Cls: typeof User): number {
  const inner = () => 2;
  let later;
  later = () => 3;
  new Cls();
  expect(fn() + inner() + later());
  return fetch("x");
}
export function direct(): number {
  return fn();
}
"#,
    );
    assert!(has_ref(&f, RefKind::Call, "constructor", Some("super")));
    // A parameter (or a plain variable) holding a function is not the module's `fn`.
    let fn_calls = refs(&f, RefKind::Call, "fn");
    assert_eq!(fn_calls.len(), 1);
    assert_eq!(fn_calls[0].enclosing, Some(id(&f, "direct")));
    assert!(refs(&f, RefKind::New, "Cls").is_empty());
    assert!(refs(&f, RefKind::Call, "later").is_empty());
    // A nested definition keeps the call.
    assert_eq!(refs(&f, RefKind::Call, "inner").len(), 1);
    // A project import shadows a builtin; a package import does not.
    assert_eq!(refs(&f, RefKind::Call, "fetch").len(), 1);
    assert!(refs(&f, RefKind::Call, "expect").is_empty());
}
