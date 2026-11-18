//! `rueca-service.toml` (RFC 0008 §4.4): every table and key, with
//! `deny_unknown_fields`, `env:` secret references and string durations
//! and sizes.
//!
//! Every role honours `[store]`, `[org].name`, `[http]`, `[auth]` and
//! `[telemetry]`; the indexer role (`indexer`, `all`, phase 2) also
//! `[[repo]]`, `[indexing]` and `[retention]`. `[org].inventory`,
//! `[indexing].webhooks` and `[retrieval]` are parsed and type-checked (their
//! secrets resolved) but drive nothing yet
//! ([`ServiceConfig::unused_sections`]). Repository URLs are checked
//! against the `[indexing].hosts` allowlist (RFC 0008 §9.2) by every role.
//!
//! **Precedence** (RFC 0007 §8.3): argument, environment, file, built-in
//! default. Environment: `RUECA_CONFIG` (the file, when `--config` is not
//! given), `RUECA_ROLE` (when `--role` is not given), `RUECA_STORE_URL`
//! (replaces `[store].url`), `RUECA_LOG` (the log filter), and the
//! variables `env:` references name.

use std::collections::BTreeMap;
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

use anyhow::{Context, bail};
use serde::{Deserialize, Deserializer};

use crate::units::{Dur, Size};

/// Scope required by `/mcp` and `/status`.
pub const SCOPE_READ: &str = "rueca:read";
/// Scope required by `/admin/*` (phase 2).
pub const SCOPE_ADMIN: &str = "rueca:admin";

/// The role a process runs (RFC 0008 §4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Role {
    #[default]
    Query,
    Indexer,
    All,
}

impl Role {
    pub fn parse(s: &str) -> anyhow::Result<Role> {
        match s.trim() {
            "query" => Ok(Role::Query),
            "indexer" => Ok(Role::Indexer),
            "all" => Ok(Role::All),
            other => bail!("unknown role `{other}` (query, indexer, all)"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Role::Query => "query",
            Role::Indexer => "indexer",
            Role::All => "all",
        }
    }
}

/// A secret: an environment reference `env:NAME` in the file, resolved at
/// load. A literal is a configuration error; the value never prints.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret {
    var: String,
    value: Option<String>,
}

impl Secret {
    /// The variable the reference names.
    pub fn var(&self) -> &str {
        &self.var
    }

    /// The resolved value (`None` before [`ServiceConfig::resolve_secrets`]).
    pub fn value(&self) -> Option<&str> {
        self.value.as_deref()
    }

    fn resolve(&mut self, key: &str, env: &dyn Fn(&str) -> Option<String>, problems: &mut Vec<String>) {
        match env(&self.var).filter(|v| !v.is_empty()) {
            Some(v) => self.value = Some(v),
            None => problems.push(format!("{key}: environment variable `{}` is not set", self.var)),
        }
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Secret(env:{}, {})", self.var, if self.value.is_some() { "resolved" } else { "unresolved" })
    }
}

impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Secret, D::Error> {
        let s = String::deserialize(d)?;
        match s.strip_prefix("env:") {
            Some(var) if !var.is_empty() && var.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') => {
                Ok(Secret { var: var.to_string(), value: None })
            }
            _ => Err(serde::de::Error::custom(
                "a secret must be an environment reference `env:NAME`, never a literal value",
            )),
        }
    }
}

/// The whole `rueca-service.toml`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceConfig {
    pub store: StoreConfig,
    #[serde(default)]
    pub org: OrgConfig,
    #[serde(default)]
    pub repo: Vec<RepoConfig>,
    #[serde(default)]
    pub http: HttpConfig,
    #[serde(default)]
    pub auth: AuthConfig,
    #[serde(default)]
    pub indexing: IndexingConfig,
    #[serde(default)]
    pub retention: RetentionConfig,
    #[serde(default)]
    pub retrieval: RetrievalConfig,
    #[serde(default)]
    pub telemetry: TelemetryConfig,
}

/// `[store]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoreConfig {
    /// `s3://`, `gs://`, `file:///`, `memory://` (`az://` is deferred).
    /// `RUECA_STORE_URL` replaces it; it may then be omitted.
    #[serde(default)]
    pub url: String,
    /// Backend options (`region`, `endpoint`, …); a value `env:NAME` is
    /// read from the environment.
    #[serde(default)]
    pub options: BTreeMap<String, String>,
    /// The replica's disk cache: the mirror the engine reads.
    #[serde(default = "default_cache_dir")]
    pub cache_dir: PathBuf,
    /// Disk budget of the cache (segments outside the current generation
    /// are evicted above it).
    #[serde(default = "default_cache_bytes")]
    pub cache_bytes: Size,
    #[serde(default)]
    pub ready_when: ReadyWhen,
    /// Generation watcher period (added in phase 1; default 5 s).
    #[serde(default = "default_poll")]
    pub poll: Dur,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReadyWhen {
    /// Ready once the serving engine is warm: the organization index and
    /// every default segment open in its caches (see `state`, *warm-up*).
    #[default]
    Index,
    /// Also opens and verifies every default segment again, timed into
    /// `rueca_segment_open_seconds`.
    Warm,
}

/// `[org]`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrgConfig {
    /// The organization served; default: the one the store's `store.json`
    /// names.
    pub name: Option<String>,
    /// Phase 3 (§7.1): parsed only.
    pub inventory: Option<InventoryConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InventoryConfig {
    pub host: String,
    pub owner: String,
    pub token: Secret,
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub archived: bool,
}

/// `[[repo]]`: a repository the indexer role keeps indexed (RFC 0008 §7.1).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoConfig {
    pub url: String,
    pub default_branch: Option<String>,
    #[serde(default)]
    pub branches: Vec<String>,
    #[serde(default)]
    pub tags: Globs,
    #[serde(default)]
    pub prs: bool,
}

/// One glob or a list of them.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(untagged)]
pub enum Globs {
    #[default]
    None,
    One(String),
    Many(Vec<String>),
}

/// `[http]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpConfig {
    #[serde(default = "default_listen")]
    pub listen: SocketAddr,
    /// `/healthz`, `/readyz`, `/metrics`.
    #[serde(default = "default_ops_listen")]
    pub ops_listen: SocketAddr,
    /// The URL clients use (`https://host/mcp`): the default OIDC audience,
    /// the PRM `resource`, an allowed host and origin.
    pub canonical_url: Option<String>,
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
    #[serde(default)]
    pub allowed_origins: Vec<String>,
    #[serde(default = "default_max_body")]
    pub max_body_bytes: Size,
    /// Outer bound of a request, and the shutdown drain.
    #[serde(default = "default_request_timeout")]
    pub request_timeout: Dur,
    /// Bound of one tool call.
    #[serde(default = "default_query_deadline")]
    pub query_deadline: Dur,
    /// Per principal, after authentication.
    #[serde(default = "RateLimit::per_principal")]
    pub rate_limit: RateLimit,
    /// Per client IP, before authentication (added in phase 1).
    #[serde(default = "RateLimit::per_ip")]
    pub ip_rate_limit: RateLimit,
    /// Proxies (addresses or CIDR blocks) whose `X-Forwarded-For` /
    /// `X-Real-IP` name the client (added in phase 1); from any other peer
    /// those headers are ignored.
    #[serde(default)]
    pub trusted_proxies: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RateLimit {
    pub per_second: u32,
    pub burst: u32,
}

impl RateLimit {
    fn per_principal() -> RateLimit {
        RateLimit { per_second: 10, burst: 50 }
    }

    fn per_ip() -> RateLimit {
        RateLimit { per_second: 20, burst: 100 }
    }
}

impl Default for HttpConfig {
    fn default() -> HttpConfig {
        HttpConfig {
            listen: default_listen(),
            ops_listen: default_ops_listen(),
            canonical_url: None,
            allowed_hosts: Vec::new(),
            allowed_origins: Vec::new(),
            max_body_bytes: default_max_body(),
            request_timeout: default_request_timeout(),
            query_deadline: default_query_deadline(),
            rate_limit: RateLimit::per_principal(),
            ip_rate_limit: RateLimit::per_ip(),
            trusted_proxies: Vec::new(),
        }
    }
}

/// `[auth]`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthConfig {
    /// `token:<name>` or `group:<name>`: the admin role.
    #[serde(default)]
    pub admins: Vec<String>,
    #[serde(default)]
    pub token: Vec<StaticToken>,
    #[serde(default)]
    pub oidc: Vec<OidcIssuer>,
}

/// `[[auth.token]]`: a static API token, stored as its SHA-256.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StaticToken {
    pub name: String,
    /// `sha256:<64 hex digits>` of the token (`rueca_…`).
    pub hash: String,
    #[serde(default = "default_scopes")]
    pub scopes: Vec<String>,
    /// Repository-id globs this token may read.
    #[serde(default)]
    pub repos: Vec<String>,
}

/// `[[auth.oidc]]`: an external OpenID Connect issuer.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OidcIssuer {
    pub issuer: String,
    /// Default: `[http].canonical_url`.
    pub audience: Option<String>,
    #[serde(default = "default_algorithms")]
    pub algorithms: Vec<String>,
    #[serde(default = "default_groups_claim")]
    pub groups_claim: String,
    #[serde(default)]
    pub rules: Vec<VisibilityRule>,
}

/// A visibility rule: `group`, or `claim` = `value`, grants `repos`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisibilityRule {
    pub group: Option<String>,
    pub claim: Option<String>,
    pub value: Option<String>,
    pub repos: Vec<String>,
}

/// `[indexing]`: the indexer role (RFC 0008 §7).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexingConfig {
    /// `git ls-remote` runs at once per host (default 8; added in phase 2).
    pub ls_remote_concurrency: Option<u32>,
    /// Job discovery and lease period of each indexer (default 5 s; added
    /// in phase 2).
    pub discover: Option<Dur>,
    /// Accept `file://` repository URLs (a local bare repository as the
    /// code host). **Tests only**: honoured by debug builds; a release
    /// build refuses a configuration that sets it.
    pub allow_file_urls: Option<bool>,
    pub jobs: Option<u32>,
    pub threads: Option<u32>,
    pub gpu: Option<bool>,
    pub org_updater: Option<bool>,
    pub budget: Option<BudgetConfig>,
    pub worker: Option<WorkerConfig>,
    pub mirror_dir: Option<PathBuf>,
    pub mirror_max_age: Option<Dur>,
    pub facts_dir: Option<PathBuf>,
    pub facts_write_through: Option<bool>,
    pub poll: Option<PollConfig>,
    pub updater_tick: Option<Dur>,
    #[serde(default)]
    pub hosts: Vec<String>,
    pub allow_private_networks: Option<bool>,
    #[serde(default)]
    pub credentials: BTreeMap<String, Secret>,
    #[serde(default)]
    pub webhooks: BTreeMap<String, Secret>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetConfig {
    pub max_bytes: Option<Size>,
    pub max_parse_time: Option<Dur>,
    pub max_depth: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerConfig {
    pub address_space: Option<Size>,
    pub cpu_time: Option<Dur>,
    pub wall_time: Option<Dur>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PollConfig {
    pub min: Option<Dur>,
    pub max: Option<Dur>,
    pub pr_heads: Option<bool>,
}

/// `[retention]` (RFC 0008 §5.8): snapshots kept, expiry, the GC grace.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetentionConfig {
    pub default_snapshots: Option<u32>,
    pub branch_snapshots: Option<u32>,
    pub tag_snapshots: Option<u32>,
    pub pr_after_close: Option<Dur>,
    pub pr_idle: Option<Dur>,
    pub grace: Option<Dur>,
}

/// `[retrieval]` (phase 3: parsed only).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetrievalConfig {
    pub preset: Option<String>,
    pub allow_remote: Option<bool>,
}

/// `[telemetry]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelemetryConfig {
    #[serde(default)]
    pub log_format: LogFormat,
    /// OTLP/HTTP collector; exported only by a build with feature `otel`.
    pub otlp_endpoint: Option<String>,
    /// Serve `/metrics` on `ops_listen`.
    #[serde(default = "yes")]
    pub prometheus: bool,
}

impl Default for TelemetryConfig {
    fn default() -> TelemetryConfig {
        TelemetryConfig { log_format: LogFormat::default(), otlp_endpoint: None, prometheus: true }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    #[default]
    Json,
    Pretty,
}

fn yes() -> bool {
    true
}
fn default_cache_dir() -> PathBuf {
    rueca_segment::Store::default_dir().join("service")
}
fn default_cache_bytes() -> Size {
    Size(40 << 30)
}
fn default_poll() -> Dur {
    Dur::secs(5)
}
fn default_listen() -> SocketAddr {
    SocketAddr::from(([0, 0, 0, 0], 8080))
}
fn default_ops_listen() -> SocketAddr {
    SocketAddr::from(([0, 0, 0, 0], 9090))
}
fn default_max_body() -> Size {
    Size(1 << 20)
}
fn default_request_timeout() -> Dur {
    Dur::secs(12)
}
fn default_query_deadline() -> Dur {
    Dur::secs(10)
}
fn default_scopes() -> Vec<String> {
    vec![SCOPE_READ.to_string()]
}
fn default_algorithms() -> Vec<String> {
    vec!["RS256".to_string()]
}
fn default_groups_claim() -> String {
    "groups".to_string()
}

/// Algorithms an OIDC issuer may use (never `none` or `HS*`).
const ASYMMETRIC: &[&str] = &["RS256", "RS384", "RS512", "PS256", "PS384", "PS512", "ES256", "ES384", "EdDSA"];

/// The arguments and environment a configuration is loaded with.
#[derive(Debug, Clone, Default)]
pub struct Sources {
    /// `--config`.
    pub path: Option<PathBuf>,
    /// `--role`.
    pub role: Option<Role>,
}

/// A loaded configuration: the file's content after precedence, with
/// secrets resolved, and the role.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub config: ServiceConfig,
    pub role: Role,
    pub path: PathBuf,
}

/// Loads, resolves and validates the configuration. `Err` lists every
/// problem found (one per line).
pub fn load(sources: &Sources, env: &dyn Fn(&str) -> Option<String>) -> Result<Loaded, Vec<String>> {
    let path = match sources.path.clone().or_else(|| env("RUECA_CONFIG").map(PathBuf::from)) {
        Some(p) => p,
        None => return Err(vec!["no configuration file: pass --config <file> or set RUECA_CONFIG".to_string()]),
    };
    let role = match sources.role {
        Some(r) => r,
        None => match env("RUECA_ROLE") {
            Some(r) => Role::parse(&r).map_err(|e| vec![format!("RUECA_ROLE: {e}")])?,
            None => Role::Query,
        },
    };
    let text = std::fs::read_to_string(&path).map_err(|e| vec![format!("{}: {e}", path.display())])?;
    let mut config = ServiceConfig::parse(&text).map_err(|e| vec![format!("{}: {e:#}", path.display())])?;
    if let Some(url) = env("RUECA_STORE_URL").filter(|u| !u.is_empty()) {
        config.store.url = url;
    }
    let mut problems = config.resolve_secrets(env);
    problems.extend(config.validate());
    if problems.is_empty() { Ok(Loaded { config, role, path }) } else { Err(problems) }
}

impl ServiceConfig {
    /// Parses the TOML text (no secret resolution, no validation).
    pub fn parse(text: &str) -> anyhow::Result<ServiceConfig> {
        toml::from_str(text).context("invalid rueca-service.toml")
    }

    /// Resolves every `env:` reference (secrets and store options);
    /// returns the problems.
    pub fn resolve_secrets(&mut self, env: &dyn Fn(&str) -> Option<String>) -> Vec<String> {
        let mut problems = Vec::new();
        if let Some(inv) = &mut self.org.inventory {
            inv.token.resolve("org.inventory.token", env, &mut problems);
        }
        for (host, s) in &mut self.indexing.credentials {
            s.resolve(&format!("indexing.credentials.{host}"), env, &mut problems);
        }
        for (host, s) in &mut self.indexing.webhooks {
            s.resolve(&format!("indexing.webhooks.{host}"), env, &mut problems);
        }
        for (key, value) in &mut self.store.options {
            let Some(var) = value.strip_prefix("env:").map(str::to_string) else { continue };
            match env(&var).filter(|v| !v.is_empty()) {
                Some(v) => *value = v,
                None => problems.push(format!("store.options.{key}: environment variable `{var}` is not set")),
            }
        }
        problems
    }

    /// Semantic checks beyond types; returns the problems.
    pub fn validate(&self) -> Vec<String> {
        let mut p = Vec::new();
        if self.store.url.is_empty() {
            p.push("store.url: missing (or set RUECA_STORE_URL)".to_string());
        }
        if let Some(name) = &self.org.name
            && let Err(e) = rueca_segment::org::validate_name(name)
        {
            p.push(format!("org.name: {e}"));
        }
        if self.store.poll.0.is_zero() {
            p.push("store.poll: must be positive".to_string());
        }
        self.validate_http(&mut p);
        self.validate_auth(&mut p);
        crate::indexer::validate(self, &mut p);
        p
    }

    fn validate_http(&self, p: &mut Vec<String>) {
        let h = &self.http;
        if let Some(u) = &h.canonical_url {
            match url::Url::parse(u) {
                Ok(parsed) if matches!(parsed.scheme(), "http" | "https") && parsed.host_str().is_some() => {}
                _ => p.push(format!("http.canonical_url: `{u}` is not an absolute http(s) URL")),
            }
        }
        Self::validate_http_lists(h, p);
        Self::validate_http_limits(h, p);
    }

    fn validate_http_lists(h: &HttpConfig, p: &mut Vec<String>) {
        for o in &h.allowed_origins {
            if url::Url::parse(o).map_or(true, |u| u.host_str().is_none()) {
                p.push(format!("http.allowed_origins: `{o}` is not an origin (scheme://host[:port])"));
            }
        }
        for proxy in &h.trusted_proxies {
            if parse_cidr(proxy).is_none() {
                p.push(format!("http.trusted_proxies: `{proxy}` is not an IP address or CIDR block"));
            }
        }
    }

    fn validate_http_limits(h: &HttpConfig, p: &mut Vec<String>) {
        for (key, rl) in [("http.rate_limit", h.rate_limit), ("http.ip_rate_limit", h.ip_rate_limit)] {
            if rl.per_second == 0 || rl.burst == 0 {
                p.push(format!("{key}: per_second and burst must be positive"));
            }
        }
        if h.query_deadline.0.is_zero() || h.request_timeout.0.is_zero() {
            p.push("http.query_deadline and http.request_timeout must be positive".to_string());
        }
        if h.max_body_bytes.0 == 0 {
            p.push("http.max_body_bytes: must be positive".to_string());
        }
    }

    fn validate_auth(&self, p: &mut Vec<String>) {
        let a = &self.auth;
        for admin in &a.admins {
            if !(admin.starts_with("token:") || admin.starts_with("group:")) {
                p.push(format!("auth.admins: `{admin}` must be `token:<name>` or `group:<name>`"));
            }
        }
        let mut names = std::collections::BTreeSet::new();
        for t in &a.token {
            if !names.insert(t.name.as_str()) {
                p.push(format!("auth.token: duplicate name `{}`", t.name));
            }
            if crate::auth::parse_hash(&t.hash).is_none() {
                p.push(format!("auth.token `{}`: hash must be `sha256:<64 hex digits>`", t.name));
            }
            check_globs(&format!("auth.token `{}`", t.name), &t.repos, p);
        }
        for o in &a.oidc {
            self.validate_issuer(o, p);
        }
    }

    fn validate_issuer(&self, o: &OidcIssuer, p: &mut Vec<String>) {
        let key = format!("auth.oidc `{}`", o.issuer);
        if url::Url::parse(&o.issuer).map_or(true, |u| !matches!(u.scheme(), "http" | "https")) {
            p.push(format!("{key}: issuer must be an http(s) URL"));
        }
        if o.audience.is_none() && self.http.canonical_url.is_none() {
            p.push(format!("{key}: set `audience` or `[http].canonical_url` (the default audience)"));
        }
        if o.algorithms.is_empty() {
            p.push(format!("{key}: `algorithms` must not be empty"));
        }
        for alg in &o.algorithms {
            if !ASYMMETRIC.contains(&alg.as_str()) {
                p.push(format!("{key}: algorithm `{alg}` is not allowed ({})", ASYMMETRIC.join(", ")));
            }
        }
        for rule in &o.rules {
            let by_group = rule.group.is_some() && rule.claim.is_none() && rule.value.is_none();
            let by_claim = rule.group.is_none() && rule.claim.is_some() && rule.value.is_some();
            if !(by_group || by_claim) {
                p.push(format!("{key}: a rule needs `group`, or `claim` and `value`"));
            }
            check_globs(&key, &rule.repos, p);
        }
    }

    /// The sections `role` parses but does not act on, for a start-up
    /// note.
    pub fn unused_sections(&self, role: Role) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.org.inventory.is_some() {
            out.push("[org].inventory (phase 3)");
        }
        if !self.indexing.webhooks.is_empty() {
            out.push("[indexing].webhooks (phase 2, webhooks)");
        }
        if role == Role::Query {
            let i = &self.indexing;
            if !self.repo.is_empty() {
                out.push("[[repo]] (indexer role)");
            }
            if i.jobs.is_some() || i.budget.is_some() || i.mirror_dir.is_some() || !i.credentials.is_empty() {
                out.push("[indexing] (indexer role)");
            }
            if self.retention.grace.is_some() || self.retention.default_snapshots.is_some() {
                out.push("[retention] (indexer role, garbage collection)");
            }
        }
        if self.retrieval.preset.is_some() {
            out.push("[retrieval] (phase 3, embeddings)");
        }
        out
    }

    /// The canonical URL, else `http://<listen>/mcp`.
    pub fn resource_url(&self) -> String {
        self.http.canonical_url.clone().unwrap_or_else(|| format!("http://{}/mcp", self.http.listen))
    }
}

fn check_globs(key: &str, globs: &[String], p: &mut Vec<String>) {
    for g in globs {
        if let Err(e) = crate::visibility::glob(g) {
            p.push(format!("{key}: repository glob `{g}`: {e}"));
        }
    }
}

/// `10.0.0.0/8`, `::1`, `192.168.1.7` → (network, prefix length).
pub fn parse_cidr(text: &str) -> Option<(IpAddr, u8)> {
    let (addr, len) = match text.trim().split_once('/') {
        Some((a, l)) => (a.parse::<IpAddr>().ok()?, Some(l.parse::<u8>().ok()?)),
        None => (text.trim().parse::<IpAddr>().ok()?, None),
    };
    let max = if addr.is_ipv4() { 32 } else { 128 };
    let len = len.unwrap_or(max);
    (len <= max).then_some((addr, len))
}

/// Whether `ip` is inside `net/len` (an IPv4-mapped IPv6 address counts
/// as its IPv4 address).
pub fn cidr_contains(net: IpAddr, len: u8, ip: IpAddr) -> bool {
    let ip = match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 => v4,
    };
    match (net, ip) {
        (IpAddr::V4(n), IpAddr::V4(i)) => same_prefix(u128::from(u32::from(n)), u128::from(u32::from(i)), len, 32),
        (IpAddr::V6(n), IpAddr::V6(i)) => same_prefix(u128::from(n), u128::from(i), len, 128),
        _ => false,
    }
}

fn same_prefix(a: u128, b: u128, len: u8, width: u32) -> bool {
    let len = u32::from(len).min(width);
    if len == 0 {
        return true;
    }
    let shift = width - len;
    (a >> shift) == (b >> shift)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The example of RFC 0008 §4.4, every table and key.
    pub const FULL: &str = r#"
[store]
url = "s3://acme-rueca/prod"
options = { region = "eu-west-1" }
cache_dir = "/var/cache/rueca"
cache_bytes = "40GiB"
ready_when = "index"

[org]
name = "acme"
inventory = { host = "github", owner = "acme", token = "env:GITHUB_TOKEN", include = ["*"], exclude = ["sandbox-*"], archived = false }

[[repo]]
url = "https://github.com/acme/billing.git"
default_branch = "main"
branches = ["release/*"]
tags = "v*"
prs = true

[http]
listen = "0.0.0.0:8080"
ops_listen = "0.0.0.0:9090"
canonical_url = "https://rueca.acme.internal/mcp"
allowed_hosts = ["rueca.acme.internal"]
allowed_origins = []
max_body_bytes = "1MiB"
request_timeout = "12s"
query_deadline = "10s"
rate_limit = { per_second = 10, burst = 50 }

[auth]
admins = ["token:ops", "group:platform"]

[[auth.token]]
name = "ci"
hash = "sha256:6f1ed002ab5595859014ebf0951522d96f1ed002ab5595859014ebf0951522d9"
scopes = ["rueca:read"]
repos = ["github.com/acme/*"]

[[auth.oidc]]
issuer = "https://login.acme.com/realms/eng"
audience = "https://rueca.acme.internal/mcp"
algorithms = ["RS256", "ES256"]
groups_claim = "groups"
rules = [
  { group = "eng", repos = ["github.com/acme/*"] },
  { group = "contractors-billing", repos = ["github.com/acme/billing"] },
]

[indexing]
jobs = 1
threads = 0
gpu = false
org_updater = true
budget = { max_bytes = "2MiB", max_parse_time = "5s", max_depth = 512 }
worker = { address_space = "24GiB", cpu_time = "4h", wall_time = "30m" }
mirror_dir = "/var/lib/rueca/mirrors"
mirror_max_age = "7d"
facts_dir = "/var/lib/rueca/facts"
facts_write_through = false
poll = { min = "45s", max = "8h", pr_heads = false }
updater_tick = "5s"
hosts = ["github.com", "gitlab.com", "bitbucket.org"]
allow_private_networks = false
credentials = { "github.com" = "env:GITHUB_TOKEN" }
webhooks = { github = "env:RUECA_GITHUB_WEBHOOK_SECRET" }

[retention]
default_snapshots = 2
branch_snapshots = 1
tag_snapshots = 1
pr_after_close = "7d"
pr_idle = "14d"
grace = "24h"

[retrieval]
preset = "qwen3-4b-q4"
allow_remote = false

[telemetry]
log_format = "json"
otlp_endpoint = "http://otel-collector:4318"
prometheus = true
"#;

    fn env_of(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let vars: BTreeMap<String, String> = vars.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |k| vars.get(k).cloned()
    }

    #[test]
    fn the_rfc_example_parses_and_validates() {
        let mut c = ServiceConfig::parse(FULL).unwrap();
        assert_eq!(c.store.cache_bytes, Size(40 << 30));
        assert_eq!(c.http.request_timeout, Dur::secs(12));
        assert_eq!(c.indexing.worker.as_ref().unwrap().cpu_time, Some(Dur::secs(4 * 3600)));
        let missing = c.resolve_secrets(&env_of(&[]));
        assert_eq!(missing.len(), 3, "{missing:?}");
        assert!(missing[0].starts_with("org.inventory.token: environment variable `GITHUB_TOKEN`"), "{missing:?}");
        let env = env_of(&[("GITHUB_TOKEN", "ghp_x"), ("RUECA_GITHUB_WEBHOOK_SECRET", "s")]);
        assert!(c.resolve_secrets(&env).is_empty());
        assert_eq!(c.org.inventory.as_ref().unwrap().token.value(), Some("ghp_x"));
        assert!(!format!("{c:?}").contains("ghp_x"), "secrets never print");
        assert_eq!(c.validate(), Vec::<String>::new());
        assert_eq!(c.unused_sections(Role::Query).len(), 6);
        assert_eq!(c.unused_sections(Role::All).len(), 3);
    }

    #[test]
    fn literal_secrets_and_unknown_keys_are_errors() {
        let literal = FULL.replace("token = \"env:GITHUB_TOKEN\"", "token = \"ghp_literal\"");
        let err = format!("{:#}", ServiceConfig::parse(&literal).unwrap_err());
        assert!(err.contains("environment reference"), "{err}");
        let unknown = FULL.replace("[retrieval]", "[retrieval]\nsurprise = 1");
        assert!(format!("{:#}", ServiceConfig::parse(&unknown).unwrap_err()).contains("surprise"));
        let bad_duration = FULL.replace("query_deadline = \"10s\"", "query_deadline = \"10\"");
        assert!(format!("{:#}", ServiceConfig::parse(&bad_duration).unwrap_err()).contains("no unit"));
    }

    #[test]
    fn semantic_problems_are_listed() {
        let text = r#"
[store]
url = "memory://x"
[http]
canonical_url = "not a url"
trusted_proxies = ["10.0.0.0/33"]
rate_limit = { per_second = 0, burst = 1 }
[auth]
admins = ["ops"]
[[auth.token]]
name = "a"
hash = "sha256:zz"
[[auth.oidc]]
issuer = "https://idp"
algorithms = ["HS256", "none"]
rules = [{ group = "g", claim = "c", value = "v", repos = ["["] }]
"#;
        let p = ServiceConfig::parse(text).unwrap().validate();
        for needle in [
            "canonical_url",
            "trusted_proxies",
            "http.rate_limit",
            "auth.admins",
            "hash must be",
            "`HS256` is not allowed",
            "`none` is not allowed",
            "a rule needs",
            "repository glob `[`",
        ] {
            assert!(p.iter().any(|x| x.contains(needle)), "{needle} not in {p:#?}");
        }
    }

    #[test]
    fn precedence_argument_environment_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("svc.toml");
        std::fs::write(&path, "[store]\nurl = \"memory://file\"\n").unwrap();
        let env = env_of(&[("RUECA_CONFIG", path.to_str().unwrap()), ("RUECA_ROLE", "all")]);
        let l = load(&Sources::default(), &env).unwrap();
        assert_eq!((l.role, l.config.store.url.as_str()), (Role::All, "memory://file"));
        let env =
            env_of(&[("RUECA_CONFIG", "/nonexistent"), ("RUECA_ROLE", "all"), ("RUECA_STORE_URL", "memory://env")]);
        let args = Sources { path: Some(path.clone()), role: Some(Role::Indexer) };
        let l = load(&args, &env).unwrap();
        assert_eq!((l.role, l.config.store.url.as_str()), (Role::Indexer, "memory://env"));
        assert!(load(&Sources::default(), &env_of(&[])).unwrap_err()[0].contains("no configuration file"));
        assert_eq!(l.config.http.query_deadline, Dur::secs(10), "built-in default");
    }

    #[test]
    fn cidrs() {
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        let (net, len) = parse_cidr("10.0.0.0/8").unwrap();
        assert!(cidr_contains(net, len, ip("10.1.2.3")));
        assert!(!cidr_contains(net, len, ip("11.0.0.1")));
        assert!(cidr_contains(net, len, ip("::ffff:10.0.0.1")));
        let (net, len) = parse_cidr("192.168.1.7").unwrap();
        assert!(cidr_contains(net, len, ip("192.168.1.7")) && !cidr_contains(net, len, ip("192.168.1.8")));
        let (net, len) = parse_cidr("fd00::/8").unwrap();
        assert!(cidr_contains(net, len, ip("fd12::1")) && !cidr_contains(net, len, ip("fe80::1")));
        assert!(cidr_contains(ip("0.0.0.0"), 0, ip("8.8.8.8")));
        assert!(parse_cidr("1.2.3.4/40").is_none() && parse_cidr("nope").is_none());
    }
}
