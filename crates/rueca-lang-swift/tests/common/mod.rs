//! Shared harness: extract a directory with the real `SwiftPack` and
//! resolve it with `rueca-resolve`.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use rueca_extract::{Budget, ProjectFiles, Registry};
use rueca_lang_swift::SwiftPack;
use rueca_model::{EdgeKind, EdgeMeta, Language, NodeId, ResolvedGraph, SnapshotInfo, Strategy, git_blob_sha};
use rueca_resolve::{FileInput, ResolveOutput, resolve};

pub fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/spm")
}

/// Files under `dir` as (repository-relative path, bytes), sorted; hidden
/// directories and build output are skipped.
pub fn walk(dir: &Path, root: &Path, out: &mut Vec<(String, Vec<u8>)>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if p.is_dir() {
            if !name.starts_with('.') && name != "build" {
                walk(&p, root, out);
            }
        } else {
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            out.push((rel, std::fs::read(&p).unwrap()));
        }
    }
}

pub fn registry() -> Registry {
    let mut r = Registry::new();
    r.register(Box::new(SwiftPack));
    r
}

pub fn project(files: &[(String, Vec<u8>)]) -> ProjectFiles {
    let mut project = ProjectFiles { paths: files.iter().map(|(p, _)| p.clone()).collect(), ..Default::default() };
    for (p, src) in files {
        if ProjectFiles::is_config_file(p) {
            project.configs.insert(p.clone(), String::from_utf8_lossy(src).into_owned());
        }
    }
    project
}

pub fn build(files: Vec<(String, Vec<u8>)>) -> ResolveOutput {
    let registry = registry();
    let project = project(&files);
    let inputs: Vec<FileInput> = files
        .iter()
        .map(|(path, src)| {
            let facts = registry
                .for_path(path)
                .map(|(lang, pack)| pack.extract(path, lang, src, &Budget::default()).expect("extract"));
            FileInput {
                path: path.clone(),
                blob_sha: git_blob_sha(src),
                language: Language::from_path(path).filter(|_| facts.is_some()),
                size: src.len() as u64,
                status: if facts.is_some() { "ok".into() } else { "skipped:unsupported".into() },
                facts,
            }
        })
        .collect();
    let snapshot = SnapshotInfo {
        repo: "local/fixture".into(),
        commit: "c".into(),
        dirty: false,
        branch: None,
        root: None,
        indexed_at: 0,
        rueca_version: "test".into(),
    };
    let out = resolve(snapshot, inputs, &project, &registry);
    out.graph.validate().expect("valid graph");
    out
}

pub fn fixture() -> ResolveOutput {
    let root = fixture_root();
    let mut files = Vec::new();
    walk(&root, &root, &mut files);
    build(files)
}

/// Lookups over a resolved graph.
pub struct G<'a>(pub &'a ResolvedGraph);

impl G<'_> {
    pub fn id(&self, qn: &str) -> NodeId {
        let hits: Vec<NodeId> = (0..self.0.nodes.len() as NodeId)
            .filter(|&i| {
                let n = &self.0.nodes[i as usize];
                n.qualified_name == qn
                    || self
                        .0
                        .files
                        .get(n.file.unwrap_or(u32::MAX) as usize)
                        .is_some_and(|f| f.path == qn && n.label == rueca_model::Label::File)
            })
            .collect();
        assert!(
            !hits.is_empty(),
            "no node {qn}; nodes: {:#?}",
            self.0.nodes.iter().map(|n| &n.qualified_name).collect::<Vec<_>>()
        );
        hits[0]
    }

    pub fn edge(&self, src: &str, dst: &str, kind: EdgeKind) -> Option<EdgeMeta> {
        let (s, d) = (self.id(src), self.id(dst));
        self.0.edges.iter().find(|e| e.src == s && e.dst == d && e.kind == kind).map(|e| e.meta)
    }

    pub fn has(&self, src: &str, dst: &str, kind: EdgeKind, strategy: Strategy, confidence: f32) {
        let m =
            self.edge(src, dst, kind).unwrap_or_else(|| panic!("missing {src} -{kind:?}-> {dst}\n{}", self.dump(src)));
        assert_eq!((m.strategy, m.confidence), (strategy, confidence), "{src} -> {dst}");
    }

    pub fn has_any(&self, src: &str, dst: &str, kind: EdgeKind) {
        assert!(self.edge(src, dst, kind).is_some(), "missing {src} -{kind:?}-> {dst}\n{}", self.dump(src));
    }

    pub fn lacks(&self, src: &str, dst: &str, kind: EdgeKind) {
        assert!(self.edge(src, dst, kind).is_none(), "unexpected {src} -{kind:?}-> {dst}");
    }

    pub fn dump(&self, src: &str) -> String {
        let s = self.id(src);
        self.0
            .edges
            .iter()
            .filter(|e| e.src == s)
            .map(|e| {
                format!(
                    "  {:?} {} {:?} {}",
                    e.kind, self.0.nodes[e.dst as usize].qualified_name, e.meta.strategy, e.meta.confidence
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn module_of(&self, path: &str) -> &str {
        let i = self.0.files.iter().position(|f| f.path == path).expect("file");
        self.0.modules[i].as_str()
    }
}
