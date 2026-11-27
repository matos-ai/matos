use axum::{Extension, Json, extract::State};
use axum_extra::routing::TypedPath;
use matos_handlers::Caller;
use serde::Deserialize;

use crate::{
    GithubConnectionResponse, IntegrationError, SetGithubPersonalTokenActiveRequest,
    state::IntegrationsState,
};

#[derive(TypedPath, Deserialize)]
#[typed_path("/api/v1/me/github/token")]
pub struct PatchTokenPath;

#[utoipa::path(
    patch,
    path = "/api/v1/me/github/token",
    operation_id = "setMyGithubTokenActive",
    tag = crate::TAG,
    request_body = SetGithubPersonalTokenActiveRequest,
    responses(
        (status = 200, description = "The token is active or inactive", body = GithubConnectionResponse),
        (status = 401, description = "Missing or invalid token", body = matos_handlers::errors::ApiErrorResponse),
        (status = 404, description = "The caller has no personal token", body = matos_handlers::errors::ApiErrorResponse),
    ),
    security(("bearer_auth" = []))
)]
pub async fn handler(
    _: PatchTokenPath,
    State(state): State<IntegrationsState>,
    Extension(caller): Extension<Caller>,
    Json(request): Json<SetGithubPersonalTokenActiveRequest>,
) -> Result<Json<GithubConnectionResponse>, IntegrationError> {
    let status = state
        .usecase
        .set_github_personal_token_active(caller.subject(), request.active)
        .await?;
    Ok(Json(status.into()))
}
