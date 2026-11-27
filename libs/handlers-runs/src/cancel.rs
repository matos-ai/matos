use axum::{Extension, Json, extract::State};
use axum_extra::routing::TypedPath;
use matos_common::RunId;
use matos_core::RunCancellation;
use matos_handlers::{ApiError, AppState, Caller};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(TypedPath, Deserialize)]
#[typed_path("/api/v1/runs/{run_id}/cancel")]
pub struct CancelRunPath {
    pub run_id: RunId,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CancelRunResponse {
    /// Always `cancelled`.
    pub status: &'static str,
    /// True when the run had already been cancelled before this call.
    pub already_cancelled: bool,
}

impl From<RunCancellation> for CancelRunResponse {
    fn from(outcome: RunCancellation) -> Self {
        Self {
            status: "cancelled",
            already_cancelled: matches!(outcome, RunCancellation::AlreadyCancelled),
        }
    }
}

#[utoipa::path(
    post,
    path = "/api/v1/runs/{run_id}/cancel",
    operation_id = "cancelRun",
    tag = super::TAG,
    params(("run_id" = RunId, Path, description = "Run identifier")),
    responses(
        (status = 200, description = "The run is cancelled; `already_cancelled` is true when it already was", body = CancelRunResponse),
        (status = 401, description = "Missing or invalid token", body = matos_handlers::errors::ApiErrorResponse),
        (status = 403, description = "Not the member who started the run and not an admin, or an agent caller", body = matos_handlers::errors::ApiErrorResponse),
        (status = 404, description = "No such run in the caller's organization", body = matos_handlers::errors::ApiErrorResponse),
        (status = 409, description = "The run already succeeded or failed", body = matos_handlers::errors::ApiErrorResponse),
    ),
    security(("bearer_auth" = []))
)]
pub async fn handler(
    CancelRunPath { run_id }: CancelRunPath,
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
) -> Result<Json<CancelRunResponse>, ApiError> {
    let outcome = state.usecase.cancel_run(caller.subject(), run_id).await?;
    Ok(Json(outcome.into()))
}
