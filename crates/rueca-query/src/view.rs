//! A [`View`] is one opened segment (one snapshot) plus the helpers every
//! tool needs: brief symbols, stable ids, symbol lookup and path resolution.

use std::sync::Arc;

use rueca_model::{ENV_PATH, EdgeKind, FileId, Label, LanguageFamily, Node, NodeId, Span, env_var_qualified_name};
use rueca_segment::{CatalogEntry, Segment};

use crate::snapshot::SnapshotDesc;
use crate::types::Brief;

/// Maximum number of alternative matches returned as `candidates`.
pub const MAX_CANDIDATES: usize = 20;

pub struct View {
    pub seg: Arc<Segment>,
    pub entry: CatalogEntry,
    /// What the envelope says about this snapshot.
    pub snap: SnapshotDesc,
    /// `commit12[+]`, cached for ids.
    id_commit: String,
}

/// How a tool names a symbol.
#[derive(Debug, Clone, Default)]
pub struct Selector<'a> {
    pub id: Option<&'a str>,
    pub qualified_name: Option<&'a str>,
    pub name: Option<&'a str>,
    pub path: Option<&'a str>,
}

impl Selector<'_> {
    pub fn is_empty(&self) -> bool {
        self.id.is_none() && self.qualified_name.is_none() && self.name.is_none()
    }
}

/// Result of a symbol lookup: the chosen node and the other matches.
#[derive(Debug, Clone)]
pub struct Lookup {
    pub node: NodeId,
    pub candidates: Vec<NodeId>,
}

/// Labels that are containers rather than symbols.
pub fn is_container(label: Label) -> bool {
    matches!(label, Label::Repository | Label::Folder | Label::File | Label::Module | Label::Package)
}

impl View {
    pub fn new(seg: Arc<Segment>, entry: CatalogEntry) -> View {
        let snap = SnapshotDesc::from_info(seg.snapshot());
        let id_commit = snap.id_commit();
        View { seg, entry, snap, id_commit }
    }

    pub fn repo(&self) -> &str {
        &self.seg.snapshot().repo
    }

    pub fn node(&self, id: NodeId) -> &Node {
        self.seg.node(id)
    }

    pub fn node_path(&self, id: NodeId) -> Option<&str> {
        self.node(id).file.map(|f| self.seg.file(f).path.as_str())
    }

    /// The node's name qualified within its module (`Class.method`), derived by
    /// walking `DEFINES` parents up to the file. `None` for containers.
    pub fn local_qualified_name(&self, id: NodeId) -> Option<String> {
        let node = self.node(id);
        if is_container(node.label) {
            return None;
        }
        // Canonical env vars (no file): `$env:NAME` → `NAME`.
        if node.file.is_none()
            && let Some(name) = env_var_name(node)
        {
            return Some(name.to_string());
        }
        let family = node.language.map(|l| l.family());
        // PHP names carry their namespace and use `::` for members: the
        // global qualified name is the local one (`App\Models\User::save`).
        if family == Some(LanguageFamily::Php) {
            return Some(node.qualified_name.clone());
        }
        // RFC 0003 families: the global name is module + separator + local
        // name, with the module recorded per file (Go import paths and Java
        // packages contain dots, data key paths and overload suffixes do not
        // follow `DEFINES` names).
        if let Some(family @ (LanguageFamily::Data | LanguageFamily::Go | LanguageFamily::C | LanguageFamily::Java)) =
            family
            && let Some(local) = self.module_local_name(node, family)
        {
            return Some(local);
        }
        let mut parts = vec![node.name.as_str()];
        let mut cur = id;
        for _ in 0..256 {
            let parents = self.seg.in_edges(cur, Some(EdgeKind::Defines));
            let Some(p) = parents.iter().find(|e| e.other != cur) else { break };
            let pn = self.node(p.other);
            if is_container(pn.label) {
                break;
            }
            parts.push(pn.name.as_str());
            cur = p.other;
        }
        parts.reverse();
        let chain = parts.join(".");
        // Without DEFINES edges the chain is just the name; outside Python the
        // global name carries the local part after the first module separator.
        let qn = &node.qualified_name;
        let family = node.language.map(|l| l.family());
        let sep = family.map_or(".", LanguageFamily::module_separator);
        let consistent = qn == &chain || qn.ends_with(&format!(".{chain}")) || qn.ends_with(&format!("{sep}{chain}"));
        if !consistent
            && family.is_some_and(|f| f != LanguageFamily::Python)
            && let Some((_, local)) = qn.split_once(sep)
        {
            return Some(local.to_string());
        }
        Some(chain)
    }

    /// `qualified_name` minus `<module><separator>`, with the module from
    /// the segment's `files.modules` (data files: the path; C/C++: none).
    fn module_local_name(&self, node: &Node, family: LanguageFamily) -> Option<String> {
        let file = node.file?;
        let module = match self.seg.module_of(file) {
            Some(m) => m.to_string(),
            None => match family {
                LanguageFamily::Data => self.seg.file(file).path.clone(),
                LanguageFamily::C => String::new(),
                _ => return None,
            },
        };
        if module.is_empty() {
            return Some(node.qualified_name.clone());
        }
        let rest = node.qualified_name.strip_prefix(module.as_str())?;
        rest.strip_prefix(family.module_separator()).map(str::to_string)
    }

    /// Pinned symbol id of a node: `repo@commit12[+]:path#local-name`
    /// (RFC 0005 §3). Canonical env vars use the pseudo-path `$env`
    /// (`repo@commit:$env#DATABASE_URL`).
    pub fn symbol_id(&self, id: NodeId) -> String {
        let node = self.node(id);
        let path = match self.node_path(id) {
            Some(p) => p,
            None if env_var_name(node).is_some() => ENV_PATH,
            None => node.qualified_name.as_str(),
        };
        let local = self.local_qualified_name(id);
        rueca_model::symbol_id(self.repo(), &self.id_commit, path, local.as_deref())
    }

    pub fn span_lines(&self, id: NodeId) -> (Option<u32>, Option<u32>) {
        let node = self.node(id);
        match node.span {
            Some(span) => {
                let (s, e) = span.lines_1based();
                (Some(s), Some(e.max(s)))
            }
            None if node.label == Label::File => match node.file {
                Some(f) => (Some(1), Some(self.seg.file(f).line_count.max(1))),
                None => (None, None),
            },
            None => (None, None),
        }
    }

    pub fn brief(&self, id: NodeId) -> Brief {
        let node = self.node(id);
        let (start_line, end_line) = self.span_lines(id);
        let path = match node.file {
            Some(f) => Some(self.seg.file(f).path.clone()),
            None if node.label == Label::Folder => Some(node.qualified_name.clone()),
            None => None,
        };
        Brief {
            id: self.symbol_id(id),
            name: node.name.clone(),
            qualified_name: node.qualified_name.clone(),
            label: node.label.as_str().to_string(),
            path,
            start_line,
            end_line,
        }
    }

    pub fn briefs(&self, ids: impl IntoIterator<Item = NodeId>) -> Vec<Brief> {
        ids.into_iter().map(|n| self.brief(n)).collect()
    }

    /// Sort key for deterministic node ordering: (path, start line, qualified name, id).
    pub fn position_key(&self, id: NodeId) -> (String, u32, String, NodeId) {
        let node = self.node(id);
        (
            self.node_path(id).unwrap_or("").to_string(),
            node.span.map(|s| s.start_line).unwrap_or(0),
            node.qualified_name.clone(),
            id,
        )
    }

    /// Resolves a user-supplied repository-relative path to a file. Accepts
    /// `./` prefixes, absolute paths under the indexed root, and unique
    /// path suffixes (`service.py` when only one file ends with it).
    pub fn resolve_file(&self, path: &str) -> anyhow::Result<FileId> {
        let p = self.normalize_path(path);
        if let Some(f) = self.seg.file_by_path(&p) {
            return Ok(f);
        }
        let suffix = format!("/{p}");
        let hits: Vec<FileId> =
            (0..self.seg.files().len() as FileId).filter(|&f| self.seg.file(f).path.ends_with(&suffix)).collect();
        match hits.len() {
            1 => Ok(hits[0]),
            0 => anyhow::bail!("file not indexed: `{path}` (repository-relative paths, e.g. `src/app.py`)"),
            _ => {
                let mut names: Vec<&str> = hits.iter().map(|&f| self.seg.file(f).path.as_str()).collect();
                names.sort_unstable();
                names.truncate(10);
                anyhow::bail!("path `{path}` is ambiguous; matching files: {}", names.join(", "))
            }
        }
    }

    pub fn normalize_path(&self, path: &str) -> String {
        let root = format!("{}/", self.entry.root.replace('\\', "/").trim_end_matches('/'));
        let mut p = path.replace('\\', "/");
        // Absolute paths: strip the (canonical) root, also when the caller
        // used a non-canonical spelling of it (symlinks such as macOS
        // `/var` → `/private/var`).
        if std::path::Path::new(path).is_absolute()
            && !p.starts_with(&root)
            && let Ok(canon) = std::fs::canonicalize(path)
        {
            p = canon.to_string_lossy().replace('\\', "/");
        }
        if let Some(rest) = p.strip_prefix(&root) {
            p = rest.to_string();
        }
        while let Some(rest) = p.strip_prefix("./") {
            p = rest.to_string();
        }
        p.trim_end_matches('/').to_string()
    }

    /// Whether `node` lives under `path` (exact file, or a directory prefix).
    fn under_path(&self, node: NodeId, path: &str) -> bool {
        let Some(np) = self.node_path(node) else { return false };
        let p = self.normalize_path(path);
        if p.is_empty() {
            return true;
        }
        np == p || np.starts_with(&format!("{p}/")) || np.ends_with(&format!("/{p}"))
    }

    /// Finds the symbol named by `sel`: `id`, then `qualified_name` (exact,
    /// then suffix on `.`/`:`/`/` boundaries), then `name` (narrowed by `path`).
    pub fn lookup(&self, sel: &Selector<'_>) -> anyhow::Result<Lookup> {
        let (what, mut found) = if let Some(id) = sel.id {
            (format!("id `{id}`"), self.find_by_id(id)?)
        } else if let Some(q) = sel.qualified_name {
            (format!("qualified_name `{q}`"), self.find_by_qualified_name(q))
        } else if let Some(name) = sel.name {
            let mut hits = self.seg.nodes_by_name(name);
            if let Some(path) = sel.path {
                hits.retain(|&n| self.under_path(n, path));
            }
            let what = match sel.path {
                Some(p) => format!("name `{name}` under `{p}`"),
                None => format!("name `{name}`"),
            };
            (what, hits)
        } else {
            anyhow::bail!("one of `id`, `qualified_name` or `name` is required");
        };
        if found.is_empty() {
            anyhow::bail!("no symbol found for {what}; try `search_symbols` to find the right name");
        }
        found.sort_by_cached_key(|&n| self.preference_key(n));
        found.dedup();
        let node = found[0];
        let candidates = found.into_iter().skip(1).take(MAX_CANDIDATES).collect();
        Ok(Lookup { node, candidates })
    }

    /// Preference when several nodes match: non-test, exported, symbols
    /// before containers, shorter qualified name, then node id.
    fn preference_key(&self, id: NodeId) -> (bool, bool, bool, usize, NodeId) {
        let (a, b, c, d) = self.preference(id);
        (a, b, c, d, id)
    }

    /// [`Self::preference_key`] without the node id, comparable across
    /// snapshots.
    pub fn preference(&self, id: NodeId) -> (bool, bool, bool, usize) {
        let n = self.node(id);
        (n.is_test, !n.exported, is_container(n.label), n.qualified_name.len())
    }

    fn find_by_id(&self, id: &str) -> anyhow::Result<Vec<NodeId>> {
        let parts = rueca_model::parse_symbol_id(id)
            .ok_or_else(|| anyhow::anyhow!("malformed symbol id `{id}` (expected `<repo>@<commit>:<path>#<name>`)"))?;
        if parts.repo != self.repo() {
            anyhow::bail!(
                "symbol id `{id}` belongs to repository `{}`, but the selected repository is `{}` (pass `repo`)",
                parts.repo,
                self.repo()
            );
        }
        let Some(file) = self.seg.file_by_path(&parts.path) else {
            if parts.path == ENV_PATH
                && let Some(name) = parts.qualified_name.as_deref()
            {
                let qn = env_var_qualified_name(name);
                return Ok(self
                    .seg
                    .nodes_by_qualified_name(&qn)
                    .into_iter()
                    .filter(|&n| self.node(n).file.is_none())
                    .collect());
            }
            anyhow::bail!("file `{}` of symbol id `{id}` is not indexed", parts.path);
        };
        match parts.qualified_name.as_deref() {
            None | Some("") => Ok(vec![self.seg.file(file).node]),
            Some(q) => Ok(self
                .seg
                .nodes_in_file(file)
                .into_iter()
                .filter(|&n| self.local_qualified_name(n).as_deref() == Some(q))
                .collect()),
        }
    }

    fn find_by_qualified_name(&self, q: &str) -> Vec<NodeId> {
        let exact = self.seg.nodes_by_qualified_name(q);
        if !exact.is_empty() {
            return exact;
        }
        let nodes = self.seg.nodes();
        let suffix: Vec<NodeId> = (0..nodes.len() as NodeId)
            .filter(|&i| {
                let n = &nodes[i as usize];
                !is_container(n.label) && is_qualified_suffix(&n.qualified_name, q)
            })
            .collect();
        if !suffix.is_empty() {
            return suffix;
        }
        // Overloads (Java, C/C++): the parameter suffix is optional, so
        // `Parser.parse` finds `com.x.Parser.parse(String)` and its siblings.
        (0..nodes.len() as NodeId)
            .filter(|&i| {
                let n = &nodes[i as usize];
                let overloadable =
                    matches!(n.language.map(|l| l.family()), Some(LanguageFamily::Java | LanguageFamily::C));
                overloadable
                    && !is_container(n.label)
                    && overload_base(&n.qualified_name).is_some_and(|b| b == q || is_qualified_suffix(b, q))
            })
            .collect()
    }

    /// Symbol nodes of `file` that have a span, excluding containers.
    pub fn file_symbols(&self, file: FileId) -> Vec<(NodeId, Span)> {
        self.seg
            .nodes_in_file(file)
            .into_iter()
            .filter_map(|n| {
                let node = self.node(n);
                if is_container(node.label) || node.file != Some(file) {
                    return None;
                }
                node.span.map(|s| (n, s))
            })
            .collect()
    }
}

/// Name of a canonical env var node (`$env:NAME`, no file).
fn env_var_name(node: &Node) -> Option<&str> {
    if node.label != Label::EnvVar || node.file.is_some() {
        return None;
    }
    node.qualified_name.strip_prefix(ENV_PATH)?.strip_prefix(':')
}

/// Whether `q` is a suffix of `qn` on a separator boundary: `.`, `:`, `/`
/// (ECMAScript names drop leading module directories: `util:formatAmount`
/// for `src/util:formatAmount`), `\`; and any boundary when `q` starts with
/// a key-path accessor (`["DATABASE_URL"]`, `[0].run`).
fn is_qualified_suffix(qn: &str, q: &str) -> bool {
    if q.is_empty() || qn.len() <= q.len() || !qn.ends_with(q) {
        return false;
    }
    let b = qn.as_bytes()[qn.len() - q.len() - 1];
    b == b'.' || b == b':' || b == b'/' || b == b'\\' || q.starts_with('[')
}

/// `a.B.m` for an overloaded `a.B.m(int, String)`.
fn overload_base(qn: &str) -> Option<&str> {
    let open = qn.strip_suffix(')')?.rfind('(')?;
    (open > 0).then(|| &qn[..open])
}

/// Innermost symbol among `symbols` whose span covers 0-based `line`.
pub fn innermost_at_line(symbols: &[(NodeId, Span)], line0: u32) -> Option<NodeId> {
    symbols
        .iter()
        .filter(|(_, s)| s.start_line <= line0 && line0 <= s.end_line)
        .min_by_key(|(n, s)| {
            (s.end_byte.saturating_sub(s.start_byte), s.end_line - s.start_line, u32::MAX - s.start_byte, *n)
        })
        .map(|(n, _)| *n)
}
