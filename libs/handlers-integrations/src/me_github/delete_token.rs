use axum::{Extension, extract::State, http::StatusCode};
use axum_extra::routing::TypedPath;
use matos_handlers::Caller;
use serde::Deserialize;

use crate::{IntegrationError, state::IntegrationsState};

#[derive(TypedPath, Deserialize)]
#[typed_path("/api/v1/me/github/token")]
pub struct DeleteTokenPath;

#[utoipa::path(
    delete,
    path = "/api/v1/me/github/token",
    operation_id = "deleteMyGithubToken",
    tag = crate::TAG,
    responses(
        (status = 204, description = "The caller's personal token is deleted, or there was none"),
        (status = 401, description = "Missing or invalid token", body = matos_handlers::errors::ApiErrorResponse),
    ),
    security(("bearer_auth" = []))
)]
pub async fn handler(
    _: DeleteTokenPath,
    State(state): State<IntegrationsState>,
    Extension(caller): Extension<Caller>,
) -> Result<StatusCode, IntegrationError> {
    state
        .usecase
        .delete_github_personal_token(caller.subject())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
