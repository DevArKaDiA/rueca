//! The organization symbol index (RFC 0006 phase 1): one ranking over
//! several repositories with comparable scores, incremental replacement,
//! generations with an atomic pointer swap, and the visibility filter.

use std::collections::HashMap;

use rueca_model::{FileEntry, Label, Language, Node, ResolvedGraph, SnapshotInfo, Span, git_blob_sha};
use rueca_segment::org::{self, OrgIndex, OrgMember, OrgScope};
use rueca_segment::{ContentSource, Segment, Store, SymbolQuery, search_symbols_merged, write_segment};

struct Mem(HashMap<String, Vec<u8>>);

impl ContentSource for Mem {
    fn content(&self, file: &FileEntry) -> std::io::Result<Vec<u8>> {
        self.0.get(&file.path).cloned().ok_or_else(|| std::io::Error::other(file.path.clone()))
    }
}

/// A graph of Python files whose `def name(` lines become functions
/// spanning to the next `def` (or the end of the file).
fn graph(repo: &str, commit: &str, root: &str, files: &[(&str, &str)]) -> (ResolvedGraph, Mem) {
    let mut g = ResolvedGraph {
        snapshot: SnapshotInfo {
            repo: repo.into(),
            commit: commit.into(),
            dirty: false,
            branch: Some("main".into()),
            root: Some(root.into()),
            indexed_at: 1_700_000_000,
            rueca_version: "test".into(),
        },
        nodes: vec![],
        edges: vec![],
        files: vec![],
        modules: vec![],
    };
    let mut mem = HashMap::new();
    for (path, src) in files {
        let fid = g.files.len() as u32;
        let fnode = g.nodes.len() as u32;
        g.nodes.push(Node {
            label: Label::File,
            name: path.rsplit('/').next().unwrap().into(),
            qualified_name: (*path).into(),
            file: Some(fid),
            span: None,
            language: Some(Language::Python),
            signature: None,
            doc: None,
            exported: true,
            is_test: false,
        });
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
        let starts: Vec<usize> = std::iter::once(0).chain(src.match_indices('\n').map(|(i, _)| i + 1)).collect();
        let defs: Vec<(usize, &str)> = src
            .lines()
            .enumerate()
            .filter_map(|(i, l)| l.strip_prefix("def ").and_then(|r| r.split('(').next()).map(|n| (i, n)))
            .collect();
        let module = path.trim_end_matches(".py").replace('/', ".");
        for (k, &(line, name)) in defs.iter().enumerate() {
            let last = defs.get(k + 1).map_or(starts.len() - 1, |d| d.0);
            let end = starts.get(last).copied().unwrap_or(src.len()).min(src.len());
            g.nodes.push(Node {
                label: Label::Function,
                name: name.into(),
                qualified_name: format!("{module}.{name}"),
                file: Some(fid),
                span: Some(Span {
                    start_byte: starts[line] as u32,
                    end_byte: end as u32,
                    start_line: line as u32,
                    start_col: 0,
                    end_line: last.saturating_sub(1).max(line) as u32,
                    end_col: 0,
                }),
                language: Some(Language::Python),
                signature: Some(format!("def {name}()")),
                doc: None,
                exported: true,
                is_test: false,
            });
        }
    }
    g.validate().unwrap();
    (g, Mem(mem))
}

const BILLING: &[(&str, &str)] = &[
    (
        "billing/invoice.py",
        "def issue_invoice(order):\n    total = order.amount\n    return total\n\ndef cancel_invoice(inv):\n    return refund(inv)\n",
    ),
    ("billing/refund.py", "def refund(inv):\n    return inv.amount\n"),
];
const PAYMENTS: &[(&str, &str)] = &[
    ("payments/charge.py", "def charge_card(card, amount):\n    return gateway.charge(card, amount)\n"),
    (
        "payments/refund.py",
        "def refund_payment(payment):\n    return gateway.refund(payment)\n\ndef refund(p):\n    return refund_payment(p)\n",
    ),
];
const SHIPPING: &[(&str, &str)] = &[(
    "shipping/label.py",
    "def print_label(order):\n    return order.address\n\ndef invoice_number(order):\n    return order.id\n",
)];

struct Org {
    _dir: tempfile::TempDir,
    store: Store,
    roots: Vec<String>,
}

impl Org {
    fn root(&self, i: usize) -> &str {
        &self.roots[i]
    }

    fn index(&self, i: usize, repo: &str, commit: &str, files: &[(&str, &str)]) {
        let (g, mem) = graph(repo, commit, self.root(i), files);
        let meta = write_segment(&g, &mem, &self.store.dir).unwrap();
        self.store.register(self.root(i), &meta).unwrap();
    }

    fn members(&self, which: &[usize]) -> Vec<OrgMember> {
        which.iter().map(|&i| OrgMember { root: self.root(i).to_string(), url: None }).collect()
    }

    fn segments(&self, idx: &OrgIndex) -> Vec<Segment> {
        idx.generation().repos.iter().map(|r| self.store.open_segment(&r.segment_id).unwrap()).collect()
    }
}

fn org3() -> Org {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("store")).unwrap();
    let mut roots = Vec::new();
    for name in ["billing", "payments", "shipping"] {
        let p = dir.path().join(name);
        std::fs::create_dir_all(&p).unwrap();
        roots.push(std::fs::canonicalize(&p).unwrap().to_string_lossy().into_owned());
    }
    let o = Org { _dir: dir, store, roots };
    o.index(0, "acme/billing", "c1", BILLING);
    o.index(1, "acme/payments", "c2", PAYMENTS);
    o.index(2, "acme/shipping", "c3", SHIPPING);
    o
}

fn q(text: &str) -> SymbolQuery {
    SymbolQuery { text: text.into(), limit: 50, ..Default::default() }
}

/// `(repo id, qualified name, score)` of an organization search.
fn org_search(
    idx: &OrgIndex,
    segs: &[Segment],
    text: &str,
    visible: &[usize],
    selected: &[usize],
) -> Vec<(String, String, f32)> {
    let sel: Vec<(usize, &Segment)> = selected.iter().map(|&i| (i, &segs[i])).collect();
    let hits = idx.search(&q(text), &OrgScope { visible, selected: &sel }).unwrap();
    hits.iter()
        .map(|h| {
            (idx.generation().repos[h.repo].repo.clone(), segs[h.repo].node(h.node).qualified_name.clone(), h.score)
        })
        .collect()
}

#[test]
fn one_ranking_with_comparable_scores() {
    let o = org3();
    let up = org::update_org(&o.store, "acme", &o.members(&[0, 1, 2])).unwrap();
    assert_eq!((up.generation, up.published, up.added.len(), up.repos), (1, true, 3, 3));
    let idx = OrgIndex::open(&o.store.dir, "acme").unwrap();
    let repos: Vec<&str> = idx.generation().repos.iter().map(|r| r.repo.as_str()).collect();
    assert_eq!(repos, ["acme/billing", "acme/payments", "acme/shipping"]);
    let segs = o.segments(&idx);
    let all = [0, 1, 2];
    let hits = org_search(&idx, &segs, "refund", &all, &all);
    // Both repositories' `refund` definitions, in one ranking.
    let names: Vec<(&str, &str)> = hits.iter().map(|(r, q, _)| (r.as_str(), q.as_str())).collect();
    assert!(names.contains(&("acme/billing", "billing.refund.refund")), "{names:?}");
    assert!(names.contains(&("acme/payments", "payments.refund.refund")), "{names:?}");
    assert!(hits.windows(2).all(|w| w[0].2 >= w[1].2), "ordered by score: {hits:?}");

    // The organization index scores exactly like one ranking over the three
    // segments with summed statistics.
    let refs: Vec<&Segment> = segs.iter().collect();
    let merged = search_symbols_merged(&refs, &q("refund")).unwrap();
    let merged: Vec<(String, String, f32)> = merged
        .iter()
        .map(|(i, h)| (idx.generation().repos[*i].repo.clone(), segs[*i].node(h.node).qualified_name.clone(), h.score))
        .collect();
    assert_eq!(hits, merged);

    // A repository filter is applied before top-k: same scores (organization
    // statistics), only that repository.
    let only = org_search(&idx, &segs, "refund", &all, &[1]);
    assert!(!only.is_empty() && only.iter().all(|(r, ..)| r == "acme/payments"), "{only:?}");
    let expected: Vec<_> = hits.iter().filter(|(r, ..)| r == "acme/payments").cloned().collect();
    assert_eq!(only, expected);

    // Paging over the merged ranking is exact.
    let sel: Vec<(usize, &Segment)> = all.iter().map(|&i| (i, &segs[i])).collect();
    let scope = OrgScope { visible: &all, selected: &sel };
    let page = |offset| idx.search(&SymbolQuery { offset, limit: 2, ..q("refund") }, &scope).unwrap();
    let full = idx.search(&q("refund"), &scope).unwrap();
    assert_eq!([page(0), page(2)].concat(), full[..4.min(full.len())].to_vec());
}

#[test]
fn visibility_filters_results_counts_and_statistics() {
    let o = org3();
    org::update_org(&o.store, "acme", &o.members(&[0, 1, 2])).unwrap();
    let idx = OrgIndex::open(&o.store.dir, "acme").unwrap();
    let segs = o.segments(&idx);
    // The caller sees billing and shipping only, and asks for everything.
    let hits = org_search(&idx, &segs, "refund invoice", &[0, 2], &[0, 1, 2]);
    assert!(!hits.is_empty());
    assert!(hits.iter().all(|(r, ..)| r != "acme/payments"), "{hits:?}");

    // Scores are those of an organization without the invisible repository:
    // its documents do not even count in the statistics.
    org::update_org(&o.store, "visible", &o.members(&[0, 2])).unwrap();
    let small = OrgIndex::open(&o.store.dir, "visible").unwrap();
    let small_segs = o.segments(&small);
    let all_small: Vec<usize> = (0..small_segs.len()).collect();
    assert_eq!(hits, org_search(&small, &small_segs, "refund invoice", &all_small, &all_small));

    // Nothing visible: nothing found.
    assert!(org_search(&idx, &segs, "refund", &[], &[0, 1, 2]).is_empty());
}

#[test]
fn incremental_replace_and_generations() {
    let o = org3();
    org::update_org(&o.store, "acme", &o.members(&[0, 1, 2])).unwrap();
    let gen1 = OrgIndex::open(&o.store.dir, "acme").unwrap();
    let gen1_segs = o.segments(&gen1);

    // Nothing changed: no new generation.
    let up = org::update_org(&o.store, "acme", &o.members(&[0, 1, 2])).unwrap();
    assert_eq!((up.generation, up.published, up.unchanged), (1, false, 3));

    // Re-index payments with a new function: only its part is replaced.
    let mut changed = PAYMENTS.to_vec();
    changed.push(("payments/dispute.py", "def open_dispute(payment):\n    return payment\n"));
    o.index(1, "acme/payments", "c2b", &changed);
    let up = org::update_org(&o.store, "acme", &o.members(&[0, 1, 2])).unwrap();
    assert_eq!(up.generation, 2);
    assert_eq!(
        (up.replaced.clone(), up.added.len(), up.removed.len(), up.unchanged),
        (vec!["acme/payments".to_string()], 0, 0, 2)
    );
    let gen2 = OrgIndex::open(&o.store.dir, "acme").unwrap();
    let gen2_segs = o.segments(&gen2);
    for k in [0, 2] {
        assert_eq!(gen1.generation().repos[k], gen2.generation().repos[k], "untouched repositories keep their entry");
    }
    let all = [0, 1, 2];
    assert_eq!(org_search(&gen2, &gen2_segs, "open_dispute", &all, &all)[0].1, "payments.dispute.open_dispute");
    // A reader of generation 1 keeps reading generation 1 (its files are kept).
    assert!(
        org_search(&gen1, &gen1_segs, "open_dispute", &all, &all)
            .iter()
            .all(|h| h.1 != "payments.dispute.open_dispute")
    );
    // The store kept the segment generation 1 reads, although the catalog
    // moved on.
    assert!(o.store.segment_path(&gen1.generation().repos[1].segment_id).exists());

    // Generation 3 drops shipping; generation 1 is pruned with its
    // payments segment and tantivy files.
    let up = org::update_org(&o.store, "acme", &o.members(&[0, 1])).unwrap();
    assert_eq!((up.generation, up.removed.clone()), (3, vec!["acme/shipping".to_string()]));
    let gen3 = OrgIndex::open(&o.store.dir, "acme").unwrap();
    assert_eq!(gen3.generation().repos.len(), 2);
    let dir = org::org_dir(&o.store.dir, "acme");
    let gens: Vec<String> = std::fs::read_dir(dir.join("generations"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    assert_eq!(gens, ["0000000002.json", "0000000003.json"]);
    assert!(!o.store.segment_path(&gen1.generation().repos[1].segment_id).exists(), "only generation 1 read it");
    let old_uuid = gen1.generation().repos[1].tantivy_segment.clone().unwrap();
    let files: Vec<String> = std::fs::read_dir(dir.join("index"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(files.iter().all(|f| !f.starts_with(&old_uuid)), "{files:?}");
    // Shipping is still read by generation 2 (kept), so its files stay.
    let ship_uuid = gen2.generation().repos[2].tantivy_segment.clone().unwrap();
    assert!(files.iter().any(|f| f.starts_with(&ship_uuid)));
}

#[test]
fn members_not_indexed_and_duplicate_ids() {
    let o = org3();
    let mut members = o.members(&[0]);
    members.push(OrgMember { root: "/nonexistent/repo".into(), url: None });
    let up = org::update_org(&o.store, "acme", &members).unwrap();
    assert_eq!(up.missing.len(), 1);
    assert_eq!(up.repos, 1);
    // Same repository id at two roots.
    o.index(2, "acme/billing", "c9", SHIPPING);
    let e = org::update_org(&o.store, "dup", &o.members(&[0, 2])).unwrap_err().to_string();
    assert!(e.contains("indexed at two roots"), "{e}");
    assert!(org::update_org(&o.store, "../x", &[]).is_err());
    assert_eq!(org::list_orgs(&o.store.dir).unwrap().iter().map(|g| g.name.as_str()).collect::<Vec<_>>(), ["acme"]);
    assert!(OrgIndex::open(&o.store.dir, "nope").is_err());
}
