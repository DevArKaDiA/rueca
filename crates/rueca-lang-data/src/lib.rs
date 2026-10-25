//! Structured data pack for Rueca: JSON/JSONC, YAML (multi-document), TOML,
//! `.env`, INI, `.properties`, XML of known kinds, HCL / Terraform /
//! OpenTofu and Dockerfile ([RFC 0003] §4–§9). Configuration becomes `Key`
//! nodes addressed by key path, well-known file kinds add `Resource`,
//! `Image`, `Route` and `EnvVar` nodes, pointers inside data become
//! references and imports, and env var providers feed the code ↔ config
//! bridge that the code packs and the resolver complete.
//!
//! [RFC 0003]: https://github.com/DevArKaDiA/rueca/blob/main/docs/rfcs/0003-structured-data-and-languages.md
//!
//! # Contract
//!
//! [`DataPack`] implements `rueca_extract::LanguagePack` for every data
//! [`Language`]: module name = the repository path with extension
//! (`config/app.yaml`), so global names are `path:key.path`;
//! `module_scope(Hcl)` is `Directory` (a Terraform module is the `.tf`
//! files of a directory); `facts_depend_on_path()` is true (kinds are chosen
//! by file name); `resolve_import` is [`resolve_import`]; no builtins, no
//! self receivers.
//!
//! # Parsers (all pure Rust, all span-preserving)
//!
//! | Format | Parser | License | Limits |
//! |---|---|---|---|
//! | JSON, JSONC | `jsonc-parser` 0.34 | MIT | lenient (comments, trailing commas); depth 512 built in |
//! | YAML | `saphyr-parser` 0.2 (events) | MIT OR Apache-2.0 | aliases never expanded; tree built with an explicit stack; 1,000 documents, 2M events |
//! | TOML | `toml_edit` 0.25 | MIT OR Apache-2.0 | recursion guard on (`unbounded` off) |
//! | XML | `roxmltree` 0.21 | MIT OR Apache-2.0 | DTDs refused, 200,000 nodes |
//! | HCL | `hcl-edit` 0.9 | MIT OR Apache-2.0 | no guard upstream: bracket depth > 128 is refused before parsing |
//! | `.env`, INI, `.properties`, Dockerfile | built here (`formats/lines.rs`, `formats/dockerfile.rs`) | — | line-oriented, cannot fail |
//!
//! # Key paths (RFC 0003 §4.3, module [`keypath`])
//!
//! A definition's local qualified name is a key path: `database.host`;
//! keys that need it are quoted with JSON escapes (`paths."/v1/a.{b}".get`,
//! `"a.b".c`); mapping elements of arrays are `[N]` (`steps[2].run`) or,
//! where a kind has an identity field, `["id"]`
//! (`containers["web"].env["DATABASE_URL"]`); documents of a multi-document
//! YAML stream are `{N}` unless they have a kind identity (Kubernetes
//! `Deployment/web`). `name` is the last segment (`[2]` for an element, the
//! identity for identity-keyed ones).
//!
//! # What becomes a node (RFC 0003 §4.4)
//!
//! | Format | Definitions | Nesting |
//! |---|---|---|
//! | JSON / YAML / TOML | every mapping entry → `Key`; mapping elements of arrays → `Key` `[N]` (≤ 64 per array); TOML tables, `[[array]]` entries and dotted keys by key path | parent entry |
//! | YAML streams (≥ 2 documents) | each document → `Document` `{N}` | — |
//! | `.env` | `NAME=value` (optional `export`) → `EnvVar` provider | top level |
//! | INI | `[section]` and `key = value` → `Key` | section |
//! | `.properties` | `a.b.c=value` → one `Key` `a.b.c` (intermediate segments are not nodes) | top level |
//! | XML (kinds) | elements under the root → `Key`, repeated ones keyed by identity (Maven `groupId:artifactId`, MSBuild `Include`, Android `android:name`) or index | element |
//! | HCL | see the table in `formats/hcl.rs`: `resource` / `data` / `module` → `Resource`, `variable` / `locals` → `Variable`, other blocks and attributes → `Key` | block |
//! | Dockerfile | stages → `Image` (`name`, unnamed `stage[N]`), `ENV` → `EnvVar`, `ARG` → `Variable` | stage |
//!
//! Scalar-only arrays produce no element nodes (the array's preview says
//! `[N items]`); values are never nodes.
//!
//! **Spans** run from the key's first byte to the end of its value (trailing
//! whitespace trimmed; block collections end with their last child);
//! `name_span` is the key token. **Signatures** are `key: <preview>`:
//! scalars as written (first line, ≤ 80 chars), maps `{a, b, c, … N more}`,
//! arrays `[N items]`; kinds use their own forms (`GET /users/{id}
//! (getUser)`, `FROM node:20 AS build`, `Deployment web (apps/v1)`,
//! `service db: image postgres:16`, `resource "aws_s3_bucket" "logs"`).
//! **Docs** are the comment block right above a key plus a trailing comment
//! on its line (YAML, TOML, INI, `.properties`, `.env`, HCL, JSONC,
//! Dockerfile; XML: the comment before an element); OpenAPI / JSON Schema
//! nodes without one take their `description` (or `title`), operations
//! their `summary`. **Secrets**: env var provider values are never put in
//! signatures (`DATABASE_URL=•••`), nor are values of keys whose name
//! matches `(?i)pass(word)?|secret|token|api[_-]?key|private[_-]?key|credential`,
//! nor Kubernetes Secret data.
//!
//! # References inside data (RFC 0003 §6)
//!
//! | Source | Fact |
//! |---|---|
//! | `$ref: '#/components/schemas/User'` (any JSON / YAML file) | `DataRef` `components.schemas.User` (JSON Pointer decoded; array indexes become `[N]`) |
//! | `$ref: './common.yaml#/X'` | import `./common.yaml` + `DataRef` `X` with the specifier as receiver |
//! | YAML alias `*a` / merge `<<: *a` | `DataRef` to the anchored key path / a base of the mapping's key |
//! | compose `depends_on`, `extends`, `env_file`, `build` | `DataRef` to `services.x` / base / imports (context folder and its Dockerfile) |
//! | Kubernetes `configMapKeyRef`, `secretKeyRef`, `envFrom` | `DataRef` (`$any`) to `ConfigMap/name.data.key` / the object |
//! | GitHub Actions `needs`, local `uses: ./…` | `DataRef` to `jobs.x` / import (from the repository root) |
//! | `package.json` `main`, `module`, `types`, `bin`, `exports`, `workspaces`; tsconfig `extends`, `references` | imports |
//! | `pyproject.toml` `[project.scripts]`, entry points, poetry scripts `pkg.mod:func` | `DataRef` (`$qn`) to the Python definition `pkg.mod.func` |
//! | Terraform `module` `source = "./x"`; `var.x`, `local.x`, `module.m`, `data.t.n`, `t.n` traversals | import of the folder; `DataRef`s resolved within the module directory |
//! | Dockerfile `FROM <stage>`, `COPY --from=<stage>`, `RUN --mount=…,from=<stage>` | base / `DataRef` to the stage |
//! | Maven `modules/module`, `parent/relativePath`; MSBuild `ProjectReference` | imports |
//!
//! # The bridge (RFC 0003 §7)
//!
//! Every `EnvVar` definition is a provider (the resolver adds `WRITES` to
//! the canonical `$env:NAME`): `.env` lines, compose `environment` (map or
//! list form), Kubernetes container `env`, GitHub Actions `env` maps,
//! Dockerfile `ENV`. Config that reads env emits `EnvVar` references from
//! the key that holds it: compose / `.env` `${VAR}`, `${VAR:-x}`, `$VAR`;
//! Kubernetes `$(VAR)`; GitHub Actions `${{ env.VAR }}`; Spring
//! `${VAR:default}` (an upper-case name; other names are `ConfigKey` reads in
//! namespace `spring`). Spring files (`application*` / `bootstrap*`) split
//! dotted YAML keys (`spring.datasource.url: x` → key path
//! `spring.datasource.url`). The code packs emit the readers.
//!
//! # File kinds (module [`kinds`])
//!
//! | Kind | Detection |
//! |---|---|
//! | `package.json`, `composer.json`, `pyproject.toml` | file name |
//! | tsconfig / jsconfig | `tsconfig*.json`, `jsconfig*.json` |
//! | `.env` | `.env`, `.env.*` |
//! | docker compose | `compose*.y(a)ml`, `docker-compose*.y(a)ml`, or a root `services:` whose entries have `image` / `build` |
//! | Kubernetes | per document: `apiVersion` + `kind` + `metadata.name` |
//! | GitHub Actions | `.github/workflows/*.y(a)ml`, `action.y(a)ml` |
//! | OpenAPI / Swagger | root `openapi: 3.*` / `swagger: "2.0"` |
//! | JSON Schema | `*.schema.json`, or root `$schema` + `type` |
//! | Spring | `application*` / `bootstrap*` `.yml` / `.yaml` / `.properties` |
//!
//! # What is not indexed (RFC 0003 §4.5)
//!
//! A skipped file keeps its `File` node and stays content-searchable, with
//! status `skipped:<reason>`: `data` (`*.min.json`, `*.geojson`, `*.har`,
//! `*.ipynb`, files under `fixtures/`, `__fixtures__/`, `testdata/`,
//! `__snapshots__/`, `test/data/`), `generated` (`@generated` or `Code
//! generated … DO NOT EDIT` in the first 512 bytes), `template` (YAML under
//! a `templates/` directory containing `{{`: Helm charts), `data-size`
//! (over [`MAX_DATA_FILE_BYTES`] unless a known kind, including OpenAPI and
//! Kubernetes bundles sniffed by content; HCL, Dockerfile and XML are
//! always kinds) and `data-shape` (files of no kind: a root array over
//! [`MAX_ROOT_ELEMENTS`], more than [`MAX_KEYS`] keys, or — with at least
//! 20 definitions — more array elements than other definitions; HCL deeper
//! than 128 brackets). Within a file: key depth ≤ [`MAX_DEPTH`],
//! ≤ [`MAX_ELEMENTS`] mapping elements per array, ≤ [`MAX_KEYS`] keys
//! (counted as the generic walk emits them) and [`MAX_DEFS`] definitions;
//! beyond, the file is `partial`. YAML syntax errors keep what was read
//! (`partial`); JSON, TOML, HCL and XML syntax errors are `skipped:parser`.
//! Invalid UTF-8 is decoded lossily (`partial`); a BOM is kept as spaces so
//! offsets stay exact. Extraction never panics and is deterministic.
//!
//! # Testing
//!
//! Unit tests cover every format, kind, skip and budget, adversarial input
//! (billion-laughs YAML, 100,000-deep brackets, random and mutated bytes)
//! and key recall against reference parsers (`serde_json`, `saphyr`'s
//! loader, `toml`). `tests/fixtures` is a small polyglot service also used
//! by the indexer golden and the CLI end-to-end test
//! (`crates/rueca-cli/tests/data.rs`). `tests/real_repo.rs` is an ignored
//! smoke test over a checkout (`RUECA_DATA_REPO=/path cargo test -p
//! rueca-lang-data --test real_repo -- --ignored --nocapture`).

pub mod keypath;
pub mod kinds;

mod emit;
mod formats;
mod text;
mod tree;

use rueca_extract::{Budget, ExtractError, LanguagePack, ModuleScope, ProjectFiles};
use rueca_model::{FileFacts, Import, Language};

pub use emit::{MAX_ANCHORS, MAX_DEFS, MAX_ELEMENTS, MAX_KEYS};
pub use kinds::Kind;
pub use tree::MAX_DEPTH;

use crate::text::CommentStyle;

/// Data files over this size are skipped unless they are a known kind
/// (`skipped:data-size`, RFC 0003 §4.5 rule 4).
pub const MAX_DATA_FILE_BYTES: usize = 256 * 1024;

/// Root arrays longer than this are data dumps (`skipped:data-shape`).
pub const MAX_ROOT_ELEMENTS: usize = 100;

/// Languages served, one per enabled format feature.
static LANGUAGES: &[Language] = &[
    #[cfg(feature = "json")]
    Language::Json,
    #[cfg(feature = "yaml")]
    Language::Yaml,
    #[cfg(feature = "toml")]
    Language::Toml,
    Language::DotEnv,
    Language::Ini,
    Language::Properties,
    #[cfg(feature = "xml")]
    Language::Xml,
    #[cfg(feature = "hcl")]
    Language::Hcl,
    #[cfg(feature = "dockerfile")]
    Language::Dockerfile,
];

/// The data pack. Stateless; construct with `DataPack` or `DataPack::default()`.
#[derive(Debug, Default)]
pub struct DataPack;

impl LanguagePack for DataPack {
    fn languages(&self) -> &'static [Language] {
        LANGUAGES
    }

    fn extractor_version(&self) -> u32 {
        1
    }

    fn extract(
        &self,
        path: &str,
        language: Language,
        source: &[u8],
        budget: &Budget,
    ) -> Result<FileFacts, ExtractError> {
        extract(path, language, source, budget)
    }

    /// The repository-relative path, extension included (`config/app.yaml`).
    fn module_name(&self, path: &str, _project: &ProjectFiles) -> String {
        path.to_string()
    }

    fn resolve_import(&self, from_path: &str, import: &Import, project: &ProjectFiles) -> Option<String> {
        resolve_import(from_path, &import.module, project)
    }

    fn is_builtin(&self, _name: &str) -> bool {
        false
    }

    fn self_receivers(&self) -> &'static [&'static str] {
        &[]
    }

    /// Terraform modules are the `.tf` files of a directory.
    fn module_scope(&self, language: Language) -> ModuleScope {
        if language == Language::Hcl { ModuleScope::Directory } else { ModuleScope::File }
    }

    /// Kinds are chosen by file name (`compose.yaml`, workflows, …).
    fn facts_depend_on_path(&self) -> bool {
        true
    }
}

/// RFC 0003 §4.5 rule 2: data files that are not configuration by name or
/// place.
fn is_data_path(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    if [".min.json", ".geojson", ".har", ".ipynb"].iter().any(|s| name.ends_with(s)) {
        return true;
    }
    let dirs: Vec<&str> = path.split('/').collect();
    let dirs = &dirs[..dirs.len().saturating_sub(1)];
    dirs.iter().any(|d| matches!(*d, "fixtures" | "__fixtures__" | "testdata" | "__snapshots__"))
        || dirs.windows(2).any(|w| w == ["test", "data"])
}

/// RFC 0003 §4.5 rule 3: a generated-file marker in the first 512 bytes.
fn is_generated(source: &[u8]) -> bool {
    let head = String::from_utf8_lossy(&source[..source.len().min(512)]);
    head.contains("@generated") || (head.contains("Code generated") && head.contains("DO NOT EDIT"))
}

/// Helm chart templates are Go templates, not YAML (RFC 0003 §5).
fn is_template(path: &str, src: &str) -> bool {
    (path.starts_with("templates/") || path.contains("/templates/")) && src.contains("{{")
}

/// Extracts one data file (see the crate docs).
pub fn extract(path: &str, language: Language, source: &[u8], budget: &Budget) -> Result<FileFacts, ExtractError> {
    if !LANGUAGES.contains(&language) {
        return Err(ExtractError::Unsupported(language));
    }
    if source.len() > budget.max_bytes {
        return Err(ExtractError::TooLarge { size: source.len(), limit: budget.max_bytes });
    }
    if is_data_path(path) {
        return Err(ExtractError::Skipped("data".into()));
    }
    if is_generated(source) {
        return Err(ExtractError::Skipped("generated".into()));
    }
    let (mut src, lossy) = match std::str::from_utf8(source) {
        Ok(s) => (std::borrow::Cow::Borrowed(s), false),
        Err(_) => (String::from_utf8_lossy(source), true),
    };
    // A BOM becomes spaces so byte offsets stay exact.
    if src.starts_with('\u{feff}') {
        src = std::borrow::Cow::Owned(format!("   {}", &src[3..]));
    }
    let src: &str = &src;
    let path_kind = kinds::by_path(path, language);
    let exempt = matches!(language, Language::Hcl | Language::Dockerfile | Language::Xml);
    if source.len() > MAX_DATA_FILE_BYTES && !exempt && path_kind.is_none() && !kinds::sniff_large_kind(src) {
        return Err(ExtractError::Skipped("data-size".into()));
    }
    if language == Language::Yaml && is_template(path, src) {
        return Err(ExtractError::Skipped("template".into()));
    }
    let style = match language {
        Language::Json => CommentStyle::Slash,
        Language::Ini => CommentStyle::Ini,
        Language::Properties => CommentStyle::Properties,
        Language::Hcl => CommentStyle::Hcl,
        Language::Xml => CommentStyle::None,
        _ => CommentStyle::Hash,
    };
    let mut out = emit::Out::new(src, style);
    let mut errors = u32::from(lossy);
    match language {
        #[cfg(feature = "hcl")]
        Language::Hcl => formats::hcl::extract(&mut out, path)?,
        #[cfg(feature = "dockerfile")]
        Language::Dockerfile => formats::dockerfile::extract(&mut out),
        #[cfg(feature = "xml")]
        Language::Xml => formats::xml::extract(&mut out, path)?,
        _ => {
            let tree = parse_tree(language, src)?;
            errors += tree.errors;
            let kind = path_kind.unwrap_or_else(|| kinds::by_shape(&tree, language));
            let ctx = emit::emit_tree(&mut out, &tree, kind);
            if kind == Kind::Generic {
                check_shape(&out, &tree)?;
            }
            kinds::apply(kind, &mut out, &tree, &ctx);
        }
    }
    let mut facts = std::mem::take(&mut out.facts);
    facts.parse_errors = errors + u32::from(out.truncated);
    facts.line_count = rueca_extract::line_count(source);
    Ok(facts)
}

fn parse_tree(language: Language, src: &str) -> Result<tree::Tree, ExtractError> {
    let parser_err = |e: String| ExtractError::Parser(e);
    Ok(match language {
        #[cfg(feature = "json")]
        Language::Json => formats::json::parse(src).map_err(parser_err)?,
        #[cfg(feature = "yaml")]
        Language::Yaml => formats::yaml::parse(src),
        #[cfg(feature = "toml")]
        Language::Toml => formats::toml::parse(src).map_err(parser_err)?,
        Language::DotEnv => formats::lines::dotenv(src),
        Language::Ini => formats::lines::ini(src),
        Language::Properties => formats::lines::properties(src),
        other => return Err(ExtractError::Unsupported(other)),
    })
}

/// RFC 0003 §4.5 rule 5, for files of no known kind: a root array of more
/// than 100 elements, more than 5,000 keys, or (in files with at least 20
/// definitions) more array elements than other keys is a data dump.
fn check_shape(out: &emit::Out<'_>, tree: &tree::Tree) -> Result<(), ExtractError> {
    let shape = || Err(ExtractError::Skipped("data-shape".into()));
    if tree.docs.iter().any(|d| d.as_seq().is_some_and(|s| s.len() > MAX_ROOT_ELEMENTS)) {
        return shape();
    }
    if out.keys >= MAX_KEYS && out.truncated {
        return shape();
    }
    let total = out.facts.definitions.len();
    if total >= 20 && out.elements * 2 > total {
        return shape();
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Imports
// ---------------------------------------------------------------------------

fn parent_dir(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

/// Joins `rel` to `dir` and folds `.` / `..`; `None` above the root.
fn normalize(dir: &str, rel: &str) -> Option<String> {
    let mut parts: Vec<&str> =
        if rel.starts_with('/') { Vec::new() } else { dir.split('/').filter(|s| !s.is_empty()).collect() };
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    Some(parts.join("/"))
}

/// The file an import of directory `dir` means for the importing kind:
/// a GitHub action's `action.yml`, a Maven module's `pom.xml`, a workspace's
/// `package.json`; else the directory itself.
fn dir_target(from: &str, dir: &str, project: &ProjectFiles) -> String {
    let from_name = from.rsplit('/').next().unwrap_or(from);
    let index: &[&str] = if kinds::is_github_actions(from) {
        &["action.yml", "action.yaml"]
    } else {
        match from_name {
            "pom.xml" => &["pom.xml"],
            "package.json" => &["package.json"],
            _ => &[],
        }
    };
    index
        .iter()
        .map(|i| if dir.is_empty() { i.to_string() } else { format!("{dir}/{i}") })
        .find(|p| project.contains(p))
        .unwrap_or_else(|| dir.to_string())
}

/// Resolves a data import: relative to the importing file (`/x` from the
/// root), with `.json` appended for extension-less `tsconfig` `extends`;
/// GitHub Actions `uses: ./x` is relative to the repository root. A
/// directory resolves to itself (`IMPORTS` File → Folder) or to the kind's
/// index file. URLs, registry and git sources are external.
pub fn resolve_import(from: &str, spec: &str, project: &ProjectFiles) -> Option<String> {
    let spec = spec.trim();
    if spec.is_empty()
        || spec.contains("://")
        || spec.starts_with("git@")
        || spec.starts_with("git::")
        || spec.starts_with('~')
        || spec.starts_with('$')
    {
        return None;
    }
    let mut candidates = vec![normalize(parent_dir(from), spec)];
    if kinds::is_github_actions(from) {
        candidates.insert(0, normalize("", spec));
    }
    for c in candidates.into_iter().flatten() {
        if !c.is_empty() && project.contains(&c) {
            return Some(c);
        }
        if from.ends_with(".json") && !c.ends_with(".json") && project.contains(&format!("{c}.json")) {
            return Some(format!("{c}.json"));
        }
        if project.is_dir(&c) {
            return Some(dir_target(from, &c, project));
        }
    }
    None
}

#[cfg(test)]
mod tests;
