//! `/admin/*` (RFC 0008 §6.8), indexer and all roles, on the main listener
//! behind authentication; every route requires the admin role
//! (`[auth].admins`), else `403`.
//!
//! | Route | Body | Answer |
//! |---|---|---|
//! | `POST /admin/index` | `{"repo", "ref"}` | `202` `{"repo", "ref", "kind", "commit"}`: the ref (a tag, `pr/<n>`, a branch, or a full 40-digit commit) resolved with `git ls-remote` and added to the job's desired state; `404` unknown repository or ref; `400` a commit prefix or a malformed name |
//! | `DELETE /admin/refs` | `{"repo", "ref"}` | `202` `{"repo", "ref", "removed": true}`: out of the catalog and the desired state; `400` the default ref; `404` no such ref |
//! | `GET /admin/jobs` | — | `200` `{"jobs": [<job object>, …]}` |

use std::sync::Arc;

use axum::Extension;
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use rueca_service::updater::unix_now;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth::{Principal, is_admin};
use crate::indexer::{AdminError, Indexer};
use crate::mcp::json_response;
use crate::state::AppState;

/// `{"repo", "ref"}`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefRequest {
    pub repo: String,
    #[serde(rename = "ref")]
    pub ref_name: String,
}

/// The indexer of an admin caller, or the refusal's status.
fn admin(st: &AppState, p: &Principal) -> Result<Arc<Indexer>, StatusCode> {
    if !is_admin(p, &st.config.auth.admins) {
        tracing::info!(principal = %p.id, "admin route refused");
        return Err(StatusCode::FORBIDDEN);
    }
    st.indexer.clone().ok_or(StatusCode::NOT_FOUND)
}

fn refusal(status: StatusCode) -> Response {
    let body = match status {
        StatusCode::FORBIDDEN => json!({"error": "forbidden", "error_description": "the admin role is required"}),
        _ => json!({"error": "not_found"}),
    };
    json_response(status, body.to_string())
}

fn answer(r: Result<Result<Value, AdminError>, tokio::task::JoinError>) -> Response {
    let (status, body) = match r {
        Ok(Ok(v)) => (StatusCode::ACCEPTED, v),
        Ok(Err(AdminError::NotFound(m))) => {
            (StatusCode::NOT_FOUND, json!({"error": "not_found", "error_description": m}))
        }
        Ok(Err(AdminError::Invalid(m))) => {
            (StatusCode::BAD_REQUEST, json!({"error": "invalid_request", "error_description": m}))
        }
        Ok(Err(AdminError::Internal(e))) => {
            tracing::warn!(error = %format!("{e:#}"), "admin request failed");
            (StatusCode::BAD_GATEWAY, json!({"error": "failed", "error_description": format!("{e:#}")}))
        }
        Err(e) => {
            tracing::error!(error = %e, "admin task failed");
            (StatusCode::INTERNAL_SERVER_ERROR, json!({"error": "internal"}))
        }
    };
    json_response(status, body.to_string())
}

/// `POST /admin/index`.
pub async fn index(
    State(st): State<Arc<AppState>>,
    Extension(p): Extension<Principal>,
    Json(req): Json<RefRequest>,
) -> Response {
    let idx = match admin(&st, &p) {
        Ok(i) => i,
        Err(status) => return refusal(status),
    };
    answer(tokio::task::spawn_blocking(move || idx.admin_index(&req.repo, &req.ref_name, unix_now())).await)
}

/// `DELETE /admin/refs`.
pub async fn remove_ref(
    State(st): State<Arc<AppState>>,
    Extension(p): Extension<Principal>,
    Json(req): Json<RefRequest>,
) -> Response {
    let idx = match admin(&st, &p) {
        Ok(i) => i,
        Err(status) => return refusal(status),
    };
    answer(tokio::task::spawn_blocking(move || idx.admin_remove(&req.repo, &req.ref_name, unix_now())).await)
}

/// `GET /admin/jobs`.
pub async fn jobs(State(st): State<Arc<AppState>>, Extension(p): Extension<Principal>) -> Response {
    let idx = match admin(&st, &p) {
        Ok(i) => i,
        Err(status) => return refusal(status),
    };
    match tokio::task::spawn_blocking(move || idx.list_jobs()).await {
        Ok(Ok(jobs)) => json_response(StatusCode::OK, json!({"jobs": jobs}).to_string()),
        Ok(Err(e)) => answer(Ok(Err(AdminError::Internal(e)))),
        Err(e) => answer(Err(e)),
    }
}
