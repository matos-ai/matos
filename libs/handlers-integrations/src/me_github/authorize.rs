use axum::{Extension, Json, extract::State};
use axum_extra::routing::TypedPath;
use chrono::Utc;
use matos_handlers::Caller;
use serde::Deserialize;

use crate::{InstallUrlResponse, IntegrationError, signer::StatePurpose, state::IntegrationsState};

#[derive(TypedPath, Deserialize)]
#[typed_path("/api/v1/me/github/authorize")]
pub struct AuthorizePath;

#[utoipa::path(
    get,
    path = "/api/v1/me/github/authorize",
    operation_id = "getMyGithubAuthorizeUrl",
    tag = crate::TAG,
    responses(
        (status = 200, description = "Where to send the caller to authorize the GitHub App; the state expires in 10 minutes", body = InstallUrlResponse),
        (status = 401, description = "Missing or invalid token", body = matos_handlers::errors::ApiErrorResponse),
        (status = 409, description = "The organization has no GitHub App yet", body = matos_handlers::errors::ApiErrorResponse),
        (status = 503, description = "secrets_key_not_configured", body = matos_handlers::errors::ApiErrorResponse),
    ),
    security(("bearer_auth" = []))
)]
pub async fn handler(
    _: AuthorizePath,
    State(state): State<IntegrationsState>,
    Extension(caller): Extension<Caller>,
) -> Result<Json<InstallUrlResponse>, IntegrationError> {
    let subject = caller.subject();
    let signer = state.github.signer()?;
    let token = signer.sign(
        StatePurpose::UserConnect,
        subject.organization_id,
        &subject.id,
        Utc::now(),
    );
    let url = state
        .usecase
        .github_authorize_url(subject, &token, state.github.apps.as_ref())
        .await?;
    Ok(Json(InstallUrlResponse { url }))
}
