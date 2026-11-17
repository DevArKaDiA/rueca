//! The organization symbol index (RFC 0006 phase 1): one ranking over
//! several repositories with comparable scores, incremental replacement,
//! generations with an atomic pointer swap, and the visibility filter.

use std::collections::HashMap;

use rueca_model::{
    EdgeKind, EdgeMeta, FileEntry, Label, Language, Node, ResolvedGraph, SnapshotInfo, Span, Strategy, git_blob_sha,
};
use rueca_segment::org::{
    self, LinkDir, LinkEdge, LinkInput, OrgIndex, OrgLinker, OrgMember, OrgScope, OrgSelection, OrgSlot, Package,
    RepoDeps,
};
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

    /// Indexes ref `name` of `repo` as a ref snapshot exported under the
    /// store; its root.
    fn index_ref(&self, repo: &str, name: &str, commit: &str, files: &[(&str, &str)]) -> String {
        let store = std::fs::canonicalize(&self.store.dir).unwrap();
        let dir = store.join("refs").join(repo.replace('/', "-")).join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let root = dir.to_string_lossy().into_owned();
        let (g, mem) = graph(repo, commit, &root, files);
        let meta = write_segment(&g, &mem, &self.store.dir).unwrap();
        self.store.register_ref(&root, &meta, name).unwrap();
        root
    }

    fn members(&self, which: &[usize]) -> Vec<OrgMember> {
        which.iter().map(|&i| OrgMember { root: self.root(i).to_string(), ..Default::default() }).collect()
    }

    fn segments(&self, idx: &OrgIndex) -> Vec<Segment> {
        idx.generation().repos.iter().map(|r| self.store.open_segment(&r.default.segment_id).unwrap()).collect()
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
    let hits = idx.search(&q(text), &OrgScope { visible, selected: &sel, view: None }).unwrap();
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
    let scope = OrgScope { visible: &all, selected: &sel, view: None };
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
    assert!(o.store.segment_path(&gen1.generation().repos[1].default.segment_id).exists());

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
    assert!(
        !o.store.segment_path(&gen1.generation().repos[1].default.segment_id).exists(),
        "only generation 1 read it"
    );
    let old_uuid = gen1.generation().repos[1].default.tantivy_segment.clone().unwrap();
    let files: Vec<String> = std::fs::read_dir(dir.join("index"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(files.iter().all(|f| !f.starts_with(&old_uuid)), "{files:?}");
    // Shipping is still read by generation 2 (kept), so its files stay.
    let ship_uuid = gen2.generation().repos[2].default.tantivy_segment.clone().unwrap();
    assert!(files.iter().any(|f| f.starts_with(&ship_uuid)));
}

#[test]
fn members_not_indexed_and_duplicate_ids() {
    let o = org3();
    let mut members = o.members(&[0]);
    members.push(OrgMember { root: "/nonexistent/repo".into(), ..Default::default() });
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

/// A linker for the refs test: payments publishes `py:payments`; billing
/// (any entry) imports `payments.charge` and calls `charge_card` from
/// `issue_invoice`.
struct StubLinker;

impl OrgLinker for StubLinker {
    fn version(&self) -> u32 {
        3
    }
    fn analyze(&self, _: &Store, seg: &Segment) -> rueca_segment::Result<RepoDeps> {
        Ok(match seg.snapshot().repo.as_str() {
            "acme/payments" => RepoDeps {
                packages: vec![Package {
                    eco: "pypi".into(),
                    name: "payments".into(),
                    dir: String::new(),
                    keys: vec!["py:payments".into()],
                }],
                ..Default::default()
            },
            "acme/billing" => RepoDeps { imports: vec!["py:payments.charge".into()], ..Default::default() },
            _ => RepoDeps::default(),
        })
    }
    fn link(
        &self,
        _: &Store,
        consumer: &LinkInput<'_>,
        deps: &[LinkInput<'_>],
    ) -> rueca_segment::Result<Vec<LinkEdge>> {
        assert_eq!(deps.len(), 1);
        // Ref consumers link against the dependency's default entry.
        assert_eq!(deps[0].segment.meta().id, deps[0].repo.default.segment_id);
        let src = consumer.segment.nodes_by_qualified_name("billing.invoice.issue_invoice")[0];
        let dst = deps[0].segment.nodes_by_qualified_name("payments.charge.charge_card")[0];
        let meta = EdgeMeta { confidence: 0.95, strategy: Strategy::ImportResolved, evidence: None };
        Ok(vec![LinkEdge { src, dep: 0, dst, kind: EdgeKind::Calls, meta, reversed: false }])
    }
}

/// payments@keep: `charge_card` kept, but a file sorted before it shifts
/// every node id.
const PAYMENTS_KEEP: &[(&str, &str)] = &[
    ("payments/aaa.py", "def audit(x):\n    return x\n"),
    ("payments/charge.py", "def charge_card(card, amount):\n    return gateway.charge(card, amount)\n"),
    ("payments/refund.py", "def refund_payment(payment):\n    return payment\n"),
];
/// payments@rename: `charge_card` renamed.
const PAYMENTS_RENAME: &[(&str, &str)] =
    &[("payments/charge.py", "def charge_card_v2(card, amount):\n    return gateway.charge(card, amount)\n")];

fn qn(seg: &Segment, n: u32) -> &str {
    seg.node(n).qualified_name.as_str()
}

#[test]
fn ref_entries_selection_and_remapped_link_edges() {
    let o = org3();
    let keep_root = o.index_ref("acme/payments", "keep", "c2k", PAYMENTS_KEEP);
    let rename_root = o.index_ref("acme/payments", "rename", "c2r", PAYMENTS_RENAME);
    let feat: Vec<(&str, &str)> = [BILLING, &[("billing/late.py", "def late_fee(inv):\n    return 1\n")][..]].concat();
    o.index_ref("acme/billing", "feat", "c1f", &feat);
    let mut members = o.members(&[0, 1, 2]);
    members[1].refs = vec!["keep".into(), "gone".into()];
    let up = org::update_org_with(&o.store, "acme", &members, &StubLinker).unwrap();
    assert_eq!((up.generation, up.repos, up.refs), (1, 3, 3));
    assert_eq!(up.refs_added, ["acme/billing@feat", "acme/payments@keep", "acme/payments@rename"]);
    assert_eq!(up.missing, ["acme/payments@gone"], "configured, not indexed");
    // The consumer's default and its ref are linked, each on its own.
    assert_eq!(up.linked, [("acme/billing".to_string(), 1), ("acme/billing@feat".to_string(), 1)]);
    assert!(up.costs.iter().any(|c| c.entry == "acme/payments@keep" && c.bytes_copied > 0), "{:?}", up.costs);

    let idx = OrgIndex::open(&o.store.dir, "acme").unwrap();
    let g = idx.generation();
    let (bill, pay) = (g.repo_index("acme/billing").unwrap(), g.repo_index("acme/payments").unwrap());
    let names: Vec<String> = g.refs_of(pay).into_iter().map(|r| r.ref_name).collect();
    assert_eq!(names, ["main", "keep", "rename"]);
    let keep = g.select(pay, &["keep"]).unwrap()[0];
    let rename = g.find_ref(pay, "rename").unwrap();
    assert_eq!(g.find_ref(pay, "main"), Some(OrgSlot::default_of(pay)));
    assert_eq!(g.find_ref(pay, "c2k"), None, "not a commit of 7+ hex digits");
    let e = g.select(pay, &["nope"]).unwrap_err().to_string();
    assert!(e.contains("indexed refs: main@c2, keep@c2k, rename@c2r"), "{e}");
    assert_eq!(g.entry(keep).unwrap().root, keep_root);
    assert_eq!(g.slot_of_segment(&g.entry(rename).unwrap().segment_id), Some(rename));
    // A ref's link set points at the dependency's default entry.
    let bill_feat = g.find_ref(bill, "feat").unwrap();
    let feat_links = g.entry(bill_feat).unwrap().links.clone().unwrap();
    assert_eq!(feat_links.deps[0].segment_id, g.repos[pay].default.segment_id);
    assert_ne!(feat_links.file, g.repos[bill].default.links.as_ref().unwrap().file, "one file per consumer segment");
    assert!(idx.entry_profile(keep).is_some() && idx.entry_profile(keep) != idx.profile(pay));

    let open = |s: OrgSlot| o.store.open_segment(&g.entry(s).unwrap().segment_id).unwrap();
    let (bill_seg, pay_seg, keep_seg) = (open(OrgSlot::default_of(bill)), open(OrgSlot::default_of(pay)), open(keep));
    let src = bill_seg.nodes_by_qualified_name("billing.invoice.issue_invoice")[0];
    let at_bill = OrgSlot::default_of(bill);
    let all = [0, 1, 2];
    let edges = |at, n, dir, view: &OrgSelection, vis: &[usize]| idx.link_edges(at, n, dir, None, vis, view).unwrap();

    // Default view: the stored edge.
    let def = edges(at_bill, src, LinkDir::Out, &OrgSelection::default(), &all);
    assert_eq!(def.len(), 1);
    assert_eq!((def[0].slot, qn(&pay_seg, def[0].node)), (OrgSlot::default_of(pay), "payments.charge.charge_card"));
    // payments@keep: the edge points at keep's own node (another id).
    let mut at_keep = OrgSelection::default();
    at_keep.set(pay, vec![keep]);
    let out = edges(at_bill, src, LinkDir::Out, &at_keep, &all);
    assert_eq!(out.len(), 1);
    assert_eq!((out[0].slot, qn(&keep_seg, out[0].node)), (keep, "payments.charge.charge_card"));
    assert_ne!(out[0].node, def[0].node);
    // … and incoming at keep's node comes from the consumer's default.
    let inc = edges(keep, out[0].node, LinkDir::In, &at_keep, &all);
    assert_eq!(inc.len(), 1);
    assert_eq!((inc[0].slot, inc[0].node, inc[0].evidence), (at_bill, src, at_bill));
    // payments@rename no longer has the target: dropped both ways.
    let mut at_rename = OrgSelection::default();
    at_rename.set(pay, vec![rename]);
    assert!(edges(at_bill, src, LinkDir::Out, &at_rename, &all).is_empty());
    let rename_seg = open(rename);
    let v2 = rename_seg.nodes_by_qualified_name("payments.charge.charge_card_v2")[0];
    assert!(edges(rename, v2, LinkDir::In, &at_rename, &all).is_empty());
    // Two refs of payments at once: one edge per entry that has the target.
    let mut both = OrgSelection::default();
    both.set(pay, vec![OrgSlot::default_of(pay), keep, rename]);
    let out: Vec<OrgSlot> = edges(at_bill, src, LinkDir::Out, &both, &all).iter().map(|e| e.slot).collect();
    assert_eq!(out, [OrgSlot::default_of(pay), keep]);
    // Incoming at the library's default: only consumer entries the view
    // reads (billing's default; billing@feat when chosen).
    let target = def[0].node;
    let consumers = |view: &OrgSelection| -> Vec<OrgSlot> {
        edges(OrgSlot::default_of(pay), target, LinkDir::In, view, &all).iter().map(|e| e.slot).collect()
    };
    assert_eq!(consumers(&OrgSelection::default()), [at_bill]);
    let mut feat_view = OrgSelection::default();
    feat_view.set(bill, vec![bill_feat]);
    assert_eq!(consumers(&feat_view), [bill_feat]);
    // The consumer's ref has its own outgoing edge.
    let feat_seg = open(bill_feat);
    let feat_src = feat_seg.nodes_by_qualified_name("billing.invoice.issue_invoice")[0];
    let out = edges(bill_feat, feat_src, LinkDir::Out, &feat_view, &all);
    assert_eq!((out.len(), out[0].evidence), (1, bill_feat));
    // Both ends must be visible.
    assert!(edges(at_bill, src, LinkDir::Out, &at_keep, &[bill]).is_empty());
    assert!(edges(keep, out[0].node, LinkDir::In, &at_keep, &[pay]).is_empty());

    // Search: a ref segment is a valid selection, scored and attributed to
    // its entry; a segment of another repository is not.
    let sel = [(pay, &keep_seg)];
    let scope = OrgScope { visible: &all, selected: &sel, view: Some(&at_keep) };
    let hits = idx.search(&q("charge card"), &scope).unwrap();
    assert!(!hits.is_empty() && hits.iter().all(|h| h.slot == keep && h.repo == pay), "{hits:?}");
    assert_eq!(qn(&keep_seg, hits[0].node), "payments.charge.charge_card");
    let wrong = [(pay, &bill_seg)];
    assert!(idx.search(&q("x"), &OrgScope { visible: &all, selected: &wrong, view: None }).is_err());
    // Routing scores payments with keep's profile when the view reads it.
    let routes = idx.route_view("audit", &all, &at_keep, &Default::default()).unwrap();
    assert_eq!(routes[0].repo, pay, "{routes:?}");

    // Refreshing one ref replaces only its entry: no other entry rebuilt,
    // nothing re-linked (billing links against payments' default).
    let keep_gen1 = g.entry(keep).unwrap().clone();
    let mut keep2 = PAYMENTS_KEEP.to_vec();
    keep2.push(("payments/zz.py", "def zz():\n    return 0\n"));
    o.index_ref("acme/payments", "keep", "c2k2", &keep2);
    let up = org::update_org_with(&o.store, "acme", &members, &StubLinker).unwrap();
    assert_eq!(up.generation, 2);
    assert_eq!(up.refs_replaced, ["acme/payments@keep"]);
    assert!(up.added.is_empty() && up.replaced.is_empty() && up.refs_added.is_empty(), "{up:?}");
    assert_eq!((up.unchanged, up.refs_unchanged), (3, 2));
    assert!(up.linked.is_empty(), "{:?}", up.linked);
    assert_eq!(up.profiled, ["acme/payments@keep"]);
    assert_eq!(up.costs.iter().map(|c| c.entry.as_str()).collect::<Vec<_>>(), ["acme/payments@keep"]);
    let g2 = org::current_generation(&o.store.dir, "acme").unwrap().unwrap();
    for (a, b) in g.repos.iter().zip(&g2.repos) {
        assert_eq!(a.default, b.default, "default entries untouched");
    }
    let keep2_entry = g2.entry(keep).unwrap();
    assert_eq!((keep2_entry.since_generation, keep_gen1.since_generation), (2, 1));
    assert_eq!(g2.entry(rename), g.entry(rename));

    // Removing a ref drops its entry next generation; its files go once no
    // kept generation reads them.
    let rename_entry = g.entry(rename).unwrap().clone();
    o.store.unregister(&rename_root).unwrap();
    let up = org::update_org_with(&o.store, "acme", &members, &StubLinker).unwrap();
    assert_eq!((up.generation, up.refs_removed.clone(), up.refs), (3, vec!["acme/payments@rename".to_string()], 2));
    assert!(o.store.segment_path(&rename_entry.segment_id).exists(), "generation 2 still reads it");
    o.index_ref("acme/payments", "keep", "c2k3", PAYMENTS_KEEP);
    assert_eq!(org::update_org_with(&o.store, "acme", &members, &StubLinker).unwrap().generation, 4);
    let dir = org::org_dir(&o.store.dir, "acme");
    assert!(!o.store.segment_path(&rename_entry.segment_id).exists());
    assert!(!dir.join("profiles").join(rename_entry.profile.as_deref().unwrap()).exists());
    let uuid = rename_entry.tantivy_segment.unwrap();
    assert!(
        std::fs::read_dir(dir.join("index")).unwrap().all(|e| !e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(&uuid))
    );
    // The replaced keep entry's link file is not one (payments has none),
    // but billing@feat's link file is still named and kept.
    assert!(dir.join("links").join(feat_links.file.unwrap()).exists());
}
