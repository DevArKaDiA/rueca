//! Compact responses (RFC 0005 §4, §7): the envelope, grouping by snapshot and
//! file, short field names, `trace` edges by node index, the text rendering
//! and the token budget.
//!
//! Every builder takes the alpha result of a tool (the `verbose` shapes, see
//! [`crate::types`]) and turns it into the envelope. The budget is counted on
//! the **text rendering** ([`render_text`], what a model reads) as
//! `ceil(bytes / 4)` — model-neutral, no tokenizer (RFC 0005 open question 1).
//! A response over budget keeps the longest prefix of its items that fits
//! (at least one item, so a cursor always makes progress), sets `truncated`
//! and, where the tool can continue, `next_cursor`.

use std::collections::HashMap;
use std::fmt::Write as _;

use serde_json::{Map, Value, json};

use crate::Response;
use crate::snapshot::{SnapshotDesc, short_commit};
use crate::types::*;

/// Default of `budget_tokens`.
pub const DEFAULT_BUDGET_TOKENS: usize = 2000;

/// Approximate tokens of `text`: `ceil(bytes / 4)`.
pub fn count_tokens(text: &str) -> usize {
    text.len().div_ceil(4)
}

/// `x` rounded to 3 significant digits, with the shortest decimal form.
pub fn sig3(x: f64) -> f64 {
    if x == 0.0 || !x.is_finite() {
        return 0.0;
    }
    let mag = x.abs().log10().floor() as i32;
    let factor = 10f64.powi(2 - mag);
    let r = (x * factor).round() / factor;
    let decimals = (2 - mag).max(0) as usize;
    format!("{r:.decimals$}").parse().unwrap_or(r)
}

/// Builds the response for `k` items with `build`, keeping the largest
/// `k` in `[min_k, n]` whose text fits `budget` tokens (`min_k` if none
/// does), and fills in `budget`.
fn fit(min_k: usize, n: usize, budget: usize, mut build: impl FnMut(usize) -> (Value, String)) -> Response {
    let full = build(n);
    if n <= min_k || count_tokens(&full.1) <= budget {
        return finish(full, budget);
    }
    let (mut lo, mut hi) = (min_k + 1, n - 1);
    let mut best = None;
    while lo <= hi {
        let mid = lo + (hi - lo) / 2;
        let r = build(mid);
        if count_tokens(&r.1) <= budget {
            best = Some(r);
            lo = mid + 1;
        } else {
            hi = mid - 1;
        }
    }
    finish(best.unwrap_or_else(|| build(min_k)), budget)
}

fn finish((mut json, text): (Value, String), budget: usize) -> Response {
    json["budget"] = json!({"limit_tokens": budget, "used_tokens": count_tokens(&text)});
    Response { json, text }
}

/// The envelope around `results`; `extra` fields go between `next_cursor`
/// and `budget`.
fn envelope(
    snaps: &[&SnapshotDesc],
    results: Value,
    truncated: bool,
    next_cursor: Option<usize>,
    extra: Vec<(&str, Value)>,
) -> Value {
    let mut m = Map::new();
    m.insert("snapshots".into(), Value::Array(snaps.iter().enumerate().map(|(i, s)| s.to_json(i)).collect()));
    m.insert("results".into(), results);
    m.insert("truncated".into(), Value::Bool(truncated));
    m.insert("next_cursor".into(), next_cursor.map_or(Value::Null, |c| Value::String(c.to_string())));
    for (k, v) in extra {
        m.insert(k.into(), v);
    }
    m.insert("budget".into(), Value::Null);
    Value::Object(m)
}

/// The snapshots that the items' `s` values refer to, renumbered in snapshot
/// order, and the old → new `s` map (RFC 0006 §6: a response lists the
/// snapshots its answer touched, not every snapshot searched). A single
/// selected snapshot is always listed, also for an empty answer.
fn touched<'a>(
    snaps: &[&'a SnapshotDesc],
    used: impl IntoIterator<Item = usize>,
) -> (Vec<&'a SnapshotDesc>, HashMap<usize, usize>) {
    if snaps.len() == 1 {
        return (snaps.to_vec(), HashMap::from([(0, 0)]));
    }
    let mut u: Vec<usize> = used.into_iter().collect();
    u.sort_unstable();
    u.dedup();
    let remap = u.iter().enumerate().map(|(new, &old)| (old, new)).collect();
    (u.iter().map(|&old| snaps[old]).collect(), remap)
}

fn with_text(tool: &str, json: Value) -> (Value, String) {
    let text = render_text(tool, &json);
    (json, text)
}

/// `next_cursor` of a page of `shown` items out of `available` at `offset`,
/// with `more` items beyond what was fetched.
fn next(offset: usize, shown: usize, available: usize, more: bool) -> (bool, Option<usize>) {
    if shown < available || more { (true, Some(offset + shown)) } else { (false, None) }
}

fn lines_of(start: Option<u32>, end: Option<u32>) -> Option<Value> {
    match (start, end) {
        (Some(s), Some(e)) => Some(json!([s, e])),
        _ => None,
    }
}

fn put(m: &mut Map<String, Value>, k: &str, v: impl Into<Value>) {
    m.insert(k.into(), v.into());
}

fn put_opt(m: &mut Map<String, Value>, k: &str, v: Option<impl Into<Value>>) {
    if let Some(v) = v {
        m.insert(k.into(), v.into());
    }
}

/// `{id, qn, label, lines}`; `path` too when `with_path`.
fn brief(b: &Brief, with_path: bool) -> Map<String, Value> {
    let mut m = Map::new();
    put(&mut m, "id", b.id.clone());
    put(&mut m, "qn", b.qualified_name.clone());
    put(&mut m, "label", b.label.clone());
    if with_path {
        put_opt(&mut m, "path", b.path.clone());
    }
    put_opt(&mut m, "lines", lines_of(b.start_line, b.end_line));
    m
}

fn briefs(v: &[Brief]) -> Value {
    Value::Array(v.iter().map(|b| Value::Object(brief(b, false))).collect())
}

/// `{id, name, qn, label, lines, sig}`: a symbol inside a file group.
fn symbol_item(b: &Brief, sig: Option<&String>) -> Map<String, Value> {
    let mut m = Map::new();
    put(&mut m, "id", b.id.clone());
    put(&mut m, "name", b.name.clone());
    put(&mut m, "qn", b.qualified_name.clone());
    put(&mut m, "label", b.label.clone());
    put_opt(&mut m, "lines", lines_of(b.start_line, b.end_line));
    put_opt(&mut m, "sig", sig.cloned());
    m
}

/// Groups `(s, path, item)` by `(s, path)` in order of first appearance.
fn group(items: impl IntoIterator<Item = (usize, Option<String>, Value)>, key: &str) -> Value {
    let mut groups: Vec<(usize, Option<String>, Vec<Value>)> = Vec::new();
    let mut index: HashMap<(usize, Option<String>), usize> = HashMap::new();
    for (s, path, v) in items {
        let i = *index.entry((s, path.clone())).or_insert_with(|| {
            groups.push((s, path, Vec::new()));
            groups.len() - 1
        });
        groups[i].2.push(v);
    }
    Value::Array(groups.into_iter().map(|(s, path, items)| json!({"s": s, "path": path, key: items})).collect())
}

pub(crate) fn search_symbols(
    snaps: &[&SnapshotDesc],
    hits: &[(usize, SymbolSearchHit)],
    offset: usize,
    more: bool,
    scores: bool,
    budget: usize,
) -> Response {
    let n = hits.len();
    fit(n.min(1), n, budget, |k| {
        let (listed, remap) = touched(snaps, hits[..k].iter().map(|(s, _)| *s));
        let items = hits[..k].iter().map(|(s, h)| {
            let mut m = symbol_item(&h.brief, h.signature.as_ref());
            if scores {
                put(&mut m, "score", sig3(h.score));
            }
            (remap[s], h.brief.path.clone(), Value::Object(m))
        });
        let (truncated, cursor) = next(offset, k, n, more);
        with_text("search_symbols", envelope(&listed, group(items, "symbols"), truncated, cursor, vec![]))
    })
}

pub(crate) fn search_code(
    snaps: &[&SnapshotDesc],
    matches: &[(usize, CodeMatch)],
    offset: usize,
    more: bool,
    files_scanned: u32,
    budget: usize,
) -> Response {
    let n = matches.len();
    fit(n.min(1), n, budget, |k| {
        let (listed, remap) = touched(snaps, matches[..k].iter().map(|(s, _)| *s));
        let items = matches[..k].iter().map(|(s, cm)| {
            let mut m = Map::new();
            put(&mut m, "line", cm.line);
            put(&mut m, "text", cm.text.clone());
            put_opt(&mut m, "before", cm.before.clone().filter(|v| !v.is_empty()));
            put_opt(&mut m, "after", cm.after.clone().filter(|v| !v.is_empty()));
            put_opt(&mut m, "symbol", cm.symbol.as_ref().map(|b| Value::Object(brief(b, false))));
            (remap[s], Some(cm.path.clone()), Value::Object(m))
        });
        let (truncated, cursor) = next(offset, k, n, more);
        let extra = vec![("files_scanned", json!(files_scanned))];
        with_text("search_code", envelope(&listed, group(items, "matches"), truncated, cursor, extra))
    })
}

pub(crate) fn get_symbol(snap: &SnapshotDesc, r: &GetSymbolResult, budget: usize) -> Response {
    // Lists are cut in this order of priority when over budget.
    let lists: [(&str, &[Brief]); 4] =
        [("children", &r.children), ("bases", &r.bases), ("subtypes", &r.subtypes), ("candidates", &r.candidates)];
    let total: usize = lists.iter().map(|(_, l)| l.len()).sum();
    fit(0, total, budget, |k| {
        let s = &r.symbol;
        let mut sym = Map::new();
        put(&mut sym, "id", s.brief.id.clone());
        put(&mut sym, "name", s.brief.name.clone());
        put(&mut sym, "qn", s.brief.qualified_name.clone());
        put(&mut sym, "label", s.brief.label.clone());
        put_opt(&mut sym, "path", s.brief.path.clone());
        put_opt(&mut sym, "lines", lines_of(s.brief.start_line, s.brief.end_line));
        put_opt(&mut sym, "lang", s.language.clone());
        put_opt(&mut sym, "sig", s.signature.clone());
        put_opt(&mut sym, "doc", s.doc.clone());
        put(&mut sym, "exported", s.exported);
        put(&mut sym, "test", s.is_test);
        let mut res = Map::new();
        put(&mut res, "s", 0);
        put(&mut res, "symbol", Value::Object(sym));
        put(&mut res, "parent", r.parent.as_ref().map_or(Value::Null, |b| Value::Object(brief(b, false))));
        let mut left = k;
        let mut omitted = Map::new();
        for (name, list) in lists {
            let keep = left.min(list.len());
            left -= keep;
            put(&mut res, name, briefs(&list[..keep]));
            if keep < list.len() {
                put(&mut omitted, name, list.len() - keep);
            }
        }
        // Counts go after the lists (text order), candidates last.
        let candidates = res.remove("candidates").unwrap_or(Value::Array(vec![]));
        put(&mut res, "callers", r.callers_count);
        put(&mut res, "callees", r.callees_count);
        put_opt(&mut res, "readers", r.readers_count);
        put_opt(&mut res, "writers", r.writers_count);
        put(&mut res, "candidates", candidates);
        let extra = if omitted.is_empty() { vec![] } else { vec![("omitted", Value::Object(omitted))] };
        with_text("get_symbol", envelope(&[snap], Value::Object(res), k < total, None, extra))
    })
}

pub(crate) fn get_snippet(snap: &SnapshotDesc, r: &SnippetResult, budget: usize) -> Response {
    let lines: Vec<&str> = if r.end_line < r.start_line { Vec::new() } else { r.text.split('\n').collect() };
    let n = lines.len();
    fit(n.min(1), n, budget, |k| {
        let end = r.start_line + k as u32 - u32::from(k > 0);
        let end = if k == 0 { r.end_line } else { end };
        let mut res = Map::new();
        put(&mut res, "s", 0);
        put(&mut res, "path", r.path.clone());
        put_opt(&mut res, "lang", r.language.clone());
        put(&mut res, "lines", json!([r.start_line, end]));
        put(&mut res, "text", lines[..k].join("\n"));
        put_opt(&mut res, "symbol", r.symbol.as_ref().map(|b| Value::Object(brief(b, false))));
        let cursor = if k < n {
            Some(r.start_line as usize + k)
        } else if r.truncated {
            Some(r.end_line as usize + 1)
        } else {
            None
        };
        with_text("get_snippet", envelope(&[snap], Value::Object(res), cursor.is_some(), cursor, vec![]))
    })
}

/// A `trace` node merged over directions.
struct TNode<'a> {
    brief: &'a Brief,
    depth: u32,
    dir: &'static str,
    view: usize,
}

/// `trace` over one snapshot, or several when the walk followed
/// cross-repository link edges: then every node carries its snapshot `s`
/// (RFC 0006 §6) and the envelope lists the snapshots the answer touched.
pub(crate) fn trace(snaps: &[&SnapshotDesc], r: &TraceResult, offset: usize, budget: usize) -> Response {
    let (listed, remap) = touched(snaps, std::iter::once(r.root_view).chain(r.nodes.iter().map(|n| n.view)));
    let multi = listed.len() > 1;
    // One entry per distinct node: `dir` is in / out / both, `d` the smallest depth.
    let mut uniq: Vec<TNode<'_>> = Vec::new();
    let mut pos: HashMap<&str, usize> = HashMap::new();
    for n in &r.nodes {
        let dir = if n.direction == "callers" { "in" } else { "out" };
        match pos.get(n.brief.id.as_str()) {
            Some(&i) => {
                let u = &mut uniq[i];
                if u.dir != dir {
                    u.dir = "both";
                }
                u.depth = u.depth.min(n.depth);
            }
            None => {
                pos.insert(&n.brief.id, uniq.len());
                uniq.push(TNode { brief: &n.brief, depth: n.depth, dir, view: n.view });
            }
        }
    }
    // Global position: root 0, node i → i + 1.
    let gpos = |id: &str| -> Option<usize> { if id == r.root.id { Some(0) } else { pos.get(id).map(|i| i + 1) } };
    let start = offset.min(uniq.len());
    let n = uniq.len() - start;
    fit(n.min(1), n, budget, |k| {
        let page = start..start + k;
        let node_json = |t: &TNode<'_>| {
            let mut m = Map::new();
            if multi {
                put(&mut m, "s", remap[&t.view]);
            }
            m.extend(brief(t.brief, true));
            put(&mut m, "d", t.depth);
            put(&mut m, "dir", t.dir);
            Value::Object(m)
        };
        let mut root = Map::new();
        if multi {
            put(&mut root, "s", remap[&r.root_view]);
        }
        root.extend(brief(&r.root, true));
        put(&mut root, "d", 0);
        let mut nodes: Vec<Value> = vec![Value::Object(root)];
        let mut local: HashMap<usize, usize> = HashMap::from([(0, 0)]);
        for i in page.clone() {
            local.insert(i + 1, nodes.len());
            nodes.push(node_json(&uniq[i]));
        }
        let path_of = |g: usize| if g == 0 { r.root.path.as_deref() } else { uniq[g - 1].brief.path.as_deref() };
        let mut edges = Vec::new();
        for e in &r.edges {
            let (Some(gs), Some(gd)) = (gpos(&e.src), gpos(&e.dst)) else { continue };
            // An edge belongs to the page of its later endpoint.
            let later = gs.max(gd);
            let on_page = if later == 0 { start == 0 } else { page.contains(&(later - 1)) };
            if !on_page {
                continue;
            }
            let mut index_of = |g: usize| -> usize {
                if let Some(&l) = local.get(&g) {
                    return l;
                }
                // An endpoint listed on an earlier page is repeated here.
                local.insert(g, nodes.len());
                nodes.push(node_json(&uniq[g - 1]));
                nodes.len() - 1
            };
            let (si, di) = (index_of(gs), index_of(gd));
            let mut edge = vec![json!(si), json!(di), json!(e.kind), json!(e.confidence), json!(e.strategy)];
            match &e.call_site {
                Some(cs) => {
                    edge.push(json!(cs.line));
                    if Some(cs.path.as_str()) != path_of(gs) {
                        edge.push(json!(cs.path));
                    }
                }
                None => edge.push(Value::Null),
            }
            edges.push(Value::Array(edge));
        }
        let res = json!({"s": remap[&r.root_view], "nodes": nodes, "edges": edges, "tests_excluded": r.tests_excluded});
        let (truncated, cursor) = next(start, k, n, r.truncated);
        with_text("trace", envelope(&listed, res, truncated, cursor, vec![]))
    })
}

pub(crate) fn outline(snap: &SnapshotDesc, r: &FileOutlineResult, offset: usize, budget: usize) -> Response {
    // Pre-order: (parent index, symbol).
    fn flatten<'a>(
        syms: &'a [OutlineSymbol],
        parent: Option<usize>,
        out: &mut Vec<(Option<usize>, &'a OutlineSymbol)>,
    ) {
        for s in syms {
            let i = out.len();
            out.push((parent, s));
            flatten(&s.children, Some(i), out);
        }
    }
    let mut flat = Vec::new();
    flatten(&r.symbols, None, &mut flat);
    let start = offset.min(flat.len());
    let n = flat.len() - start;
    fit(n.min(1), n, budget, |k| {
        let page = start..start + k;
        let mut children: Vec<Vec<usize>> = vec![Vec::new(); k];
        let mut roots = Vec::new();
        for i in page.clone() {
            match flat[i].0 {
                Some(p) if page.contains(&p) => children[p - start].push(i - start),
                _ => roots.push(i - start),
            }
        }
        fn build(j: usize, start: usize, flat: &[(Option<usize>, &OutlineSymbol)], children: &[Vec<usize>]) -> Value {
            let s = flat[start + j].1;
            let mut m = symbol_item(&s.brief, s.signature.as_ref());
            if !children[j].is_empty() {
                let kids = children[j].iter().map(|&c| build(c, start, flat, children)).collect();
                put(&mut m, "children", Value::Array(kids));
            }
            Value::Object(m)
        }
        let symbols: Vec<Value> = roots.iter().map(|&j| build(j, start, &flat, &children)).collect();
        let mut res = Map::new();
        put(&mut res, "s", 0);
        put(&mut res, "path", r.path.clone());
        put_opt(&mut res, "lang", r.language.clone());
        put(&mut res, "line_count", r.line_count);
        put(&mut res, "symbols", Value::Array(symbols));
        let (truncated, cursor) = next(start, k, n, false);
        with_text("get_file_outline", envelope(&[snap], Value::Object(res), truncated, cursor, vec![]))
    })
}

pub(crate) fn list_repositories(
    snaps: &[&SnapshotDesc],
    repos: &[(usize, RepositorySummary)],
    org: Option<&str>,
    offset: usize,
    budget: usize,
) -> Response {
    let start = offset.min(repos.len());
    let page = &repos[start..];
    let n = page.len();
    fit(n.min(1), n, budget, |k| {
        let (listed, remap) = touched(snaps, page[..k].iter().map(|(s, _)| *s));
        let items: Vec<Value> = page[..k]
            .iter()
            .map(|(s, r)| {
                let mut m = Map::new();
                put(&mut m, "s", remap[s]);
                put(&mut m, "repo", r.repo.clone());
                put(&mut m, "default_ref", r.default_ref.clone());
                put(&mut m, "refs", json!(r.refs));
                put(&mut m, "languages", json!(r.languages));
                put(&mut m, "files", r.files);
                put(&mut m, "symbols", r.symbols);
                put(&mut m, "edges", r.edges);
                if !r.packages.is_empty() {
                    put(&mut m, "packages", json!(r.packages));
                }
                if !r.depends_on.is_empty() {
                    put(&mut m, "depends_on", json!(r.depends_on));
                }
                put_opt(&mut m, "score", r.score);
                Value::Object(m)
            })
            .collect();
        let (truncated, cursor) = next(start, k, n, false);
        let extra = vec![("org", org.map_or(Value::Null, |o| Value::String(o.to_string())))];
        with_text("list_repositories", envelope(&listed, Value::Array(items), truncated, cursor, extra))
    })
}

// ---------------------------------------------------------------------------
// Text rendering
// ---------------------------------------------------------------------------

/// The compact text rendering of a tool response (MCP text content, CLI
/// `--text`). A `verbose` response (no envelope) renders as compact JSON.
///
/// The header names each snapshot as `repo@commit12[+] (ref)`; items show
/// the **local** name of the symbol id, so an item's id is
/// `<header repo@commit>:<group path>#<name>`, and the name also works as a
/// `qualified_name` argument. Lines are `L<start>-<end>`.
pub fn render_text(tool: &str, v: &Value) -> String {
    let Some(snaps) = v.get("snapshots").and_then(Value::as_array) else {
        return v.to_string();
    };
    let multi = snaps.len() > 1;
    let mut out = String::new();
    for s in snaps {
        if multi {
            let _ = write!(out, "[{}] ", s["s"]);
        }
        out.push_str(&snapshot_header(s));
        out.push('\n');
    }
    let r = &v["results"];
    match tool {
        "search_symbols" => text_search_symbols(&mut out, r, multi),
        "search_code" => text_search_code(&mut out, r, multi),
        "get_symbol" => text_get_symbol(&mut out, r),
        "get_snippet" => text_get_snippet(&mut out, r),
        "trace" => text_trace(&mut out, r),
        "get_file_outline" => text_outline(&mut out, r),
        "list_repositories" => text_repositories(&mut out, r, multi),
        _ => out.push_str(&r.to_string()),
    }
    if let Some(o) = v.get("omitted").and_then(Value::as_object) {
        let parts: Vec<String> = o.iter().map(|(k, n)| format!("{n} {k}")).collect();
        let _ = writeln!(out, "(omitted: {})", parts.join(", "));
    }
    if v["truncated"] == true {
        match v["next_cursor"].as_str() {
            Some(c) => {
                let _ = writeln!(out, "(truncated; continue with \"cursor\": \"{c}\")");
            }
            None => out.push_str("(truncated)\n"),
        }
    }
    out
}

fn snapshot_header(s: &Value) -> String {
    let commit = s["commit"].as_str().unwrap_or_default();
    let plus = if s["overlay"] == true { "+" } else { "" };
    format!(
        "{}@{}{plus} ({})",
        s["repo"].as_str().unwrap_or_default(),
        short_commit(commit),
        s["ref"].as_str().unwrap_or_default()
    )
}

fn str_of<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or_default()
}

/// `L3-9`, `L3`, or empty.
fn span(v: &Value) -> String {
    match v.get("lines").and_then(Value::as_array).map(|a| (a[0].as_u64(), a.get(1).and_then(Value::as_u64))) {
        Some((Some(s), Some(e))) if s == e => format!("L{s}"),
        Some((Some(s), Some(e))) => format!("L{s}-{e}"),
        _ => String::new(),
    }
}

/// Local name of an item (the `#` part of its id), else its path / name.
fn local_name(v: &Value) -> String {
    let id = str_of(v, "id");
    match id.split_once('#') {
        Some((_, local)) if !local.is_empty() => local.to_string(),
        _ => match rueca_model::parse_symbol_id(id) {
            Some(p) => p.path,
            None => str_of(v, "name").to_string(),
        },
    }
}

fn id_path(v: &Value) -> Option<String> {
    rueca_model::parse_symbol_id(str_of(v, "id")).map(|p| p.path)
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `Label name L3-9`, with ` path` before the lines when it differs from `here`.
fn brief_text(b: &Value, here: Option<&str>) -> String {
    let mut s = format!("{} {}", str_of(b, "label"), local_name(b));
    let path = b.get("path").and_then(Value::as_str).map(str::to_string).or_else(|| id_path(b));
    if let Some(p) = path.as_deref()
        && Some(p) != here
        && p != local_name(b)
    {
        s.push(' ');
        s.push_str(p);
    }
    let sp = span(b);
    if !sp.is_empty() {
        s.push(' ');
        s.push_str(&sp);
    }
    s
}

fn group_header(out: &mut String, g: &Value, multi: bool) {
    if multi {
        let _ = write!(out, "[{}] ", g["s"]);
    }
    out.push_str(g["path"].as_str().unwrap_or("(no file)"));
    out.push('\n');
}

fn text_search_symbols(out: &mut String, r: &Value, multi: bool) {
    for g in r.as_array().into_iter().flatten() {
        group_header(out, g, multi);
        for s in g["symbols"].as_array().into_iter().flatten() {
            let _ = write!(out, "  {} {}", str_of(s, "label"), local_name(s));
            let sp = span(s);
            if !sp.is_empty() {
                let _ = write!(out, " {sp}");
            }
            if let Some(sig) = s.get("sig").and_then(Value::as_str) {
                let _ = write!(out, "  {}", one_line(sig));
            }
            if let Some(score) = s.get("score") {
                let _ = write!(out, "  ({score})");
            }
            out.push('\n');
        }
    }
}

fn text_search_code(out: &mut String, r: &Value, multi: bool) {
    for g in r.as_array().into_iter().flatten() {
        group_header(out, g, multi);
        let here = g["path"].as_str();
        let mut current: Option<&str> = None;
        for m in g["matches"].as_array().into_iter().flatten() {
            let sym = m.get("symbol");
            let sid = sym.map(|s| str_of(s, "id"));
            if sid != current {
                match sym {
                    Some(s) => {
                        let _ = writeln!(out, "  [{}]", brief_text(s, here));
                    }
                    None => out.push_str("  [top level]\n"),
                }
                current = sid;
            }
            let line = m["line"].as_u64().unwrap_or(0);
            let before = m.get("before").and_then(Value::as_array).cloned().unwrap_or_default();
            for (i, b) in before.iter().enumerate() {
                let n = line.saturating_sub((before.len() - i) as u64);
                let _ = writeln!(out, "  {n}- {}", b.as_str().unwrap_or_default());
            }
            let _ = writeln!(out, "  {line}: {}", str_of(m, "text"));
            for (i, a) in m.get("after").and_then(Value::as_array).into_iter().flatten().enumerate() {
                let _ = writeln!(out, "  {}- {}", line + 1 + i as u64, a.as_str().unwrap_or_default());
            }
        }
    }
}

fn text_get_symbol(out: &mut String, r: &Value) {
    let s = &r["symbol"];
    let here = s.get("path").and_then(Value::as_str);
    let _ = write!(out, "{} {}", str_of(s, "label"), local_name(s));
    if let Some(p) = here {
        let _ = write!(out, "  {p}");
    }
    let sp = span(s);
    if !sp.is_empty() {
        let _ = write!(out, " {sp}");
    }
    if let Some(l) = s.get("lang").and_then(Value::as_str) {
        let _ = write!(out, "  {l}");
    }
    if s["exported"] == true {
        out.push_str(" exported");
    }
    if s["test"] == true {
        out.push_str(" test");
    }
    out.push('\n');
    if str_of(s, "qn") != local_name(s) {
        let _ = writeln!(out, "  qn: {}", str_of(s, "qn"));
    }
    if let Some(sig) = s.get("sig").and_then(Value::as_str) {
        let _ = writeln!(out, "  {}", one_line(sig));
    }
    if let Some(doc) = s.get("doc").and_then(Value::as_str) {
        let doc = doc.trim().replace('\n', "\n    ");
        let _ = writeln!(out, "  doc: {doc}");
    }
    if r["parent"].is_object() {
        let _ = writeln!(out, "  parent: {}", brief_text(&r["parent"], here));
    }
    for name in ["children", "bases", "subtypes"] {
        list_text(out, name, &r[name], here);
    }
    let _ = write!(out, "  callers {}, callees {}", r["callers"], r["callees"]);
    for k in ["readers", "writers"] {
        if let Some(n) = r.get(k) {
            let _ = write!(out, ", {k} {n}");
        }
    }
    out.push('\n');
    list_text(out, "candidates", &r["candidates"], here);
}

fn list_text(out: &mut String, name: &str, list: &Value, here: Option<&str>) {
    let Some(items) = list.as_array().filter(|a| !a.is_empty()) else { return };
    let _ = writeln!(out, "  {name}:");
    for b in items {
        let _ = writeln!(out, "    {}", brief_text(b, here));
    }
}

fn text_get_snippet(out: &mut String, r: &Value) {
    let _ = write!(out, "{} {}", str_of(r, "path"), span(r));
    if let Some(s) = r.get("symbol") {
        let _ = write!(out, " ({} {})", str_of(s, "label"), local_name(s));
    }
    out.push('\n');
    out.push_str(str_of(r, "text"));
    out.push('\n');
}

fn text_trace(out: &mut String, r: &Value) {
    let nodes = r["nodes"].as_array().cloned().unwrap_or_default();
    let edges = r["edges"].as_array().cloned().unwrap_or_default();
    for (i, n) in nodes.iter().enumerate() {
        let _ = write!(out, "{i}");
        if let Some(s) = n.get("s") {
            let _ = write!(out, " [{s}]");
        }
        if let Some(dir) = n.get("dir").and_then(Value::as_str) {
            let _ = write!(out, " {dir} d{}", n["d"]);
        }
        let _ = write!(out, " {} {}", str_of(n, "label"), local_name(n));
        if let Some(p) = n.get("path").and_then(Value::as_str)
            && p != local_name(n)
        {
            let _ = write!(out, "  {p}");
        }
        let sp = span(n);
        if !sp.is_empty() {
            let _ = write!(out, " {sp}");
        }
        // Each edge is printed on its later endpoint: `→j` (this node calls
        // j) or `←j` (j calls this node).
        for e in &edges {
            let (Some(src), Some(dst)) = (e[0].as_u64(), e[1].as_u64()) else { continue };
            let (src, dst) = (src as usize, dst as usize);
            if src.max(dst) != i {
                continue;
            }
            let (arrow, other) = if src == i { ("→", dst) } else { ("←", src) };
            let _ = write!(
                out,
                "  {arrow}{other} {} {} {}",
                e[2].as_str().unwrap_or_default(),
                e[3],
                e[4].as_str().unwrap_or_default()
            );
            if let Some(line) = e.get(5).and_then(Value::as_u64) {
                let _ = write!(out, " @{line}");
            }
            if let Some(p) = e.get(6).and_then(Value::as_str) {
                let _ = write!(out, " {p}");
            }
        }
        out.push('\n');
    }
    if let Some(t) = r["tests_excluded"].as_u64().filter(|&t| t > 0) {
        let _ = writeln!(out, "({t} tests excluded; `include_tests: true` lists them)");
    }
}

fn text_repositories(out: &mut String, r: &Value, multi: bool) {
    for item in r.as_array().into_iter().flatten() {
        if multi {
            let _ = write!(out, "[{}] ", item["s"]);
        }
        let refs: Vec<&str> = item["refs"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        let _ = write!(
            out,
            "{}  refs {}  {} files, {} symbols, {} edges",
            str_of(item, "repo"),
            refs.join(", "),
            item["files"],
            item["symbols"],
            item["edges"]
        );
        let mut langs: Vec<(&String, u64)> =
            item["languages"].as_object().into_iter().flatten().map(|(k, v)| (k, v.as_u64().unwrap_or(0))).collect();
        langs.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        if !langs.is_empty() {
            let parts: Vec<String> = langs.iter().map(|(l, n)| format!("{l} {n}")).collect();
            let _ = write!(out, "  {}", parts.join(", "));
        }
        let list =
            |k: &str| -> Vec<&str> { item[k].as_array().into_iter().flatten().filter_map(Value::as_str).collect() };
        if !list("packages").is_empty() {
            let _ = write!(out, "  packages {}", list("packages").join(", "));
        }
        if !list("depends_on").is_empty() {
            let _ = write!(out, "  depends on {}", list("depends_on").join(", "));
        }
        if let Some(score) = item.get("score") {
            let _ = write!(out, "  ({score})");
        }
        out.push('\n');
    }
}

fn text_outline(out: &mut String, r: &Value) {
    let _ = write!(out, "{}", str_of(r, "path"));
    let lang = r.get("lang").and_then(Value::as_str);
    let _ = writeln!(out, " ({}{} lines)", lang.map(|l| format!("{l}, ")).unwrap_or_default(), r["line_count"]);
    fn rec(out: &mut String, syms: &Value, depth: usize) {
        for s in syms.as_array().into_iter().flatten() {
            let _ = write!(out, "{:indent$}{} {}", "", str_of(s, "label"), local_name(s), indent = depth * 2);
            let sp = span(s);
            if !sp.is_empty() {
                let _ = write!(out, " {sp}");
            }
            if let Some(sig) = s.get("sig").and_then(Value::as_str) {
                let _ = write!(out, "  {}", one_line(sig));
            }
            out.push('\n');
            if let Some(c) = s.get("children") {
                rec(out, c, depth + 1);
            }
        }
    }
    rec(out, &r["symbols"], 1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn significant_digits() {
        assert_eq!(sig3(116.36391), 116.0);
        assert_eq!(sig3(12.3456), 12.3);
        assert_eq!(sig3(0.016393), 0.0164);
        assert_eq!(sig3(1234.5), 1230.0);
        assert_eq!(sig3(0.0), 0.0);
    }

    #[test]
    fn fit_keeps_the_longest_prefix() {
        // Each item is 40 bytes = 10 tokens.
        let build = |k: usize| (json!({"k": k}), "x".repeat(40 * k));
        let r = fit(1, 10, 35, build);
        assert_eq!(r.json["k"], 3);
        assert_eq!(r.json["budget"], json!({"limit_tokens": 35, "used_tokens": 30}));
        // Nothing fits: the minimum is kept.
        assert_eq!(fit(1, 10, 5, build).json["k"], 1);
        assert_eq!(fit(0, 10, 5, build).json["k"], 0);
        // Everything fits.
        assert_eq!(fit(1, 10, 1000, build).json["k"], 10);
        assert_eq!(fit(0, 0, 10, build).json["k"], 0);
    }

    #[test]
    fn grouping_keeps_first_appearance_order() {
        let g = group(
            vec![
                (0, Some("a".to_string()), json!(1)),
                (0, Some("b".to_string()), json!(2)),
                (0, Some("a".to_string()), json!(3)),
                (1, Some("a".to_string()), json!(4)),
            ],
            "items",
        );
        assert_eq!(
            g,
            json!([
                {"s": 0, "path": "a", "items": [1, 3]},
                {"s": 0, "path": "b", "items": [2]},
                {"s": 1, "path": "a", "items": [4]}
            ])
        );
    }
}
