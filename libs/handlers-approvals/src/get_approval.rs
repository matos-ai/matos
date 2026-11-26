use axum::{Extension, Json, extract::State};
use axum_extra::routing::TypedPath;
use matos_common::ApprovalId;
use matos_handlers::{ApiError, AppState, Caller};
use serde::Deserialize;

use crate::response::ApprovalResponse;

#[derive(TypedPath, Deserialize)]
#[typed_path("/api/v1/approvals/{approval_id}")]
pub struct ApprovalPath {
    pub approval_id: ApprovalId,
}

#[utoipa::path(
    get,
    path = "/api/v1/approvals/{approval_id}",
    operation_id = "getApproval",
    tag = super::TAG,
    params(("approval_id" = ApprovalId, Path, description = "Approval identifier")),
    responses(
        (status = 200, description = "The approval", body = ApprovalResponse),
        (status = 401, description = "Missing or invalid token", body = matos_handlers::errors::ApiErrorResponse),
        (status = 403, description = "The policy refused the read; agents have no access to approvals", body = matos_handlers::errors::ApiErrorResponse),
        (status = 404, description = "No such approval in the caller's organization", body = matos_handlers::errors::ApiErrorResponse),
    ),
    security(("bearer_auth" = []))
)]
pub async fn handler(
    ApprovalPath { approval_id }: ApprovalPath,
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
) -> Result<Json<ApprovalResponse>, ApiError> {
    let approval = state
        .usecase
        .get_approval(caller.subject(), approval_id)
        .await?;
    Ok(Json(approval.into()))
}
