//! End-to-end resolution with the real Python and TypeScript packs on small
//! inline multi-file projects.

use telar_extract::{Budget, ProjectFiles, Registry};
use telar_lang_python::PythonPack;
use telar_lang_typescript::TypeScriptPack;
use telar_model::{EdgeKind, Language, NodeId, ResolvedGraph, SnapshotInfo, Strategy, git_blob_sha};
use telar_resolve::{FileInput, ResolveOutput, resolve};

fn registry() -> Registry {
    let mut r = Registry::new();
    r.register(Box::new(PythonPack));
    r.register(Box::new(TypeScriptPack));
    r
}

fn build(files: &[(&str, &str)]) -> ResolveOutput {
    let registry = registry();
    let project = ProjectFiles { paths: files.iter().map(|(p, _)| p.to_string()).collect(), configs: Default::default() };
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
        root: None,
        indexed_at: 0,
        telar_version: "test".into(),
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
                    && n.label != telar_model::Label::File
                    && (n.qualified_name.ends_with(&format!(".{local}")) || n.qualified_name.ends_with(&format!(":{local}")))
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
        ("app/base.py", "__all__ = ['Base']\n\nclass Base:\n    def __init__(self):\n        self.count = 0\n\n    def bump(self):\n        self.count += 1\n\n\nclass Hidden:\n    pass\n"),
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
    assert_eq!(g.edge(g.id("app/child.py", "Child"), g.id("app/base.py", "Base"), Inherits).0, Strategy::ImportResolved);
    assert_eq!(g.edge(child_init, base_init, Calls), (Strategy::TypeResolved, 0.85));
    assert_eq!(g.edge(child_init, bump, Calls), (Strategy::TypeResolved, 0.85));
    assert_eq!(g.edge(total, count, References), (Strategy::TypeResolved, 0.85));
    assert_eq!(g.edge(bump, count, Writes), (Strategy::TypeResolved, 0.9));
}

#[test]
fn typescript_local_export_of_import_and_commonjs() {
    let out = build(&[
        ("src/hono.ts", "export class Hono {\n  get(path: string): this {\n    return this;\n  }\n}\n"),
        ("src/index.ts", "import { Hono } from './hono';\nexport { Hono };\nexport type { Hono as App } from './hono';\n"),
        ("src/main.ts", "import { Hono } from './index';\n\nexport function main(): Hono {\n  const app = new Hono();\n  return app;\n}\n"),
        ("lib/util.js", "function helper(x) {\n  return x;\n}\nmodule.exports = { helper };\n"),
        ("lib/use.js", "const { helper } = require('./util');\n\nfunction run() {\n  return helper(1);\n}\nmodule.exports = run;\n"),
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
