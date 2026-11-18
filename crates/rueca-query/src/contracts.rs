//! `list_contracts` (RFC 0001 §11.4, RFC 0006 phase 4): the contract tables
//! of the selected snapshots — routes, spec operations and RPC methods
//! provided, and the calls to them — with how many counterparts each has
//! among the **visible** repositories.
//!
//! - **Listed**: the rows of the snapshots `repo` / `ref` select (in an
//!   organization, every visible repository at the call's view by default;
//!   locally, the default root).
//! - **Counterparts** (`matches`): the rows of every visible entry the call
//!   reads (in an organization, each visible repository at its entry in the
//!   view, also when `repo` narrows the listing; locally, the selected
//!   snapshots) that match the row's key with `match_keys`: providers of a
//!   consumed key, consumers of a provided key. A row never counts itself
//!   (same snapshot and node).
//! - **Dangling**: consumed rows with no counterpart, computed per query
//!   (nothing is stored).
//! - Sorted by (kind, key, repository, path, line, side, symbol); paged with
//!   `cursor` (an entry offset) like `search_symbols`.

use std::collections::{HashMap, HashSet};

use rueca_model::contract::{match_bucket, match_keys};
use rueca_model::{ContractKind, ContractRow, ContractSide};

use crate::args::{ContractSideArg, ListContractsArgs, clamp_int};
use crate::types::{ContractEntry, ListContractsResult};
use crate::view::View;
use crate::{Engine, Response, Selection, compact};

/// Default and maximum of `limit`.
const DEFAULT_LIMIT: i64 = 50;
const MAX_LIMIT: i64 = 1000;

/// A row of a view: (view index, row).
type RowRef<'a> = (usize, &'a ContractRow);

/// Rows by [`match_bucket`].
type Bucketed<'a> = HashMap<(ContractKind, String), Vec<RowRef<'a>>>;

/// The rows counterparts are counted against, by side.
#[derive(Default)]
struct Counterparts<'a> {
    providers: Bucketed<'a>,
    consumers: Bucketed<'a>,
}

impl<'a> Counterparts<'a> {
    fn build(views: &'a [View]) -> Counterparts<'a> {
        let mut c = Counterparts::default();
        for (vi, v) in views.iter().enumerate() {
            for r in v.seg.contracts() {
                let group = match r.side {
                    ContractSide::Provides => &mut c.providers,
                    ContractSide::Consumes => &mut c.consumers,
                };
                group.entry(match_bucket(r.kind, &r.key)).or_default().push((vi, r));
            }
        }
        c
    }

    /// Counterparts of row `r` of view `vi`.
    fn count(&self, vi: usize, r: &ContractRow) -> u32 {
        let (group, provided) = match r.side {
            ContractSide::Consumes => (&self.providers, false),
            ContractSide::Provides => (&self.consumers, true),
        };
        let matches = |o: &ContractRow| {
            if provided { match_keys(r.kind, &r.key, &o.key) } else { match_keys(r.kind, &o.key, &r.key) }
        };
        let others = group.get(&match_bucket(r.kind, &r.key)).into_iter().flatten();
        others.filter(|(vj, o)| !(*vj == vi && o.node == r.node) && matches(o).is_some()).count() as u32
    }
}

/// What `side`, `kind` and `query` keep (`query` lower-cased).
struct Filter {
    side: Option<ContractSideArg>,
    kind: Option<ContractKind>,
    query: Option<String>,
}

impl Filter {
    fn of(a: &ListContractsArgs) -> Filter {
        let query = a.query.as_deref().map(str::trim).filter(|q| !q.is_empty()).map(str::to_lowercase);
        Filter { side: a.side, kind: a.kind.map(|k| k.to_kind()), query }
    }

    /// Whether row `r` (of `v`) passes the side, kind and text filters.
    fn row(&self, v: &View, r: &ContractRow) -> bool {
        let side = match self.side {
            None => true,
            Some(ContractSideArg::Provided) => r.side == ContractSide::Provides,
            Some(ContractSideArg::Consumed | ContractSideArg::Dangling) => r.side == ContractSide::Consumes,
        };
        side && self.kind.is_none_or(|k| k == r.kind) && self.text(v, r)
    }

    fn text(&self, v: &View, r: &ContractRow) -> bool {
        let Some(q) = &self.query else { return true };
        let symbol = &v.node(r.node).qualified_name;
        [r.key.as_str(), r.raw.as_str(), symbol.as_str()].iter().any(|t| t.to_lowercase().contains(q.as_str()))
    }
}

/// The entry of row `r` of view `v`.
fn entry(v: &View, r: &ContractRow, matches: u32) -> ContractEntry {
    let node = v.node(r.node);
    ContractEntry {
        repo: v.repo().to_string(),
        ref_name: v.snap.ref_name.clone(),
        path: v.seg.files().get(r.file as usize).map(|f| f.path.clone()),
        line: r.span.start_line + 1,
        symbol: node.qualified_name.clone(),
        id: v.symbol_id(r.node),
        label: node.label.as_str().to_string(),
        kind: r.kind.as_str().to_string(),
        side: r.side.as_str().to_string(),
        key: r.key.clone(),
        framework: r.framework.clone(),
        raw: r.raw.clone(),
        matches,
    }
}

/// Every listed entry (rows of the first `listed` views passing `filter`;
/// dangling ones only for `side: dangling`), sorted, with its view.
fn entries(views: &[View], listed: usize, filter: &Filter) -> Vec<(usize, ContractEntry)> {
    let counterparts = Counterparts::build(views);
    let dangling = filter.side == Some(ContractSideArg::Dangling);
    let mut out = Vec::new();
    for (vi, v) in views.iter().enumerate().take(listed) {
        for r in v.seg.contracts().iter().filter(|r| filter.row(v, r)) {
            let n = counterparts.count(vi, r);
            if !dangling || n == 0 {
                out.push((vi, entry(v, r, n)));
            }
        }
    }
    out.sort_by(|(_, a), (_, b)| sort_key(a).cmp(&sort_key(b)));
    out
}

/// (kind, key, repository, path, line, side, symbol, ref).
type SortKey<'e> = (&'e str, &'e str, &'e str, Option<&'e str>, u32, &'e str, &'e str, &'e str);

fn sort_key(e: &ContractEntry) -> SortKey<'_> {
    (&e.kind, &e.key, &e.repo, e.path.as_deref(), e.line, &e.side, &e.symbol, &e.ref_name)
}

impl Engine {
    pub(crate) fn list_contracts(&self, a: ListContractsArgs) -> anyhow::Result<Response> {
        let limit = clamp_int(a.limit, DEFAULT_LIMIT, 1, MAX_LIMIT) as usize;
        let offset = crate::tools::cursor_offset(&a.common, None)?;
        let sel = self.select(&a.common, None)?;
        let listed = sel.views.len();
        let views = self.with_counterpart_views(sel)?;
        let all = entries(&views, listed, &Filter::of(&a));
        let more = all.len() > offset.saturating_add(limit);
        let page: Vec<(usize, ContractEntry)> = all.into_iter().skip(offset).take(limit).collect();
        if a.common.verbose() {
            let contracts = page.into_iter().map(|(_, e)| e).collect();
            let next_offset = more.then_some((offset + limit) as u64);
            return Ok(Response::verbose(serde_json::to_value(ListContractsResult {
                contracts,
                truncated: more,
                next_offset,
            })?));
        }
        let snaps: Vec<_> = views[..listed].iter().map(|v| &v.snap).collect();
        Ok(compact::list_contracts(&snaps, &page, offset, more, crate::tools::budget(&a.common)))
    }

    /// The selection's views, then (organization mode) the entries the
    /// call's view reads of every other visible repository, whose rows are
    /// counterparts too.
    fn with_counterpart_views(&self, sel: Selection) -> anyhow::Result<Vec<View>> {
        let Selection { mut views, org, .. } = sel;
        let Some(org) = org else { return Ok(views) };
        let have: HashSet<_> = views.iter().filter_map(|v| v.org_slot).collect();
        let store = rueca_segment::Store::open(&self.store_dir)?;
        for &i in &org.visible {
            for slot in org.view.slots_of(i).into_iter().filter(|s| !have.contains(s)) {
                self.deadline.check()?;
                views.push(self.slot_view(&store, &org, slot)?);
            }
        }
        Ok(views)
    }
}
