//! End-to-end resolution with the real Python and TypeScript packs on small
//! inline multi-file projects.

use rueca_extract::{Budget, ProjectFiles, Registry};
use rueca_lang_python::PythonPack;
use rueca_lang_typescript::TypeScriptPack;
use rueca_model::{EdgeKind, Language, NodeId, ResolvedGraph, SnapshotInfo, Strategy, git_blob_sha};
use rueca_resolve::{FileInput, ResolveOutput, resolve};

fn registry() -> Registry {
    let mut r = Registry::new();
    r.register(Box::new(PythonPack));
    r.register(Box::new(TypeScriptPack));
    r
}

fn build(files: &[(&str, &str)]) -> ResolveOutput {
    let registry = registry();
    let project =
        ProjectFiles { paths: files.iter().map(|(p, _)| p.to_string()).collect(), configs: Default::default() };
    let inputs = files
        .iter()
        .map(|(path, src)| {
            let language = Language::from_path(path);
            let facts = registry
                .for_path(path)
                .map(|(lang, pack)| pack.extract(path, lang, src.as_bytes(), &Budget::default()).expect("extract"));
            FileInput {
                path: path.to_string(),
                blob_sha: git_blob_sha(src.as_bytes()),
                language,
                size: src.len() as u64,
                status: if facts.is_some() { "ok".into() } else { "skipped:unsupported".into() },
                facts,
            }
        })
        .collect();
    let snapshot = SnapshotInfo {
        repo: "local/test".into(),
        commit: "worktree".into(),
        dirty: true,
        branch: None,
        root: None,
        indexed_at: 0,
        rueca_version: "test".into(),
    };
    let out = resolve(snapshot, inputs, &project, &registry);
    out.graph.validate().unwrap();
    out
}

struct G<'g>(&'g ResolvedGraph);

impl G<'_> {
    /// The node in `path` whose qualified name ends with `local` after the
    /// module separator.
    #[track_caller]
    fn id(&self, path: &str, local: &str) -> NodeId {
        let f = self.0.files.iter().position(|f| f.path == path).unwrap_or_else(|| panic!("no file {path}")) as u32;
        let hits: Vec<_> = self
            .0
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| {
                n.file == Some(f)
                    && n.label != rueca_model::Label::File
                    && (n.qualified_name.ends_with(&format!(".{local}"))
                        || n.qualified_name.ends_with(&format!(":{local}")))
            })
            .map(|(i, _)| i as NodeId)
            .collect();
        assert_eq!(hits.len(), 1, "expected one node {local} in {path}, got {hits:?}");
        hits[0]
    }
    fn file(&self, path: &str) -> NodeId {
        self.0.files.iter().find(|f| f.path == path).unwrap().node
    }
    #[track_caller]
    fn edge(&self, src: NodeId, dst: NodeId, kind: EdgeKind) -> (Strategy, f32) {
        let e = self.0.edges.iter().find(|e| e.src == src && e.dst == dst && e.kind == kind).unwrap_or_else(|| {
            panic!(
                "missing {kind} {} -> {}",
                self.0.nodes[src as usize].qualified_name, self.0.nodes[dst as usize].qualified_name
            )
        });
        (e.meta.strategy, e.meta.confidence)
    }
}

const PY: &[(&str, &str)] = &[
    ("pkg/__init__.py", "from .models import User\n"),
    (
        "pkg/models.py",
        r#"
class Base:
    def save(self):
        return 1


class User(Base):
    """A user."""

    def __init__(self, name):
        self.name = name

    def greet(self):
        self.save()
        return helper(self.name)


def helper(x):
    return x
"#,
    ),
    ("pkg/deco.py", "def decorate(f):\n    return f\n"),
    (
        "pkg/service.py",
        r#"
import os
import pkg.models
from pkg import User
from . import models
from pkg.deco import decorate


def make():
    u = User("a")
    models.helper(1)
    pkg.models.helper(2)
    os.path.join("a", "b")
    return u


@decorate
def decorated():
    def inner():
        return 1
    return inner()
"#,
    ),
    (
        "tests/test_service.py",
        r#"
from pkg.service import make


def test_make():
    assert make()
"#,
    ),
    ("README.md", "# demo\n"),
];

#[test]
fn python_project() {
    let out = build(PY);
    let g = G(&out.graph);
    use EdgeKind::*;
    let make = g.id("pkg/service.py", "make");
    let user = g.id("pkg/models.py", "User");
    let init = g.id("pkg/models.py", "User.__init__");
    let helper = g.id("pkg/models.py", "helper");
    let greet = g.id("pkg/models.py", "User.greet");
    let save = g.id("pkg/models.py", "Base.save");
    let base = g.id("pkg/models.py", "Base");

    assert_eq!(g.edge(make, user, Calls), (Strategy::ImportResolved, 0.95));
    assert_eq!(g.edge(make, init, Calls).0, Strategy::ImportResolved);
    assert_eq!(g.edge(make, helper, Calls), (Strategy::ImportResolved, 0.9));
    assert_eq!(g.edge(greet, save, Calls), (Strategy::TypeResolved, 0.85));
    assert_eq!(g.edge(greet, helper, Calls), (Strategy::SameModule, 0.9));
    assert_eq!(g.edge(user, base, Inherits).0, Strategy::SameModule);

    let decorate = g.id("pkg/deco.py", "decorate");
    let decorated = g.id("pkg/service.py", "decorated");
    assert_eq!(g.edge(decorate, decorated, Decorates), (Strategy::ImportResolved, 0.95));
    let inner = g.id("pkg/service.py", "decorated.inner");
    assert_eq!(g.edge(decorated, inner, Calls), (Strategy::SameModule, 0.95));

    let test = g.id("tests/test_service.py", "test_make");
    assert_eq!(g.edge(test, make, Calls).0, Strategy::ImportResolved);
    assert_eq!(g.edge(test, make, Tests).0, Strategy::ImportResolved);

    let service = g.file("pkg/service.py");
    assert_eq!(g.edge(service, g.file("pkg/models.py"), Imports), (Strategy::ImportResolved, 1.0));
    assert_eq!(g.edge(service, g.file("pkg/deco.py"), Imports).0, Strategy::ImportResolved);
    assert_eq!(g.edge(g.file("pkg/__init__.py"), g.file("pkg/models.py"), Imports).0, Strategy::ImportResolved);
    assert!(out.stats.references_external >= 1, "os.path.join is external");
}

const TS: &[(&str, &str)] = &[
    (
        "src/util/math.ts",
        r#"
export function add(a: number, b: number): number {
  return a + b;
}

export class Calc {
  constructor(private base: number) {}

  sum(x: number): number {
    return add(this.base, x);
  }
}
"#,
    ),
    ("src/util/index.ts", "export * from './math';\nexport { add as plus } from './math';\n"),
    (
        "src/Logger.ts",
        r#"
export default class Logger {
  log(msg: string): void {
    console.log(msg);
  }
}
"#,
    ),
    (
        "src/base.ts",
        r#"
export abstract class Base {
  protected baseOnly(): number {
    return 1;
  }
}

export interface Thing {
  run(): void;
}
"#,
    ),
    (
        "src/app.ts",
        r#"
import { add, Calc, plus } from './util';
import Logger from './Logger';
import * as U from './util';
import { Base, Thing } from './base';

export class App extends Base implements Thing {
  run(): void {
    const c = new Calc(1);
    add(1, 2);
    plus(3, 4);
    U.add(5, 6);
    this.helper();
    this.baseOnly();
    new Logger().log('x');
  }

  private helper(): void {}
}
"#,
    ),
];

#[test]
fn typescript_project() {
    let out = build(TS);
    let g = G(&out.graph);
    use EdgeKind::*;
    let run = g.id("src/app.ts", "App.run");
    let app = g.id("src/app.ts", "App");
    let add = g.id("src/util/math.ts", "add");
    let calc = g.id("src/util/math.ts", "Calc");
    let ctor = g.id("src/util/math.ts", "Calc.constructor");
    let sum = g.id("src/util/math.ts", "Calc.sum");
    let logger = g.id("src/Logger.ts", "Logger");
    let helper = g.id("src/app.ts", "App.helper");
    let base = g.id("src/base.ts", "Base");
    let base_only = g.id("src/base.ts", "Base.baseOnly");
    let thing = g.id("src/base.ts", "Thing");

    assert_eq!(g.edge(run, add, Calls), (Strategy::ImportResolved, 0.95));
    assert_eq!(g.edge(run, calc, Calls), (Strategy::ImportResolved, 0.95));
    assert_eq!(g.edge(run, ctor, Calls).0, Strategy::ImportResolved);
    assert_eq!(g.edge(run, logger, Calls).0, Strategy::ImportResolved);
    assert_eq!(g.edge(run, helper, Calls), (Strategy::TypeResolved, 0.9));
    assert_eq!(g.edge(run, base_only, Calls), (Strategy::TypeResolved, 0.85));
    assert_eq!(g.edge(sum, add, Calls), (Strategy::SameModule, 0.9));
    assert_eq!(g.edge(app, base, Inherits).0, Strategy::ImportResolved);
    assert_eq!(g.edge(app, thing, Implements).0, Strategy::ImportResolved);

    let app_file = g.file("src/app.ts");
    assert_eq!(g.edge(app_file, g.file("src/util/index.ts"), Imports).0, Strategy::ImportResolved);
    assert_eq!(g.edge(g.file("src/util/index.ts"), g.file("src/util/math.ts"), Imports).0, Strategy::ImportResolved);
}

#[test]
fn real_packs_are_deterministic() {
    let mut files: Vec<(&str, &str)> = PY.iter().chain(TS).copied().collect();
    let a = serde_json::to_string(&build(&files).graph).unwrap();
    files.reverse();
    let b = serde_json::to_string(&build(&files).graph).unwrap();
    assert_eq!(a, b);
}

#[test]
fn python_star_super_and_fields() {
    let out = build(&[
        ("app/__init__.py", ""),
        (
            "app/base.py",
            "__all__ = ['Base']\n\nclass Base:\n    def __init__(self):\n        self.count = 0\n\n    def bump(self):\n        self.count += 1\n\n\nclass Hidden:\n    pass\n",
        ),
        (
            "app/child.py",
            "from .base import *\n\n\nclass Child(Base):\n    def __init__(self):\n        super().__init__()\n        self.bump()\n\n    def total(self):\n        return self.count\n",
        ),
    ]);
    let g = G(&out.graph);
    use EdgeKind::*;
    let child_init = g.id("app/child.py", "Child.__init__");
    let base_init = g.id("app/base.py", "Base.__init__");
    let bump = g.id("app/base.py", "Base.bump");
    let count = g.id("app/base.py", "Base.count");
    let total = g.id("app/child.py", "Child.total");
    assert_eq!(
        g.edge(g.id("app/child.py", "Child"), g.id("app/base.py", "Base"), Inherits).0,
        Strategy::ImportResolved
    );
    assert_eq!(g.edge(child_init, base_init, Calls), (Strategy::TypeResolved, 0.85));
    assert_eq!(g.edge(child_init, bump, Calls), (Strategy::TypeResolved, 0.85));
    assert_eq!(g.edge(total, count, References), (Strategy::TypeResolved, 0.85));
    assert_eq!(g.edge(bump, count, Writes), (Strategy::TypeResolved, 0.9));
}

#[test]
fn typescript_local_export_of_import_and_commonjs() {
    let out = build(&[
        ("src/hono.ts", "export class Hono {\n  get(path: string): this {\n    return this;\n  }\n}\n"),
        (
            "src/index.ts",
            "import { Hono } from './hono';\nexport { Hono };\nexport type { Hono as App } from './hono';\n",
        ),
        (
            "src/main.ts",
            "import { Hono } from './index';\n\nexport function main(): Hono {\n  const app = new Hono();\n  return app;\n}\n",
        ),
        ("lib/util.js", "function helper(x) {\n  return x;\n}\nmodule.exports = { helper };\n"),
        (
            "lib/use.js",
            "const { helper } = require('./util');\n\nfunction run() {\n  return helper(1);\n}\nmodule.exports = run;\n",
        ),
    ]);
    let g = G(&out.graph);
    use EdgeKind::*;
    let main = g.id("src/main.ts", "main");
    let hono = g.id("src/hono.ts", "Hono");
    assert_eq!(g.edge(main, hono, Calls), (Strategy::ImportResolved, 0.95));
    assert_eq!(g.edge(main, hono, References).0, Strategy::ImportResolved);
    let run = g.id("lib/use.js", "run");
    let helper = g.id("lib/util.js", "helper");
    assert_eq!(g.edge(run, helper, Calls), (Strategy::ImportResolved, 0.95));
    assert_eq!(g.edge(g.file("lib/use.js"), g.file("lib/util.js"), Imports).0, Strategy::ImportResolved);
}

/// `export default x` / `export { a as b }` are recorded by the TypeScript
/// pack as re-exports of the file's own names, so default and renamed
/// imports bind exactly (the default-export heuristic would pick the class
/// named after the file).
#[test]
fn typescript_self_reexports_bind_default_and_renamed_imports() {
    let out = build(&[
        (
            "src/user.ts",
            "export class User {}\nexport function helper() {}\nfunction main() {}\nexport default helper;\nexport { main as start };\n",
        ),
        ("src/app.ts", "import h, { start } from './user';\nexport function run() { h(); start(); }\n"),
        (
            "lib/express.js",
            "function createApplication() {}\nfunction other() {}\nmodule.exports = createApplication;\n",
        ),
        ("lib/index.js", "import e from './express';\nexport function boot() { e(); }\n"),
    ]);
    let g = G(&out.graph);
    let run = g.id("src/app.ts", "run");
    assert_eq!(g.edge(run, g.id("src/user.ts", "helper"), EdgeKind::Calls).0, Strategy::ImportResolved);
    assert_eq!(g.edge(run, g.id("src/user.ts", "main"), EdgeKind::Calls).0, Strategy::ImportResolved);
    let user = g.id("src/user.ts", "User");
    assert!(!out.graph.edges.iter().any(|e| e.src == run && e.dst == user), "default bound to the class");
    let boot = g.id("lib/index.js", "boot");
    assert_eq!(g.edge(boot, g.id("lib/express.js", "createApplication"), EdgeKind::Calls).0, Strategy::ImportResolved);
    // A self re-export never yields an IMPORTS self-loop.
    let f = g.file("src/user.ts");
    assert!(!out.graph.edges.iter().any(|e| e.src == f && e.dst == f));
}

/// CommonJS and object-literal patterns of the TS/JS micro-benchmark: a
/// required module that is a class or a function (its default export), the
/// methods of `module.exports = { … }` and of `export default { … }`, the
/// methods of a returned object literal typed through the factory, and
/// `super(…)` to the nearest explicit constructor.
#[test]
fn typescript_modules_as_values_and_object_literal_methods() {
    let out = build(&[
        ("lib/queue.js", "class Queue {\n  constructor() {}\n  push(x) { return x; }\n}\nmodule.exports = Queue;\n"),
        ("lib/log.js", "module.exports = function log(m) {\n  return m;\n};\n"),
        (
            "lib/service.js",
            "function helper(f) { return f; }\nmodule.exports = {\n  start() { return helper(true); },\n  stop: () => helper(false),\n};\n",
        ),
        (
            "lib/worker.js",
            "const Queue = require('./queue');\nconst log = require('./log');\nconst service = require('./service');\nfunction run(items) {\n  const q = new Queue();\n  q.push(1);\n  log('x');\n  service.stop();\n  return service.start();\n}\nmodule.exports = { run };\n",
        ),
        (
            "src/app.ts",
            "export default {\n  start(): string { return this.stop(); },\n  stop(): string { return 'bye'; },\n};\n",
        ),
        (
            "src/counter.ts",
            "export function makeCounter() {\n  const step = () => 1;\n  return {\n    next: () => step(),\n    peek() { return 0; },\n  };\n}\nexport function use(): number {\n  const c = makeCounter();\n  return c.next() + c.peek();\n}\n",
        ),
        (
            "src/main.ts",
            "import app from './app';\nexport function session(): string {\n  return app.start() + app.stop();\n}\n",
        ),
        (
            "src/model.ts",
            "export class Model {\n  constructor(readonly id: string) {}\n}\nexport class User extends Model {\n  constructor(id: string) {\n    super(id);\n  }\n}\n",
        ),
    ]);
    let g = G(&out.graph);
    use EdgeKind::Calls;
    let run = g.id("lib/worker.js", "run");
    assert_eq!(g.edge(run, g.id("lib/queue.js", "Queue"), Calls), (Strategy::ImportResolved, 0.9));
    assert_eq!(g.edge(run, g.id("lib/queue.js", "Queue.constructor"), Calls).0, Strategy::ImportResolved);
    assert_eq!(g.edge(run, g.id("lib/queue.js", "Queue.push"), Calls).0, Strategy::TypeResolved);
    assert_eq!(g.edge(run, g.id("lib/log.js", "log"), Calls).0, Strategy::ImportResolved);
    assert_eq!(g.edge(run, g.id("lib/service.js", "start"), Calls).0, Strategy::ImportResolved);
    assert_eq!(g.edge(run, g.id("lib/service.js", "stop"), Calls).0, Strategy::ImportResolved);
    let helper = g.id("lib/service.js", "helper");
    assert_eq!(g.edge(g.id("lib/service.js", "start"), helper, Calls).0, Strategy::SameModule);

    let session = g.id("src/main.ts", "session");
    let start = g.id("src/app.ts", "default.start");
    let stop = g.id("src/app.ts", "default.stop");
    assert_eq!(g.edge(session, start, Calls), (Strategy::ImportResolved, 0.85));
    assert_eq!(g.edge(session, stop, Calls).0, Strategy::ImportResolved);
    // `this` in an object-literal method is the object.
    assert_eq!(g.edge(start, stop, Calls).0, Strategy::TypeResolved);

    let use_ = g.id("src/counter.ts", "use");
    let next = g.id("src/counter.ts", "makeCounter.next");
    assert_eq!(g.edge(use_, next, Calls).0, Strategy::TypeResolved);
    assert_eq!(g.edge(use_, g.id("src/counter.ts", "makeCounter.peek"), Calls).0, Strategy::TypeResolved);
    assert_eq!(g.edge(next, g.id("src/counter.ts", "makeCounter.step"), Calls).0, Strategy::SameModule);

    let user_ctor = g.id("src/model.ts", "User.constructor");
    assert_eq!(g.edge(user_ctor, g.id("src/model.ts", "Model.constructor"), Calls).0, Strategy::TypeResolved);
}

/// Object-literal methods are properties, not names in scope: a bare call
/// inside one goes to the imported function of that name.
#[test]
fn typescript_object_literal_methods_are_not_in_scope() {
    let out = build(&[
        ("src/http.ts", "export function request(url: string): string {\n  return url;\n}\n"),
        (
            "src/api.ts",
            "import { request } from './http';\nexport function createApi() {\n  return {\n    request(url: string) { return request(url); },\n  };\n}\n",
        ),
    ]);
    let g = G(&out.graph);
    let method = g.id("src/api.ts", "createApi.request");
    let target = g.id("src/http.ts", "request");
    assert_eq!(g.edge(method, target, EdgeKind::Calls).0, Strategy::ImportResolved);
    assert!(!out.graph.edges.iter().any(|e| e.src == method && e.dst == method && e.kind == EdgeKind::Calls));
}
