//! `rueca contracts`: the `list_contracts` tool, every page of it.
//!
//! The tool pages its answer (at most 1,000 entries, a token budget); the
//! command follows `next_cursor` until the listing ends or `--limit`
//! entries are printed, merging the pages into one envelope (snapshots
//! listed once, `s` renumbered), printed as JSON or as the tool's compact
//! text.

use rueca_query::Engine;
use serde_json::{Map, Value, json};

/// Entries asked per page (the tool's maximum).
const PAGE: usize = 1000;
/// Token budget per page (the tool's maximum): pages are cut by `limit`.
const PAGE_BUDGET: u64 = 1_000_000;

/// `rueca contracts` filters (the tool's arguments).
pub struct Filters {
    pub repo: Option<String>,
    pub ref_name: Option<String>,
    pub side: Option<String>,
    pub kind: Option<String>,
    pub query: Option<String>,
    pub limit: Option<usize>,
}

impl Filters {
    /// The tool arguments, without paging.
    fn args(&self) -> Map<String, Value> {
        let mut m = Map::new();
        let fields = [
            ("repo", &self.repo),
            ("ref", &self.ref_name),
            ("side", &self.side),
            ("kind", &self.kind),
            ("query", &self.query),
        ];
        for (k, v) in fields {
            if let Some(v) = v {
                m.insert(k.into(), Value::String(v.clone()));
            }
        }
        m.insert("budget_tokens".into(), json!(PAGE_BUDGET));
        m
    }
}

/// The listing, merged over pages.
#[derive(Default)]
struct Listing {
    snapshots: Vec<Value>,
    results: Vec<Value>,
    next_cursor: Option<String>,
}

impl Listing {
    /// Adds a page: its snapshots (once each) and its items (`s` renumbered).
    fn add(&mut self, page: &Value) {
        let mut remap = Vec::new();
        for s in page["snapshots"].as_array().into_iter().flatten() {
            remap.push(self.snapshot_index(s));
        }
        for item in page["results"].as_array().into_iter().flatten() {
            let mut item = item.clone();
            if let Some(&s) = item["s"].as_u64().and_then(|s| remap.get(s as usize)) {
                item["s"] = json!(s);
            }
            self.results.push(item);
        }
        self.next_cursor = page["next_cursor"].as_str().map(str::to_string);
    }

    /// The index of snapshot `s` in the merged list, added when new.
    fn snapshot_index(&mut self, s: &Value) -> usize {
        let same = |a: &Value| ["repo", "commit", "ref", "overlay"].iter().all(|k| a[k] == s[k]);
        if let Some(i) = self.snapshots.iter().position(same) {
            return i;
        }
        let mut s = s.clone();
        s["s"] = json!(self.snapshots.len());
        self.snapshots.push(s);
        self.snapshots.len() - 1
    }

    fn envelope(self) -> Value {
        json!({
            "snapshots": self.snapshots,
            "results": self.results,
            "truncated": self.next_cursor.is_some(),
            "next_cursor": self.next_cursor,
        })
    }
}

/// Runs `list_contracts` page by page and prints the merged listing.
pub fn cmd_contracts(engine: &Engine, f: &Filters, json: bool, pretty: bool) -> anyhow::Result<()> {
    let listing = collect(engine, f)?;
    let v = listing.envelope();
    if json {
        println!("{}", crate::render(&v, pretty));
    } else {
        print!("{}", rueca_query::render_text("list_contracts", &v));
    }
    Ok(())
}

/// Every page up to `f.limit` entries.
fn collect(engine: &Engine, f: &Filters) -> anyhow::Result<Listing> {
    let mut listing = Listing::default();
    let mut remaining = f.limit.unwrap_or(usize::MAX);
    let mut args = f.args();
    while remaining > 0 {
        args.insert("limit".into(), json!(remaining.min(PAGE)));
        let page = engine.respond("list_contracts", Value::Object(args.clone()))?.json;
        let before = listing.results.len();
        listing.add(&page);
        remaining = remaining.saturating_sub(listing.results.len() - before);
        match &listing.next_cursor {
            Some(c) => args.insert("cursor".into(), Value::String(c.clone())),
            None => break,
        };
    }
    Ok(listing)
}
