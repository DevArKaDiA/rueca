//! Snapshot selection in organization mode (RFC 0006 §4, §9.3): which entry
//! of each repository a call reads — its default, the refs `ref` names, or
//! the ref snapshot an `id` is pinned to.

use rueca_model::SymbolIdParts;
use rueca_segment::org::{OrgGeneration, OrgSelection, OrgSlot};
use rueca_segment::{CatalogEntry, Store};

use crate::args::CommonArgs;
use crate::view::View;
use crate::{Engine, OrgCtx, Selection};

/// How an error tells the caller to index another ref of an organization.
const INDEX_HINT: &str = "`rueca index --org <file> <member> --ref <name>` indexes another ref (a branch, `pr/<n>`, a \
                          tag or a commit)";

/// The entry of repository `repo` that ref `name` names: [`OrgGeneration::find_ref`],
/// else `HEAD` / `worktree` / `refs/heads/<default branch>` for the default entry
/// (the local-mode names of the default ref).
pub(crate) fn find_slot(g: &OrgGeneration, repo: usize, name: &str) -> Option<OrgSlot> {
    if let Some(slot) = g.find_ref(repo, name) {
        return Some(slot);
    }
    let default = || Some(OrgSlot::default_of(repo));
    match name {
        "HEAD" | "worktree" => default(),
        _ => match name.strip_prefix("refs/heads/") {
            Some(b) => g.find_ref(repo, b),
            None => None,
        },
    }
}

/// The error for ref `name` that repository `repo` does not have.
fn unknown_ref(g: &OrgGeneration, repo: usize, name: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "ref `{name}` is not indexed for repository `{}`; indexed refs: {}. {INDEX_HINT}",
        g.repos[repo].repo,
        g.indexed_refs(repo)
    )
}

/// The error for ref `name` that none of `repos` has (their refs only, so
/// nothing about repositories the caller cannot read is named).
fn unknown_everywhere(g: &OrgGeneration, repos: &[usize], name: &str) -> anyhow::Error {
    let listed: Vec<String> = repos.iter().map(|&i| format!("{}: {}", g.repos[i].repo, g.indexed_refs(i))).collect();
    anyhow::anyhow!(
        "ref `{name}` is not indexed for any selected repository; indexed refs: {}. {INDEX_HINT}",
        listed.join("; ")
    )
}

/// The view of a call that names no ref: every repository at its default,
/// except the repository of `id` when the id's commit (or ref) is one of its
/// ref snapshots.
fn id_view(g: &OrgGeneration, repos: &[usize], id: Option<&SymbolIdParts>) -> OrgSelection {
    let mut sel = OrgSelection::default();
    let Some(p) = id else { return sel };
    let Some(commit) = p.commit.as_deref() else { return sel };
    if let Some(i) = g.repo_index(&p.repo).filter(|i| repos.contains(i))
        && let Some(slot) = g.find_ref(i, commit).filter(|s| !s.is_default())
    {
        sel.set(i, vec![slot]);
    }
    sel
}

/// The view of `repos` at refs `names`: with `star`, each repository at the
/// refs it has (others at their default; a ref none has is an error, and a
/// `pr/<n>` more than one has is ambiguous); otherwise every repository
/// must have every ref. `repos` are the visible ones, so errors name no
/// other.
fn ref_view(g: &OrgGeneration, repos: &[usize], names: &[&str], star: bool) -> anyhow::Result<OrgSelection> {
    let mut sel = OrgSelection::default();
    if star {
        if let Some(n) = names.iter().find(|n| repos.iter().all(|&i| find_slot(g, i, n).is_none())) {
            return Err(unknown_everywhere(g, repos, n));
        }
        crate::check_pr_refs(names, |n| {
            repos.iter().filter(|&&i| find_slot(g, i, n).is_some()).map(|&i| g.repos[i].repo.clone()).collect()
        })?;
        for &i in repos {
            sel.set(i, names.iter().filter_map(|n| find_slot(g, i, n)).collect());
        }
        return Ok(sel);
    }
    for &i in repos {
        let slots = names.iter().map(|n| find_slot(g, i, n).ok_or_else(|| unknown_ref(g, i, n)));
        sel.set(i, slots.collect::<anyhow::Result<_>>()?);
    }
    Ok(sel)
}

impl Engine {
    /// Organization mode of [`Engine::select`]: the repositories `repo` (else
    /// an `id`, else every visible one) selects, each at the entries of the
    /// call's view, in selection order and each repository's ref order.
    pub(crate) fn select_org(
        &self,
        store: &Store,
        name: &str,
        c: &CommonArgs,
        id: Option<&SymbolIdParts>,
    ) -> anyhow::Result<Selection> {
        let (entries, mut org) = self.org_entries(name)?;
        let index = org.index.clone();
        let g = index.generation();
        self.evict_stale(g.slots().into_iter().filter_map(|s| g.entry(s)).map(|e| e.segment_id.as_str()));
        let (selected, star) = self.selected_entries(store, &entries, c, id)?;
        let repos: Vec<usize> =
            selected.iter().filter_map(|e| g.repos.iter().position(|r| r.default.root == e.root)).collect();
        let names = c.ref_names();
        org.view = if names.is_empty() { id_view(g, &repos, id) } else { ref_view(g, &repos, &names, star)? };
        let mut views = Vec::new();
        for &i in &repos {
            for slot in org.view.slots_of(i) {
                views.push(self.slot_view(store, &org, slot)?);
            }
        }
        Ok(Selection { views, org: Some(org), catalog: Vec::new() })
    }

    /// The view of organization entry `slot`, opened.
    pub(crate) fn slot_view(&self, store: &Store, org: &OrgCtx, slot: OrgSlot) -> anyhow::Result<View> {
        let g = org.index.generation();
        let (Some(r), Some(e)) = (g.repos.get(slot.repo), g.entry(slot)) else {
            anyhow::bail!("organization entry {slot:?} is out of range");
        };
        let seg = self.open_cached(store, &e.segment_id)?;
        let entry = CatalogEntry {
            root: e.root.clone(),
            repo: r.repo.clone(),
            segment_id: e.segment_id.clone(),
            indexed_at: e.snapshot.indexed_at,
            ref_name: slot.ref_idx.and_then(|k| r.refs.get(k)).map(|x| x.ref_name.clone()),
        };
        let mut v = View::new(seg, entry);
        v.org_slot = Some(slot);
        Ok(v)
    }
}
