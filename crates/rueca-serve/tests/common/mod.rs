//! Shared test helpers: a service store filled with small Python graphs and
//! an organization generation, a running `rueca serve` on ephemeral ports,
//! and a blocking HTTP client.

#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rueca_model::{FileEntry, Label, Language, Node, ResolvedGraph, SnapshotInfo, Span, git_blob_sha};
use rueca_segment::{ContentSource, write_segment};
use rueca_serve::{Loaded, Role, Running, ServiceConfig};
use rueca_service::{PullWhich, RefKind, Retention, ServiceStore, publish_generation, publish_snapshot, pull_repo};
use serde_json::{Value, json};

pub const TOKEN: &str = "rueca_test_all_repositories";
pub const TOKEN_NO_RULES: &str = "rueca_test_no_rules";
/// An admin token that sees every repository.
pub const TOKEN_OPS: &str = "rueca_test_ops";

struct Mem(HashMap<String, Vec<u8>>);

impl ContentSource for Mem {
    fn content(&self, file: &FileEntry) -> std::io::Result<Vec<u8>> {
        self.0.get(&file.path).cloned().ok_or_else(|| std::io::Error::other(file.path.clone()))
    }
}

/// A graph of Python files: one `File` node per file and one `Function`
/// per `def name(` line.
fn graph(repo: &str, commit: &str, files: &[(&str, &str)]) -> (ResolvedGraph, Mem) {
    let mut g = ResolvedGraph {
        snapshot: SnapshotInfo {
            repo: repo.into(),
            commit: commit.into(),
            dirty: false,
            branch: Some("main".into()),
            root: None,
            indexed_at: 1_700_000_000,
            rueca_version: "test".into(),
        },
        nodes: vec![],
        edges: vec![],
        files: vec![],
        modules: vec![],
        contracts: vec![],
    };
    let mut mem = HashMap::new();
    for (path, src) in files {
        let fid = g.files.len() as u32;
        let fnode = g.nodes.len() as u32;
        g.nodes.push(node(Label::File, path, path, fid, None));
        g.files.push(FileEntry {
            path: (*path).into(),
            blob_sha: git_blob_sha(src.as_bytes()),
            language: Some(Language::Python),
            size: src.len() as u64,
            line_count: src.lines().count() as u32,
            node: fnode,
            status: "ok".into(),
        });
        mem.insert(path.to_string(), src.as_bytes().to_vec());
        for (line, l) in src.lines().enumerate() {
            if let Some(name) = l.strip_prefix("def ").and_then(|r| r.split('(').next()) {
                let span = Span {
                    start_byte: 0,
                    end_byte: src.len() as u32,
                    start_line: line as u32,
                    start_col: 0,
                    end_line: line as u32,
                    end_col: 0,
                };
                let qn = format!("{}.{name}", path.trim_end_matches(".py").replace('/', "."));
                g.nodes.push(node(Label::Function, name, &qn, fid, Some(span)));
            }
        }
    }
    g.validate().unwrap();
    (g, Mem(mem))
}

fn node(label: Label, name: &str, qn: &str, file: u32, span: Option<Span>) -> Node {
    Node {
        label,
        name: name.rsplit('/').next().unwrap().into(),
        qualified_name: qn.into(),
        file: Some(file),
        span,
        language: Some(Language::Python),
        signature: None,
        doc: None,
        exported: true,
        is_test: false,
    }
}

/// An indexer and an updater publishing to one service store.
pub struct Publisher {
    pub url: String,
    indexer_dir: tempfile::TempDir,
    pub builder_dir: tempfile::TempDir,
    commits: usize,
}

impl Publisher {
    pub fn new(url: &str) -> Publisher {
        Publisher {
            url: url.to_string(),
            indexer_dir: tempfile::tempdir().unwrap(),
            builder_dir: tempfile::tempdir().unwrap(),
            commits: 0,
        }
    }

    fn open(&self, dir: &Path) -> ServiceStore {
        ServiceStore::open(&self.url, &BTreeMap::new(), dir, "acme").unwrap()
    }

    /// Publishes a snapshot of `repo` with `files` on `main`.
    pub fn snapshot(&mut self, repo: &str, files: &[(&str, &str)]) {
        self.commits += 1;
        let (g, mem) = graph(repo, &format!("{:040x}", self.commits), files);
        let meta = write_segment(&g, &mem, self.indexer_dir.path()).unwrap();
        let indexer = self.open(self.indexer_dir.path());
        publish_snapshot(&indexer, repo, "main", RefKind::Default, &meta.id, &Retention::default()).unwrap();
    }

    /// Builds and publishes the next generation from `repos`' default refs.
    pub fn generation(&self, repos: &[&str]) -> u64 {
        let builder = self.open(self.builder_dir.path());
        for repo in repos {
            pull_repo(&builder, repo, PullWhich::Default).unwrap();
        }
        rueca_segment::org::update_org(&builder.local, "acme", &builder.mirror_members().unwrap()).unwrap();
        publish_generation(&builder).unwrap().generation
    }

    /// An engine over the builder's mirror (the "local" side of the
    /// differential tests).
    pub fn local_engine(&self) -> rueca_query::Engine {
        let dir = std::fs::canonicalize(self.builder_dir.path()).unwrap();
        rueca_query::Engine::for_org(dir, "acme").unwrap()
    }
}

pub const BILLING: (&str, &str) = (
    "billing/invoices.py",
    "def issue_invoice(order):\n    return order\n\ndef void_invoice(invoice):\n    return None\n",
);
pub const CORE: (&str, &str) = ("core/orders.py", "def create_order(cart):\n    return cart\n");
pub const SECRET: (&str, &str) = ("secret/plans.py", "def invoice_plan_secret(x):\n    return x\n");

/// A store with three repositories and generation 1.
pub fn populated(url: &str) -> Publisher {
    let mut p = Publisher::new(url);
    p.snapshot("github.com/acme/billing", &[BILLING]);
    p.snapshot("github.com/acme/core", &[CORE]);
    p.snapshot("github.com/hidden/secret", &[SECRET]);
    assert_eq!(p.generation(&["github.com/acme/billing", "github.com/acme/core", "github.com/hidden/secret"]), 1);
    p
}

/// A configuration: `memory://` store `url`, ephemeral ports, a fast
/// watcher, three static tokens, plus `extra` TOML appended.
pub fn config(url: &str, cache: &Path, extra: &str) -> Loaded {
    config_as(url, cache, extra, Role::Query)
}

/// [`config`] for `role`.
pub fn config_as(url: &str, cache: &Path, extra: &str, role: Role) -> Loaded {
    let text = format!(
        r#"
[store]
url = "{url}"
cache_dir = "{cache}"
poll = "100ms"

[http]
listen = "127.0.0.1:0"
ops_listen = "127.0.0.1:0"
{http}

[auth]
admins = ["token:all", "token:ops"]

[[auth.token]]
name = "all"
hash = "{all}"
repos = ["github.com/acme/*"]

[[auth.token]]
name = "none"
hash = "{none}"

[[auth.token]]
name = "ops"
hash = "{ops}"
repos = ["**"]

[telemetry]
log_format = "pretty"
{rest}
"#,
        cache = cache.display(),
        http = extra_http(extra),
        all = rueca_serve::auth::hash_token(TOKEN),
        none = rueca_serve::auth::hash_token(TOKEN_NO_RULES),
        ops = rueca_serve::auth::hash_token(TOKEN_OPS),
        rest = extra_rest(extra),
    );
    let mut config = ServiceConfig::parse(&text).unwrap();
    let problems = config.validate();
    assert!(problems.is_empty(), "{problems:?}");
    let _ = config.resolve_secrets(&|_| None);
    Loaded { config, role, path: PathBuf::from("test.toml") }
}

/// `extra` lines before the first table header go into `[http]`.
fn extra_http(extra: &str) -> String {
    extra.split("\n[").next().unwrap_or("").to_string()
}

fn extra_rest(extra: &str) -> String {
    match extra.split_once("\n[") {
        Some((_, rest)) => format!("\n[{rest}"),
        None => String::new(),
    }
}

/// A running service on its own runtime.
pub struct Server {
    rt: Option<tokio::runtime::Runtime>,
    running: Option<Running>,
    pub base: String,
    pub ops: String,
    pub cache: tempfile::TempDir,
}

impl Server {
    pub fn start(url: &str, extra: &str) -> Server {
        Server::start_as(url, extra, Role::Query)
    }

    /// A server of `role`.
    pub fn start_as(url: &str, extra: &str, role: Role) -> Server {
        let cache = tempfile::tempdir().unwrap();
        let loaded = config_as(url, cache.path(), extra, role);
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let running = rt.block_on(rueca_serve::start(loaded)).unwrap();
        Server {
            base: format!("http://{}", running.addr),
            ops: format!("http://{}", running.ops_addr),
            rt: Some(rt),
            running: Some(running),
            cache,
        }
    }

    pub fn state(&self) -> &rueca_serve::AppState {
        &self.running.as_ref().unwrap().state
    }

    /// Waits until `/readyz` answers 200 (at most 30 s).
    pub fn wait_ready(&self) -> Value {
        let started = Instant::now();
        loop {
            let r = get(&format!("{}/readyz", self.ops), &[]);
            if r.status == 200 {
                return r.json();
            }
            assert!(started.elapsed() < Duration::from_secs(30), "not ready: {}", r.body);
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// `POST /mcp` with a bearer token and extra headers.
    pub fn mcp(&self, token: Option<&str>, body: &Value, headers: &[(&str, &str)]) -> Reply {
        let mut h: Vec<(String, String)> = headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        if let Some(t) = token {
            h.push(("authorization".into(), format!("Bearer {t}")));
        }
        let h: Vec<(&str, &str)> = h.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        post(&format!("{}/mcp", self.base), &body.to_string(), &h)
    }

    /// A legacy `tools/call` (no `_meta`) with [`TOKEN`]-like auth.
    pub fn call(&self, token: &str, tool: &str, args: Value) -> Reply {
        let body =
            json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": {"name": tool, "arguments": args}});
        self.mcp(Some(token), &body, &[("mcp-protocol-version", "2025-06-18")])
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let (Some(rt), Some(running)) = (self.rt.take(), self.running.take()) {
            rt.block_on(running.shutdown(Duration::from_secs(2)));
            rt.shutdown_timeout(Duration::from_secs(2));
        }
    }
}

/// An HTTP response.
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Reply {
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("{e}: {}", self.body))
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder().http_status_as_error(false).build().new_agent()
}

fn reply(r: Result<http::Response<ureq::Body>, ureq::Error>) -> Reply {
    let mut r = r.unwrap();
    let headers =
        r.headers().iter().map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string())).collect();
    Reply { status: r.status().as_u16(), headers, body: r.body_mut().read_to_string().unwrap_or_default() }
}

pub fn get(url: &str, headers: &[(&str, &str)]) -> Reply {
    let mut req = agent().get(url);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    reply(req.call())
}

/// `DELETE` with a JSON body.
pub fn delete(url: &str, body: &str, headers: &[(&str, &str)]) -> Reply {
    let mut req = agent().delete(url).header("content-type", "application/json");
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    reply(req.force_send_body().send(body))
}

/// A local code host: a repository committed in `src` and cloned bare to
/// `<name>.git`, served as a `file://` URL.
pub struct GitHost {
    pub dir: tempfile::TempDir,
    pub bare: PathBuf,
}

impl GitHost {
    pub fn new(name: &str, files: &[(&str, &str)]) -> GitHost {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        for (path, text) in files {
            let p = src.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, text).unwrap();
        }
        GitHost::git(&src, &["init", "--quiet", "-b", "main"]);
        GitHost::git(&src, &["add", "-A"]);
        GitHost::git(&src, &["commit", "--quiet", "-m", "first"]);
        GitHost::git(&src, &["tag", "v1"]);
        GitHost::git(dir.path(), &["clone", "--quiet", "--bare", "src", &format!("{name}.git")]);
        let bare = dir.path().join(format!("{name}.git"));
        GitHost { dir, bare }
    }

    pub fn url(&self) -> String {
        format!("file://{}", self.bare.display())
    }

    fn git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "tag.gpgsign=false",
            ])
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }
}

pub fn post(url: &str, body: &str, headers: &[(&str, &str)]) -> Reply {
    let mut req = agent().post(url).header("content-type", "application/json");
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    reply(req.send(body))
}

/// The `main` of a test binary without the libtest harness: runs `tests`
/// in order (those whose name contains a non-flag argument, when any is
/// given), each under `catch_unwind`, and fails when one panicked.
pub fn run_tests(tests: &[(&str, fn())]) -> std::process::ExitCode {
    let filters: Vec<String> = std::env::args().skip(1).filter(|a| !a.starts_with('-')).collect();
    let selected: Vec<&(&str, fn())> = tests
        .iter()
        .filter(|(name, _)| filters.is_empty() || filters.iter().any(|f| name.contains(f.as_str())))
        .collect();
    println!("\nrunning {} tests", selected.len());
    let mut failed = Vec::new();
    for (name, test) in selected {
        let started = Instant::now();
        let ok = std::panic::catch_unwind(*test).is_ok();
        println!("test {name} ... {} ({:.1?})", if ok { "ok" } else { "FAILED" }, started.elapsed());
        if !ok {
            failed.push(*name);
        }
    }
    println!(
        "\ntest result: {}. {} failed: {failed:?}\n",
        if failed.is_empty() { "ok" } else { "FAILED" },
        failed.len()
    );
    if failed.is_empty() { std::process::ExitCode::SUCCESS } else { std::process::ExitCode::FAILURE }
}
