#![allow(clippy::unwrap_used)]

use chrono::{DateTime, Utc};
use matos_common::{CoreError, OrganizationId, RunId};
use matos_core::{MatosUseCase, domain::authorization::Subject, infrastructure::postgres};
use sqlx::PgPool;
use uuid::Uuid;

const INITIATOR: &str = "initiator-1";

fn database_url() -> Option<String> {
    match std::env::var("DATABASE_URL") {
        Ok(url) => Some(url),
        Err(_) if std::env::var("REQUIRE_DATABASE_URL").as_deref() == Ok("1") => {
            panic!("DATABASE_URL is required when REQUIRE_DATABASE_URL=1")
        }
        Err(_) => None,
    }
}

async fn setup() -> Option<(PgPool, MatosUseCase, OrganizationId)> {
    let Some(url) = database_url() else {
        eprintln!("skipped: DATABASE_URL is unset");
        return None;
    };
    let pool = postgres::connect(&url, 3).await.unwrap();
    let usecase = MatosUseCase::with_test_policies(pool.clone());
    let organization = organization(&pool).await;
    Some((pool, usecase, organization))
}

async fn organization(pool: &PgPool) -> OrganizationId {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO organizations (id, slug, name, issuer_url) VALUES ($1, $2, 'Test', $3)",
    )
    .bind(id)
    .bind(format!("test-{}", id.simple()))
    .bind(format!("http://idp.test/realms/{id}"))
    .execute(pool)
    .await
    .unwrap();
    OrganizationId(id)
}

async fn run(pool: &PgPool, org: OrganizationId, status: &str) -> RunId {
    let transcript = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO transcripts (id, organization_id, submitted_by, title, body, created_at)
         VALUES ($1, $2, 'test', 't', 'b', now())",
    )
    .bind(transcript)
    .bind(org.0)
    .execute(pool)
    .await
    .unwrap();
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO runs (id, organization_id, transcript_id, initiated_by, status, pipeline, created_at)
         VALUES ($1, $2, $3, $4, $5, '{}', now())",
    )
    .bind(id)
    .bind(org.0)
    .bind(transcript)
    .bind(INITIATOR)
    .bind(status)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO task_grants (token_sha256, run_id, organization_id, repository, branch, expires_at, model)
         VALUES ($1, $2, $3, 'o/r', 'b', now() + interval '1 hour', 'm')",
    )
    .bind(id.as_bytes().to_vec())
    .bind(id)
    .bind(org.0)
    .execute(pool)
    .await
    .unwrap();
    RunId(id)
}

fn subject(org: OrganizationId, id: &str, is_admin: bool, actor: Option<&str>) -> Subject {
    Subject {
        id: id.into(),
        organization_id: org,
        actor: actor.map(str::to_owned),
        is_admin,
    }
}

async fn add_member(pool: &PgPool, org: OrganizationId, subject: &str, name: &str) {
    sqlx::query(
        "INSERT INTO members (organization_id, subject, name, last_seen_at) VALUES ($1, $2, $3, now())",
    )
    .bind(org.0)
    .bind(subject)
    .bind(name)
    .execute(pool)
    .await
    .unwrap();
}

struct Stored {
    status: String,
    failure: Option<String>,
    finished_at: Option<DateTime<Utc>>,
}

async fn stored(pool: &PgPool, run: RunId) -> Stored {
    let row: (String, Option<String>, Option<DateTime<Utc>>) =
        sqlx::query_as("SELECT status, failure, finished_at FROM runs WHERE id = $1")
            .bind(run.0)
            .fetch_one(pool)
            .await
            .unwrap();
    Stored {
        status: row.0,
        failure: row.1,
        finished_at: row.2,
    }
}

async fn live_grants(pool: &PgPool, run: RunId) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM task_grants WHERE run_id = $1 AND revoked_at IS NULL")
        .bind(run.0)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn finished_events(pool: &PgPool, run: RunId) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT envelope::text FROM outbox_events WHERE partition_key = $1 AND kind = 'run.finished'",
    )
    .bind(run.0)
    .fetch_all(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn the_initiator_cancels_a_running_run() {
    let Some((pool, usecase, org)) = setup().await else {
        return;
    };
    add_member(&pool, org, INITIATOR, "Ada Lovelace").await;
    let run = run(&pool, org, "running").await;

    let outcome = usecase
        .cancel_run(subject(org, INITIATOR, false, None), run)
        .await
        .unwrap();

    assert_eq!(format!("{outcome:?}"), "Cancelled");
    let after = stored(&pool, run).await;
    assert_eq!(after.status, "cancelled");
    assert_eq!(after.failure.as_deref(), Some("Arrêté par Ada Lovelace"));
    assert!(after.finished_at.is_some());
    assert_eq!(live_grants(&pool, run).await, 0);
    let events = finished_events(&pool, run).await;
    assert_eq!(events.len(), 1);
    assert!(
        events[0].contains("\"status\": \"cancelled\""),
        "{}",
        events[0]
    );
    assert_eq!(
        usecase.run_cancellation(org, run).await.unwrap().as_deref(),
        Some("Arrêté par Ada Lovelace")
    );
}

#[tokio::test]
async fn the_initiator_cancels_a_queued_run() {
    let Some((pool, usecase, org)) = setup().await else {
        return;
    };
    let run = run(&pool, org, "queued").await;

    let outcome = usecase
        .cancel_run(subject(org, INITIATOR, false, None), run)
        .await
        .unwrap();

    assert_eq!(format!("{outcome:?}"), "Cancelled");
    let after = stored(&pool, run).await;
    assert_eq!(after.status, "cancelled");
    assert_eq!(
        after.failure.as_deref(),
        Some(format!("Arrêté par {INITIATOR}").as_str())
    );
}

#[tokio::test]
async fn an_admin_who_did_not_start_the_run_cancels_it() {
    let Some((pool, usecase, org)) = setup().await else {
        return;
    };
    add_member(&pool, org, "admin-1", "Grace Hopper").await;
    let run = run(&pool, org, "running").await;

    let outcome = usecase
        .cancel_run(subject(org, "admin-1", true, None), run)
        .await
        .unwrap();

    assert_eq!(format!("{outcome:?}"), "Cancelled");
    assert_eq!(
        stored(&pool, run).await.failure.as_deref(),
        Some("Arrêté par Grace Hopper")
    );
}

#[tokio::test]
async fn another_member_is_denied_and_the_run_is_unchanged() {
    let Some((pool, usecase, org)) = setup().await else {
        return;
    };
    let run = run(&pool, org, "running").await;

    let outcome = usecase
        .cancel_run(subject(org, "someone-else", false, None), run)
        .await;

    assert!(matches!(outcome, Err(CoreError::Forbidden { .. })));
    let after = stored(&pool, run).await;
    assert_eq!(after.status, "running");
    assert!(after.finished_at.is_none());
    assert_eq!(live_grants(&pool, run).await, 1);
    assert!(finished_events(&pool, run).await.is_empty());
}

#[tokio::test]
async fn an_agent_is_denied_even_as_the_initiator_or_an_admin() {
    let Some((pool, usecase, org)) = setup().await else {
        return;
    };
    let run = run(&pool, org, "running").await;

    for (is_admin, id) in [(false, INITIATOR), (true, "admin-1")] {
        let outcome = usecase
            .cancel_run(subject(org, id, is_admin, Some("agent-writer")), run)
            .await;
        assert!(matches!(outcome, Err(CoreError::Forbidden { .. })));
    }
    assert_eq!(stored(&pool, run).await.status, "running");
}

#[tokio::test]
async fn a_run_of_another_organization_is_not_found() {
    let Some((pool, usecase, org)) = setup().await else {
        return;
    };
    let other = organization(&pool).await;
    let run = run(&pool, other, "running").await;

    let outcome = usecase
        .cancel_run(subject(org, INITIATOR, true, None), run)
        .await;

    assert!(matches!(outcome, Err(CoreError::NotFound)));
    assert_eq!(stored(&pool, run).await.status, "running");
}

#[tokio::test]
async fn cancelling_twice_emits_a_single_event() {
    let Some((pool, usecase, org)) = setup().await else {
        return;
    };
    let run = run(&pool, org, "running").await;
    let who = subject(org, INITIATOR, false, None);

    let first = usecase.cancel_run(who.clone(), run).await.unwrap();
    let second = usecase.cancel_run(who, run).await.unwrap();

    assert_eq!(format!("{first:?}"), "Cancelled");
    assert_eq!(format!("{second:?}"), "AlreadyCancelled");
    assert_eq!(finished_events(&pool, run).await.len(), 1);
}

#[tokio::test]
async fn a_finished_run_cannot_be_cancelled() {
    let Some((pool, usecase, org)) = setup().await else {
        return;
    };
    for status in ["succeeded", "failed"] {
        let run = run(&pool, org, status).await;

        let outcome = usecase
            .cancel_run(subject(org, INITIATOR, false, None), run)
            .await;

        assert!(
            matches!(&outcome, Err(CoreError::Conflict(code)) if code == "run_already_finished"),
            "{status}: {outcome:?}"
        );
        assert_eq!(stored(&pool, run).await.status, status);
        assert!(finished_events(&pool, run).await.is_empty());
    }
}
