//! Hand-built fixture: a small Python + TypeScript snapshot written as a real
//! segment into a temporary store.

#![allow(dead_code)]

use std::collections::HashMap;
use std::path::PathBuf;

use rueca_model::*;
use rueca_segment::{ContentSource, SegmentMeta, Store};

pub const PY: &str = "import os

class Base:
    def run(self):
        return 1

class InvoiceService(Base):
    \"\"\"Issues invoices.\"\"\"
    def issue(self, invoice):
        helper()
        return self.run()

def helper():
    return os.getcwd()

def test_issue():
    InvoiceService().issue(None)
";

/// CRLF line endings on purpose.
pub const TS: &str = "export function formatAmount(n: number): string {\r\n  return n.toFixed(2);\r\n}\r\nexport class Session {\r\n  request() { return formatAmount(1); }\r\n}\r\nfunction helper() {}\r\n";

pub struct Sources(pub HashMap<String, Vec<u8>>);

impl ContentSource for Sources {
    fn content(&self, file: &FileEntry) -> std::io::Result<Vec<u8>> {
        self.0
            .get(&file.path)
            .cloned()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, file.path.clone()))
    }
}

/// Span covering 0-based lines `l0..=l1` of `text` (end excludes the newline).
pub fn span(text: &str, l0: u32, l1: u32) -> Span {
    let mut starts = vec![0usize];
    for (i, b) in text.bytes().enumerate() {
        if b == b'\n' {
            starts.push(i + 1);
        }
    }
    let start = starts[l0 as usize];
    let mut end = starts.get(l1 as usize + 1).map(|s| s - 1).unwrap_or(text.len());
    if text.as_bytes().get(end.wrapping_sub(1)) == Some(&b'\r') {
        end -= 1;
    }
    let end_col = end - starts[l1 as usize];
    Span {
        start_byte: start as u32,
        end_byte: end as u32,
        start_line: l0,
        start_col: 0,
        end_line: l1,
        end_col: end_col as u32,
    }
}

pub fn line_span(text: &str, l0: u32) -> Span {
    span(text, l0, l0)
}

pub struct Builder {
    pub graph: ResolvedGraph,
    pub sources: HashMap<String, Vec<u8>>,
}

impl Builder {
    pub fn new(repo: &str, commit: &str, root: &str) -> Builder {
        Builder {
            graph: ResolvedGraph {
                snapshot: SnapshotInfo {
                    repo: repo.into(),
                    commit: commit.into(),
                    dirty: false,
                    root: Some(root.into()),
                    indexed_at: 1_700_000_000,
                    rueca_version: "test".into(),
                },
                nodes: vec![],
                edges: vec![],
                files: vec![],
                modules: vec![],
            },
            sources: HashMap::new(),
        }
    }

    pub fn file(&mut self, path: &str, text: &str) -> (FileId, NodeId) {
        let fid = self.graph.files.len() as FileId;
        let nid = self.graph.nodes.len() as NodeId;
        let language = Language::from_path(path);
        let line_count = text.lines().count() as u32;
        self.graph.nodes.push(Node {
            label: Label::File,
            name: path.rsplit('/').next().unwrap().into(),
            qualified_name: path.into(),
            file: Some(fid),
            span: None,
            language,
            signature: None,
            doc: None,
            exported: false,
            is_test: false,
        });
        self.graph.files.push(FileEntry {
            path: path.into(),
            blob_sha: git_blob_sha(text.as_bytes()),
            language,
            size: text.len() as u64,
            line_count,
            node: nid,
            status: "ok".into(),
        });
        self.sources.insert(path.into(), text.as_bytes().to_vec());
        (fid, nid)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn sym(
        &mut self,
        parent: NodeId,
        file: FileId,
        label: Label,
        name: &str,
        qn: &str,
        span: Span,
        signature: Option<&str>,
        exported: bool,
        is_test: bool,
    ) -> NodeId {
        let id = self.graph.nodes.len() as NodeId;
        let language = self.graph.files[file as usize].language;
        self.graph.nodes.push(Node {
            label,
            name: name.into(),
            qualified_name: qn.into(),
            file: Some(file),
            span: Some(span),
            language,
            signature: signature.map(Into::into),
            doc: None,
            exported,
            is_test,
        });
        self.edge(parent, id, EdgeKind::Defines, 1.0, Strategy::Syntactic, None);
        id
    }

    pub fn edge(
        &mut self,
        src: NodeId,
        dst: NodeId,
        kind: EdgeKind,
        confidence: f32,
        strategy: Strategy,
        evidence: Option<(FileId, Span)>,
    ) {
        self.graph.edges.push(Edge { src, dst, kind, meta: EdgeMeta { confidence, strategy, evidence } });
    }

    /// Writes the segment into `store` and registers it for `root`.
    pub fn write(&self, store: &std::path::Path, root: &str) -> SegmentMeta {
        self.graph.validate().expect("valid graph");
        let st = Store::open(store).expect("open store");
        let meta = rueca_segment::write_segment(&self.graph, &Sources(self.sources.clone()), store).expect("write");
        st.register(root, &meta).expect("register");
        meta
    }
}

/// Node ids of the standard fixture.
pub struct Ids {
    pub base: NodeId,
    pub run: NodeId,
    pub invoice_service: NodeId,
    pub issue: NodeId,
    pub helper: NodeId,
    pub test_issue: NodeId,
    pub format_amount: NodeId,
    pub session: NodeId,
    pub request: NodeId,
    pub ts_helper: NodeId,
}

pub fn demo(repo: &str, root: &str) -> (Builder, Ids) {
    let mut b = Builder::new(repo, "abc123", root);
    let (pf, pn) = b.file("pkg/service.py", PY);
    let (tf, tn) = b.file("src/util/index.ts", TS);
    let base =
        b.sym(pn, pf, Label::Class, "Base", "pkg.service.Base", span(PY, 2, 4), Some("class Base:"), true, false);
    let run = b.sym(
        base,
        pf,
        Label::Method,
        "run",
        "pkg.service.Base.run",
        span(PY, 3, 4),
        Some("def run(self)"),
        true,
        false,
    );
    let invoice_service = b.sym(
        pn,
        pf,
        Label::Class,
        "InvoiceService",
        "pkg.service.InvoiceService",
        span(PY, 6, 10),
        Some("class InvoiceService(Base):"),
        true,
        false,
    );
    b.graph.nodes[invoice_service as usize].doc = Some("Issues invoices.".into());
    let issue = b.sym(
        invoice_service,
        pf,
        Label::Method,
        "issue",
        "pkg.service.InvoiceService.issue",
        span(PY, 8, 10),
        Some("def issue(self, invoice)"),
        true,
        false,
    );
    let helper = b.sym(
        pn,
        pf,
        Label::Function,
        "helper",
        "pkg.service.helper",
        span(PY, 12, 13),
        Some("def helper()"),
        true,
        false,
    );
    let test_issue = b.sym(
        pn,
        pf,
        Label::Function,
        "test_issue",
        "pkg.service.test_issue",
        span(PY, 15, 16),
        Some("def test_issue()"),
        true,
        true,
    );
    let format_amount = b.sym(
        tn,
        tf,
        Label::Function,
        "formatAmount",
        "src/util/index:formatAmount",
        span(TS, 0, 2),
        Some("export function formatAmount(n: number): string"),
        true,
        false,
    );
    let session = b.sym(
        tn,
        tf,
        Label::Class,
        "Session",
        "src/util/index:Session",
        span(TS, 3, 5),
        Some("export class Session"),
        true,
        false,
    );
    let request = b.sym(
        session,
        tf,
        Label::Method,
        "request",
        "src/util/index:Session.request",
        span(TS, 4, 4),
        Some("request()"),
        true,
        false,
    );
    let ts_helper = b.sym(
        tn,
        tf,
        Label::Function,
        "helper",
        "src/util/index:helper",
        span(TS, 6, 6),
        Some("function helper()"),
        false,
        false,
    );
    b.edge(invoice_service, base, EdgeKind::Inherits, 0.95, Strategy::SameModule, Some((pf, line_span(PY, 6))));
    b.edge(issue, helper, EdgeKind::Calls, 0.95, Strategy::SameModule, Some((pf, line_span(PY, 9))));
    b.edge(issue, run, EdgeKind::Calls, 0.6, Strategy::Heuristic, Some((pf, line_span(PY, 10))));
    b.edge(test_issue, invoice_service, EdgeKind::Calls, 0.9, Strategy::SameModule, Some((pf, line_span(PY, 16))));
    b.edge(test_issue, issue, EdgeKind::Calls, 0.5, Strategy::UniqueName, Some((pf, line_span(PY, 16))));
    b.edge(test_issue, issue, EdgeKind::Tests, 0.5, Strategy::UniqueName, Some((pf, line_span(PY, 16))));
    b.edge(request, format_amount, EdgeKind::Calls, 0.95, Strategy::SameModule, Some((tf, line_span(TS, 4))));
    let ids = Ids { base, run, invoice_service, issue, helper, test_issue, format_amount, session, request, ts_helper };
    (b, ids)
}

/// A temporary store with the demo snapshot registered for a temporary root.
pub struct Fixture {
    pub _dir: tempfile::TempDir,
    pub store: PathBuf,
    pub root: PathBuf,
    pub ids: Ids,
}

pub fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store");
    let root = dir.path().join("demo");
    std::fs::create_dir_all(&root).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    let (b, ids) = demo("local/demo", root.to_str().unwrap());
    b.write(&store, root.to_str().unwrap());
    Fixture { _dir: dir, store, root, ids }
}
