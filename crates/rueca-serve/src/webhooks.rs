//! `POST /webhooks/github`, `/webhooks/gitlab`, `/webhooks/bitbucket`
//! (RFC 0008 §7.3), on the main listener of the indexer and all roles, one
//! route per host with a secret in `[indexing].webhooks`. No bearer token:
//! the signature authenticates the delivery.
//!
//! The handler never waits on the network:
//!
//! 1. a body over [`MAX_BODY`] (25 MB, the hosts' own cap) → `413`;
//! 2. the signature, constant time → `401` on failure:
//!    - GitHub `X-Hub-Signature-256: sha256=<hex>`, HMAC-SHA256 of the body;
//!    - Bitbucket Cloud `X-Hub-Signature: sha256=<hex>`, the same;
//!    - GitLab with a signing token (`whsec_<base64 key>`): Standard
//!      Webhooks, `webhook-signature: v1,<base64>` (space-separated list)
//!      over `<webhook-id>.<webhook-timestamp>.<body>`, the timestamp within
//!      [`TIMESTAMP_TOLERANCE`]; with any other secret: the legacy
//!      `X-Gitlab-Token`, compared in constant time (SHA-256 of both sides),
//!      which a start-up warning flags as weaker (a replayable shared
//!      secret, no body integrity);
//! 3. a body that is not the event's JSON → `400`;
//! 4. the delivery id (`X-GitHub-Delivery`; `Idempotency-Key`, else
//!    `webhook-id`; `X-Request-UUID`) seen in the last 24 h (at most 100,000
//!    ids, [`DeliveryLog`]) → `200 {"result": "duplicate"}`; duplicates
//!    across replicas coalesce anyway (the desired state is a CAS merge). An
//!    id whose confirmation (step 6) failed is forgotten, so the host's
//!    redelivery (same id) is heard;
//! 5. an event that changes nothing indexed (`ping`, an approval) or a
//!    repository not in `[[repo]]` → `202 {"result": "ignored"}`;
//! 6. otherwise `202 {"result": "accepted"}`, and a blocking task confirms
//!    the hint with `git ls-remote` and merges it into the job
//!    ([`rueca_service::webhooks::apply_hint`]), then wakes this indexer's
//!    job discovery.
//!
//! `rueca_webhook_deliveries_total{host, result}` counts deliveries by
//! answer (`accepted`, `ignored`, `duplicate`, `unauthorized`,
//! `too_large`, `invalid`); `rueca_webhook_confirmations_total{host,
//! result}` the background merges (`changed`, `unchanged`, `failed`). A
//! crash between the answer and the merge is recovered by polling.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use hmac::{Hmac, KeyInit, Mac};
use rueca_service::reconcile::RepoSpec;
use rueca_service::webhooks::{Hint, Parsed, WebhookHost, match_repo, parse};
use serde_json::json;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::config::ServiceConfig;
use crate::mcp::json_response;
use crate::metrics::Metrics;
use crate::state::{AppState, unix_now};

/// Largest delivery accepted (GitHub's and GitLab's cap).
pub const MAX_BODY: usize = 25 * 1024 * 1024;
/// Delivery ids remembered.
pub const DELIVERY_IDS: usize = 100_000;
/// How long a delivery id is remembered.
pub const DELIVERY_TTL: Duration = Duration::from_secs(24 * 3600);
/// Standard Webhooks: the most a `webhook-timestamp` may differ from now.
pub const TIMESTAMP_TOLERANCE: u64 = 5 * 60;

type HmacSha256 = Hmac<Sha256>;

/// How a host's deliveries are authenticated.
#[derive(Clone)]
enum HostSecret {
    /// HMAC-SHA256 key (GitHub, Bitbucket).
    Hmac(Vec<u8>),
    /// GitLab signing token: the decoded Standard Webhooks key.
    StandardWebhooks(Vec<u8>),
    /// GitLab legacy secret token, by its SHA-256.
    Token([u8; 32]),
}

/// Recently seen delivery ids: at most `cap`, none older than `ttl`.
#[derive(Debug)]
pub struct DeliveryLog {
    seen: HashMap<String, u64>,
    order: VecDeque<(String, u64)>,
    cap: usize,
    ttl: u64,
}

impl DeliveryLog {
    pub fn new(cap: usize, ttl: Duration) -> DeliveryLog {
        DeliveryLog { seen: HashMap::new(), order: VecDeque::new(), cap: cap.max(1), ttl: ttl.as_secs() }
    }

    /// Whether `id` was seen within the window at `now`; records it if not.
    pub fn seen_or_record(&mut self, id: &str, now: u64) -> bool {
        self.expire(now);
        if self.seen.contains_key(id) {
            return true;
        }
        self.seen.insert(id.to_string(), now);
        self.order.push_back((id.to_string(), now));
        while self.order.len() > self.cap {
            self.pop_oldest();
        }
        false
    }

    /// Forgets `id` (its confirmation failed: a redelivery must be heard).
    pub fn forget(&mut self, id: &str) {
        self.seen.remove(id);
    }

    pub fn len(&self) -> usize {
        self.seen.len()
    }

    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }

    fn expire(&mut self, now: u64) {
        while self.order.front().is_some_and(|(_, at)| at.saturating_add(self.ttl) <= now) {
            self.pop_oldest();
        }
    }

    fn pop_oldest(&mut self) {
        if let Some((id, at)) = self.order.pop_front()
            && self.seen.get(&id) == Some(&at)
        {
            self.seen.remove(&id);
        }
    }
}

/// The webhook receivers of an indexer role.
pub struct Webhooks {
    secrets: BTreeMap<WebhookHost, HostSecret>,
    deliveries: Mutex<DeliveryLog>,
}

impl Webhooks {
    /// The receivers `[indexing].webhooks` configures (`None`: none). A
    /// route whose secret is not resolved, or an invalid GitLab signing
    /// token, refuses to start.
    pub fn new(c: &ServiceConfig) -> anyhow::Result<Option<Webhooks>> {
        let mut secrets = BTreeMap::new();
        for (name, secret) in &c.indexing.webhooks {
            let host = WebhookHost::parse(name)
                .ok_or_else(|| anyhow::anyhow!("indexing.webhooks.{name}: unknown host (github, gitlab, bitbucket)"))?;
            let Some(value) = secret.value() else {
                anyhow::bail!(
                    "indexing.webhooks.{name}: environment variable `{}` is not set: the webhook route refuses to start",
                    secret.var()
                );
            };
            secrets.insert(host, host_secret(host, value)?);
        }
        if secrets.is_empty() {
            return Ok(None);
        }
        if matches!(secrets.get(&WebhookHost::GitLab), Some(HostSecret::Token(_))) {
            tracing::warn!(
                "indexing.webhooks.gitlab is a legacy secret token (X-Gitlab-Token): a shared secret sent with every \
                 delivery, replayable and without body integrity; prefer a signing token (whsec_…)"
            );
        }
        let deliveries = Mutex::new(DeliveryLog::new(DELIVERY_IDS, DELIVERY_TTL));
        Ok(Some(Webhooks { secrets, deliveries }))
    }

    /// The hosts with a route.
    pub fn hosts(&self) -> Vec<WebhookHost> {
        self.secrets.keys().copied().collect()
    }

    /// Steps 2–5 of the module docs for a delivery read whole.
    fn accept(&self, host: WebhookHost, headers: &HeaderMap, body: &[u8], specs: &[RepoSpec], now: u64) -> Outcome {
        let Some(secret) = self.secrets.get(&host) else { return Outcome::Unauthorized };
        if !verify(secret, headers, body, now) {
            return Outcome::Unauthorized;
        }
        let event = header(headers, event_header(host)).unwrap_or_default();
        let parsed = match parse(host, event, body) {
            Ok(p) => p,
            Err(e) => return Outcome::Invalid(format!("{e:#}")),
        };
        if let Some(id) = delivery_id(host, headers) {
            let key = format!("{}:{id}", host.as_str());
            if self.deliveries.lock().unwrap_or_else(|p| p.into_inner()).seen_or_record(&key, now) {
                return Outcome::Duplicate;
            }
        }
        let hint = match parsed {
            Parsed::Ignored(why) => return Outcome::Ignored(why),
            Parsed::Hint(h) => h,
        };
        match match_repo(specs, &hint) {
            Some(spec) => Outcome::Accepted(Box::new((spec.clone(), hint))),
            None => Outcome::Ignored("the repository is not configured".to_string()),
        }
    }
}

fn host_secret(host: WebhookHost, value: &str) -> anyhow::Result<HostSecret> {
    if host != WebhookHost::GitLab {
        return Ok(HostSecret::Hmac(value.as_bytes().to_vec()));
    }
    match value.strip_prefix("whsec_") {
        Some(key) => {
            let key = STANDARD
                .decode(key)
                .map_err(|e| anyhow::anyhow!("indexing.webhooks.gitlab: the signing token is not base64: {e}"))?;
            Ok(HostSecret::StandardWebhooks(key))
        }
        None => Ok(HostSecret::Token(Sha256::digest(value.as_bytes()).into())),
    }
}

/// What [`Webhooks::accept`] decided.
enum Outcome {
    Unauthorized,
    Invalid(String),
    Duplicate,
    Ignored(String),
    Accepted(Box<(RepoSpec, Hint)>),
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok()).map(str::trim).filter(|v| !v.is_empty())
}

fn event_header(host: WebhookHost) -> &'static str {
    match host {
        WebhookHost::GitHub => "x-github-event",
        WebhookHost::GitLab => "x-gitlab-event",
        WebhookHost::Bitbucket => "x-event-key",
    }
}

/// The host's delivery id, the same on redelivery.
fn delivery_id(host: WebhookHost, headers: &HeaderMap) -> Option<&str> {
    match host {
        WebhookHost::GitHub => header(headers, "x-github-delivery"),
        WebhookHost::GitLab => header(headers, "idempotency-key").or_else(|| header(headers, "webhook-id")),
        WebhookHost::Bitbucket => header(headers, "x-request-uuid"),
    }
}

/// Step 2 of the module docs.
fn verify(secret: &HostSecret, headers: &HeaderMap, body: &[u8], now: u64) -> bool {
    match secret {
        HostSecret::Hmac(key) => {
            let sig = header(headers, "x-hub-signature-256").or_else(|| header(headers, "x-hub-signature"));
            sig.and_then(|s| decode_hex(s.strip_prefix("sha256=").unwrap_or(s)))
                .is_some_and(|tag| hmac_ok(key, &[body], &tag))
        }
        HostSecret::StandardWebhooks(key) => verify_standard(key, headers, body, now),
        HostSecret::Token(want) => header(headers, "x-gitlab-token")
            .is_some_and(|t| bool::from(Sha256::digest(t.as_bytes()).as_slice().ct_eq(want.as_slice()))),
    }
}

/// HMAC-SHA256 of the concatenated `parts` against `tag`, constant time.
fn hmac_ok(key: &[u8], parts: &[&[u8]], tag: &[u8]) -> bool {
    let Ok(mut mac) = HmacSha256::new_from_slice(key) else { return false };
    for p in parts {
        mac.update(p);
    }
    mac.verify_slice(tag).is_ok()
}

/// Standard Webhooks (`webhook-id`, `webhook-timestamp`,
/// `webhook-signature`).
fn verify_standard(key: &[u8], headers: &HeaderMap, body: &[u8], now: u64) -> bool {
    let (Some(id), Some(ts), Some(sigs)) =
        (header(headers, "webhook-id"), header(headers, "webhook-timestamp"), header(headers, "webhook-signature"))
    else {
        return false;
    };
    let fresh = ts.parse::<u64>().is_ok_and(|t| t.abs_diff(now) <= TIMESTAMP_TOLERANCE);
    if !fresh {
        return false;
    }
    let signed: [&[u8]; 5] = [id.as_bytes(), b".", ts.as_bytes(), b".", body];
    sigs.split_whitespace()
        .filter_map(|s| s.strip_prefix("v1,"))
        .filter_map(|s| STANDARD.decode(s).ok())
        .any(|tag| hmac_ok(key, &signed, &tag))
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

/// `POST /webhooks/github`.
pub async fn github(State(st): State<Arc<AppState>>, headers: HeaderMap, body: Body) -> Response {
    receive(st, WebhookHost::GitHub, headers, body).await
}

/// `POST /webhooks/gitlab`.
pub async fn gitlab(State(st): State<Arc<AppState>>, headers: HeaderMap, body: Body) -> Response {
    receive(st, WebhookHost::GitLab, headers, body).await
}

/// `POST /webhooks/bitbucket`.
pub async fn bitbucket(State(st): State<Arc<AppState>>, headers: HeaderMap, body: Body) -> Response {
    receive(st, WebhookHost::Bitbucket, headers, body).await
}

fn count(metrics: &Metrics, host: WebhookHost, result: &str) {
    metrics.counter("rueca_webhook_deliveries_total", &[("host", host.as_str()), ("result", result)]).increment(1);
}

fn answer(status: StatusCode, body: serde_json::Value) -> Response {
    json_response(status, body.to_string())
}

async fn receive(st: Arc<AppState>, host: WebhookHost, headers: HeaderMap, body: Body) -> Response {
    let (Some(hooks), Some(idx)) = (st.webhooks.clone(), st.indexer.clone()) else {
        return answer(StatusCode::NOT_FOUND, json!({"error": "not_found"}));
    };
    let Some(bytes) = read_body(&headers, body).await else {
        count(&st.metrics, host, "too_large");
        return answer(StatusCode::PAYLOAD_TOO_LARGE, json!({"error": "too_large", "max_bytes": MAX_BODY}));
    };
    match hooks.accept(host, &headers, &bytes, idx.specs(), unix_now()) {
        Outcome::Accepted(b) => {
            let (spec, hint) = *b;
            count(&st.metrics, host, "accepted");
            let body = json!({"result": "accepted", "repo": spec.repo, "refs": hint.refs.len()});
            let key = delivery_id(host, &headers).map(|id| format!("{}:{id}", host.as_str()));
            let metrics = st.metrics.clone();
            tokio::task::spawn_blocking(move || {
                if !confirm(&idx, &metrics, &spec, &hint)
                    && let Some(key) = key
                {
                    hooks.deliveries.lock().unwrap_or_else(|p| p.into_inner()).forget(&key);
                }
            });
            answer(StatusCode::ACCEPTED, body)
        }
        other => refuse(&st.metrics, host, other),
    }
}

/// The whole body, unless it is (or declares to be) over [`MAX_BODY`].
async fn read_body(headers: &HeaderMap, body: Body) -> Option<bytes::Bytes> {
    let declared = header(headers, "content-length").and_then(|v| v.parse::<u64>().ok());
    if declared.is_some_and(|n| n > MAX_BODY as u64) {
        return None;
    }
    axum::body::to_bytes(body, MAX_BODY).await.ok()
}

/// The answer to a delivery that is not accepted.
fn refuse(metrics: &Metrics, host: WebhookHost, outcome: Outcome) -> Response {
    let (result, status, body) = match outcome {
        Outcome::Unauthorized => {
            tracing::info!(host = host.as_str(), "webhook signature refused");
            ("unauthorized", StatusCode::UNAUTHORIZED, json!({"error": "invalid_signature"}))
        }
        Outcome::Invalid(why) => {
            ("invalid", StatusCode::BAD_REQUEST, json!({"error": "invalid_payload", "error_description": why}))
        }
        Outcome::Duplicate => ("duplicate", StatusCode::OK, json!({"result": "duplicate"})),
        Outcome::Ignored(why) => ("ignored", StatusCode::ACCEPTED, json!({"result": "ignored", "reason": why})),
        Outcome::Accepted(_) => ("accepted", StatusCode::ACCEPTED, json!({"result": "accepted"})),
    };
    count(metrics, host, result);
    answer(status, body)
}

/// Step 6: confirm and merge, then wake job discovery; whether the merge
/// was written (or had nothing to change).
fn confirm(idx: &crate::indexer::Indexer, metrics: &Metrics, spec: &RepoSpec, hint: &Hint) -> bool {
    let host = hint.host.as_str();
    let result = match idx.apply_hint(spec, hint, unix_now()) {
        Ok(applied) => {
            tracing::info!(host, repo = %applied.repo, event = %hint.event, changed = applied.changed,
                changes = ?applied.plan.changes, skipped = ?applied.plan.skipped, "webhook confirmed");
            if applied.changed {
                idx.wake.notify_one();
                "changed"
            } else {
                "unchanged"
            }
        }
        Err(e) => {
            tracing::warn!(host, repo = %spec.repo, error = %format!("{e:#}"), "webhook confirmation failed; polling will catch up");
            "failed"
        }
    };
    metrics.counter("rueca_webhook_confirmations_total", &[("host", host), ("result", result)]).increment(1);
    result != "failed"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delivery_log_is_bounded_in_size_and_time() {
        let mut log = DeliveryLog::new(3, Duration::from_secs(100));
        assert!(!log.seen_or_record("a", 0));
        assert!(log.seen_or_record("a", 1));
        for id in ["b", "c", "d"] {
            assert!(!log.seen_or_record(id, 2));
        }
        assert_eq!(log.len(), 3);
        assert!(!log.seen_or_record("a", 3), "evicted by size");
        assert!(log.seen_or_record("d", 101));
        assert!(!log.seen_or_record("d", 102), "expired after the window");
    }

    #[test]
    fn signatures() {
        let body = b"{\"x\":1}";
        let mut mac = HmacSha256::new_from_slice(b"s3cret").unwrap();
        mac.update(body);
        let hex: String = mac.finalize().into_bytes().iter().map(|b| format!("{b:02x}")).collect();
        let mut h = HeaderMap::new();
        h.insert("x-hub-signature-256", format!("sha256={hex}").parse().unwrap());
        let key = HostSecret::Hmac(b"s3cret".to_vec());
        assert!(verify(&key, &h, body, 0));
        assert!(!verify(&key, &h, b"{\"x\":2}", 0), "another body");
        assert!(!verify(&HostSecret::Hmac(b"other".to_vec()), &h, body, 0), "another key");
        assert!(!verify(&key, &HeaderMap::new(), body, 0), "no signature");
        let token = host_secret(WebhookHost::GitLab, "plain").unwrap();
        let mut g = HeaderMap::new();
        g.insert("x-gitlab-token", "plain".parse().unwrap());
        assert!(verify(&token, &g, body, 0));
        g.insert("x-gitlab-token", "plaim".parse().unwrap());
        assert!(!verify(&token, &g, body, 0));
        assert_eq!(decode_hex("0aff"), Some(vec![10, 255]));
        assert!(decode_hex("0g").is_none() && decode_hex("abc").is_none());
    }

    #[test]
    fn standard_webhooks_signatures_and_timestamps() {
        let key = b"0123456789abcdef".to_vec();
        let secret = host_secret(WebhookHost::GitLab, &format!("whsec_{}", STANDARD.encode(&key))).unwrap();
        let body = b"{}";
        let mut mac = HmacSha256::new_from_slice(&key).unwrap();
        mac.update(b"msg_1.1000.{}");
        let sig = STANDARD.encode(mac.finalize().into_bytes());
        let mut h = HeaderMap::new();
        h.insert("webhook-id", "msg_1".parse().unwrap());
        h.insert("webhook-timestamp", "1000".parse().unwrap());
        h.insert("webhook-signature", format!("v1,bm9wZQ== v1,{sig}").parse().unwrap());
        assert!(verify(&secret, &h, body, 1000 + TIMESTAMP_TOLERANCE));
        assert!(!verify(&secret, &h, body, 1000 + TIMESTAMP_TOLERANCE + 1), "too old");
        assert!(!verify(&secret, &h, b"{ }", 1000), "another body");
        assert!(host_secret(WebhookHost::GitLab, "whsec_!!").is_err());
    }
}
