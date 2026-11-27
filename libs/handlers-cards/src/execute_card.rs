use std::sync::Arc;

use axum::{Extension, Json, extract::State, http::StatusCode};
use axum_extra::routing::TypedPath;
use matos_common::{CardId, CoreError, RunId};
use matos_core::domain::{
    agent_task::RepositoryTarget,
    execution::{RepositoryRef, ports::CodeHost},
};
use matos_github::GithubApps;
use matos_handlers::{ApiError, AppState, Caller};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{
    response::CardResponse,
    run_agent::{RunAgentError, with_extra_repositories},
};

#[derive(Clone)]
pub struct CodeHostHandle(pub Arc<GithubApps>);

#[derive(TypedPath, Deserialize)]
#[typed_path("/api/v1/cards/{card_id}/executions")]
pub struct ExecuteCardPath {
    pub card_id: CardId,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ExecuteCardRequest {
    /// `owner/name`, one of the repositories of the organization's installation.
    pub repository: String,
    /// `owner/name` of other repositories the run modifies as well, each one
    /// reachable by the installation: at most 4, distinct, and not
    /// `repository`. Each gets the same branch and its own draft pull
    /// request.
    #[serde(default)]
    pub extra_repositories: Vec<String>,
    /// The version the caller last read; a stale one is refused with 409.
    pub expected_version: i32,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ExecuteCardResponse {
    pub run_id: RunId,
    pub card: CardResponse,
}

#[utoipa::path(
    post,
    path = "/api/v1/cards/{card_id}/executions",
    operation_id = "executeCard",
    tag = super::TAG,
    params(("card_id" = CardId, Path, description = "Card identifier")),
    request_body = ExecuteCardRequest,
    responses(
        (status = 202, description = "The card is In progress and an Execute run is queued", body = ExecuteCardResponse),
        (status = 401, description = "Missing or invalid token", body = matos_handlers::errors::ApiErrorResponse),
        (status = 403, description = "The policy refused, or repository_not_allowed: the installation does not reach that repository", body = matos_handlers::errors::ApiErrorResponse),
        (status = 404, description = "No such card in the caller's organization", body = matos_handlers::errors::ApiErrorResponse),
        (status = 409, description = "card_not_executable, execution_already_active, github_not_connected, or the card changed since it was read", body = matos_handlers::errors::ApiErrorResponse),
        (status = 422, description = "Not an owner/name repository, or too_many_repositories (more than 4 `extra_repositories`, a duplicate, or the primary repository among them)", body = matos_handlers::errors::ApiErrorResponse),
    ),
    security(("bearer_auth" = []))
)]
pub async fn handler(
    ExecuteCardPath { card_id }: ExecuteCardPath,
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    Extension(CodeHostHandle(github)): Extension<CodeHostHandle>,
    Json(request): Json<ExecuteCardRequest>,
) -> Result<(StatusCode, Json<ExecuteCardResponse>), RunAgentError> {
    let repository: RepositoryRef = request.repository.parse()?;
    let extras = with_extra_repositories(
        RepositoryTarget::new(repository.clone(), None)?,
        &request.extra_repositories,
    )?
    .extra_repositories
    .into_vec();
    let subject = caller.subject();
    state
        .usecase
        .check_card_executable(subject.clone(), card_id)
        .await?;
    let organization_id = subject.organization_id;
    let not_connected = || CoreError::Conflict("github_not_connected".into());
    if github.client(organization_id).await?.is_none() {
        return Err(RunAgentError::from(not_connected()));
    }
    let installation = state
        .usecase
        .github_installation(subject.clone())
        .await?
        .ok_or_else(not_connected)?;
    let reachable = github
        .repositories(organization_id, installation.installation_id)
        .await?;
    for wanted in std::iter::once(&repository).chain(&extras) {
        if !reachable.contains(wanted) {
            return Err(ApiError::Forbidden(Some("repository_not_allowed".into())).into());
        }
    }
    let base_branch = github
        .default_branch(organization_id, installation.installation_id, &repository)
        .await?;
    let started = state
        .usecase
        .execute_card(
            subject,
            card_id,
            repository,
            base_branch,
            extras,
            request.expected_version,
        )
        .await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(ExecuteCardResponse {
            run_id: started.run_id,
            card: started.card.into(),
        }),
    ))
}
