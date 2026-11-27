use axum::{Extension, extract::State, http::StatusCode};
use axum_extra::routing::TypedPath;
use matos_handlers::Caller;
use serde::Deserialize;

use crate::{IntegrationError, state::IntegrationsState};

#[derive(TypedPath, Deserialize)]
#[typed_path("/api/v1/me/github")]
pub struct DisconnectPath;

#[utoipa::path(
    delete,
    path = "/api/v1/me/github",
    operation_id = "disconnectMyGithub",
    tag = crate::TAG,
    responses(
        (status = 204, description = "The caller's GitHub connection is removed and its grant revoked"),
        (status = 401, description = "Missing or invalid token", body = matos_handlers::errors::ApiErrorResponse),
    ),
    security(("bearer_auth" = []))
)]
pub async fn handler(
    _: DisconnectPath,
    State(state): State<IntegrationsState>,
    Extension(caller): Extension<Caller>,
) -> Result<StatusCode, IntegrationError> {
    state
        .usecase
        .disconnect_github_user(caller.subject(), state.github.apps.as_ref())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
