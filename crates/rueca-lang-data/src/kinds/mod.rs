//! File kinds (RFC 0003 §5): semantic layers over the parsed tree, chosen by
//! path and shape. Kinds add labels, edges and signatures; they never remove
//! the generic keys, so an unrecognized variant degrades to plain keys.

use rueca_model::{Label, Language, LocalDefId, RefKind};

use crate::emit::{Ctx, DocIdentity, Out};
use crate::text;
use crate::tree::{Node, Tree, Value};

mod compose;
mod github_actions;
mod kubernetes;
mod manifests;
mod openapi;

/// The kind of a mapping-shaped file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Plain keys.
    Generic,
    /// `package.json`.
    PackageJson,
    /// `tsconfig*.json`, `jsconfig*.json`.
    TsConfig,
    /// `composer.json`.
    Composer,
    /// `pyproject.toml`.
    PyProject,
    /// `.env`, `.env.*`.
    DotEnv,
    /// Docker compose files.
    Compose,
    /// Kubernetes manifests (detected per document).
    Kubernetes,
    /// GitHub Actions workflows and actions.
    GithubActions,
    /// OpenAPI / Swagger.
    OpenApi,
    /// JSON Schema.
    JsonSchema,
    /// Spring Boot `application*` / `bootstrap*` config.
    Spring,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Generic => "generic",
            Kind::PackageJson => "package.json",
            Kind::TsConfig => "tsconfig",
            Kind::Composer => "composer.json",
            Kind::PyProject => "pyproject",
            Kind::DotEnv => "dotenv",
            Kind::Compose => "compose",
            Kind::Kubernetes => "kubernetes",
            Kind::GithubActions => "github-actions",
            Kind::OpenApi => "openapi",
            Kind::JsonSchema => "json-schema",
            Kind::Spring => "spring",
        }
    }

    /// Dotted keys are split into key-path segments (`server.port`).
    pub(crate) fn splits_dotted_keys(self) -> bool {
        self == Kind::Spring
    }

    /// Kind identity of a document: Kubernetes `Kind/name`.
    pub(crate) fn doc_identity(self, doc: &Node) -> Option<DocIdentity> {
        match self {
            Kind::Kubernetes => kubernetes::identity(doc),
            _ => None,
        }
    }

    /// Identity of a mapping element in the list under key `list_key`.
    pub(crate) fn element_identity(self, list_key: &str, elem: &Node) -> Option<String> {
        match self {
            Kind::Kubernetes => kubernetes::element_identity(list_key, elem),
            Kind::GithubActions => github_actions::element_identity(list_key, elem),
            _ => None,
        }
    }
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Whether `path` is a GitHub Actions workflow or action definition.
pub(crate) fn is_github_actions(path: &str) -> bool {
    let name = file_name(path);
    let yaml = name.ends_with(".yml") || name.ends_with(".yaml");
    (yaml && (path.starts_with(".github/workflows/") || path.contains("/.github/workflows/")))
        || name == "action.yml"
        || name == "action.yaml"
}

/// Whether `path` names a compose file.
pub(crate) fn is_compose_name(path: &str) -> bool {
    let name = file_name(path);
    let stem = name.strip_suffix(".yml").or_else(|| name.strip_suffix(".yaml"));
    stem.is_some_and(|s| s == "compose" || s.starts_with("compose.") || s.starts_with("docker-compose"))
}

/// Kind decided from the path alone (before parsing).
pub(crate) fn by_path(path: &str, language: Language) -> Option<Kind> {
    let name = file_name(path);
    let (stem, ext) = name.rsplit_once('.').unwrap_or((name, ""));
    Some(match language {
        Language::Json => match name {
            "package.json" => Kind::PackageJson,
            "composer.json" => Kind::Composer,
            _ if (stem.starts_with("tsconfig") || stem.starts_with("jsconfig")) => Kind::TsConfig,
            _ if name.ends_with(".schema.json") => Kind::JsonSchema,
            _ => return None,
        },
        Language::Yaml if is_github_actions(path) => Kind::GithubActions,
        Language::Yaml if is_compose_name(path) => Kind::Compose,
        Language::Yaml | Language::Properties if is_spring_name(stem, ext) => Kind::Spring,
        Language::Toml if name == "pyproject.toml" => Kind::PyProject,
        Language::DotEnv => Kind::DotEnv,
        _ => return None,
    })
}

fn is_spring_name(stem: &str, ext: &str) -> bool {
    matches!(ext, "yml" | "yaml" | "properties") && (stem.starts_with("application") || stem.starts_with("bootstrap"))
}

/// Whether the content looks like a large-but-valuable kind (OpenAPI,
/// Kubernetes bundles): these get the general size budget instead of the
/// data-file one (RFC 0003 §4.5 rule 4).
pub(crate) fn sniff_large_kind(src: &str) -> bool {
    let head = &src[..src.len().min(4096)];
    let has = |k: &str| head.contains(k);
    has("openapi") || has("swagger") || (has("apiVersion") && has("kind"))
}

/// Kind decided from the parsed tree when the path did not decide.
pub(crate) fn by_shape(tree: &Tree, language: Language) -> Kind {
    let Some(first) = tree.docs.iter().find(|d| !d.is_null()) else { return Kind::Generic };
    if openapi::is_openapi(first) {
        return Kind::OpenApi;
    }
    if language == Language::Json && first.get("$schema").is_some() && first.get("type").is_some() {
        return Kind::JsonSchema;
    }
    if language == Language::Yaml {
        if tree.docs.iter().any(|d| kubernetes::identity(d).is_some()) {
            return Kind::Kubernetes;
        }
        if compose::is_compose_shape(first) {
            return Kind::Compose;
        }
    }
    Kind::Generic
}

/// Applies the kind's semantics after the generic walk.
pub(crate) fn apply(kind: Kind, out: &mut Out<'_>, tree: &Tree, ctx: &Ctx<'_>) {
    match kind {
        Kind::Generic | Kind::Composer => {}
        Kind::PackageJson => manifests::package_json(out, tree),
        Kind::TsConfig => manifests::tsconfig(out, tree),
        Kind::PyProject => manifests::pyproject(out, tree, ctx),
        Kind::DotEnv => dotenv(out, tree, ctx),
        Kind::Compose => compose::apply(out, tree, ctx),
        Kind::Kubernetes => kubernetes::apply(out, tree, ctx),
        Kind::GithubActions => github_actions::apply(out, tree, ctx),
        Kind::OpenApi | Kind::JsonSchema => openapi::apply(kind, out, tree, ctx),
        Kind::Spring => spring(out, ctx),
    }
}

/// Marks a definition as an env var provider: label `EnvVar`, name = the
/// variable, value never in the signature (RFC 0003 §4.6).
pub(crate) fn provider(out: &mut Out<'_>, id: LocalDefId, name: &str, sep: &str, has_value: bool) {
    out.relabel(id, Label::EnvVar);
    let d = out.get(id);
    d.name = name.to_string();
    d.signature = Some(if has_value { format!("{name}{sep}{}", text::MASK) } else { name.to_string() });
}

/// `.env`: every top-level entry is a provider; `${OTHER}` in a value is a
/// read of `OTHER` by that provider.
fn dotenv(out: &mut Out<'_>, tree: &Tree, ctx: &Ctx<'_>) {
    let Some(doc) = tree.docs.first() else { return };
    for e in doc.as_map().unwrap_or(&[]) {
        let Some(id) = ctx.def_of(&e.value) else { continue };
        provider(out, id, &e.key, "=", !e.value.is_null());
        if !matches!(out.slice(&e.value.span).as_bytes().first(), Some(b'\'')) {
            out.interpolation_reads(&e.value.span, Some(id));
        }
    }
}

/// Spring placeholders `${NAME:default}`: an upper-case name reads an env
/// var, any other name reads the config key (namespace `spring`).
fn spring(out: &mut Out<'_>, ctx: &Ctx<'_>) {
    for site in &ctx.scalars {
        let Value::Scalar(_) = &site.node.value else { continue };
        let raw = out.slice(&site.node.span);
        let mut found = Vec::new();
        let mut rest = raw;
        let mut base = site.node.span.start;
        while let Some(i) = rest.find("${") {
            let after = &rest[i + 2..];
            let Some(close) = after.find('}') else { break };
            let inner = &after[..close];
            let name = inner.split(':').next().unwrap_or("").trim();
            found.push((name.to_string(), base + i..base + i + 2 + close + 1));
            base += i + 2 + close + 1;
            rest = &after[close + 1..];
        }
        for (name, r) in found {
            if name.is_empty() {
                continue;
            }
            let env = name.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_');
            if env {
                out.reference(RefKind::EnvVar, &name, None, site.enclosing, &r);
            } else {
                out.reference(RefKind::ConfigKey, &name, Some("spring"), site.enclosing, &r);
            }
        }
    }
}

/// All scalars under `node` (strings of a list or a single string).
pub(crate) fn strings(node: &Node) -> Vec<&Node> {
    match &node.value {
        Value::Scalar(_) => vec![node],
        Value::Seq(items) => items.iter().filter(|i| matches!(i.value, Value::Scalar(_))).collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_kinds() {
        use Language::*;
        assert_eq!(by_path("package.json", Json), Some(Kind::PackageJson));
        assert_eq!(by_path("web/tsconfig.app.json", Json), Some(Kind::TsConfig));
        assert_eq!(by_path("api/user.schema.json", Json), Some(Kind::JsonSchema));
        assert_eq!(by_path(".github/workflows/ci.yml", Yaml), Some(Kind::GithubActions));
        assert_eq!(by_path("x/.github/workflows/ci.yaml", Yaml), Some(Kind::GithubActions));
        assert_eq!(by_path(".github/actions/setup/action.yml", Yaml), Some(Kind::GithubActions));
        assert_eq!(by_path("docker-compose.prod.yml", Yaml), Some(Kind::Compose));
        assert_eq!(by_path("deploy/compose.yaml", Yaml), Some(Kind::Compose));
        assert_eq!(by_path("src/main/resources/application-prod.yml", Yaml), Some(Kind::Spring));
        assert_eq!(by_path("application.properties", Properties), Some(Kind::Spring));
        assert_eq!(by_path("pyproject.toml", Toml), Some(Kind::PyProject));
        assert_eq!(by_path(".env.example", DotEnv), Some(Kind::DotEnv));
        assert_eq!(by_path("config/app.yaml", Yaml), None);
    }
}
