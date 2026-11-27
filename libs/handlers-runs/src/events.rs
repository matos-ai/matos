use axum::{
    Extension, Json,
    extract::{Query, State},
};
use axum_extra::routing::TypedPath;
use chrono::{DateTime, Utc};
use matos_common::RunId;
use matos_core::domain::run::event::RunEvent;
use matos_handlers::{ApiError, AppState, Caller};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

const DEFAULT_LIMIT: u32 = 200;
const MAX_LIMIT: u32 = 1000;

#[derive(TypedPath, Deserialize)]
#[typed_path("/api/v1/runs/{run_id}/events")]
pub struct RunEventsPath {
    pub run_id: RunId,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct RunEventsQuery {
    /// Sequence number of the last event the client holds, 0 by default.
    pub after: Option<i64>,
    /// 1 to 1000, 200 by default.
    pub limit: Option<u32>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RunEventResponse {
    pub seq: i64,
    /// One of `assistant_text`, `tool_use`, `tool_result`, `system`.
    pub kind: String,
    pub payload: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RunEventsResponse {
    pub events: Vec<RunEventResponse>,
    /// The cursor to pass as `after` for the next page; null when the page is not full.
    pub next_after: Option<i64>,
}

impl From<RunEvent> for RunEventResponse {
    fn from(event: RunEvent) -> Self {
        Self {
            seq: event.seq,
            kind: event.kind.as_str().to_owned(),
            payload: event.payload,
            created_at: event.created_at,
        }
    }
}

fn effective_limit(limit: Option<u32>) -> u32 {
    limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

fn page(events: Vec<RunEvent>, limit: u32) -> RunEventsResponse {
    let full = u32::try_from(events.len()).is_ok_and(|len| len == limit);
    let next_after = events.last().filter(|_| full).map(|event| event.seq);
    RunEventsResponse {
        events: events.into_iter().map(RunEventResponse::from).collect(),
        next_after,
    }
}

#[utoipa::path(
    get,
    path = "/api/v1/runs/{run_id}/events",
    operation_id = "listRunEvents",
    tag = super::TAG,
    params(("run_id" = RunId, Path, description = "Run identifier"), RunEventsQuery),
    responses(
        (status = 200, description = "The run's stored events after the cursor, in order; `next_after` is set when more may exist", body = RunEventsResponse),
        (status = 401, description = "Missing or invalid token", body = matos_handlers::errors::ApiErrorResponse),
        (status = 404, description = "No such run in the caller's organization", body = matos_handlers::errors::ApiErrorResponse),
        (status = 422, description = "A negative cursor", body = matos_handlers::errors::ApiErrorResponse),
    ),
    security(("bearer_auth" = []))
)]
pub async fn handler(
    RunEventsPath { run_id }: RunEventsPath,
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    Query(query): Query<RunEventsQuery>,
) -> Result<Json<RunEventsResponse>, ApiError> {
    let after = query.after.unwrap_or(0);
    if after < 0 {
        return Err(ApiError::BadRequest("after must be 0 or more".to_owned()));
    }
    let limit = effective_limit(query.limit);
    let events = state
        .usecase
        .list_run_events(caller.subject(), run_id, after, limit)
        .await?;
    Ok(Json(page(events, limit)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use matos_common::OrganizationId;
    use matos_core::domain::run::event::RunEventKind;

    fn event(seq: i64, kind: RunEventKind) -> RunEvent {
        RunEvent {
            run_id: RunId(uuid::Uuid::nil()),
            organization_id: OrganizationId(uuid::Uuid::nil()),
            seq,
            kind,
            payload: serde_json::json!({ "text": format!("event {seq}") }),
            created_at: DateTime::from_timestamp(1_700_000_000 + seq, 0).unwrap(),
        }
    }

    #[test]
    fn a_full_page_carries_the_last_seq_as_cursor() {
        let response = page(
            vec![
                event(11, RunEventKind::System),
                event(12, RunEventKind::System),
            ],
            2,
        );

        assert_eq!(response.next_after, Some(12));
    }

    #[test]
    fn a_short_or_empty_page_has_no_cursor() {
        let short = page(vec![event(1, RunEventKind::System)], 2);
        let empty = page(Vec::new(), 2);

        assert_eq!(short.next_after, None);
        assert_eq!(empty.next_after, None);
    }

    #[test]
    fn the_limit_defaults_to_200_and_is_clamped() {
        assert_eq!(effective_limit(None), 200);
        assert_eq!(effective_limit(Some(0)), 1);
        assert_eq!(effective_limit(Some(50)), 50);
        assert_eq!(effective_limit(Some(5000)), 1000);
    }

    #[test]
    fn an_event_maps_kind_payload_and_timestamp() {
        let source = event(7, RunEventKind::ToolResult);
        let created_at = source.created_at;

        let json = serde_json::to_value(RunEventResponse::from(source)).unwrap();

        assert_eq!(json["seq"], 7);
        assert_eq!(json["kind"], "tool_result");
        assert_eq!(json["payload"], serde_json::json!({ "text": "event 7" }));
        assert_eq!(json["created_at"], serde_json::json!(created_at));
        assert_eq!(json["created_at"], "2023-11-14T22:13:27Z");
    }
}
