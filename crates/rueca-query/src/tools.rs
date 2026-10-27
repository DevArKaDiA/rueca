//! The tool implementations: snapshot selection, per-snapshot execution,
//! one ranking across snapshots (the organization index, or summed BM25
//! statistics), and the output format (compact envelope or the alpha
//! `verbose` shapes).

use std::collections::{BTreeMap, HashMap, HashSet};

use rueca_model::{EdgeKind, FileId, Label, NodeId, SymbolIdParts};
use rueca_segment::org::{LinkDir, OrgScope, RouteMode};
use rueca_segment::{ContentMatch, ContentQuery, EdgeRef, Segment, SymbolQuery};

use crate::args::{
    CommonArgs, DirectionArg, GetFileOutlineArgs, GetSnippetArgs, GetSymbolArgs, ListRepositoriesArgs, RepoArg,
    SearchCodeArgs, SearchSymbolsArgs, TraceArgs, clamp_int,
};
use crate::text::{join_window, split_lines, window};
use crate::types::*;
use crate::view::{Lookup, Selector, View, innermost_at_line, is_container};
use crate::{Engine, OrgCtx, Response, compact};

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
                let mut selected: Vec<(usize, &Segment)> =
                    views.iter().filter_map(|v| v.org_repo.map(|i| (i, v.seg.as_ref()))).collect();
                let view_of: HashMap<usize, usize> =
                    views.iter().enumerate().filter_map(|(vi, v)| v.org_repo.map(|i| (i, vi))).collect();
                // Repository routing (RFC 0006 phase 2): the query also
                // scores the repositories; their relevance weighs (or, in
                // `top` mode, narrows) the symbol ranking.
                let mut boost: HashMap<usize, f32> = HashMap::new();
                if selected.len() > 1 && self.route.mode != RouteMode::Off {
                    let routes = org.index.route(&base.text, &org.visible, &self.route)?;
                    match self.route.mode {
                        RouteMode::Prior => {
                            boost = routes.iter().map(|r| (r.repo, self.route.prior(r.score))).collect();
                        }
                        RouteMode::Top => {
                            let keep: HashSet<usize> = routes
                                .iter()
                                .filter(|r| r.score >= self.route.ratio)
                                .take(self.route.top.max(1))
                                .map(|r| r.repo)
                                .collect();
                            selected.retain(|(i, _)| keep.contains(i));
                        }
                        RouteMode::Off => {}
                    }
                }
                org.index
                    .search_with(
                        &base,
                        &OrgScope { visible: &org.visible, selected: &selected },
                        &boost,
                        self.route.local_stats && self.route.mode != RouteMode::Off,
                    )?
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
        let selection = self.select(&a.common, parts.as_ref())?;
        let org = selection.org;
        let mut views = selection.views;
        let (root_view, found, _) = resolve_symbol(&views, &sel, parts.as_ref())?;
        let root = (root_view, found.node);
        let filter = NeighbourFilter { labels: &labels, include_tests };
        let dirs: &[Dir] = match a.direction.unwrap_or(DirectionArg::Both) {
            DirectionArg::Callers => &[Dir::Callers],
            DirectionArg::Callees => &[Dir::Callees],
            DirectionArg::Both => &[Dir::Callers, Dir::Callees],
        };
        let walk = TraceWalk { dirs, kinds: &kinds, depth, min_conf, limit: offset + limit, filter: &filter };
        let out = self.bfs(&mut views, org.as_ref(), root, &walk)?;

        let qn = |k: Key| views[k.0].node(k.1).qualified_name.clone();
        let mut nodes: Vec<(u32, Dir, String, Key)> = out.nodes.iter().map(|&(d, dir, k)| (d, dir, qn(k), k)).collect();
        nodes.sort();
        let mut edges: Vec<(EdgeSortKey, (rueca_model::EdgeMeta, usize))> =
            out.edges.into_iter().map(|((src, dst, kind), meta)| ((qn(src), src, qn(dst), dst, kind), meta)).collect();
        edges.sort_by(|a, b| a.0.cmp(&b.0));
        let result = TraceResult {
            root_view,
            root: views[root_view].brief(root.1),
            nodes: nodes
                .into_iter()
                .map(|(d, dir, _, (vi, n))| TraceNode {
                    brief: views[vi].brief(n),
                    depth: d,
                    direction: dir.as_str().to_string(),
                    view: vi,
                })
                .collect(),
            edges: edges
                .into_iter()
                .map(|((_, src, _, dst, kind), (meta, ev))| TraceEdge {
                    src: views[src.0].symbol_id(src.1),
                    dst: views[dst.0].symbol_id(dst.1),
                    kind: kind.as_str().to_string(),
                    confidence: f32_to_f64(meta.confidence),
                    strategy: meta.strategy.as_str().to_string(),
                    call_site: meta.evidence.and_then(|(f, span)| {
                        let file = views[ev].seg.files().get(f as usize)?;
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
        let snaps: Vec<_> = views.iter().map(|v| &v.snap).collect();
        Ok(compact::trace(&snaps, &result, offset, budget(&a.common)))
    }

    /// The view of organization repository `repo` (index into the
    /// generation), opened and appended to `views` when not selected yet.
    fn view_of_repo(&self, views: &mut Vec<View>, org: &OrgCtx, repo: usize) -> anyhow::Result<usize> {
        if let Some(i) = views.iter().position(|v| v.org_repo == Some(repo)) {
            return Ok(i);
        }
        let g = org
            .index
            .generation()
            .repos
            .get(repo)
            .ok_or_else(|| anyhow::anyhow!("organization repository #{repo} is out of range"))?;
        let store = rueca_segment::Store::open(&self.store_dir)?;
        let seg = self.open_cached(&store, &g.segment_id)?;
        let entry = rueca_segment::CatalogEntry {
            root: g.root.clone(),
            repo: g.repo.clone(),
            segment_id: g.segment_id.clone(),
            indexed_at: g.snapshot.indexed_at,
        };
        let mut v = View::new(seg, entry);
        v.org_repo = Some(repo);
        views.push(v);
        Ok(views.len() - 1)
    }

    /// Edges of `key` of kind `kind` in direction `dir`: the snapshot's own
    /// edges, then (organization mode) its cross-repository link edges whose
    /// other end is visible, opening that repository's view when needed.
    /// Each item: (other end, metadata, view whose files the evidence names),
    /// ordered by the other end's qualified name, then key.
    fn neighbours(
        &self,
        views: &mut Vec<View>,
        org: Option<&OrgCtx>,
        key: Key,
        dir: Dir,
        kind: EdgeKind,
    ) -> anyhow::Result<Vec<(Key, rueca_model::EdgeMeta, usize)>> {
        let (vi, n) = key;
        let local = match dir {
            Dir::Callers => views[vi].seg.in_edges(n, Some(kind)),
            Dir::Callees => views[vi].seg.out_edges(n, Some(kind)),
        };
        let mut out: Vec<(Key, rueca_model::EdgeMeta, usize)> =
            local.into_iter().map(|e| ((vi, e.other), e.meta, vi)).collect();
        if let (Some(org), Some(repo)) = (org, views[vi].org_repo) {
            let ld = match dir {
                Dir::Callers => LinkDir::In,
                Dir::Callees => LinkDir::Out,
            };
            for e in org.index.link_edges(repo, n, ld, Some(kind), &org.visible)? {
                let other = self.view_of_repo(views, org, e.repo)?;
                let ev = self.view_of_repo(views, org, e.evidence_repo)?;
                out.push(((other, e.node), e.meta, ev));
            }
        }
        out.sort_by_cached_key(|(k, _, _)| (views[k.0].node(k.1).qualified_name.clone(), *k));
        Ok(out)
    }

    /// Breadth-first traversal from `root`, level by level, callers before
    /// callees. Each node is visited once per direction; `walk.limit` bounds
    /// the number of discovered nodes (all directions together). Neighbours
    /// rejected by the filter are neither reported nor expanded, and their
    /// edges are dropped. In organization mode the walk follows link edges
    /// into other repositories (RFC 0006 §6).
    fn bfs(
        &self,
        views: &mut Vec<View>,
        org: Option<&OrgCtx>,
        root: Key,
        walk: &TraceWalk<'_>,
    ) -> anyhow::Result<BfsOut> {
        let mut state = BfsState {
            out: BfsOut { nodes: Vec::new(), edges: BTreeMap::new(), truncated: false, tests_excluded: HashSet::new() },
            visited: walk.dirs.iter().map(|&d| (d, HashSet::from([root]))).collect(),
            root,
        };
        let mut frontier: HashMap<Dir, Vec<Key>> = walk.dirs.iter().map(|&d| (d, vec![root])).collect();
        for level in 1..=walk.depth {
            for &dir in walk.dirs {
                let mut current = std::mem::take(frontier.get_mut(&dir).expect("dir"));
                current.sort_by_cached_key(|&k| (views[k.0].node(k.1).qualified_name.clone(), k));
                let mut next = Vec::new();
                for from in current {
                    for &kind in walk.kinds {
                        let hop = Hop { level, dir, from, kind };
                        for nb in self.neighbours(views, org, from, dir, kind)? {
                            state.visit(views, walk, hop, nb, &mut next);
                        }
                    }
                }
                frontier.insert(dir, next);
            }
        }
        Ok(state.out)
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
            match repos.iter_mut().find(|(_, r)| r.repo == v.repo()) {
                Some((_, r)) => r.refs.push(v.snap.ref_name.clone()),
                None => repos.push((vi, repository_summary(v, sel.org.as_ref()))),
            }
        }
        repos.sort_by(|a, b| a.1.repo.cmp(&b.1.repo));
        // `query`: the organization's repository routing, best first.
        if let (Some(q), Some(org)) = (a.query.as_deref().filter(|q| !q.trim().is_empty()), &sel.org) {
            let routes = org.index.route(q, &org.visible, &self.route)?;
            let score: HashMap<usize, f32> = routes.iter().map(|r| (r.repo, r.score)).collect();
            for (vi, r) in &mut repos {
                r.score = views[*vi].org_repo.and_then(|i| score.get(&i)).map(|&s| compact::sig3(f32_to_f64(s)));
            }
            repos.sort_by(|a, b| {
                b.1.score.unwrap_or(0.0).total_cmp(&a.1.score.unwrap_or(0.0)).then(a.1.repo.cmp(&b.1.repo))
            });
        }
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

/// The summary of the repository of view `v` (its own ref only).
fn repository_summary(v: &View, org: Option<&OrgCtx>) -> RepositorySummary {
    let seg = &v.seg;
    let mut languages = BTreeMap::new();
    for l in seg.files().iter().filter_map(|f| f.language) {
        *languages.entry(l.as_str().to_string()).or_insert(0u32) += 1;
    }
    let symbols = seg.nodes().iter().filter(|n| !is_container(n.label)).count() as u32;
    let (packages, depends_on) = match (org, v.org_repo) {
        (Some(org), Some(i)) => org_dependencies(org, i),
        _ => (Vec::new(), Vec::new()),
    };
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
        packages,
        depends_on,
        score: None,
    }
}

/// The packages organization repository `i` publishes and the visible
/// repositories it depends on.
fn org_dependencies(org: &OrgCtx, i: usize) -> (Vec<String>, Vec<String>) {
    let mut packages = Vec::new();
    if let Some(p) = org.index.profile(i) {
        packages = p.deps.packages.iter().map(|p| p.name.clone()).collect();
        packages.sort();
        packages.dedup();
    }
    let generation = org.index.generation();
    let depends_on = match &generation.repos[i].links {
        // Only dependencies the caller may see are named.
        Some(l) => l
            .deps
            .iter()
            .filter(|d| generation.repo_index(&d.repo).is_some_and(|j| org.visible.contains(&j)))
            .map(|d| d.repo.clone())
            .collect(),
        None => Vec::new(),
    };
    (packages, depends_on)
}

/// A node of a trace: (view index, node).
type Key = (usize, NodeId);

/// Edge order in `trace` results: (src qualified name, src, dst qualified
/// name, dst, kind).
type EdgeSortKey = (String, Key, String, Key, EdgeKind);

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

/// What a `trace` walk follows.
struct TraceWalk<'a> {
    dirs: &'a [Dir],
    kinds: &'a [EdgeKind],
    depth: u32,
    min_conf: f32,
    limit: usize,
    filter: &'a NeighbourFilter<'a>,
}

pub(crate) struct BfsOut {
    /// (depth, direction, node), in discovery order.
    pub nodes: Vec<(u32, Dir, Key)>,
    /// (src, dst, kind) → strongest edge metadata and the view its evidence
    /// file belongs to.
    pub edges: BTreeMap<(Key, Key, EdgeKind), (rueca_model::EdgeMeta, usize)>,
    pub truncated: bool,
    /// Test neighbours skipped by the filter.
    pub tests_excluded: HashSet<Key>,
}

/// One expansion step of a trace: from node `from`, at depth `level`, in
/// direction `dir`, along edges of `kind`.
#[derive(Clone, Copy)]
struct Hop {
    level: u32,
    dir: Dir,
    from: Key,
    kind: EdgeKind,
}

/// The state of a trace traversal ([`Engine::bfs`]).
struct BfsState {
    out: BfsOut,
    /// Nodes discovered per direction.
    visited: HashMap<Dir, HashSet<Key>>,
    root: Key,
}

impl BfsState {
    /// A neighbour (node, edge metadata, evidence view) reached by `hop`:
    /// reported and queued in `next` when new, its edge kept when it is
    /// the strongest; dropped below the confidence floor, when the filter
    /// rejects it, or when a new node would exceed the limit.
    fn visit(
        &mut self,
        views: &[View],
        walk: &TraceWalk<'_>,
        hop: Hop,
        nb: (Key, rueca_model::EdgeMeta, usize),
        next: &mut Vec<Key>,
    ) {
        let (other, meta, ev) = nb;
        if meta.confidence < walk.min_conf {
            return;
        }
        let Some(is_new) = self.admit(views, walk, hop, other) else { return };
        if is_new {
            next.push(other);
        }
        let key = match hop.dir {
            Dir::Callers => (other, hop.from, hop.kind),
            Dir::Callees => (hop.from, other, hop.kind),
        };
        self.out
            .edges
            .entry(key)
            .and_modify(|m| {
                if meta.confidence > m.0.confidence {
                    *m = (meta, ev);
                }
            })
            .or_insert((meta, ev));
    }

    /// Whether `other` is reported: `None` when filtered out or over the
    /// limit, else whether it was discovered now.
    fn admit(&mut self, views: &[View], walk: &TraceWalk<'_>, hop: Hop, other: Key) -> Option<bool> {
        if other != self.root
            && let Err(is_test) = walk.filter.check(&views[other.0], other.1)
        {
            if is_test {
                self.out.tests_excluded.insert(other);
            }
            return None;
        }
        if self.visited[&hop.dir].contains(&other) {
            return Some(false);
        }
        if self.out.nodes.len() >= walk.limit {
            self.out.truncated = true;
            return None;
        }
        self.visited.get_mut(&hop.dir).expect("dir").insert(other);
        self.out.nodes.push((hop.level, hop.dir, other));
        Some(true)
    }
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
