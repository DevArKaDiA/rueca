//! The two listeners: the main one (`/mcp`, `/status`, PRM) and the ops one
//! (`/healthz`, `/readyz`, `/metrics`).
//!
//! Main listener layers, outermost first (RFC 0008 §6.5): request log →
//! `Host` / `Origin` and query-string token checks (403 / 400) → body limit
//! (413) → IP limiter (429) → [`authenticate`] (401 / 403) → principal
//! limiter (429) → `[http].request_timeout` (503) → the handler.
//! `/admin/*` and `/webhooks/*` are the indexer role's (phase 2): 404 here.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde_json::{Value, json};
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::timeout::TimeoutLayer;

use crate::auth::{Challenge, Principal, is_admin};
use crate::config::{SCOPE_ADMIN, SCOPE_READ};
use crate::mcp::{self, json_response};
use crate::state::AppState;

/// The main listener's router.
pub fn main_router(st: Arc<AppState>) -> Router {
    let max_body = usize::try_from(st.config.http.max_body_bytes.0).unwrap_or(usize::MAX);
    let protected = Router::new()
        .route("/mcp", post(mcp::handle))
        .route("/status", get(status))
        .route_layer(TimeoutLayer::with_status_code(StatusCode::SERVICE_UNAVAILABLE, st.config.http.request_timeout.0))
        .route_layer(middleware::from_fn_with_state(st.clone(), authenticate));
    Router::new()
        .merge(protected)
        .route("/.well-known/oauth-protected-resource", get(prm))
        .route("/.well-known/oauth-protected-resource/mcp", get(prm))
        .layer(middleware::from_fn_with_state(st.clone(), limit_ip))
        .layer(RequestBodyLimitLayer::new(max_body))
        .layer(axum::extract::DefaultBodyLimit::disable())
        .layer(middleware::from_fn_with_state(st.clone(), guard))
        .layer(middleware::from_fn(log_request))
        .with_state(st)
}

/// The ops listener's router.
pub fn ops_router(st: Arc<AppState>) -> Router {
    let mut r = Router::new().route("/healthz", get(healthz)).route("/readyz", get(readyz));
    if st.config.telemetry.prometheus {
        r = r.route("/metrics", get(metrics));
    }
    r.with_state(st)
}

/// Debug log of every request: method, path (never the query), status,
/// duration.
async fn log_request(req: Request, next: Next) -> Response {
    let (method, path) = (req.method().clone(), req.uri().path().to_string());
    let started = Instant::now();
    let resp = next.run(req).await;
    let ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    tracing::debug!(%method, %path, status = resp.status().as_u16(), ms, "http request");
    resp
}

/// Query-string tokens (400, never logged) and `Host` / `Origin` (403).
async fn guard(State(st): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let query_token = req.uri().query().is_some_and(|q| {
        q.split('&').any(|kv| kv.split('=').next().is_some_and(|k| k.eq_ignore_ascii_case("access_token")))
    });
    if query_token {
        st.metrics.auth_failure("invalid");
        let body = json!({"error": "invalid_request", "error_description": "tokens are accepted only in the Authorization header"});
        return json_response(StatusCode::BAD_REQUEST, body.to_string());
    }
    if let Err(why) = st.guard.check(req.headers()) {
        tracing::info!(path = %req.uri().path(), reason = why, "request refused");
        return json_response(
            StatusCode::FORBIDDEN,
            json!({"error": "forbidden", "error_description": why}).to_string(),
        );
    }
    next.run(req).await
}

async fn limit_ip(State(st): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let peer = req.extensions().get::<ConnectInfo<SocketAddr>>().map(|c| c.0.ip());
    if let Some(peer) = peer {
        let ip = st.limits.client_ip(peer, req.headers());
        if let Err(wait) = st.limits.check_ip(ip) {
            st.metrics.rate_limited("ip");
            return too_many(wait);
        }
    }
    next.run(req).await
}

/// Authentication, the `rueca:read` scope, then the principal limiter.
async fn authenticate(State(st): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let (mut parts, body) = req.into_parts();
    let principal = match st.auth.authenticate(&parts).await {
        Ok(p) if p.scopes.contains(SCOPE_READ) => p,
        Ok(p) => {
            tracing::info!(principal = %p.id, "missing scope rueca:read");
            return challenge(&st, &Challenge::InsufficientScope(SCOPE_READ.to_string()));
        }
        Err(c) => return challenge(&st, &c),
    };
    if let Err(wait) = st.limits.check_principal(&principal.id) {
        st.metrics.rate_limited("principal");
        return too_many(wait);
    }
    parts.extensions.insert(principal);
    next.run(Request::from_parts(parts, body)).await
}

fn too_many(wait: Duration) -> Response {
    let secs = wait.as_secs() + u64::from(wait.subsec_nanos() > 0);
    let mut resp = json_response(
        StatusCode::TOO_MANY_REQUESTS,
        json!({"error": "rate_limited", "retry_after_seconds": secs.max(1)}).to_string(),
    );
    resp.headers_mut().insert(http::header::RETRY_AFTER, HeaderValue::from(secs.max(1)));
    resp
}

/// 401 / 403 with the `WWW-Authenticate` challenge of RFC 0008 §6.3.
fn challenge(st: &AppState, c: &Challenge) -> Response {
    st.metrics.auth_failure(c.reason());
    let prm = st.prm_url().map(|u| format!(", resource_metadata=\"{u}\"")).unwrap_or_default();
    let (status, header, error, description) = match c {
        Challenge::Missing => (
            StatusCode::UNAUTHORIZED,
            format!("Bearer scope=\"{SCOPE_READ}\"{prm}"),
            "unauthorized",
            "a bearer token is required".to_string(),
        ),
        Challenge::Invalid(why) => {
            let why = sanitize(why);
            (
                StatusCode::UNAUTHORIZED,
                format!("Bearer error=\"invalid_token\", error_description=\"{why}\"{prm}"),
                "invalid_token",
                why,
            )
        }
        Challenge::InsufficientScope(scope) => (
            StatusCode::FORBIDDEN,
            format!("Bearer error=\"insufficient_scope\", scope=\"{scope}\"{prm}"),
            "insufficient_scope",
            format!("the token lacks scope {scope}"),
        ),
    };
    let mut resp = json_response(status, json!({"error": error, "error_description": description}).to_string());
    if let Ok(v) = HeaderValue::from_str(&header) {
        resp.headers_mut().insert(http::header::WWW_AUTHENTICATE, v);
    }
    resp
}

/// A header-safe description (no quotes or control characters).
fn sanitize(s: &str) -> String {
    s.chars().filter(|c| *c != '"' && *c != '\\' && !c.is_control()).collect()
}

/// Protected Resource Metadata (RFC 9728), only with an OIDC issuer.
async fn prm(State(st): State<Arc<AppState>>) -> Response {
    if st.issuers.is_empty() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let org = st.serving().map(|s| s.org.clone()).or_else(|| st.config.org.name.clone()).unwrap_or_default();
    let body = json!({
        "resource": st.resource,
        "authorization_servers": st.issuers,
        "scopes_supported": [SCOPE_READ, SCOPE_ADMIN],
        "bearer_methods_supported": ["header"],
        "resource_name": format!("Rueca ({org})"),
    });
    json_response(StatusCode::OK, body.to_string())
}

/// `GET /status`: generation, the repositories visible to the caller with
/// their refs and commits, lag; admins also see the cache.
async fn status(State(st): State<Arc<AppState>>, axum::Extension(p): axum::Extension<Principal>) -> Response {
    let Some(current) = st.current() else {
        let body = json!({"ready": false, "generation": null, "error": st.last_error().is_some()});
        return json_response(StatusCode::OK, body.to_string());
    };
    let g = &current.generation;
    let visibility = st.visibility.resolve(&p.repos, g.generation, &current.repos);
    let repos: Vec<Value> = g
        .repos
        .iter()
        .filter(|r| visibility.allows(&r.repo))
        .map(|r| {
            let refs: Vec<Value> =
                r.refs.iter().map(|x| json!({"ref": x.ref_name, "commit": x.entry.snapshot.commit})).collect();
            json!({
                "repo": r.repo,
                "default": {"ref": r.default.snapshot.branch, "commit": r.default.snapshot.commit},
                "refs": refs,
            })
        })
        .collect();
    let mut body = json!({
        "ready": true,
        "org": g.name,
        "generation": g.generation,
        "published_at": g.created_at,
        "lag_seconds": st.lag_seconds(),
        "repositories": repos,
        "role": st.role.as_str(),
    });
    if is_admin(&p, &st.config.auth.admins)
        && let Some(serving) = st.serving()
    {
        let usage = st.cache_usage(&serving);
        body["cache"] = json!({"bytes": usage, "budget": st.config.store.cache_bytes.0, "warm": format!("{}/{}", current.warm.0, current.warm.1)});
        body["repositories_total"] = json!(g.repos.len());
    }
    json_response(StatusCode::OK, body.to_string())
}

async fn healthz() -> Response {
    json_response(StatusCode::OK, json!({"ok": true}).to_string())
}

async fn readyz(State(st): State<Arc<AppState>>) -> Response {
    match st.current() {
        Some(c) => {
            let body = json!({"generation": c.generation.generation, "warm": format!("{}/{}", c.warm.0, c.warm.1)});
            json_response(StatusCode::OK, body.to_string())
        }
        None => {
            let body =
                json!({"ready": false, "reason": st.last_error().unwrap_or_else(|| "no generation pulled yet".into())});
            json_response(StatusCode::SERVICE_UNAVAILABLE, body.to_string())
        }
    }
}

async fn metrics(State(st): State<Arc<AppState>>) -> Response {
    st.refresh_gauges();
    (StatusCode::OK, [(http::header::CONTENT_TYPE, "text/plain; version=0.0.4")], st.metrics.render()).into_response()
}
