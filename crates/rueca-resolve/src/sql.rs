//! SQL (`rueca-lang-sql`): a database namespace is global, so a name
//! resolves against every SQL definition of the snapshot by qualified
//! name, and the candidates are narrowed by place.
//!
//! | Reference (SQL file `f`) | Target | Edge strategy | Confidence |
//! |---|---|---|---|
//! | `Read` without receiver naming a CTE of the enclosing definitions | that CTE (`Variable`) | `SameModule` | 0.95 |
//! | `Read` / `Write` (relation), `Call` (routine): definitions whose qualified name is the written one (`s.t` or `t`), one of them earlier in `f` | the last one before the reference | `SameModule` | 0.95 |
//! | …in other files of `f`'s migration set (same directory, ordered names), at or before `f` | the latest by migration order (the effective definition: a table dropped and created again is the later `CREATE`) | `SameModule` | 0.90 |
//! | …all in one migration set, `f` outside it | the latest by migration order | `SameModule` | 0.85 |
//! | …one later in `f` | the first of them | `SameModule` | 0.95 |
//! | …nearest directory: the candidates sharing the deepest directory with `f` are all in one file | the first of them in that file | `SameModule` | 0.85 |
//! | no exact name; exactly one qualified name whose segments end with the written ones, or the other way round (`t` ↔ `s.t`) | as above, among its definitions | `UniqueName` | 0.60 |
//! | `Read` / `Write` with receiver `$type:T` (a column) | the `Field` `T.c` of the relation `T` resolves to (in its file first, else the column an `ALTER TABLE` added, by the rules above) | as the relation | relation's |
//!
//! Relations are `Struct` definitions (tables, views, dbt models,
//! snapshots), routines `Function` definitions that are not local, columns
//! `Field`s. Several candidates left after these rules resolve to nothing.
//! Migration order comes from the file name (`rueca_resolve` docs: the
//! leading version — Flyway `V1_2__x.sql`, `20240101120000_x.sql`,
//! `0003_x.up.sql`, or the directory of `up.sql` / `migration.sql`);
//! definitions in down / undo migrations are candidates only for their own
//! file.

use std::collections::HashMap;

use rueca_extract::TYPE_RECEIVER_PREFIX;

use super::*;

/// SQL definitions by qualified name and by last segment.
#[derive(Default)]
pub(crate) struct SqlIndex {
    by_qn: HashMap<String, Vec<NodeId>>,
    by_last: HashMap<String, Vec<NodeId>>,
}

/// What a reference may target.
#[derive(Clone, Copy, PartialEq)]
enum Want {
    Relation,
    Routine,
    Column,
}

impl Want {
    fn accepts(self, label: Label) -> bool {
        match self {
            Want::Relation => label == Label::Struct,
            Want::Routine => label == Label::Function,
            Want::Column => label == Label::Field,
        }
    }
}

/// A file's place in an ordered migration set: the set's directory, the
/// version, and whether it is a down / undo migration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Migration {
    dir: String,
    version: Vec<u64>,
    down: bool,
}

/// The migration a path is, from its name: a leading version (Flyway
/// `V1_2__x`, `U3__x` undo, timestamps `20240101120000_x`, numeric
/// prefixes `0003_x`, Diesel `2024-01-01-000000_x`); `up.sql`,
/// `down.sql` and `migration.sql` take the version of their directory.
pub(crate) fn migration_of(path: &str) -> Option<Migration> {
    let (dir, file) = path.rsplit_once('/').unwrap_or(("", path));
    let stem = file.strip_suffix(".sql").unwrap_or(file);
    let (stem, down) = match stem.strip_suffix(".down") {
        Some(s) => (s, true),
        None => (stem.strip_suffix(".up").unwrap_or(stem), false),
    };
    if matches!(stem, "up" | "down" | "migration") {
        let (parent, name) = dir.rsplit_once('/').unwrap_or(("", dir));
        let version = leading_version(name)?;
        return Some(Migration { dir: parent.to_string(), version, down: stem == "down" });
    }
    let (body, undo) = match stem.as_bytes() {
        [b'V' | b'v', d, ..] if d.is_ascii_digit() => (&stem[1..], false),
        [b'U' | b'u', d, ..] if d.is_ascii_digit() => (&stem[1..], true),
        _ => (stem, false),
    };
    let version = leading_version(body)?;
    Some(Migration { dir: dir.to_string(), version, down: down || undo })
}

/// The digit groups a name starts with, separated by `.`, `_` or `-`
/// (Flyway's `__` ends the version).
fn leading_version(name: &str) -> Option<Vec<u64>> {
    if !name.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    let head = name.split("__").next().unwrap_or(name);
    let mut out = Vec::new();
    for part in head.split(['.', '_', '-']) {
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            break;
        }
        out.push(part.bytes().fold(0u64, |n, b| n.saturating_mul(10).saturating_add(u64::from(b - b'0'))));
    }
    (!out.is_empty()).then_some(out)
}

impl Resolver<'_> {
    pub(crate) fn is_sql(&self, f: FileId) -> bool {
        self.families[f as usize] == Some(LanguageFamily::Sql)
    }

    fn sql_index(&self) -> &SqlIndex {
        self.sql_defs.get_or_init(|| {
            let mut idx = SqlIndex::default();
            for (i, n) in self.nodes.iter().enumerate() {
                if n.language.map(|l| l.family()) != Some(LanguageFamily::Sql) || n.file.is_none() {
                    continue;
                }
                idx.by_qn.entry(n.qualified_name.clone()).or_default().push(i as NodeId);
                let last = n.qualified_name.rsplit('.').next().unwrap_or(&n.qualified_name);
                idx.by_last.entry(last.to_string()).or_default().push(i as NodeId);
            }
            idx
        })
    }

    /// A reference written in a SQL file (module docs).
    pub(crate) fn resolve_sql(&self, f: FileId, item: &RefItem<'_>) -> Outcome {
        let receiver = item.receiver.map(str::trim);
        if let Some(table) = receiver.and_then(|r| r.strip_prefix(TYPE_RECEIVER_PREFIX)) {
            return self.sql_column(f, item, table);
        }
        if item.kind == RefKind::Call {
            return self.sql_object(f, item, receiver, item.name, Want::Routine);
        }
        if receiver.is_none()
            && let Some(n) = self.sql_cte(f, item)
        {
            return Outcome::Hit(n, Strategy::SameModule, 0.95);
        }
        self.sql_object(f, item, receiver, item.name, Want::Relation)
    }

    /// A CTE named like the reference among the children of its
    /// enclosing definitions (innermost first).
    fn sql_cte(&self, f: FileId, item: &RefItem<'_>) -> Option<NodeId> {
        let defs = self.defs(f);
        let mut scope = item.enclosing;
        while let Some(d) = scope {
            let hit = self.children[f as usize][d as usize]
                .iter()
                .find(|&&c| defs[c as usize].label == Label::Variable && defs[c as usize].name == item.name);
            if let Some(&c) = hit {
                return Some(self.node_of(f, c));
            }
            scope = self.parents[f as usize][d as usize];
        }
        None
    }

    /// A relation or routine named `receiver.name` (module docs).
    fn sql_object(&self, f: FileId, item: &RefItem<'_>, receiver: Option<&str>, name: &str, want: Want) -> Outcome {
        let key = receiver.map_or_else(|| name.to_string(), |r| format!("{r}.{name}"));
        let exact = self.sql_candidates(self.sql_index().by_qn.get(&key), want, f);
        if !exact.is_empty() {
            return self.sql_pick(f, item, &exact);
        }
        let written: Vec<&str> = key.split('.').collect();
        let loose: Vec<NodeId> = self
            .sql_candidates(self.sql_index().by_last.get(name), want, f)
            .into_iter()
            .filter(|&n| suffix_compatible(&written, &self.nodes[n as usize].qualified_name))
            .collect();
        let mut qns: Vec<&str> = loose.iter().map(|&n| self.nodes[n as usize].qualified_name.as_str()).collect();
        qns.sort_unstable();
        qns.dedup();
        match (qns.len(), self.sql_pick(f, item, &loose)) {
            (1, Outcome::Hit(n, _, c)) => Outcome::Hit(n, Strategy::UniqueName, c.min(0.6)),
            _ if want == Want::Routine && self.packs[f as usize].is_some_and(|p| p.is_builtin(name)) => {
                Outcome::External
            }
            _ => Outcome::Miss,
        }
    }

    /// The definitions of `nodes` a `want` reference from `f` may target
    /// (down / undo migrations only from their own file).
    fn sql_candidates(&self, nodes: Option<&Vec<NodeId>>, want: Want, f: FileId) -> Vec<NodeId> {
        let Some(nodes) = nodes else { return Vec::new() };
        nodes
            .iter()
            .copied()
            .filter(|&n| {
                let node = &self.nodes[n as usize];
                let Some(file) = node.file else { return false };
                // A declaration with one body (a subprogram of an Oracle
                // package specification) is not a target: its body is.
                want.accepts(node.label)
                    && !self.body_of.contains_key(&n)
                    && (file == f || !migration_of(&self.files[file as usize].path).is_some_and(|m| m.down))
            })
            .collect()
    }

    /// Chooses among same-named candidates by place (module docs): one
    /// defined earlier in `f`, else by migration order among the other
    /// files, else one defined later in `f`, else the nearest directory.
    fn sql_pick(&self, f: FileId, item: &RefItem<'_>, cands: &[NodeId]) -> Outcome {
        let start = |n: NodeId| self.nodes[n as usize].span.map_or(0, |s| s.start_byte);
        let (here, others): (Vec<NodeId>, Vec<NodeId>) =
            cands.iter().partition(|&&n| self.nodes[n as usize].file == Some(f));
        if let Some(n) = here.iter().copied().filter(|&n| start(n) <= item.span.start_byte).max_by_key(|&n| start(n)) {
            return Outcome::Hit(n, Strategy::SameModule, 0.95);
        }
        if let Some(hit) = self.sql_by_migration(f, &others) {
            return hit;
        }
        if let Some(&n) = here.first() {
            return Outcome::Hit(n, Strategy::SameModule, 0.95);
        }
        match self.sql_nearest(f, &others) {
            Some(n) => Outcome::Hit(n, Strategy::SameModule, 0.85),
            None => Outcome::Miss,
        }
    }

    /// The effective definition by migration order (module docs).
    fn sql_by_migration(&self, f: FileId, cands: &[NodeId]) -> Option<Outcome> {
        let migs: Vec<(NodeId, Migration)> = cands
            .iter()
            .filter_map(|&n| {
                let file = self.nodes[n as usize].file?;
                migration_of(&self.files[file as usize].path).map(|m| (n, m))
            })
            .collect();
        if migs.is_empty() {
            return None;
        }
        let here = migration_of(&self.files[f as usize].path);
        if let Some(h) = here.filter(|h| migs.iter().any(|(_, m)| m.dir == h.dir)) {
            let latest = migs
                .iter()
                .filter(|(_, m)| m.dir == h.dir && m.version <= h.version)
                .max_by(|a, b| self.migration_order(a, b));
            return latest.map(|(n, _)| Outcome::Hit(*n, Strategy::SameModule, 0.9));
        }
        let one_set = migs.len() == cands.len() && migs.iter().all(|(_, m)| m.dir == migs[0].1.dir);
        let latest = migs.iter().max_by(|a, b| self.migration_order(a, b));
        one_set.then(|| latest.map(|(n, _)| Outcome::Hit(*n, Strategy::SameModule, 0.85)))?
    }

    /// Migration order, then path, then position: total and deterministic.
    fn migration_order(&self, a: &(NodeId, Migration), b: &(NodeId, Migration)) -> std::cmp::Ordering {
        let key = |(n, m): &(NodeId, Migration)| {
            let node = &self.nodes[*n as usize];
            let path = node.file.map_or("", |file| self.files[file as usize].path.as_str());
            (m.version.clone(), path, node.span.map_or(0, |s| s.start_byte))
        };
        key(a).cmp(&key(b))
    }

    /// The candidates in the deepest directory shared with `f`, when they
    /// are all in one file: the first of them.
    fn sql_nearest(&self, f: FileId, cands: &[NodeId]) -> Option<NodeId> {
        let here = self.files[f as usize].path.as_str();
        let depth = |n: NodeId| {
            let file = self.nodes[n as usize].file.map_or("", |x| self.files[x as usize].path.as_str());
            shared_dirs(here, file)
        };
        let best = cands.iter().map(|&n| depth(n)).max()?;
        let near: Vec<NodeId> = cands.iter().copied().filter(|&n| depth(n) == best).collect();
        let file = self.nodes[near[0] as usize].file;
        near.iter().all(|&n| self.nodes[n as usize].file == file).then_some(near[0])
    }

    /// Column `item.name` of relation `table` (`$type:` receiver).
    fn sql_column(&self, f: FileId, item: &RefItem<'_>, table: &str) -> Outcome {
        let (receiver, name) = match table.rsplit_once('.') {
            Some((r, n)) => (Some(r), n),
            None => (None, table),
        };
        let Outcome::Hit(t, strategy, conf) = self.sql_object(f, item, receiver, name, Want::Relation) else {
            return Outcome::Miss;
        };
        let qn = format!("{}.{}", self.nodes[t as usize].qualified_name, item.name);
        let cols = self.sql_candidates(self.sql_index().by_qn.get(&qn), Want::Column, f);
        let table_file = self.nodes[t as usize].file;
        if let Some(&c) = cols.iter().find(|&&c| self.nodes[c as usize].file == table_file) {
            return Outcome::Hit(c, strategy, conf);
        }
        match self.sql_pick(f, item, &cols) {
            Outcome::Hit(c, s, k) => Outcome::Hit(c, s.max(strategy), k.min(conf)),
            other => other,
        }
    }
}

/// Number of leading directories two paths share.
fn shared_dirs(a: &str, b: &str) -> usize {
    let da: Vec<&str> = a.split('/').collect();
    let db: Vec<&str> = b.split('/').collect();
    da[..da.len() - 1].iter().zip(&db[..db.len() - 1]).take_while(|(x, y)| x == y).count()
}

/// Whether the written segments are a suffix of the definition's
/// qualified name, or the other way round.
fn suffix_compatible(written: &[&str], qn: &str) -> bool {
    let def: Vec<&str> = qn.split('.').collect();
    let n = written.len().min(def.len());
    written[written.len() - n..] == def[def.len() - n..]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_names() {
        let m = |p: &str| migration_of(p).map(|m| (m.dir, m.version, m.down));
        assert_eq!(m("db/migration/V1_2__init.sql"), Some(("db/migration".into(), vec![1, 2], false)));
        assert_eq!(m("db/migration/U3__undo.sql"), Some(("db/migration".into(), vec![3], true)));
        assert_eq!(m("migrations/20240101120000_users.sql"), Some(("migrations".into(), vec![20240101120000], false)));
        assert_eq!(m("migrations/000002_add.down.sql"), Some(("migrations".into(), vec![2], true)));
        assert_eq!(m("migrations/2024-01-02-000000_x/up.sql"), Some(("migrations".into(), vec![2024, 1, 2, 0], false)));
        assert_eq!(
            m("prisma/migrations/20230101_init/migration.sql"),
            Some(("prisma/migrations".into(), vec![20230101], false))
        );
        assert_eq!(m("db/schema.sql"), None);
        assert!(migration_of("m/V2__b.sql").unwrap().version > migration_of("m/V1_9__a.sql").unwrap().version);
    }

    #[test]
    fn compatibility_and_dirs() {
        assert!(suffix_compatible(&["users"], "app.users"));
        assert!(suffix_compatible(&["app", "users"], "users"));
        assert!(!suffix_compatible(&["other", "users"], "app.users"));
        assert_eq!(shared_dirs("a/b/c.sql", "a/b/d.sql"), 2);
        assert_eq!(shared_dirs("a/x.sql", "b/y.sql"), 0);
    }
}
