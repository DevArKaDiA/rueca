//! Shared helpers: a small on-disk project and a `telar` command builder.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use serde_json::Value;

pub const MODELS_PY: &str = "class Base:
    def save(self):
        return validate(self)


def validate(obj):
    return obj is not None
";

pub const SERVICE_PY: &str = "\"\"\"Billing service.\"\"\"
from pkg.models import Base, validate


class InvoiceService(Base):
    \"\"\"Issues invoices.\"\"\"

    def issue(self, invoice):
        if validate(invoice):
            return self.save()
        raise ValueError(\"invalid invoice\")


def main():
    svc = InvoiceService()
    svc.issue({\"id\": 1})
";

pub const UTIL_TS: &str = "export function formatAmount(n: number): string {
  return n.toFixed(2);
}

export class Session {
  request(path: string): string {
    return formatAmount(path.length);
  }
}
";

pub const APP_TS: &str = "import { Session, formatAmount } from \"./util\";

export function run(): string {
  const s = new Session();
  return s.request(\"/x\") + formatAmount(2);
}
";

pub fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

/// A temp dir with `proj/` (the working tree) and `store/`.
pub struct Project {
    pub dir: tempfile::TempDir,
    pub root: PathBuf,
    pub store: PathBuf,
}

pub fn python_project() -> Project {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    write(&root, "pkg/__init__.py", "");
    write(&root, "pkg/models.py", MODELS_PY);
    write(&root, "pkg/service.py", SERVICE_PY);
    write(&root, "README.md", "# demo\n");
    let root = std::fs::canonicalize(root).unwrap();
    let store = dir.path().join("store");
    Project { dir, root, store }
}

pub fn add_typescript(p: &Project) {
    write(&p.root, "web/src/util.ts", UTIL_TS);
    write(&p.root, "web/src/app.ts", APP_TS);
}

pub fn telar(store: &Path) -> Command {
    let mut c = Command::cargo_bin("telar").unwrap();
    c.arg("--store").arg(store).env_remove("TELAR_LOG").env_remove("TELAR_HOME");
    c
}

pub fn stdout_json(out: &std::process::Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "{e}: stdout={} stderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

/// `telar index <root> --json`, asserting success.
pub fn index(p: &Project) -> Value {
    let out = telar(&p.store).arg("index").arg(&p.root).arg("--json").output().unwrap();
    assert!(out.status.success(), "index failed: {}", String::from_utf8_lossy(&out.stderr));
    stdout_json(&out)
}

/// `telar query <tool> <args> --root <root>`, asserting success.
pub fn query(p: &Project, tool: &str, args: Value) -> Value {
    let out = telar(&p.store).arg("--root").arg(&p.root).args(["query", tool, &args.to_string()]).output().unwrap();
    assert!(
        out.status.success(),
        "{tool} {args}: {} {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    stdout_json(&out)
}

/// Qualified names of a list of briefs.
pub fn qns(v: &Value) -> Vec<String> {
    v.as_array().unwrap().iter().map(|b| b["qualified_name"].as_str().unwrap().to_string()).collect()
}
