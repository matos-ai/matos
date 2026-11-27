use axum::{Extension, Json, extract::State};
use axum_extra::routing::TypedPath;
use matos_common::CoreError;
use matos_core::domain::agent::SecretToken;
use matos_handlers::Caller;
use serde::Deserialize;

use crate::{
    GithubConnectionResponse, IntegrationError, PutGithubPersonalTokenRequest,
    state::IntegrationsState,
};

#[derive(TypedPath, Deserialize)]
#[typed_path("/api/v1/me/github/token")]
pub struct PutTokenPath;

#[utoipa::path(
    put,
    path = "/api/v1/me/github/token",
    operation_id = "putMyGithubToken",
    tag = crate::TAG,
    request_body = PutGithubPersonalTokenRequest,
    responses(
        (status = 200, description = "The token is valid, stored sealed and active; it is not returned", body = GithubConnectionResponse),
        (status = 400, description = "GitHub refused the token, nothing is stored", body = matos_handlers::errors::ApiErrorResponse),
        (status = 401, description = "Missing or invalid token", body = matos_handlers::errors::ApiErrorResponse),
        (status = 503, description = "secrets_key_not_configured", body = matos_handlers::errors::ApiErrorResponse),
    ),
    security(("bearer_auth" = []))
)]
pub async fn handler(
    _: PutTokenPath,
    State(state): State<IntegrationsState>,
    Extension(caller): Extension<Caller>,
    Json(request): Json<PutGithubPersonalTokenRequest>,
) -> Result<Json<GithubConnectionResponse>, IntegrationError> {
    state.github.signer()?;
    let status = state
        .usecase
        .save_github_personal_token(
            caller.subject(),
            SecretToken::new(request.token),
            state.github.apps.as_ref(),
        )
        .await
        .map_err(|error| match error {
            CoreError::Invalid(message) => IntegrationError::TokenRefused(message),
            other => other.into(),
        })?;
    Ok(Json(status.into()))
}
