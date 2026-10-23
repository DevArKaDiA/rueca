//! The six tool implementations.

use std::collections::{BTreeMap, HashMap, HashSet};

use rueca_model::{EdgeKind, FileId, Label, NodeId};
use rueca_segment::{ContentQuery, EdgeRef, SymbolQuery};

use crate::Engine;
use crate::args::{
    DirectionArg, GetFileOutlineArgs, GetSnippetArgs, GetSymbolArgs, SearchCodeArgs, SearchSymbolsArgs, TraceArgs,
    clamp_int,
};
use crate::text::{join_window, split_lines, window};
use crate::types::*;
use crate::view::{Selector, View, innermost_at_line, is_container};

fn id_repo(id: Option<&str>) -> Option<String> {
    id.and_then(rueca_model::parse_symbol_id).map(|p| p.repo)
}

fn parse_labels(labels: Option<&[String]>) -> anyhow::Result<Vec<Label>> {
    let mut out = Vec::new();
    for l in labels.unwrap_or_default() {
        match Label::parse(l) {
            Some(label) => out.push(label),
            None => anyhow::bail!(
                "unknown label `{l}` (valid: {})",
                Label::ALL.iter().map(|l| l.as_str()).collect::<Vec<_>>().join(", ")
            ),
        }
    }
    Ok(out)
}

fn parse_edge_kinds(kinds: Option<&[String]>) -> anyhow::Result<Vec<EdgeKind>> {
    let Some(kinds) = kinds else { return Ok(vec![EdgeKind::Calls]) };
    let mut out = Vec::new();
    for k in kinds {
        match EdgeKind::parse(k) {
            Some(kind) if !out.contains(&kind) => out.push(kind),
            Some(_) => {}
            None => anyhow::bail!(
                "unknown edge type `{k}` (valid: {})",
                EdgeKind::ALL.iter().map(|k| k.as_str()).collect::<Vec<_>>().join(", ")
            ),
        }
    }
    if out.is_empty() {
        anyhow::bail!("`edge_types` must not be empty");
    }
    Ok(out)
}

fn distinct_others(edges: &[EdgeRef]) -> u32 {
    edges.iter().map(|e| e.other).collect::<HashSet<_>>().len() as u32
}

impl Engine {
    pub(crate) fn search_symbols(&self, a: SearchSymbolsArgs) -> anyhow::Result<SearchSymbolsResult> {
        let limit = clamp_int(a.limit, 20, 1, 200) as usize;
        let offset = a.offset.unwrap_or(0).max(0) as usize;
        let labels = parse_labels(a.labels.as_deref())?;
        let view = self.view(a.repo.as_deref(), None)?;
        let query = SymbolQuery {
            text: a.query,
            labels,
            path_glob: a.path_glob,
            language: a.language.map(|l| l.to_language()),
            limit: limit + 1,
            offset,
        };
        let mut hits = view.seg.search_symbols(&query)?;
        let truncated = hits.len() > limit;
        hits.truncate(limit);
        let results = hits
            .iter()
            .map(|h| SymbolSearchHit {
                brief: view.brief(h.node),
                signature: view.node(h.node).signature.clone(),
                score: f32_to_f64(h.score),
            })
            .collect();
        Ok(SearchSymbolsResult { results, truncated, next_offset: truncated.then_some((offset + limit) as u64) })
    }

    pub(crate) fn search_code(&self, a: SearchCodeArgs) -> anyhow::Result<SearchCodeResult> {
        if a.pattern.is_empty() {
            anyhow::bail!("`pattern` must not be empty");
        }
        let limit = clamp_int(a.limit, 50, 1, 500) as usize;
        let context = clamp_int(a.context_lines, 0, 0, 10) as u32;
        let view = self.view(a.repo.as_deref(), None)?;
        let query = ContentQuery {
            pattern: a.pattern,
            regex: a.regex.unwrap_or(false),
            case_insensitive: a.case_insensitive.unwrap_or(false),
            path_glob: a.path_glob,
            language: a.language.map(|l| l.to_language()),
            max_matches: limit + 1,
            context_lines: context,
            ..ContentQuery::default()
        };
        let res = view.seg.search_content(&query)?;
        let truncated = res.truncated || res.matches.len() > limit;
        let mut symbols_by_file: HashMap<FileId, Vec<(NodeId, rueca_model::Span)>> = HashMap::new();
        let matches = res
            .matches
            .into_iter()
            .take(limit)
            .map(|m| {
                let syms = symbols_by_file.entry(m.file).or_insert_with(|| view.file_symbols(m.file));
                let symbol = innermost_at_line(syms, m.line.saturating_sub(1)).map(|n| view.brief(n));
                CodeMatch {
                    path: view.seg.file(m.file).path.clone(),
                    line: m.line,
                    text: m.text,
                    before: (context > 0).then_some(m.before),
                    after: (context > 0).then_some(m.after),
                    symbol,
                }
            })
            .collect();
        Ok(SearchCodeResult { matches, truncated, files_scanned: res.files_scanned })
    }

    pub(crate) fn get_symbol(&self, a: GetSymbolArgs) -> anyhow::Result<GetSymbolResult> {
        let sel = Selector {
            id: a.id.as_deref(),
            qualified_name: a.qualified_name.as_deref(),
            name: a.name.as_deref(),
            path: a.path.as_deref(),
        };
        if sel.is_empty() {
            anyhow::bail!("one of `id`, `qualified_name` or `name` is required");
        }
        let view = self.view(a.repo.as_deref(), id_repo(sel.id).as_deref())?;
        let found = view.lookup(&sel)?;
        let n = found.node;
        let node = view.node(n);
        let seg = &view.seg;

        let parent = seg.in_edges(n, Some(EdgeKind::Defines)).iter().map(|e| e.other).find(|&p| p != n);
        let mut children: Vec<NodeId> = seg.out_edges(n, Some(EdgeKind::Defines)).iter().map(|e| e.other).collect();
        children.sort_by_cached_key(|&c| view.position_key(c));
        children.dedup();
        let related = |incoming: bool| {
            let mut v: Vec<NodeId> = [EdgeKind::Inherits, EdgeKind::Implements]
                .into_iter()
                .flat_map(|k| if incoming { seg.in_edges(n, Some(k)) } else { seg.out_edges(n, Some(k)) })
                .map(|e| e.other)
                .collect();
            v.sort_by_cached_key(|&c| view.position_key(c));
            v.dedup();
            v
        };
        let bases = related(false);
        let subtypes = related(true);

        Ok(GetSymbolResult {
            symbol: SymbolDetail {
                brief: view.brief(n),
                language: node.language.map(|l| l.as_str().to_string()),
                signature: node.signature.clone(),
                doc: node.doc.clone(),
                exported: node.exported,
                is_test: node.is_test,
            },
            parent: parent.map(|p| view.brief(p)),
            children: view.briefs(children),
            bases: view.briefs(bases),
            subtypes: view.briefs(subtypes),
            callers_count: distinct_others(&seg.in_edges(n, Some(EdgeKind::Calls))),
            callees_count: distinct_others(&seg.out_edges(n, Some(EdgeKind::Calls))),
            candidates: view.briefs(found.candidates),
        })
    }

    pub(crate) fn get_snippet(&self, a: GetSnippetArgs) -> anyhow::Result<SnippetResult> {
        let context = clamp_int(a.context_lines, 0, 0, 50) as u32;
        let max_lines = clamp_int(a.max_lines, 300, 1, 2000) as u32;
        let sel = Selector {
            id: a.id.as_deref(),
            qualified_name: a.qualified_name.as_deref(),
            name: a.name.as_deref(),
            path: a.path.as_deref(),
        };
        if sel.is_empty() && a.path.is_none() {
            anyhow::bail!("give a symbol (`id`, `qualified_name` or `name`) or a `path` with `start_line`");
        }
        if let (Some(s), Some(e)) = (a.start_line, a.end_line)
            && e < s
        {
            anyhow::bail!("`end_line` ({e}) is before `start_line` ({s})");
        }
        let view = self.view(a.repo.as_deref(), id_repo(sel.id).as_deref())?;
        let (file, start, end, symbol) = if !sel.is_empty() {
            let found = view.lookup(&sel)?;
            let n = found.node;
            let node = view.node(n);
            let Some(file) = node.file else {
                anyhow::bail!("`{}` is a {} and has no source", node.qualified_name, node.label);
            };
            let (s, e) = view.span_lines(n);
            let s = s.unwrap_or(1);
            let e = e.unwrap_or(u32::MAX);
            (file, s, e, Some(view.brief(n)))
        } else {
            let path = a.path.as_deref().unwrap_or_default();
            let file = view.resolve_file(path)?;
            let s = a.start_line.unwrap_or(1);
            if s < 1 {
                anyhow::bail!("`start_line` is 1-based (got {s})");
            }
            let e = a.end_line.unwrap_or(i64::from(u32::MAX));
            (file, s.min(i64::from(u32::MAX)) as u32, e.min(i64::from(u32::MAX)) as u32, None)
        };
        let entry = view.seg.file(file);
        let content = view.seg.content(file)?;
        let lines = split_lines(&content);
        let line_count = lines.len() as u32;
        if symbol.is_none() && start > line_count.max(1) {
            anyhow::bail!("`start_line` {start} is past the end of `{}` ({line_count} lines)", entry.path);
        }
        let w = window(start, end, context, max_lines, line_count);
        Ok(SnippetResult {
            path: entry.path.clone(),
            language: entry.language.map(|l| l.as_str().to_string()),
            start_line: w.start,
            end_line: w.end,
            text: join_window(&lines, w),
            truncated: w.truncated,
            symbol,
        })
    }

    pub(crate) fn trace(&self, a: TraceArgs) -> anyhow::Result<TraceResult> {
        let depth = clamp_int(a.depth, 2, 1, 10) as u32;
        let limit = clamp_int(a.limit, 200, 1, 2000) as usize;
        let min_conf = a.min_confidence.unwrap_or(0.0);
        if !min_conf.is_finite() {
            anyhow::bail!("`min_confidence` must be a number between 0 and 1");
        }
        let min_conf = min_conf.clamp(0.0, 1.0) as f32;
        let kinds = parse_edge_kinds(a.edge_types.as_deref())?;
        let sel = Selector {
            id: a.id.as_deref(),
            qualified_name: a.qualified_name.as_deref(),
            name: a.name.as_deref(),
            path: a.path.as_deref(),
        };
        if sel.is_empty() {
            anyhow::bail!("one of `id`, `qualified_name` or `name` is required");
        }
        let view = self.view(a.repo.as_deref(), id_repo(sel.id).as_deref())?;
        let root = view.lookup(&sel)?.node;
        let dirs: &[Dir] = match a.direction.unwrap_or(DirectionArg::Both) {
            DirectionArg::Callers => &[Dir::Callers],
            DirectionArg::Callees => &[Dir::Callees],
            DirectionArg::Both => &[Dir::Callers, Dir::Callees],
        };
        let out = bfs(&view, root, dirs, &kinds, depth, min_conf, limit);

        let mut nodes: Vec<(u32, Dir, String, NodeId)> =
            out.nodes.iter().map(|&(d, dir, n)| (d, dir, view.node(n).qualified_name.clone(), n)).collect();
        nodes.sort();
        let mut edges: Vec<(EdgeSortKey, rueca_model::EdgeMeta)> = out
            .edges
            .into_iter()
            .map(|((src, dst, kind), meta)| {
                let key =
                    (view.node(src).qualified_name.clone(), src, view.node(dst).qualified_name.clone(), dst, kind);
                (key, meta)
            })
            .collect();
        edges.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(TraceResult {
            root: view.brief(root),
            nodes: nodes
                .into_iter()
                .map(|(d, dir, _, n)| TraceNode { brief: view.brief(n), depth: d, direction: dir.as_str().to_string() })
                .collect(),
            edges: edges
                .into_iter()
                .map(|((_, src, _, dst, kind), meta)| TraceEdge {
                    src: view.symbol_id(src),
                    dst: view.symbol_id(dst),
                    kind: kind.as_str().to_string(),
                    confidence: f32_to_f64(meta.confidence),
                    strategy: meta.strategy.as_str().to_string(),
                    call_site: meta.evidence.and_then(|(f, span)| {
                        let file = view.seg.files().get(f as usize)?;
                        Some(CallSite { path: file.path.clone(), line: span.start_line + 1 })
                    }),
                })
                .collect(),
            truncated: out.truncated,
        })
    }

    pub(crate) fn get_file_outline(&self, a: GetFileOutlineArgs) -> anyhow::Result<FileOutlineResult> {
        let view = self.view(a.repo.as_deref(), None)?;
        let file = view.resolve_file(&a.path)?;
        let entry = view.seg.file(file);
        let symbols = view.file_symbols(file);
        let tree = outline_tree(&view, &symbols);
        Ok(FileOutlineResult {
            path: entry.path.clone(),
            language: entry.language.map(|l| l.as_str().to_string()),
            line_count: entry.line_count,
            symbols: tree,
        })
    }
}

/// Edge order in `trace` results: (src qualified name, src, dst qualified
/// name, dst, kind).
type EdgeSortKey = (String, NodeId, String, NodeId, EdgeKind);

/// Traversal direction of one BFS. `Callers` sorts before `Callees`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Dir {
    Callers,
    Callees,
}

impl Dir {
    fn as_str(self) -> &'static str {
        match self {
            Dir::Callers => "callers",
            Dir::Callees => "callees",
        }
    }
}

pub(crate) struct BfsOut {
    /// (depth, direction, node), in discovery order.
    pub nodes: Vec<(u32, Dir, NodeId)>,
    /// (src, dst, kind) → strongest edge metadata.
    pub edges: BTreeMap<(NodeId, NodeId, EdgeKind), rueca_model::EdgeMeta>,
    pub truncated: bool,
}

/// Breadth-first traversal from `root`, level by level, callers before
/// callees. Each node is visited once per direction; `limit` bounds the
/// number of discovered nodes (all directions together).
pub(crate) fn bfs(
    view: &View,
    root: NodeId,
    dirs: &[Dir],
    kinds: &[EdgeKind],
    depth: u32,
    min_conf: f32,
    limit: usize,
) -> BfsOut {
    let mut out = BfsOut { nodes: Vec::new(), edges: BTreeMap::new(), truncated: false };
    let mut visited: HashMap<Dir, HashSet<NodeId>> = dirs.iter().map(|&d| (d, HashSet::from([root]))).collect();
    let mut frontier: HashMap<Dir, Vec<NodeId>> = dirs.iter().map(|&d| (d, vec![root])).collect();
    for level in 1..=depth {
        for &dir in dirs {
            let mut current = std::mem::take(frontier.get_mut(&dir).expect("dir"));
            current.sort_by_cached_key(|&n| (view.node(n).qualified_name.clone(), n));
            let mut next = Vec::new();
            for n in current {
                for &kind in kinds {
                    let mut edges = match dir {
                        Dir::Callers => view.seg.in_edges(n, Some(kind)),
                        Dir::Callees => view.seg.out_edges(n, Some(kind)),
                    };
                    edges.sort_by_cached_key(|e| (view.node(e.other).qualified_name.clone(), e.other));
                    for e in edges {
                        if e.meta.confidence < min_conf {
                            continue;
                        }
                        let seen = visited[&dir].contains(&e.other);
                        if !seen {
                            if out.nodes.len() >= limit {
                                out.truncated = true;
                                continue;
                            }
                            visited.get_mut(&dir).expect("dir").insert(e.other);
                            next.push(e.other);
                            out.nodes.push((level, dir, e.other));
                        }
                        let key = match dir {
                            Dir::Callers => (e.other, n, kind),
                            Dir::Callees => (n, e.other, kind),
                        };
                        out.edges
                            .entry(key)
                            .and_modify(|m| {
                                if e.meta.confidence > m.confidence {
                                    *m = e.meta;
                                }
                            })
                            .or_insert(e.meta);
                    }
                }
            }
            frontier.insert(dir, next);
        }
    }
    out
}

/// Nests `symbols` by `DEFINES` parent (when the parent is in the same file)
/// or else by span containment; children ordered by line.
fn outline_tree(view: &View, symbols: &[(NodeId, rueca_model::Span)]) -> Vec<OutlineSymbol> {
    let ids: HashSet<NodeId> = symbols.iter().map(|(n, _)| *n).collect();
    let mut parent_of: HashMap<NodeId, NodeId> = HashMap::new();
    for &(n, span) in symbols {
        let defines_parent = view
            .seg
            .in_edges(n, Some(EdgeKind::Defines))
            .iter()
            .map(|e| e.other)
            .find(|p| *p != n && ids.contains(p) && !is_container(view.node(*p).label));
        let parent = defines_parent.or_else(|| {
            symbols
                .iter()
                .filter(|&&(m, s)| m != n && s.contains(&span) && (s != span || m < n))
                .min_by_key(|(m, s)| (s.end_byte - s.start_byte, std::cmp::Reverse(s.start_byte), *m))
                .map(|(m, _)| *m)
        });
        if let Some(p) = parent {
            parent_of.insert(n, p);
        }
    }
    // Break any accidental cycles by dropping the parent link of nodes on them.
    for &(n, _) in symbols {
        let mut cur = n;
        let mut steps = 0;
        while let Some(&p) = parent_of.get(&cur) {
            steps += 1;
            if p == n || steps > symbols.len() {
                parent_of.remove(&n);
                break;
            }
            cur = p;
        }
    }
    let mut children: HashMap<Option<NodeId>, Vec<(u32, u32, NodeId)>> = HashMap::new();
    for &(n, span) in symbols {
        children.entry(parent_of.get(&n).copied()).or_default().push((span.start_line, span.start_byte, n));
    }
    for v in children.values_mut() {
        v.sort();
    }
    fn build(
        view: &View,
        children: &HashMap<Option<NodeId>, Vec<(u32, u32, NodeId)>>,
        parent: Option<NodeId>,
    ) -> Vec<OutlineSymbol> {
        children
            .get(&parent)
            .map(|v| {
                v.iter()
                    .map(|&(_, _, n)| OutlineSymbol {
                        brief: view.brief(n),
                        signature: view.node(n).signature.clone(),
                        children: build(view, children, Some(n)),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
    build(view, &children, None)
}
