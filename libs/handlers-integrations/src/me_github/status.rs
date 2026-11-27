use axum::{Extension, Json, extract::State};
use axum_extra::routing::TypedPath;
use matos_handlers::Caller;
use serde::Deserialize;

use crate::{GithubConnectionResponse, IntegrationError, state::IntegrationsState};

#[derive(TypedPath, Deserialize)]
#[typed_path("/api/v1/me/github")]
pub struct StatusPath;

#[utoipa::path(
    get,
    path = "/api/v1/me/github",
    operation_id = "getMyGithubConnection",
    tag = crate::TAG,
    responses(
        (status = 200, description = "Whether the caller connected their own GitHub account", body = GithubConnectionResponse),
        (status = 401, description = "Missing or invalid token", body = matos_handlers::errors::ApiErrorResponse),
    ),
    security(("bearer_auth" = []))
)]
pub async fn handler(
    _: StatusPath,
    State(state): State<IntegrationsState>,
    Extension(caller): Extension<Caller>,
) -> Result<Json<GithubConnectionResponse>, IntegrationError> {
    let status = state
        .usecase
        .github_connection_status(caller.subject())
        .await?;
    Ok(Json(status.into()))
}
