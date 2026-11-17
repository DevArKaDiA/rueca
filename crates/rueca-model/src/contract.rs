//! Contracts between services (RFC 0001 §9.4, RFC 0006 §3.3): HTTP routes,
//! RPC methods and topics that one piece of code provides and another
//! consumes, matched by a canonical key.
//!
//! Packs emit a [`ContractHint`] per route declaration, spec operation or
//! call site ([`crate::FileFacts::contracts`]); the resolver turns them into
//! rows of the segment contract table ([`ContractRow`]) and into
//! `Strategy::Contract` edges inside one snapshot; the organization linker
//! matches them across repositories. Everything here is a pure function of
//! its arguments.
//!
//! # Canonical keys
//!
//! - **HTTP** ([`http_key`], [`http_key_from_parts`]): `"<METHOD> <path>"`.
//!   The method is upper-cased, `*` when unknown or not one of
//!   [`HTTP_METHODS`]. The path drops the scheme,
//!   host and port of an absolute URL (`https://api.x.com:8443/users` →
//!   `/users`) and anything before the first `/` that looks like a base URL
//!   (an interpolation, or text with `.` or `:`: `${BASE}/users`,
//!   `localhost:8080/users`); a bare relative path keeps its first segment
//!   (`users/42` → `/users/42`). The query string and fragment are dropped,
//!   empty segments (`//`) are dropped, the path starts with `/` and has no
//!   trailing `/` (the root stays `/`). Every **parameter segment** becomes
//!   `{}`: a segment containing `:id` (at its start), `{id}`, `<id>`,
//!   `<int:id>`, `[id]` / `[...slug]` (Next.js), `${x}`, `%s` / `%d` / `%v` /
//!   `%i` / `%f` / `%x`, a `*` wildcard (`*`, `**`, `{rest:*}`), or a concatenation /
//!   interpolation gap ([`Part::Hole`]). Literal segments stay literal
//!   (`GET /users/42`); keys are case-sensitive. A path made only of
//!   interpolation gaps (`${base}/${path}`, `"/" + path`) has no key.
//! - **RPC** ([`rpc_key`]): `"<package>.<Service>/<Method>"`, or
//!   `"<Service>/<Method>"` when the package is empty or unknown (code-side
//!   stubs); a package-less key matches any package. Code that knows only
//!   the method writes `"*/<Method>"` ([`ANY_SERVICE`]), which matches that
//!   method of any service.
//! - **Topic**: the topic name as written; the framework names the broker.
//!
//! # Matching
//!
//! [`match_http`], [`match_rpc`] and [`match_keys`] compare a provider key
//! with a consumer key: exact (same key) > template (a provider `{}` segment
//! matches any one consumer segment; for RPC, a package-less side) >
//! method wildcard (either HTTP method is `*`, or either RPC service).
//! Segment counts must be equal; no prefix or suffix tolerance.
//! [`MatchQuality::confidence`] gives the edge confidence; when several
//! providers match one consumer with the same best quality, each edge gets
//! [`AMBIGUOUS_CONFIDENCE`].
//!
//! # Test providers
//!
//! A provider that is a test, or that is defined in a test file
//! ([`is_test_source`]), is a test double (a fake sidecar, a mock server):
//! it keeps its row but is never a match candidate, in one snapshot or
//! across repositories.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{FileId, LocalDefId, NodeId, Span};

/// Which side of a contract a hint is on. Stored by variant index (postcard):
/// new variants are appended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContractSide {
    /// Declares the endpoint: a route handler, a spec operation, an RPC
    /// method, a topic consumer group's subscription target.
    Provides,
    /// Uses the endpoint: an HTTP call, an RPC stub call, a publish.
    Consumes,
}

impl ContractSide {
    /// Every variant, in declaration (on-disk) order.
    pub const ALL: &'static [ContractSide] = &[ContractSide::Provides, ContractSide::Consumes];

    /// `provides` / `consumes`.
    pub fn as_str(self) -> &'static str {
        match self {
            ContractSide::Provides => "provides",
            ContractSide::Consumes => "consumes",
        }
    }

    /// Parses [`ContractSide::as_str`].
    pub fn parse(s: &str) -> Option<ContractSide> {
        ContractSide::ALL.iter().copied().find(|v| v.as_str() == s)
    }
}

impl fmt::Display for ContractSide {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The kind of contract, which decides the key format and the matcher.
/// Stored by variant index (postcard): new variants are appended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContractKind {
    /// HTTP route: key `"<METHOD> <path>"` ([`http_key`]).
    Http,
    /// RPC method: key `"<package>.<Service>/<Method>"` ([`rpc_key`]).
    Rpc,
    /// Message topic / queue: key = the name.
    Topic,
}

impl ContractKind {
    /// Every variant, in declaration (on-disk) order.
    pub const ALL: &'static [ContractKind] = &[ContractKind::Http, ContractKind::Rpc, ContractKind::Topic];

    /// `http` / `rpc` / `topic`.
    pub fn as_str(self) -> &'static str {
        match self {
            ContractKind::Http => "http",
            ContractKind::Rpc => "rpc",
            ContractKind::Topic => "topic",
        }
    }

    /// Parses [`ContractKind::as_str`].
    pub fn parse(s: &str) -> Option<ContractKind> {
        ContractKind::ALL.iter().copied().find(|v| v.as_str() == s)
    }
}

impl fmt::Display for ContractKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One contract end found in a file by its pack.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContractHint {
    pub side: ContractSide,
    pub kind: ContractKind,
    /// Canonical key ([`http_key`], [`rpc_key`], topic name).
    pub key: String,
    /// The text as written (route, URL expression, `rpc` line), at most
    /// [`MAX_RAW_CHARS`] characters ([`truncate_raw`]): the evidence.
    pub raw: String,
    /// The route declaration, call site or spec operation.
    pub span: Span,
    /// `Provides`: the handler / `Route` / `RpcMethod` definition;
    /// `Consumes`: the enclosing definition; `None` = file level.
    pub owner: Option<LocalDefId>,
    /// Short lowercase name of the framework or spec: `flask`, `express`,
    /// `requests`, `openapi`, `proto`, `grpc-python`, …
    pub framework: String,
}

impl ContractHint {
    /// Whether the hint comes from a specification ([`is_spec_framework`]).
    pub fn is_spec(&self) -> bool {
        is_spec_framework(&self.framework)
    }
}

/// One row of a segment's contract table: a [`ContractHint`] placed on a
/// node of the resolved graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContractRow {
    /// The owner definition's node, else the file's `File` node.
    pub node: NodeId,
    pub side: ContractSide,
    pub kind: ContractKind,
    pub key: String,
    pub raw: String,
    pub file: FileId,
    pub span: Span,
    pub framework: String,
}

impl ContractRow {
    /// Whether the row comes from a specification ([`is_spec_framework`]).
    pub fn is_spec(&self) -> bool {
        is_spec_framework(&self.framework)
    }
}

/// Frameworks whose `Provides` hints are specifications (an OpenAPI
/// operation, a `.proto` RPC method) rather than code: code providers link
/// to them with `IMPLEMENTS`.
pub const SPEC_FRAMEWORKS: &[&str] = &["openapi", "proto"];

/// Whether `framework` names a specification ([`SPEC_FRAMEWORKS`]).
pub fn is_spec_framework(framework: &str) -> bool {
    SPEC_FRAMEWORKS.contains(&framework)
}

/// Whether a provider in the file at `path` is test code: a test path of
/// the ranking prior ([`crate::is_test_path`]) or a Go `_test.go` file
/// (the Go pack flags only `Test…` / `Benchmark…` functions as tests, so
/// the fake servers and handlers of a `_test.go` file are not).
pub fn is_test_source(path: &str) -> bool {
    crate::is_test_path(path) || path.ends_with("_test.go")
}

/// Maximum length of [`ContractHint::raw`], in characters.
pub const MAX_RAW_CHARS: usize = 200;

/// `s` trimmed and cut to [`MAX_RAW_CHARS`] characters.
pub fn truncate_raw(s: &str) -> String {
    s.trim().chars().take(MAX_RAW_CHARS).collect()
}

/// Confidence of each edge when several providers match one consumer with
/// the same best [`MatchQuality`].
pub const AMBIGUOUS_CONFIDENCE: f32 = 0.5;

/// How a provider key matched a consumer key, best first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum MatchQuality {
    /// Same key.
    Exact,
    /// A provider `{}` segment matched a literal consumer segment, or one
    /// RPC key has no package.
    Template,
    /// Either HTTP method is `*`, or either RPC service ([`ANY_SERVICE`]).
    MethodWildcard,
}

impl MatchQuality {
    /// Edge confidence: exact 0.9, template 0.85, method wildcard 0.7.
    pub fn confidence(self) -> f32 {
        match self {
            MatchQuality::Exact => 0.9,
            MatchQuality::Template => 0.85,
            MatchQuality::MethodWildcard => 0.7,
        }
    }

    /// `exact` / `template` / `method-wildcard`.
    pub fn as_str(self) -> &'static str {
        match self {
            MatchQuality::Exact => "exact",
            MatchQuality::Template => "template",
            MatchQuality::MethodWildcard => "method-wildcard",
        }
    }
}

impl fmt::Display for MatchQuality {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// HTTP keys
// ---------------------------------------------------------------------------

/// A piece of a URL built by concatenation or interpolation, for
/// [`http_key_from_parts`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part<'a> {
    /// Literal text (a string literal piece). Parameter syntax inside it
    /// (`{id}`, `:id`, `%s`, …) is still recognized.
    Lit(&'a str),
    /// A value unknown statically: a variable, a call, an interpolated
    /// expression.
    Hole,
}

/// Placeholder segment of a canonical path.
const PARAM: &str = "{}";

/// One unit of a URL being canonicalized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tok {
    Char(char),
    /// An interpolation or concatenation gap (`${x}`, `%s`, [`Part::Hole`]).
    Hole,
    /// Parameter syntax (`{id}`, `<int:id>`).
    Param,
}

/// Canonical HTTP key of `method` (`None` or a non-literal method = `*`)
/// and a URL or path as written. `None` when the text has no `/` at all or
/// its path is only interpolation gaps. See the module docs for the rules.
pub fn http_key(method: Option<&str>, raw_url_or_path: &str) -> Option<String> {
    http_key_from_parts(method, &[Part::Lit(raw_url_or_path)])
}

/// [`http_key`] for a URL built from pieces: `"/users/" + id` is
/// `[Part::Lit("/users/"), Part::Hole]`, a template literal
/// `` `${base}/users/${id}` `` is `[Hole, Lit("/users/"), Hole]`.
pub fn http_key_from_parts(method: Option<&str>, parts: &[Part<'_>]) -> Option<String> {
    let mut toks = Vec::new();
    for p in parts {
        match p {
            Part::Lit(s) => tokenize(s, &mut toks),
            Part::Hole => toks.push(Tok::Hole),
        }
    }
    let path = canonical_path(&toks)?;
    Some(format!("{} {path}", http_method(method)))
}

/// HTTP methods a key names; any other method text is `*`.
pub const HTTP_METHODS: &[&str] = &["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS", "TRACE"];

/// Upper-cased method, `*` when absent or not one of [`HTTP_METHODS`].
fn http_method(method: Option<&str>) -> &'static str {
    let m = method.map(|m| m.trim().to_ascii_uppercase()).unwrap_or_default();
    HTTP_METHODS.iter().find(|&&k| k == m).copied().unwrap_or("*")
}

/// Splits literal text into characters, `${…}` / `%s`-style holes and
/// `{…}` / `<…>` parameters.
fn tokenize(s: &str, out: &mut Vec<Tok>) {
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let (tok, len) = token_at(&chars, i);
        out.push(tok);
        i += len;
    }
}

/// The token starting at `chars[i]` and its length in characters.
fn token_at(chars: &[char], i: usize) -> (Tok, usize) {
    let c = chars[i];
    let next = chars.get(i + 1).copied();
    match c {
        '$' if next == Some('{') => match closing(chars, i + 1, '{', '}') {
            Some(end) => (Tok::Hole, end + 1 - i),
            None => (Tok::Char(c), 1),
        },
        '{' => closing(chars, i, '{', '}').map_or((Tok::Char(c), 1), |end| (Tok::Param, end + 1 - i)),
        '<' => closing(chars, i, '<', '>').map_or((Tok::Char(c), 1), |end| (Tok::Param, end + 1 - i)),
        '%' if is_format_verb(next, chars.get(i + 2).copied()) => (Tok::Hole, 2),
        _ => (Tok::Char(c), 1),
    }
}

/// `%s`, `%v`, `%i`, `%x`, and `%d` / `%f` unless they are a
/// percent-encoded byte (`%d0`, `%fa`).
fn is_format_verb(verb: Option<char>, after: Option<char>) -> bool {
    match verb {
        Some('s' | 'v' | 'i' | 'x') => true,
        Some('d' | 'f') => !after.is_some_and(|c| c.is_ascii_hexdigit()),
        _ => false,
    }
}

/// Index of the bracket closing the one at `chars[open]` (nesting counted),
/// on the same path segment.
fn closing(chars: &[char], open: usize, l: char, r: char) -> Option<usize> {
    let mut depth = 0usize;
    for (j, &c) in chars.iter().enumerate().skip(open) {
        if c == l {
            depth += 1;
        } else if c == r {
            depth -= 1;
            if depth == 0 {
                return Some(j);
            }
        } else if c == '/' && l == '<' {
            return None;
        }
    }
    None
}

/// The canonical path of a tokenized URL.
fn canonical_path(toks: &[Tok]) -> Option<String> {
    let toks = trim_space(toks);
    let path = strip_origin(toks)?;
    let end = path.iter().position(|t| matches!(t, Tok::Char('?' | '#'))).unwrap_or(path.len());
    let segments: Vec<&[Tok]> = path[..end].split(|t| *t == Tok::Char('/')).filter(|s| !s.is_empty()).collect();
    if !segments.is_empty() && segments.iter().all(|s| s.iter().all(|t| *t == Tok::Hole)) {
        return None;
    }
    let canon: Vec<String> = segments.iter().map(|s| canonical_segment(s)).collect();
    Some(format!("/{}", canon.join("/")))
}

fn trim_space(toks: &[Tok]) -> &[Tok] {
    let space = |t: &Tok| matches!(t, Tok::Char(c) if c.is_whitespace());
    let start = toks.iter().position(|t| !space(t)).unwrap_or(toks.len());
    let end = toks.iter().rposition(|t| !space(t)).map_or(start, |e| e + 1);
    &toks[start..end]
}

/// The path part of a URL: from its first `/` after `scheme://host`, after
/// `//host`, or after a base-URL-like prefix. `None` without any `/`.
fn strip_origin(toks: &[Tok]) -> Option<&[Tok]> {
    if let Some(after) = after_scheme(toks) {
        let slash = after.iter().position(|t| *t == Tok::Char('/')).unwrap_or(after.len());
        return Some(&after[slash..]);
    }
    let slash = toks.iter().position(|t| *t == Tok::Char('/'))?;
    let prefix = &toks[..slash];
    let host_like = prefix.iter().any(|t| matches!(t, Tok::Hole | Tok::Param | Tok::Char('.' | ':')));
    if prefix.is_empty() || host_like {
        return Some(&toks[slash..]);
    }
    // A bare relative path (`users/42`): keep its first segment. The leading
    // `/` is added by the caller; segments are split on `/` anyway.
    Some(toks)
}

/// The tokens after `scheme://` or a protocol-relative `//`, if present.
fn after_scheme(toks: &[Tok]) -> Option<&[Tok]> {
    if toks.starts_with(&[Tok::Char('/'), Tok::Char('/')]) {
        return Some(&toks[2..]);
    }
    let colon =
        toks.iter().position(|t| !matches!(t, Tok::Char(c) if c.is_ascii_alphanumeric() || "+.-".contains(*c)))?;
    let scheme_ok = colon > 0 && matches!(toks[0], Tok::Char(c) if c.is_ascii_alphabetic());
    let sep = [Tok::Char(':'), Tok::Char('/'), Tok::Char('/')];
    (scheme_ok && toks[colon..].starts_with(&sep)).then(|| &toks[colon + 3..])
}

/// `{}` for a parameter segment, else its literal text.
fn canonical_segment(seg: &[Tok]) -> String {
    if seg.iter().any(|t| !matches!(t, Tok::Char(_))) {
        return PARAM.to_string();
    }
    let text: String = seg.iter().filter_map(|t| if let Tok::Char(c) = t { Some(*c) } else { None }).collect();
    let param = (text.starts_with(':') && text.len() > 1)
        || (text.starts_with('[') && text.ends_with(']') && text.len() > 2)
        || text.contains('*');
    if param { PARAM.to_string() } else { text }
}

// ---------------------------------------------------------------------------
// RPC keys
// ---------------------------------------------------------------------------

/// Service of an RPC key written by code that knows the method but not its
/// service (a Go server method taking `*pb.<Method>Request`, a call through
/// a client field typed in another file): `*/Method` matches that method of
/// any service, at [`MatchQuality::MethodWildcard`].
pub const ANY_SERVICE: &str = "*";

/// Method of a service-marker key `"<Service>/*"`: a `Provides` hint whose
/// owner is a type that serves `<Service>` (a Go struct embedding
/// `Unimplemented<Service>Server`) while its methods may be declared in
/// other files. The resolver turns the marker into one row per method of
/// the type shaped like an RPC handler, `"<Service>/<Method>"`; the marker
/// itself is never a row.
pub const ANY_METHOD: &str = "*";

/// Canonical RPC key: `pkg.Service/Method`, or `Service/Method` when
/// `package` is empty.
pub fn rpc_key(package: &str, service: &str, method: &str) -> String {
    let package = package.trim().trim_matches('.');
    if package.is_empty() { format!("{service}/{method}") } else { format!("{package}.{service}/{method}") }
}

// ---------------------------------------------------------------------------
// Matching
// ---------------------------------------------------------------------------

/// How the provider HTTP key matches the consumer key, if it does.
pub fn match_http(provider_key: &str, consumer_key: &str) -> Option<MatchQuality> {
    if provider_key == consumer_key {
        return Some(MatchQuality::Exact);
    }
    let (pm, pp) = provider_key.split_once(' ')?;
    let (cm, cp) = consumer_key.split_once(' ')?;
    let wildcard = match (pm, cm) {
        _ if pm == cm => false,
        ("*", _) | (_, "*") => true,
        _ => return None,
    };
    let template = match_path(pp, cp)?;
    Some(if wildcard {
        MatchQuality::MethodWildcard
    } else if template {
        MatchQuality::Template
    } else {
        MatchQuality::Exact
    })
}

/// Whether two canonical paths match segment by segment (`None`: no), and
/// whether a provider `{}` stood for a literal consumer segment.
fn match_path(provider: &str, consumer: &str) -> Option<bool> {
    let p: Vec<&str> = provider.split('/').filter(|s| !s.is_empty()).collect();
    let c: Vec<&str> = consumer.split('/').filter(|s| !s.is_empty()).collect();
    if p.len() != c.len() {
        return None;
    }
    let mut template = false;
    for (ps, cs) in p.iter().zip(&c) {
        if ps == cs {
            continue;
        }
        if *ps != PARAM {
            return None;
        }
        template = true;
    }
    Some(template)
}

/// How the provider RPC key matches the consumer key, if it does: exact,
/// template when one side has no package (`Service/Method`), or method
/// wildcard when one side's service is `*` ([`ANY_SERVICE`]).
pub fn match_rpc(provider_key: &str, consumer_key: &str) -> Option<MatchQuality> {
    if provider_key == consumer_key {
        return Some(MatchQuality::Exact);
    }
    let (ps, pm) = provider_key.rsplit_once('/')?;
    let (cs, cm) = consumer_key.rsplit_once('/')?;
    if pm != cm {
        return None;
    }
    if ps == ANY_SERVICE || cs == ANY_SERVICE {
        return Some(MatchQuality::MethodWildcard);
    }
    let packageless = !ps.contains('.') || !cs.contains('.');
    (packageless && service_name(ps) == service_name(cs)).then_some(MatchQuality::Template)
}

/// `Service` of `pkg.Service` (or of `Service`).
fn service_name(qualified: &str) -> &str {
    qualified.rsplit_once('.').map_or(qualified, |(_, n)| n)
}

/// [`match_http`], [`match_rpc`], or exact equality for topics.
pub fn match_keys(kind: ContractKind, provider_key: &str, consumer_key: &str) -> Option<MatchQuality> {
    match kind {
        ContractKind::Http => match_http(provider_key, consumer_key),
        ContractKind::Rpc => match_rpc(provider_key, consumer_key),
        ContractKind::Topic => (provider_key == consumer_key).then_some(MatchQuality::Exact),
    }
}

/// The group of keys a key may match in: same kind and same shape (HTTP:
/// path segment count; RPC: method name; topic: the name). Keys of
/// different buckets never match ([`match_keys`]), so matchers index
/// providers by bucket.
pub fn match_bucket(kind: ContractKind, key: &str) -> (ContractKind, String) {
    let shape = match kind {
        ContractKind::Http => {
            let path = key.split_once(' ').map_or(key, |(_, p)| p);
            path.split('/').filter(|s| !s.is_empty()).count().to_string()
        }
        ContractKind::Rpc => key.rsplit_once('/').map_or(key, |(_, m)| m).to_string(),
        ContractKind::Topic => key.to_string(),
    };
    (kind, shape)
}

/// The providers among `providers` (`(identity, provider key)`) matching
/// consumer key `key` with the best quality: their distinct identities in
/// first-seen order, and that quality (`None`: nothing matches). Shared by
/// the in-snapshot resolver and the organization linker.
pub fn best_matches<'a, T: PartialEq>(
    kind: ContractKind,
    key: &str,
    providers: impl IntoIterator<Item = (T, &'a str)>,
) -> (Vec<T>, Option<MatchQuality>) {
    let mut best: Option<MatchQuality> = None;
    let mut ids: Vec<T> = Vec::new();
    for (id, pkey) in providers {
        let Some(q) = match_keys(kind, pkey, key) else { continue };
        if best.is_none_or(|b| q < b) {
            best = Some(q);
            ids.clear();
        }
        if best == Some(q) && !ids.contains(&id) {
            ids.push(id);
        }
    }
    (ids, best)
}

/// Edge confidence of a best match shared by `providers` distinct
/// providers: the quality's confidence for one, [`AMBIGUOUS_CONFIDENCE`]
/// for several.
pub fn match_confidence(quality: MatchQuality, providers: usize) -> f32 {
    if providers > 1 { AMBIGUOUS_CONFIDENCE } else { quality.confidence() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(method: Option<&str>, raw: &str) -> Option<String> {
        http_key(method, raw)
    }

    #[test]
    fn test_sources() {
        for p in ["tests/clients/fake_dapr_server.py", "client/client_test.go", "src/__tests__/a.ts", "x.spec.ts"] {
            assert!(is_test_source(p), "{p}");
        }
        for p in ["client/client.go", "pkg/testing_utils.go", "server/routes.go", "latest.go"] {
            assert!(!is_test_source(p), "{p}");
        }
    }

    #[test]
    fn http_parameter_forms() {
        let cases = [
            ("/users/:id", "GET /users/{}"),
            ("/users/{id}", "GET /users/{}"),
            ("/users/<id>", "GET /users/{}"),
            ("/users/<int:id>", "GET /users/{}"),
            ("/users/[id]", "GET /users/{}"),
            ("/blog/[...slug]", "GET /blog/{}"),
            ("/users/${user.id}/posts", "GET /users/{}/posts"),
            ("/users/%s", "GET /users/{}"),
            ("/users/%d/x", "GET /users/{}/x"),
            ("/static/*", "GET /static/{}"),
            ("/files/{rest:*}", "GET /files/{}"),
            ("/users/{id}.json", "GET /users/{}"),
            ("/users/42", "GET /users/42"),
            ("/a%20b", "GET /a%20b"),
            ("/x/%d0", "GET /x/%d0"),
            ("/v/%x/%f", "GET /v/{}/{}"),
        ];
        for (raw, want) in cases {
            assert_eq!(k(Some("get"), raw).as_deref(), Some(want), "{raw}");
        }
    }

    #[test]
    fn http_origin_query_and_slashes() {
        let cases = [
            ("https://api.example.com:8443/v1/users/?page=2#top", "/v1/users"),
            ("http://localhost/", "/"),
            ("https://api.example.com", "/"),
            ("//cdn.example.com/a/b", "/a/b"),
            ("${BASE_URL}/users/${id}", "/users/{}"),
            ("localhost:8080/users", "/users"),
            ("api.example.com/users", "/users"),
            ("users/42", "/users/42"),
            ("/", "/"),
            ("  /a//b/  ", "/a/b"),
            ("/search?q=${term}", "/search"),
        ];
        for (raw, want) in cases {
            assert_eq!(k(Some("POST"), raw), Some(format!("POST {want}")), "{raw}");
        }
    }

    #[test]
    fn http_no_key() {
        for raw in ["", "users", "${url}", "${base}/${path}", "%s", "?a=b"] {
            assert_eq!(k(Some("GET"), raw), None, "{raw}");
        }
        assert_eq!(http_key_from_parts(Some("GET"), &[Part::Lit("/"), Part::Hole]), None);
    }

    #[test]
    fn http_methods() {
        assert_eq!(k(None, "/a").as_deref(), Some("* /a"));
        assert_eq!(k(Some(""), "/a").as_deref(), Some("* /a"));
        assert_eq!(k(Some("method"), "/a").as_deref(), Some("* /a"));
        assert_eq!(k(Some("options"), "/a").as_deref(), Some("OPTIONS /a"));
        assert_eq!(k(Some("${m}"), "/a").as_deref(), Some("* /a"));
        assert_eq!(k(Some(" delete "), "/a").as_deref(), Some("DELETE /a"));
    }

    #[test]
    fn http_parts() {
        let key =
            http_key_from_parts(Some("get"), &[Part::Hole, Part::Lit("/users/"), Part::Hole, Part::Lit("/posts")]);
        assert_eq!(key.as_deref(), Some("GET /users/{}/posts"));
        let key = http_key_from_parts(Some("get"), &[Part::Lit("/users/"), Part::Hole]);
        assert_eq!(key.as_deref(), Some("GET /users/{}"));
        let key = http_key_from_parts(None, &[Part::Lit("https://x.io/items/"), Part::Hole, Part::Lit("?full=1")]);
        assert_eq!(key.as_deref(), Some("* /items/{}"));
    }

    #[test]
    fn rpc_keys() {
        assert_eq!(rpc_key("helloworld", "Greeter", "SayHello"), "helloworld.Greeter/SayHello");
        assert_eq!(rpc_key("", "Greeter", "SayHello"), "Greeter/SayHello");
    }

    #[test]
    fn http_matching() {
        use MatchQuality::*;
        let m = match_http;
        assert_eq!(m("GET /users/{}", "GET /users/{}"), Some(Exact));
        assert_eq!(m("GET /users/{}", "GET /users/42"), Some(Template));
        assert_eq!(m("GET /users/42", "GET /users/{}"), None);
        assert_eq!(m("* /users/{}", "GET /users/{}"), Some(MethodWildcard));
        assert_eq!(m("GET /users/{}", "* /users/7"), Some(MethodWildcard));
        assert_eq!(m("GET /users/{}", "POST /users/{}"), None);
        assert_eq!(m("GET /users/{}", "GET /users/{}/posts"), None);
        assert_eq!(m("GET /Users", "GET /users"), None);
        assert_eq!(m("GET /", "GET /"), Some(Exact));
        assert_eq!(m("bad", "GET /"), None);
    }

    #[test]
    fn rpc_matching() {
        use MatchQuality::*;
        assert_eq!(match_rpc("pkg.Greeter/SayHello", "pkg.Greeter/SayHello"), Some(Exact));
        assert_eq!(match_rpc("pkg.Greeter/SayHello", "Greeter/SayHello"), Some(Template));
        assert_eq!(match_rpc("Greeter/SayHello", "a.b.Greeter/SayHello"), Some(Template));
        assert_eq!(match_rpc("a.Greeter/SayHello", "b.Greeter/SayHello"), None);
        assert_eq!(match_rpc("pkg.Greeter/SayHello", "Greeter/SayBye"), None);
        assert_eq!(match_rpc("pkg.Greeter/SayHello", "*/SayHello"), Some(MethodWildcard));
        assert_eq!(match_rpc("*/SayHello", "Greeter/SayHello"), Some(MethodWildcard));
        assert_eq!(match_rpc("*/SayHello", "*/SayHello"), Some(Exact));
        assert_eq!(match_rpc("*/SayHello", "pkg.Greeter/SayBye"), None);
        assert_eq!(match_keys(ContractKind::Topic, "orders", "orders"), Some(Exact));
        assert_eq!(match_keys(ContractKind::Topic, "orders", "Orders"), None);
        assert_eq!(match_keys(ContractKind::Http, "GET /a/{}", "GET /a/b"), Some(Template));
    }

    #[test]
    fn buckets_and_best_matches() {
        assert_eq!(match_bucket(ContractKind::Http, "GET /a/{}").1, "2");
        assert_eq!(match_bucket(ContractKind::Http, "GET /").1, "0");
        assert_eq!(match_bucket(ContractKind::Rpc, "p.S/M").1, "M");
        let k = http_key(Some("get"), "/users/<int:id>").unwrap();
        assert_eq!(match_bucket(ContractKind::Http, &k), match_bucket(ContractKind::Http, "GET /users/42"));
        let providers = [(1, "GET /users/{}"), (2, "GET /users/me"), (3, "* /users/me"), (2, "GET /users/me")];
        assert_eq!(best_matches(ContractKind::Http, "GET /users/me", providers), (vec![2], Some(MatchQuality::Exact)));
        assert_eq!(
            best_matches(ContractKind::Http, "GET /users/7", providers),
            (vec![1], Some(MatchQuality::Template))
        );
        let none: [(u32, &str); 0] = [];
        assert_eq!(best_matches(ContractKind::Http, "GET /x", none), (vec![], None));
        assert_eq!(match_confidence(MatchQuality::Template, 1), 0.85);
        assert_eq!(match_confidence(MatchQuality::Exact, 2), AMBIGUOUS_CONFIDENCE);
    }

    #[test]
    fn confidences_and_names() {
        assert_eq!(MatchQuality::Exact.confidence(), 0.9);
        assert_eq!(MatchQuality::Template.confidence(), 0.85);
        assert_eq!(MatchQuality::MethodWildcard.confidence(), 0.7);
        assert!(AMBIGUOUS_CONFIDENCE < MatchQuality::MethodWildcard.confidence());
        assert!(MatchQuality::Exact < MatchQuality::Template && MatchQuality::Template < MatchQuality::MethodWildcard);
        for s in ContractSide::ALL {
            assert_eq!(ContractSide::parse(s.as_str()), Some(*s));
            assert_eq!(serde_json::to_string(s).unwrap(), format!("\"{s}\""));
        }
        for v in ContractKind::ALL {
            assert_eq!(ContractKind::parse(v.as_str()), Some(*v));
            assert_eq!(serde_json::to_string(v).unwrap(), format!("\"{v}\""));
        }
        assert!(is_spec_framework("openapi") && is_spec_framework("proto") && !is_spec_framework("flask"));
        assert_eq!(truncate_raw(&"x".repeat(300)).len(), MAX_RAW_CHARS);
    }
}
