//! The code ↔ config bridge and data pointers (RFC 0003 §6, §7).
//!
//! - `EnvVar` references → one file-less canonical `EnvVar` node per name
//!   (`$env:NAME`, created when anything reads or sets the name), `READS`
//!   from the reader (`contract`, 0.95). Every `EnvVar` *definition* (a
//!   provider: `.env` line, manifest `env` entry, Dockerfile `ENV`) gets a
//!   `WRITES` edge to it (`syntactic`, 1.0).
//! - `ConfigKey` references → `READS` to `Key` definitions: every key with
//!   that exact key path in the namespace's config scope (`contract`, 0.90);
//!   without a scope match, the only `Key` in the snapshot with that path
//!   (`unique_name`, 0.60); several candidates → none.
//! - `DataRef` references and the bases of data definitions → exact key
//!   paths only, never name matching: same file (`syntactic`, 1.0), same
//!   package for directory-scoped files (`same_module`, 0.95), an imported
//!   file (`import_resolved`, 0.95), a global qualified name (`$qn`,
//!   `import_resolved`, 0.95) or any data file (`$any`, unique,
//!   `unique_name`, 0.60).

use super::*;

/// Config scopes of the known namespaces (RFC 0003 §7.3): whether `path` is
/// a config file of `namespace`, and the key path to look for in it (`None`
/// = not in scope).
pub(crate) fn config_scope<'k>(namespace: &str, path: &str, key: &'k str) -> Option<std::borrow::Cow<'k, str>> {
    use std::borrow::Cow;
    let name = basename(path);
    let (stem, ext) = name.rsplit_once('.').unwrap_or((name, ""));
    let parent = parent_dir(path).map(basename).unwrap_or("");
    let (ns, arg) = match namespace.split_once(':') {
        Some((n, a)) => (n, Some(a)),
        None => (namespace, None),
    };
    match ns {
        // application.yml, application-prod.properties, bootstrap.yml
        "spring" => {
            let config = (stem.starts_with("application") && matches!(ext, "yml" | "yaml" | "properties"))
                || stem.starts_with("bootstrap");
            config.then_some(Cow::Borrowed(key))
        }
        // config('app.name') → key `name` of config/app.php
        "laravel" => {
            let (file, rest) = key.split_once('.')?;
            (parent == "config" && ext == "php" && stem == file).then_some(Cow::Borrowed(rest))
        }
        // Configuration["A:B:C"] ≡ A.B.C in appsettings*.json
        "dotnet" => (stem.starts_with("appsettings") && ext == "json").then(|| Cow::Owned(key.replace(':', "."))),
        // node-config: config/*.{json,yaml,yml}
        "node-config" => (parent == "config" && matches!(ext, "json" | "yaml" | "yml")).then_some(Cow::Borrowed(key)),
        // viper: files named by SetConfigName (`viper:<name>`), else config.*
        "viper" => (stem == arg.unwrap_or("config")).then_some(Cow::Borrowed(key)),
        _ => None,
    }
}

/// Whether a namespace has a known config scope.
fn known_namespace(namespace: &str) -> bool {
    matches!(namespace.split(':').next(), Some("spring" | "laravel" | "dotnet" | "node-config" | "viper"))
}

impl<'a> Resolver<'a> {
    /// `Key` definitions by key path and data definitions by local name.
    pub(crate) fn build_data_tables(&mut self) {
        let mut keys: HashMap<&'a str, Vec<NodeId>> = HashMap::new();
        let mut data = Vec::with_capacity(self.files.len());
        for i in 0..self.files.len() {
            let fid = i as FileId;
            let defs = self.defs(fid);
            let is_data = self.families[i] == Some(LanguageFamily::Data);
            let mut local: HashMap<&'a str, NodeId> = HashMap::new();
            for (j, d) in defs.iter().enumerate() {
                let node = self.def_node[i][j];
                if d.label == Label::Key {
                    let v = keys.entry(d.qualified_name.as_str()).or_default();
                    if !v.contains(&node) {
                        v.push(node);
                    }
                }
                if is_data {
                    local.entry(d.qualified_name.as_str()).or_insert(node);
                }
            }
            data.push(local);
        }
        self.keys_by_path = keys;
        self.data_by_local = data;
    }

    /// One canonical `EnvVar` node per name read or set anywhere, in name
    /// order (appended after every definition node).
    pub(crate) fn build_env_nodes(&mut self) {
        let mut names: BTreeSet<&'a str> = BTreeSet::new();
        for f in self.files {
            let Some(facts) = f.facts.as_ref() else { continue };
            names.extend(facts.references.iter().filter(|r| r.kind == RefKind::EnvVar).map(|r| r.name.as_str()));
            names.extend(facts.definitions.iter().filter(|d| d.label == Label::EnvVar).map(|d| d.name.as_str()));
        }
        for name in names {
            if name.is_empty() {
                continue;
            }
            let id = self.nodes.len() as NodeId;
            self.nodes.push(Node {
                label: Label::EnvVar,
                name: name.to_string(),
                qualified_name: env_var_qualified_name(name),
                file: None,
                span: None,
                language: None,
                signature: None,
                doc: None,
                exported: true,
                is_test: false,
            });
            self.env_nodes.insert(name.to_string(), id);
        }
    }

    /// `WRITES` from every env var provider definition to its canonical node.
    pub(crate) fn env_provider_edges(&mut self) {
        let mut out = Vec::new();
        for i in 0..self.files.len() {
            let fid = i as FileId;
            for (j, d) in self.defs(fid).iter().enumerate() {
                if d.label != Label::EnvVar {
                    continue;
                }
                if let Some(&env) = self.env_nodes.get(d.name.as_str()) {
                    out.push((self.def_node[i][j], env, fid, d.name_span));
                }
            }
        }
        for (src, dst, f, span) in out {
            let meta = EdgeMeta { confidence: 1.0, strategy: Strategy::Syntactic, evidence: Some((f, span)) };
            self.add_edge(src, dst, EdgeKind::Writes, meta);
        }
    }

    /// Targets of a bridge or data reference (`EnvVar`, `ConfigKey`,
    /// `DataRef`), with the edge kind to add.
    pub(crate) fn bridge_targets(&self, f: FileId, item: &RefItem<'_>) -> (Vec<(NodeId, Strategy, f32)>, bool) {
        match item.kind {
            RefKind::EnvVar => {
                let t = self.env_nodes.get(item.name).map(|&n| vec![(n, Strategy::Contract, 0.95)]);
                (t.unwrap_or_default(), false)
            }
            RefKind::ConfigKey => (self.config_key_targets(item.receiver, item.name), false),
            RefKind::DataRef => match self.data_ref(f, item.receiver, item.name) {
                Outcome::Hit(n, s, c) => (vec![(n, s, c)], false),
                Outcome::External => (Vec::new(), true),
                Outcome::Miss => (Vec::new(), false),
            },
            _ => (Vec::new(), false),
        }
    }

    /// RFC 0003 §7.3: keys in the namespace's scope, else the unique key.
    fn config_key_targets(&self, namespace: Option<&str>, path: &str) -> Vec<(NodeId, Strategy, f32)> {
        if path.is_empty() {
            return Vec::new();
        }
        if let Some(ns) = namespace.map(str::trim).filter(|ns| known_namespace(ns)) {
            let mut hits = Vec::new();
            for (i, f) in self.files.iter().enumerate() {
                let Some(key) = config_scope(ns, &f.path, path) else { continue };
                let keys = self.keys_by_path.get(key.as_ref()).into_iter().flatten().copied();
                hits.extend(keys.filter(|&n| self.nodes[n as usize].file == Some(i as FileId)));
            }
            hits.sort_unstable();
            hits.dedup();
            if !hits.is_empty() {
                return hits.into_iter().map(|n| (n, Strategy::Contract, 0.9)).collect();
            }
        }
        match self.keys_by_path.get(path).map(Vec::as_slice) {
            Some([n]) => vec![(*n, Strategy::UniqueName, 0.6)],
            _ => Vec::new(),
        }
    }

    /// Definition with local key path `path` in data file `f`.
    fn data_def(&self, f: FileId, path: &str) -> Option<NodeId> {
        self.data_by_local.get(f as usize)?.get(path).copied()
    }

    /// Resolves a data pointer (see the `DataRef` docs in `rueca-model`).
    pub(crate) fn data_ref(&self, f: FileId, receiver: Option<&str>, path: &str) -> Outcome {
        match receiver.map(str::trim) {
            None | Some("") => self.data_ref_local(f, path),
            Some(DATA_REF_QN) => match self.nodes_by_qn().get(path).map(Vec::as_slice) {
                Some([n]) => Outcome::Hit(*n, Strategy::ImportResolved, 0.95),
                _ => Outcome::Miss,
            },
            Some(DATA_REF_ANY) => self.data_ref_any(path),
            Some(spec) => self.data_ref_imported(f, spec, path),
        }
    }

    /// A key path in the same file, else in another file of its package.
    fn data_ref_local(&self, f: FileId, path: &str) -> Outcome {
        if let Some(n) = self.data_def(f, path) {
            return Outcome::Hit(n, Strategy::Syntactic, 1.0);
        }
        let found = self.module_files(f).into_iter().filter(|&o| o != f).find_map(|o| self.data_def(o, path));
        found.map_or(Outcome::Miss, |n| Outcome::Hit(n, Strategy::SameModule, 0.95))
    }

    /// The only data file definition with key path `path`.
    fn data_ref_any(&self, path: &str) -> Outcome {
        let mut found = self.data_by_local.iter().filter_map(|m| m.get(path).copied());
        match (found.next(), found.next()) {
            (Some(n), None) => Outcome::Hit(n, Strategy::UniqueName, 0.6),
            _ => Outcome::Miss,
        }
    }

    /// A key path in the file `f` imports with specifier `spec`.
    fn data_ref_imported(&self, f: FileId, spec: &str, path: &str) -> Outcome {
        let Some(k) = self.imports(f).iter().position(|imp| imp.module == spec) else { return Outcome::Miss };
        match self.import_targets[f as usize][k] {
            Some(t) => {
                self.data_def(t, path).map_or(Outcome::Miss, |n| Outcome::Hit(n, Strategy::ImportResolved, 0.95))
            }
            None => Outcome::External,
        }
    }

    /// A base of a data definition (YAML merge key, compose `extends`,
    /// Dockerfile `FROM <stage>`): `key.path` in the same file, or
    /// `<import specifier>#key.path`.
    pub(crate) fn data_base(&self, f: FileId, text: &str) -> Outcome {
        match text.split_once('#') {
            Some((spec, path)) if !spec.is_empty() => self.data_ref(f, Some(spec), path),
            Some((_, path)) => self.data_ref(f, None, path),
            None => self.data_ref(f, None, text),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::config_scope;

    #[test]
    fn scopes() {
        let s = |ns, path, key| config_scope(ns, path, key).map(|c| c.into_owned());
        assert_eq!(s("spring", "src/main/resources/application.yml", "server.port").as_deref(), Some("server.port"));
        assert_eq!(s("spring", "application-prod.properties", "a").as_deref(), Some("a"));
        assert_eq!(s("spring", "bootstrap.yml", "a").as_deref(), Some("a"));
        assert_eq!(s("spring", "config/app.yml", "a"), None);
        assert_eq!(s("laravel", "config/app.php", "app.name").as_deref(), Some("name"));
        assert_eq!(s("laravel", "config/app.php", "database.default"), None);
        assert_eq!(s("laravel", "app/app.php", "app.name"), None);
        assert_eq!(
            s("dotnet", "appsettings.Development.json", "Logging:LogLevel").as_deref(),
            Some("Logging.LogLevel")
        );
        assert_eq!(s("node-config", "config/default.json", "db.host").as_deref(), Some("db.host"));
        assert_eq!(s("node-config", "default.json", "db.host"), None);
        assert_eq!(s("viper", "deploy/config.yaml", "a.b").as_deref(), Some("a.b"));
        assert_eq!(s("viper:app", "app.toml", "a.b").as_deref(), Some("a.b"));
        assert_eq!(s("viper:app", "config.toml", "a.b"), None);
        assert_eq!(s("django", "settings.py", "X"), None);
    }
}
