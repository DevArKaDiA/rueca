//! OIDC access tokens (RFC 0008 §6.3), validated with `jsonwebtoken`
//! (pure-Rust backend):
//!
//! 1. the unverified `iss` selects a configured issuer (else 401); `alg`
//!    must be one of its `algorithms` (never `none` or `HS*`);
//! 2. `kid` is looked up in the issuer's JWKS, fetched from its discovery
//!    document (`<issuer>/.well-known/openid-configuration` → `jwks_uri`)
//!    and cached for its `Cache-Control: max-age` (clamped to 60 s–24 h;
//!    1 h without one); an unknown `kid` refetches it at most once per
//!    60 s; a failed refetch keeps the previous keys;
//! 3. signature, `exp` and `nbf` with 60 s leeway, `iss` exact, `aud`
//!    containing the issuer's audience (default `[http].canonical_url`);
//! 4. scopes from `scope` (space separated) or `scp` (array), groups from
//!    `groups_claim`, and the repository globs of every matching rule.
//!
//! Fetches use `ureq` over rustls (`ring`) with the platform's root
//! certificates, on a blocking thread.

use std::collections::BTreeSet;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use jsonwebtoken::errors::ErrorKind;
use jsonwebtoken::jwk::{Jwk, JwkSet};
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde_json::Value;
use tokio::sync::Mutex;
use ureq::Agent;
use ureq::tls::{Certificate, RootCerts, TlsConfig, TlsProvider};

use crate::auth::{Challenge, Principal, UNKNOWN_ISSUER};
use crate::config::OidcIssuer;

/// Leeway on `exp` and `nbf`.
const LEEWAY_SECS: u64 = 60;
/// Minimum time between two JWKS fetches caused by unknown `kid`s.
const REFRESH_EVERY: Duration = Duration::from_secs(60);
/// JWKS lifetime bounds and default.
const MIN_TTL: Duration = Duration::from_secs(60);
const MAX_TTL: Duration = Duration::from_secs(24 * 3600);
const DEFAULT_TTL: Duration = Duration::from_secs(3600);

/// The configured issuers and their key caches.
pub struct Oidc {
    issuers: Vec<Issuer>,
    agent: Agent,
}

struct Issuer {
    config: OidcIssuer,
    audience: String,
    algorithms: Vec<Algorithm>,
    keys: Mutex<KeyCache>,
}

#[derive(Default)]
struct KeyCache {
    set: Option<Arc<JwkSet>>,
    expires: Option<Instant>,
    last_fetch: Option<Instant>,
}

impl Oidc {
    pub fn new(issuers: &[OidcIssuer], default_audience: &str) -> anyhow::Result<Oidc> {
        let mut out = Vec::new();
        for c in issuers {
            let algorithms = c
                .algorithms
                .iter()
                .map(|a| Algorithm::from_str(a).map_err(|_| anyhow::anyhow!("unknown algorithm `{a}`")))
                .collect::<anyhow::Result<Vec<_>>>()?;
            out.push(Issuer {
                audience: c.audience.clone().unwrap_or_else(|| default_audience.to_string()),
                algorithms,
                config: c.clone(),
                keys: Mutex::new(KeyCache::default()),
            });
        }
        let https = issuers.iter().any(|i| i.issuer.starts_with("https://"));
        Ok(Oidc { issuers: out, agent: http_agent(https)? })
    }

    pub fn issuers(&self) -> Vec<String> {
        self.issuers.iter().map(|i| i.config.issuer.clone()).collect()
    }

    /// Validates a JWT; see the module docs.
    pub async fn authenticate(&self, token: &str) -> Result<Principal, Challenge> {
        let invalid = |m: &str| Challenge::Invalid(m.to_string());
        let header = jsonwebtoken::decode_header(token).map_err(|_| invalid("malformed token"))?;
        let unverified: Value =
            jsonwebtoken::dangerous::insecure_decode_claims(token).map_err(|_| invalid("malformed token"))?;
        let iss = unverified.get("iss").and_then(Value::as_str).unwrap_or_default();
        let issuer = self.issuers.iter().find(|i| i.config.issuer == iss).ok_or_else(|| invalid(UNKNOWN_ISSUER))?;
        if !issuer.algorithms.contains(&header.alg) {
            return Err(invalid("token algorithm is not accepted"));
        }
        let jwk = issuer.key(&self.agent, header.kid.as_deref()).await?;
        let key = DecodingKey::from_jwk(&jwk).map_err(|_| invalid("issuer key is unusable"))?;
        let mut validation = Validation::new(header.alg);
        validation.leeway = LEEWAY_SECS;
        validation.validate_nbf = true;
        validation.set_issuer(&[iss]);
        validation.set_audience(&[issuer.audience.as_str()]);
        validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
        let claims = jsonwebtoken::decode::<Value>(token, &key, &validation)
            .map_err(|e| Challenge::Invalid(describe(e.kind()).to_string()))?
            .claims;
        let sub = claims.get("sub").and_then(Value::as_str).ok_or_else(|| invalid("token has no subject"))?;
        let groups = strings(claims.get(&issuer.config.groups_claim));
        let repos = issuer.repos(&claims, &groups);
        Ok(Principal { id: format!("oidc:{iss}#{sub}"), scopes: scopes(&claims), groups, repos })
    }
}

impl Issuer {
    /// The key `kid` names (no `kid`: the set's only key), refreshing the
    /// set when it expired or does not know `kid` (at most once per 60 s).
    async fn key(&self, agent: &Agent, kid: Option<&str>) -> Result<Jwk, Challenge> {
        let mut cache = self.keys.lock().await;
        let now = Instant::now();
        let fresh = cache.expires.is_some_and(|e| e > now);
        let known = cache.set.as_ref().and_then(|s| pick(s, kid)).is_some();
        let may_refetch = cache.last_fetch.is_none_or(|t| now.duration_since(t) >= REFRESH_EVERY);
        if !fresh || (!known && may_refetch) {
            cache.last_fetch = Some(now);
            let issuer = self.config.issuer.clone();
            let agent = agent.clone();
            let fetched = tokio::task::spawn_blocking(move || fetch_jwks(&agent, &issuer)).await;
            match fetched {
                Ok(Ok((set, ttl))) => {
                    cache.set = Some(Arc::new(set));
                    cache.expires = Some(now + ttl);
                }
                Ok(Err(e)) => {
                    tracing::warn!(issuer = %self.config.issuer, error = %format!("{e:#}"), "JWKS fetch failed")
                }
                Err(e) => tracing::warn!(issuer = %self.config.issuer, error = %e, "JWKS fetch task failed"),
            }
        }
        let set = cache.set.clone().ok_or_else(|| Challenge::Invalid("issuer keys are unavailable".to_string()))?;
        pick(&set, kid).cloned().ok_or_else(|| Challenge::Invalid("token key is unknown".to_string()))
    }

    /// Repository globs of every rule the claims satisfy.
    fn repos(&self, claims: &Value, groups: &[String]) -> Vec<String> {
        let mut out = BTreeSet::new();
        for rule in &self.config.rules {
            let by_group = rule.group.as_ref().is_some_and(|g| groups.contains(g));
            let by_claim = match (&rule.claim, &rule.value) {
                (Some(c), Some(v)) => strings(claims.get(c)).contains(v),
                _ => false,
            };
            if by_group || by_claim {
                out.extend(rule.repos.iter().cloned());
            }
        }
        out.into_iter().collect()
    }
}

fn pick<'s>(set: &'s JwkSet, kid: Option<&str>) -> Option<&'s Jwk> {
    match kid {
        Some(k) => set.find(k),
        None if set.keys.len() == 1 => set.keys.first(),
        None => None,
    }
}

/// A claim as a list of strings: an array of strings, or one string.
fn strings(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).map(str::to_string).collect(),
        _ => Vec::new(),
    }
}

/// `scope` (space separated) or `scp` (array or string).
fn scopes(claims: &Value) -> BTreeSet<String> {
    match claims.get("scope").and_then(Value::as_str) {
        Some(s) => s.split_whitespace().map(str::to_string).collect(),
        None => strings(claims.get("scp")).iter().flat_map(|s| s.split_whitespace()).map(str::to_string).collect(),
    }
}

fn describe(kind: &ErrorKind) -> &'static str {
    match kind {
        ErrorKind::ExpiredSignature => "token expired",
        ErrorKind::ImmatureSignature => "token is not valid yet",
        ErrorKind::InvalidAudience => "token audience does not match",
        ErrorKind::InvalidIssuer => "token issuer does not match",
        ErrorKind::InvalidSignature => "token signature does not verify",
        ErrorKind::MissingRequiredClaim(_) => "token lacks a required claim",
        ErrorKind::InvalidAlgorithm => "token algorithm is not accepted",
        _ => "invalid token",
    }
}

/// A blocking HTTP agent over rustls (`ring`) trusting the platform's
/// roots (loaded only when `https`): discovery documents, JWKS, OTLP.
pub(crate) fn http_agent(https: bool) -> anyhow::Result<Agent> {
    let roots = if https { native_roots()? } else { Vec::new() };
    let tls = TlsConfig::builder()
        .provider(TlsProvider::Rustls)
        .root_certs(RootCerts::from(roots))
        .unversioned_rustls_crypto_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .build();
    Ok(Agent::config_builder()
        .tls_config(tls)
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(10)))
        .build()
        .new_agent())
}

fn native_roots() -> anyhow::Result<Vec<Certificate<'static>>> {
    let found = rustls_native_certs::load_native_certs();
    if found.certs.is_empty() {
        bail!("no root certificates found on this system: {:?}", found.errors);
    }
    Ok(found.certs.iter().map(|c| Certificate::from_der(c.as_ref()).to_owned()).collect())
}

/// Discovery document → `jwks_uri` → the key set and its lifetime.
fn fetch_jwks(agent: &Agent, issuer: &str) -> anyhow::Result<(JwkSet, Duration)> {
    let discovery = format!("{}/.well-known/openid-configuration", issuer.trim_end_matches('/'));
    let (doc, _) = get_json(agent, &discovery)?;
    let jwks_uri = doc.get("jwks_uri").and_then(Value::as_str).context("discovery document has no `jwks_uri`")?;
    let (keys, max_age) = get_json(agent, jwks_uri)?;
    let set: JwkSet = serde_json::from_value(keys).with_context(|| format!("{jwks_uri} is not a JWK set"))?;
    let ttl = max_age.map_or(DEFAULT_TTL, |a| a.clamp(MIN_TTL, MAX_TTL));
    tracing::info!(issuer, keys = set.keys.len(), ttl_s = ttl.as_secs(), "fetched JWKS");
    Ok((set, ttl))
}

/// GET a JSON document; returns it and its `Cache-Control: max-age`.
fn get_json(agent: &Agent, url: &str) -> anyhow::Result<(Value, Option<Duration>)> {
    let mut resp = agent.get(url).header("accept", "application/json").call().with_context(|| format!("GET {url}"))?;
    if resp.status() != 200 {
        bail!("GET {url}: HTTP {}", resp.status());
    }
    let max_age = resp
        .headers()
        .get("cache-control")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').find_map(|d| d.trim().strip_prefix("max-age=")?.parse::<u64>().ok()))
        .map(Duration::from_secs);
    let text = resp.body_mut().read_to_string().with_context(|| format!("reading {url}"))?;
    Ok((serde_json::from_str(&text).with_context(|| format!("{url} is not JSON"))?, max_age))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn claims_to_scopes_and_groups() {
        assert_eq!(scopes(&json!({"scope": "rueca:read  openid"})).len(), 2);
        assert!(scopes(&json!({"scp": ["rueca:read"]})).contains("rueca:read"));
        assert!(scopes(&json!({})).is_empty());
        assert_eq!(strings(Some(&json!("eng"))), ["eng"]);
        assert_eq!(strings(Some(&json!(["a", 1, "b"]))), ["a", "b"]);
    }

    #[test]
    fn rules_by_group_or_claim() {
        let cfg: OidcIssuer = toml::from_str(
            r#"
issuer = "https://idp"
rules = [
  { group = "eng", repos = ["github.com/acme/*"] },
  { claim = "dept", value = "billing", repos = ["github.com/acme/billing", "github.com/acme/ledger"] },
]
"#,
        )
        .unwrap();
        let issuer =
            Issuer { audience: "a".into(), algorithms: vec![], config: cfg, keys: Mutex::new(KeyCache::default()) };
        assert_eq!(issuer.repos(&json!({}), &["eng".into()]), ["github.com/acme/*"]);
        assert_eq!(issuer.repos(&json!({"dept": ["billing"]}), &[]).len(), 2);
        assert!(issuer.repos(&json!({"dept": "sales"}), &["ops".into()]).is_empty());
    }
}
