//! The tool implementations: snapshot selection, per-snapshot execution,
//! one ranking across snapshots (the organization index, or summed BM25
//! statistics), and the output format (compact envelope or the alpha
//! `verbose` shapes).

use std::collections::{BTreeMap, HashMap, HashSet};

use rueca_model::{EdgeKind, FileId, Label, NodeId, SymbolIdParts};
use rueca_segment::org::OrgScope;
use rueca_segment::{ContentMatch, ContentQuery, EdgeRef, Segment, SymbolQuery};

use crate::args::{
    CommonArgs, DirectionArg, GetFileOutlineArgs, GetSnippetArgs, GetSymbolArgs, ListRepositoriesArgs, RepoArg,
    SearchCodeArgs, SearchSymbolsArgs, TraceArgs, clamp_int,
};
use crate::text::{join_window, split_lines, window};
use crate::types::*;
use crate::view::{Lookup, Selector, View, innermost_at_line, is_container};
use crate::{Engine, Response, compact};

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

/// Item offset named by `cursor` (else `fallback`, else 0).
fn cursor_offset(c: &CommonArgs, fallback: Option<i64>) -> anyhow::Result<usize> {
    match c.cursor.as_deref() {
        Some(s) => s.trim().parse::<usize>().map_err(|_| {
            anyhow::anyhow!("invalid `cursor` `{s}`: pass the `next_cursor` of a previous response unchanged")
        }),
        None => Ok(fallback.unwrap_or(0).max(0) as usize),
    }
}

/// `budget_tokens`, clamped to [100, 1,000,000].
fn budget(c: &CommonArgs) -> usize {
    clamp_int(c.budget_tokens, compact::DEFAULT_BUDGET_TOKENS as i64, 100, 1_000_000) as usize
}

fn selector<'a>(
    id: Option<&'a str>,
    qualified_name: Option<&'a str>,
    name: Option<&'a str>,
    path: Option<&'a str>,
) -> (Selector<'a>, Option<SymbolIdParts>) {
    (Selector { id, qualified_name, name, path }, id.and_then(rueca_model::parse_symbol_id))
}

/// The snapshot an `id` lives in: among `views` of the id's repository, the one
/// whose ref or commit the id names, else the first (local mode resolves an id
/// with a stale commit against the current snapshot, so ids survive a
/// re-index). With no view of that repository, view 0, whose lookup reports the
/// mismatch.
fn pick_for_id(views: &[View], parts: Option<&SymbolIdParts>) -> usize {
    let Some(p) = parts else { return 0 };
    let same: Vec<usize> = (0..views.len()).filter(|&i| views[i].repo() == p.repo).collect();
    if let Some(c) = &p.commit
        && let Some(&i) = same.iter().find(|&&i| views[i].snap.matches_ref(c))
    {
        return i;
    }
    same.first().copied().unwrap_or(0)
}

/// Resolves a symbol selector over the selected snapshots: an `id` within its
/// own snapshot; a name in every snapshot, keeping the best match (the
/// lookup preference, then snapshot order) and listing the best match of each
/// other snapshot as an extra candidate.
fn resolve_symbol(
    views: &[View],
    sel: &Selector<'_>,
    parts: Option<&SymbolIdParts>,
) -> anyhow::Result<(usize, Lookup, Vec<Brief>)> {
    if sel.id.is_some() || views.len() == 1 {
        let vi = if sel.id.is_some() { pick_for_id(views, parts) } else { 0 };
        return Ok((vi, views[vi].lookup(sel)?, Vec::new()));
    }
    let mut found = Vec::new();
    let mut first_err = None;
    for (vi, v) in views.iter().enumerate() {
        match v.lookup(sel) {
            Ok(l) => found.push((vi, l)),
            Err(e) => {
                first_err.get_or_insert(e);
            }
        }
    }
    if found.is_empty() {
        return Err(first_err.unwrap_or_else(|| anyhow::anyhow!("no symbol found")));
    }
    found.sort_by_key(|(vi, l)| (views[*vi].preference(l.node), *vi));
    let (vi, l) = found.remove(0);
    let extra = found.iter().map(|(vj, lj)| views[*vj].brief(lj.node)).collect();
    Ok((vi, l, extra))
}

/// Resolves a file path over the selected snapshots; a path present in several
/// is ambiguous.
fn resolve_path(views: &[View], path: &str) -> anyhow::Result<(usize, FileId)> {
    if views.len() == 1 {
        return Ok((0, views[0].resolve_file(path)?));
    }
    let mut hits = Vec::new();
    let mut first_err = None;
    for (vi, v) in views.iter().enumerate() {
        match v.resolve_file(path) {
            Ok(f) => hits.push((vi, f)),
            Err(e) => {
                first_err.get_or_insert(e);
            }
        }
    }
    match hits.len() {
        1 => Ok(hits[0]),
        0 => Err(first_err.unwrap_or_else(|| anyhow::anyhow!("file not indexed: `{path}`"))),
        _ => {
            let repos: Vec<String> = hits.iter().map(|(vi, _)| views[*vi].snap.header()).collect();
            anyhow::bail!("`{path}` is indexed in several repositories ({}); pass one as `repo`", repos.join(", "))
        }
    }
}

fn verbose<T: serde::Serialize>(value: &T) -> anyhow::Result<Response> {
    Ok(Response::verbose(serde_json::to_value(value)?))
}

impl Engine {
    pub(crate) fn search_symbols(&self, a: SearchSymbolsArgs) -> anyhow::Result<Response> {
        let verbose_fmt = a.common.verbose();
        let limit = clamp_int(a.limit, if verbose_fmt { 20 } else { 10 }, 1, 200) as usize;
        let offset = cursor_offset(&a.common, a.offset)?;
        let labels = parse_labels(a.labels.as_deref())?;
        let sel = self.select(&a.common, None)?;
        let views = &sel.views;
        let base = SymbolQuery {
            text: a.query,
            labels,
            path_glob: a.path_glob,
            language: a.language.map(|l| l.to_language()),
            limit: limit + 1,
            offset,
        };
        // (view, node, score): one ranking over every selected snapshot
        // (RFC 0006 §6) — the organization index, or the snapshots' indexes
        // scored with summed statistics.
        let mut ranked: Vec<(usize, NodeId, f64)> = match &sel.org {
            Some(org) => {
                let selected: Vec<(usize, &Segment)> =
                    views.iter().filter_map(|v| v.org_repo.map(|i| (i, v.seg.as_ref()))).collect();
                let view_of: HashMap<usize, usize> =
                    views.iter().enumerate().filter_map(|(vi, v)| v.org_repo.map(|i| (i, vi))).collect();
                org.index
                    .search(&base, &OrgScope { visible: &org.visible, selected: &selected })?
                    .into_iter()
                    .filter_map(|h| view_of.get(&h.repo).map(|&vi| (vi, h.node, f32_to_f64(h.score))))
                    .collect()
            }
            None if views.len() == 1 => {
                views[0].seg.search_symbols(&base)?.into_iter().map(|h| (0, h.node, f32_to_f64(h.score))).collect()
            }
            None => {
                let segs: Vec<&Segment> = views.iter().map(|v| v.seg.as_ref()).collect();
                rueca_segment::search_symbols_merged(&segs, &base)?
                    .into_iter()
                    .map(|(vi, h)| (vi, h.node, f32_to_f64(h.score)))
                    .collect()
            }
        };
        let more = ranked.len() > limit;
        ranked.truncate(limit);
        let hits: Vec<(usize, SymbolSearchHit)> = ranked
            .into_iter()
            .map(|(vi, n, score)| {
                let v = &views[vi];
                (vi, SymbolSearchHit { brief: v.brief(n), signature: v.node(n).signature.clone(), score })
            })
            .collect();
        if verbose_fmt {
            return verbose(&SearchSymbolsResult {
                results: hits.into_iter().map(|(_, h)| h).collect(),
                truncated: more,
                next_offset: more.then_some((offset + limit) as u64),
            });
        }
        let snaps: Vec<_> = views.iter().map(|v| &v.snap).collect();
        Ok(compact::search_symbols(&snaps, &hits, offset, more, a.scores.unwrap_or(false), budget(&a.common)))
    }

    pub(crate) fn search_code(&self, a: SearchCodeArgs) -> anyhow::Result<Response> {
        if a.pattern.is_empty() {
            anyhow::bail!("`pattern` must not be empty");
        }
        let verbose_fmt = a.common.verbose();
        let limit = clamp_int(a.limit, if verbose_fmt { 50 } else { 20 }, 1, 500) as usize;
        let skip = cursor_offset(&a.common, None)?;
        let context = clamp_int(a.context_lines, 0, 0, 10) as u32;
        let views = self.scope(&a.common, None)?;
        let base = ContentQuery {
            pattern: a.pattern,
            regex: a.regex.unwrap_or(false),
            case_insensitive: a.case_insensitive.unwrap_or(false),
            path_glob: a.path_glob,
            language: a.language.map(|l| l.to_language()),
            max_matches: skip + limit + 1,
            context_lines: context,
            ..ContentQuery::default()
        };
        let mut all: Vec<(usize, ContentMatch)> = Vec::new();
        let mut more = false;
        let mut files_scanned = 0u32;
        for (vi, view) in views.iter().enumerate() {
            if all.len() > skip + limit {
                more = true;
                break;
            }
            let res = view.seg.search_content(&base)?;
            more |= res.truncated;
            files_scanned += res.files_scanned;
            all.extend(res.matches.into_iter().map(|m| (vi, m)));
        }
        more |= all.len() > skip + limit;
        let mut symbols_by_file: HashMap<(usize, FileId), Vec<(NodeId, rueca_model::Span)>> = HashMap::new();
        let matches: Vec<(usize, CodeMatch)> = all
            .into_iter()
            .skip(skip)
            .take(limit)
            .map(|(vi, m)| {
                let view = &views[vi];
                let syms = symbols_by_file.entry((vi, m.file)).or_insert_with(|| view.file_symbols(m.file));
                let symbol = innermost_at_line(syms, m.line.saturating_sub(1)).map(|n| view.brief(n));
                let cm = CodeMatch {
                    path: view.seg.file(m.file).path.clone(),
                    line: m.line,
                    text: m.text,
                    before: (context > 0).then_some(m.before),
                    after: (context > 0).then_some(m.after),
                    symbol,
                };
                (vi, cm)
            })
            .collect();
        if verbose_fmt {
            return verbose(&SearchCodeResult {
                matches: matches.into_iter().map(|(_, m)| m).collect(),
                truncated: more,
                files_scanned,
            });
        }
        let snaps: Vec<_> = views.iter().map(|v| &v.snap).collect();
        Ok(compact::search_code(&snaps, &matches, skip, more, files_scanned, budget(&a.common)))
    }

    pub(crate) fn get_symbol(&self, a: GetSymbolArgs) -> anyhow::Result<Response> {
        let (sel, parts) = selector(a.id.as_deref(), a.qualified_name.as_deref(), a.name.as_deref(), a.path.as_deref());
        if sel.is_empty() {
            anyhow::bail!("one of `id`, `qualified_name` or `name` is required");
        }
        let views = self.scope(&a.common, parts.as_ref())?;
        let (vi, found, extra) = resolve_symbol(&views, &sel, parts.as_ref())?;
        let view = &views[vi];
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
        // The code ↔ config bridge (RFC 0003 §7.4): who reads / sets it.
        let bridge = matches!(node.label, Label::EnvVar | Label::Key);
        let count_in = |kind| bridge.then(|| distinct_others(&seg.in_edges(n, Some(kind))));
        let mut candidates = view.briefs(found.candidates);
        candidates.extend(extra);
        candidates.truncate(crate::view::MAX_CANDIDATES);

        let result = GetSymbolResult {
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
            readers_count: count_in(EdgeKind::Reads),
            writers_count: count_in(EdgeKind::Writes),
            candidates,
        };
        if a.common.verbose() {
            return verbose(&result);
        }
        Ok(compact::get_symbol(&view.snap, &result, budget(&a.common)))
    }

    pub(crate) fn get_snippet(&self, a: GetSnippetArgs) -> anyhow::Result<Response> {
        let context = clamp_int(a.context_lines, 0, 0, 50) as u32;
        let max_lines = clamp_int(a.max_lines, 300, 1, 2000) as u32;
        let (sel, parts) = selector(a.id.as_deref(), a.qualified_name.as_deref(), a.name.as_deref(), a.path.as_deref());
        if sel.is_empty() && a.path.is_none() {
            anyhow::bail!("give a symbol (`id`, `qualified_name` or `name`) or a `path` with `start_line`");
        }
        if let (Some(s), Some(e)) = (a.start_line, a.end_line)
            && e < s
        {
            anyhow::bail!("`end_line` ({e}) is before `start_line` ({s})");
        }
        // `cursor` = the line to continue from.
        let resume = match a.common.cursor {
            Some(_) => Some(cursor_offset(&a.common, None)?.clamp(1, u32::MAX as usize) as u32),
            None => None,
        };
        let views = self.scope(&a.common, parts.as_ref())?;
        let (vi, file, start, end, symbol) = if !sel.is_empty() {
            let (vi, found, _) = resolve_symbol(&views, &sel, parts.as_ref())?;
            let view = &views[vi];
            let n = found.node;
            let node = view.node(n);
            let Some(file) = node.file else {
                anyhow::bail!("`{}` is a {} and has no source", node.qualified_name, node.label);
            };
            let (s, e) = view.span_lines(n);
            let s = s.unwrap_or(1);
            let e = e.unwrap_or(u32::MAX);
            (vi, file, s, e, Some(view.brief(n)))
        } else {
            let path = a.path.as_deref().unwrap_or_default();
            let (vi, file) = resolve_path(&views, path)?;
            let s = a.start_line.unwrap_or(1);
            if s < 1 {
                anyhow::bail!("`start_line` is 1-based (got {s})");
            }
            let e = a.end_line.unwrap_or(i64::from(u32::MAX));
            (vi, file, s.min(i64::from(u32::MAX)) as u32, e.min(i64::from(u32::MAX)) as u32, None)
        };
        let view = &views[vi];
        let entry = view.seg.file(file);
        let content = view.seg.content(file)?;
        let lines = split_lines(&content);
        let line_count = lines.len() as u32;
        if symbol.is_none() && start > line_count.max(1) {
            anyhow::bail!("`start_line` {start} is past the end of `{}` ({line_count} lines)", entry.path);
        }
        let mut w = window(start, end, context, max_lines, line_count);
        // A cursor continues a cut snippet from that line.
        if let Some(from) = resume
            && from > w.start
        {
            let full_end = end.saturating_add(context).min(line_count);
            w = window(from.min(full_end.max(1)), full_end, 0, max_lines, line_count);
        }
        let result = SnippetResult {
            path: entry.path.clone(),
            language: entry.language.map(|l| l.as_str().to_string()),
            start_line: w.start,
            end_line: w.end,
            text: join_window(&lines, w),
            truncated: w.truncated,
            symbol,
        };
        if a.common.verbose() {
            return verbose(&result);
        }
        Ok(compact::get_snippet(&view.snap, &result, budget(&a.common)))
    }

    pub(crate) fn trace(&self, a: TraceArgs) -> anyhow::Result<Response> {
        let verbose_fmt = a.common.verbose();
        let depth = clamp_int(a.depth, if verbose_fmt { 2 } else { 1 }, 1, 10) as u32;
        let limit = clamp_int(a.limit, if verbose_fmt { 200 } else { 50 }, 1, 2000) as usize;
        let offset = if verbose_fmt { 0 } else { cursor_offset(&a.common, None)? };
        let min_conf = a.min_confidence.unwrap_or(0.0);
        if !min_conf.is_finite() {
            anyhow::bail!("`min_confidence` must be a number between 0 and 1");
        }
        let min_conf = min_conf.clamp(0.0, 1.0) as f32;
        let kinds = parse_edge_kinds(a.edge_types.as_deref())?;
        let (sel, parts) = selector(a.id.as_deref(), a.qualified_name.as_deref(), a.name.as_deref(), a.path.as_deref());
        if sel.is_empty() {
            anyhow::bail!("one of `id`, `qualified_name` or `name` is required");
        }
        let labels = parse_labels(a.labels.as_deref())?;
        let include_tests = a.include_tests.unwrap_or(kinds.contains(&EdgeKind::Tests));
        let views = self.scope(&a.common, parts.as_ref())?;
        let (vi, found, _) = resolve_symbol(&views, &sel, parts.as_ref())?;
        let view = &views[vi];
        let root = found.node;
        let filter = NeighbourFilter { labels: &labels, include_tests };
        let dirs: &[Dir] = match a.direction.unwrap_or(DirectionArg::Both) {
            DirectionArg::Callers => &[Dir::Callers],
            DirectionArg::Callees => &[Dir::Callees],
            DirectionArg::Both => &[Dir::Callers, Dir::Callees],
        };
        let out = bfs(view, root, dirs, &kinds, depth, min_conf, offset + limit, &filter);

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
        let result = TraceResult {
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
            tests_excluded: out.tests_excluded.len() as u32,
        };
        if verbose_fmt {
            return verbose(&result);
        }
        Ok(compact::trace(&view.snap, &result, offset, budget(&a.common)))
    }

    pub(crate) fn get_file_outline(&self, a: GetFileOutlineArgs) -> anyhow::Result<Response> {
        let offset = cursor_offset(&a.common, None)?;
        let views = self.scope(&a.common, None)?;
        let (vi, file) = resolve_path(&views, &a.path)?;
        let view = &views[vi];
        let entry = view.seg.file(file);
        let symbols = view.file_symbols(file);
        let tree = outline_tree(view, &symbols);
        let result = FileOutlineResult {
            path: entry.path.clone(),
            language: entry.language.map(|l| l.as_str().to_string()),
            line_count: entry.line_count,
            symbols: tree,
        };
        if a.common.verbose() {
            return verbose(&result);
        }
        Ok(compact::outline(&view.snap, &result, offset, budget(&a.common)))
    }
}

impl Engine {
    pub(crate) fn list_repositories(&self, a: ListRepositoriesArgs) -> anyhow::Result<Response> {
        let offset = cursor_offset(&a.common, None)?;
        // A listing reads every repository unless `repo` narrows it.
        let mut common = a.common.clone();
        if common.repo.is_none() {
            common.repo = Some(RepoArg::One("*".into()));
        }
        let sel = self.select(&common, None)?;
        let views = &sel.views;
        let mut repos: Vec<(usize, RepositorySummary)> = Vec::new();
        for (vi, v) in views.iter().enumerate() {
            if let Some((_, r)) = repos.iter_mut().find(|(_, r)| r.repo == v.repo()) {
                r.refs.push(v.snap.ref_name.clone());
                continue;
            }
            let seg = &v.seg;
            let mut languages = BTreeMap::new();
            for f in seg.files() {
                if let Some(l) = f.language {
                    *languages.entry(l.as_str().to_string()).or_insert(0u32) += 1;
                }
            }
            let symbols = seg.nodes().iter().filter(|n| !is_container(n.label)).count() as u32;
            repos.push((
                vi,
                RepositorySummary {
                    repo: v.repo().to_string(),
                    root: v.snap.root.clone(),
                    default_ref: v.snap.ref_name.clone(),
                    commit: v.snap.commit.clone(),
                    refs: vec![v.snap.ref_name.clone()],
                    languages,
                    files: seg.files().len() as u32,
                    symbols,
                    edges: seg.meta().edge_count,
                },
            ));
        }
        repos.sort_by(|a, b| a.1.repo.cmp(&b.1.repo));
        if a.common.verbose() {
            return verbose(&ListRepositoriesResult {
                org: self.org.clone(),
                repositories: repos.into_iter().map(|(_, r)| r).collect(),
            });
        }
        let snaps: Vec<_> = views.iter().map(|v| &v.snap).collect();
        Ok(compact::list_repositories(&snaps, &repos, self.org.as_deref(), offset, budget(&a.common)))
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
    /// Test neighbours skipped by the filter.
    pub tests_excluded: HashSet<NodeId>,
}

/// Which neighbours `trace` reports and traverses (the root is never filtered).
pub(crate) struct NeighbourFilter<'a> {
    /// Allowed labels; empty = all.
    pub labels: &'a [Label],
    pub include_tests: bool,
}

impl NeighbourFilter<'_> {
    /// `Err(true)` = skipped because it is a test.
    fn check(&self, view: &View, n: NodeId) -> Result<(), bool> {
        let node = view.node(n);
        let is_test = node.is_test || node.label == Label::Test;
        if is_test && !self.include_tests {
            return Err(true);
        }
        if !self.labels.is_empty() && !self.labels.contains(&node.label) {
            return Err(false);
        }
        Ok(())
    }
}

/// Breadth-first traversal from `root`, level by level, callers before
/// callees. Each node is visited once per direction; `limit` bounds the
/// number of discovered nodes (all directions together). Neighbours rejected
/// by `filter` are neither reported nor expanded, and their edges are dropped.
#[allow(clippy::too_many_arguments)]
pub(crate) fn bfs(
    view: &View,
    root: NodeId,
    dirs: &[Dir],
    kinds: &[EdgeKind],
    depth: u32,
    min_conf: f32,
    limit: usize,
    filter: &NeighbourFilter<'_>,
) -> BfsOut {
    let mut out =
        BfsOut { nodes: Vec::new(), edges: BTreeMap::new(), truncated: false, tests_excluded: HashSet::new() };
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
                        if e.other != root
                            && let Err(is_test) = filter.check(view, e.other)
                        {
                            if is_test {
                                out.tests_excluded.insert(e.other);
                            }
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
