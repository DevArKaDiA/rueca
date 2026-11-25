//! Shell (RFC 0003 §10.4c): functions are global once the file that
//! defines them has run or been sourced, so a name resolves through the
//! file and the files it sources, transitively; scripts run by path are
//! linked to their files.
//!
//! | Reference (shell file `f`) | Target | Edge strategy | Confidence |
//! |---|---|---|---|
//! | `Call` / `Read` `name` (`Read`: a variable, or a function a `trap` names), `f` defines it at file level | that definition | `SameModule` | 0.90 |
//! | …else exactly one definition among the files `f` sources (`source` / `.` star imports, transitively, breadth-first, depth ≤ 10) | that definition | `ImportResolved` | 0.95 |
//! | …several sourced files define it | none (which one wins depends on the order the files run in) | — | — |
//! | …none does | strategy 6: the only non-test shell function of that name in the snapshot (a library calling a sibling library its entry script sources) | `UniqueName` | 0.60 |
//! | `Call` with receiver `$script` (`./deploy.sh`, `bash x.sh`) | the `File` node of the script the path resolves to (`LanguagePack::resolve_import`); external when it names no file of the snapshot | `ImportResolved` | 0.90 |
//! | `EnvVar` `NAME` that a sourced file assigns at file level (`Variable` / `Constant`) | that variable, as a `REFERENCES` edge instead of a `READS` of `$env:NAME` | `ImportResolved` | 0.95 |
//!
//! A shell file's own definition wins over a sourced one: scripts source
//! their libraries first and define their own functions after.

use std::rc::Rc;

use rueca_extract::SCRIPT_RECEIVER;

use super::*;

/// Confidence of a call to a script by path.
const SCRIPT_CONFIDENCE: f32 = 0.9;

impl Resolver<'_> {
    pub(crate) fn is_shell(&self, f: FileId) -> bool {
        self.families[f as usize] == Some(LanguageFamily::Shell)
    }

    /// The files `f` sources, transitively (breadth-first, each once,
    /// depth ≤ 10, `f` itself excluded). Memoized.
    fn shell_sources(&self, f: FileId) -> Rc<Vec<FileId>> {
        if let Some(v) = self.shell_memo.borrow().get(&f) {
            return v.clone();
        }
        let mut seen: HashSet<FileId> = HashSet::from([f]);
        let mut out = Vec::new();
        let mut level = vec![f];
        for _ in 0..MAX_DEPTH {
            let mut next = Vec::new();
            for &t in &level {
                for &i in self.import_targets[t as usize].iter().flatten() {
                    if seen.insert(i) {
                        next.push(i);
                    }
                }
            }
            if next.is_empty() {
                break;
            }
            out.extend(&next);
            level = next;
        }
        let v = Rc::new(out);
        self.shell_memo.borrow_mut().insert(f, v.clone());
        v
    }

    /// Whether a shell definition may be the target of a `kind` reference.
    fn shell_compatible(&self, kind: RefKind, n: NodeId) -> bool {
        let label = self.nodes[n as usize].label;
        match kind {
            RefKind::Call => label == Label::Function,
            // `$NAME` of a variable; a `trap` handler naming a function.
            RefKind::Read => matches!(label, Label::Variable | Label::Constant | Label::EnvVar | Label::Function),
            _ => false,
        }
    }

    /// The file-level definition named `name` of shell file `g`, if it fits.
    fn shell_def(&self, g: FileId, kind: RefKind, name: &str) -> Option<NodeId> {
        if !self.is_shell(g) {
            return None;
        }
        self.top_level_def(g, name).filter(|&n| self.shell_compatible(kind, n))
    }

    /// A reference written in a shell file (module docs).
    pub(crate) fn resolve_shell(&self, f: FileId, item: &RefItem<'_>) -> Outcome {
        match item.receiver.map(str::trim) {
            Some(SCRIPT_RECEIVER) => self.shell_script(f, item.name),
            Some(_) => Outcome::Miss,
            None => self.shell_name(f, item),
        }
    }

    fn shell_name(&self, f: FileId, item: &RefItem<'_>) -> Outcome {
        if let Some(n) = self.shell_def(f, item.kind, item.name) {
            return Outcome::Hit(n, Strategy::SameModule, 0.9);
        }
        let sources = self.shell_sources(f);
        let mut found = sources.iter().filter_map(|&g| self.shell_def(g, item.kind, item.name));
        match (found.next(), found.next()) {
            (Some(n), None) => Outcome::Hit(n, Strategy::ImportResolved, 0.95),
            (Some(_), Some(_)) => Outcome::Miss,
            (None, _) if item.kind == RefKind::Call => self.unique_name(f, item.kind, item.name, false),
            (None, _) => Outcome::Miss,
        }
    }

    /// `./deploy.sh` → the script's `File` node.
    fn shell_script(&self, f: FileId, path: &str) -> Outcome {
        let Some(pack) = self.packs[f as usize] else { return Outcome::Miss };
        let import = Import {
            module: path.to_string(),
            relative_level: 0,
            names: Vec::new(),
            module_alias: None,
            type_only: false,
            reexport: false,
            span: Span::default(),
        };
        let from = self.files[f as usize].path.as_str();
        match pack.resolve_import(from, &import, self.project).and_then(|p| self.file_id(&p)) {
            Some(t) => Outcome::Hit(self.file_node[t as usize], Strategy::ImportResolved, SCRIPT_CONFIDENCE),
            None => Outcome::External,
        }
    }

    /// An env read of a name a sourced file assigns: the variable it reads
    /// (module docs).
    pub(crate) fn shell_sourced_variable(&self, f: FileId, item: &RefItem<'_>) -> Option<NodeId> {
        if item.kind != RefKind::EnvVar || !self.is_shell(f) {
            return None;
        }
        let sources = self.shell_sources(f);
        let mut found = sources.iter().filter_map(|&g| {
            self.shell_def(g, RefKind::Read, item.name)
                .filter(|&n| matches!(self.nodes[n as usize].label, Label::Variable | Label::Constant))
        });
        match (found.next(), found.next()) {
            (Some(n), None) => Some(n),
            _ => None,
        }
    }
}
