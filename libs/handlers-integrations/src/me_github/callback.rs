use axum::{Extension, Json, extract::State};
use axum_extra::routing::TypedPath;
use chrono::Utc;
use matos_handlers::Caller;
use serde::Deserialize;

use crate::{
    ConnectGithubUserRequest, GithubConnectionResponse, IntegrationError, signer::StatePurpose,
    state::IntegrationsState,
};

#[derive(TypedPath, Deserialize)]
#[typed_path("/api/v1/me/github/callback")]
pub struct CallbackPath;

#[utoipa::path(
    post,
    path = "/api/v1/me/github/callback",
    operation_id = "connectMyGithub",
    tag = crate::TAG,
    request_body = ConnectGithubUserRequest,
    responses(
        (status = 200, description = "Connected", body = GithubConnectionResponse),
        (status = 400, description = "The state is forged, expired, or was issued for another purpose or another member", body = matos_handlers::errors::ApiErrorResponse),
        (status = 401, description = "Missing or invalid token", body = matos_handlers::errors::ApiErrorResponse),
        (status = 409, description = "The organization has no GitHub App yet", body = matos_handlers::errors::ApiErrorResponse),
        (status = 503, description = "secrets_key_not_configured", body = matos_handlers::errors::ApiErrorResponse),
    ),
    security(("bearer_auth" = []))
)]
pub async fn handler(
    _: CallbackPath,
    State(state): State<IntegrationsState>,
    Extension(caller): Extension<Caller>,
    Json(request): Json<ConnectGithubUserRequest>,
) -> Result<Json<GithubConnectionResponse>, IntegrationError> {
    let subject = caller.subject();
    state.github.signer()?.verify(
        StatePurpose::UserConnect,
        &request.state,
        subject.organization_id,
        &subject.id,
        Utc::now(),
    )?;
    let status = state
        .usecase
        .connect_github_user(subject, &request.code, state.github.apps.as_ref())
        .await?;
    Ok(Json(status.into()))
}
