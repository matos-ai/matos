#![allow(clippy::unwrap_used)]

use matos_common::{CoreError, OrganizationId, RunId};
use matos_core::{
    MatosUseCase,
    domain::{
        authorization::Subject,
        run::event::{RunEventDraft, RunEventKind, SequencedRunEvent},
    },
    infrastructure::postgres,
};
use sqlx::PgPool;
use uuid::Uuid;

fn database_url() -> Option<String> {
    match std::env::var("DATABASE_URL") {
        Ok(url) => Some(url),
        Err(_) if std::env::var("REQUIRE_DATABASE_URL").as_deref() == Ok("1") => {
            panic!("DATABASE_URL is required when REQUIRE_DATABASE_URL=1")
        }
        Err(_) => None,
    }
}

async fn pool() -> Option<PgPool> {
    let Some(url) = database_url() else {
        eprintln!("skipped: DATABASE_URL is unset");
        return None;
    };
    Some(postgres::connect(&url, 2).await.unwrap())
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

async fn run(pool: &PgPool, org: OrganizationId) -> RunId {
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
         VALUES ($1, $2, $3, 'test', 'running', '{}', now())",
    )
    .bind(id)
    .bind(org.0)
    .bind(transcript)
    .execute(pool)
    .await
    .unwrap();
    RunId(id)
}

fn admin(organization_id: OrganizationId) -> Subject {
    Subject {
        id: "admin-1".into(),
        organization_id,
        actor: None,
        is_admin: true,
    }
}

fn text(seq: i64, text: &str) -> SequencedRunEvent {
    SequencedRunEvent {
        seq,
        draft: RunEventDraft::AssistantText {
            text: text.into(),
            truncated: false,
        },
    }
}

fn batch() -> Vec<SequencedRunEvent> {
    vec![
        text(1, "one"),
        SequencedRunEvent {
            seq: 2,
            draft: RunEventDraft::ToolUse {
                id: "t1".into(),
                tool: "Bash".into(),
                input: "ls".into(),
            },
        },
        SequencedRunEvent {
            seq: 3,
            draft: RunEventDraft::ToolResult {
                id: "t1".into(),
                succeeded: true,
                output: "ok".into(),
            },
        },
        SequencedRunEvent {
            seq: 4,
            draft: RunEventDraft::System {
                text: "done".into(),
            },
        },
    ]
}

#[tokio::test]
async fn append_then_list_after_orders_and_filters() {
    let Some(pool) = pool().await else { return };
    let usecase = MatosUseCase::with_test_policies(pool.clone());
    let org = organization(&pool).await;
    let run_id = run(&pool, org).await;
    let mut events = batch();
    events.reverse();
    usecase
        .append_run_events(org, run_id, events)
        .await
        .unwrap();

    let all = usecase
        .list_run_events(admin(org), run_id, 0, 100)
        .await
        .unwrap();
    assert_eq!(all.iter().map(|e| e.seq).collect::<Vec<_>>(), [1, 2, 3, 4]);
    assert_eq!(all[0].payload["text"], "one");

    let tail = usecase
        .list_run_events(admin(org), run_id, 2, 100)
        .await
        .unwrap();
    assert_eq!(tail.iter().map(|e| e.seq).collect::<Vec<_>>(), [3, 4]);
}

#[tokio::test]
async fn kinds_round_trip() {
    let Some(pool) = pool().await else { return };
    let usecase = MatosUseCase::with_test_policies(pool.clone());
    let org = organization(&pool).await;
    let run_id = run(&pool, org).await;
    usecase
        .append_run_events(org, run_id, batch())
        .await
        .unwrap();
    let stored = usecase
        .list_run_events(admin(org), run_id, 0, 100)
        .await
        .unwrap();
    let kinds: Vec<RunEventKind> = stored.iter().map(|e| e.kind).collect();
    assert_eq!(
        kinds,
        [
            RunEventKind::AssistantText,
            RunEventKind::ToolUse,
            RunEventKind::ToolResult,
            RunEventKind::System
        ]
    );
    for event in &stored {
        assert_eq!(
            RunEventKind::try_from(event.kind.as_str()).unwrap(),
            event.kind
        );
    }
}

#[tokio::test]
async fn replay_is_a_no_op_and_keeps_original_payload() {
    let Some(pool) = pool().await else { return };
    let usecase = MatosUseCase::with_test_policies(pool.clone());
    let org = organization(&pool).await;
    let run_id = run(&pool, org).await;
    usecase
        .append_run_events(org, run_id, batch())
        .await
        .unwrap();
    usecase
        .append_run_events(org, run_id, batch())
        .await
        .unwrap();
    usecase
        .append_run_events(org, run_id, vec![text(1, "changed"), text(5, "five")])
        .await
        .unwrap();
    usecase
        .append_run_events(org, run_id, Vec::new())
        .await
        .unwrap();

    let all = usecase
        .list_run_events(admin(org), run_id, 0, 100)
        .await
        .unwrap();
    assert_eq!(
        all.iter().map(|e| e.seq).collect::<Vec<_>>(),
        [1, 2, 3, 4, 5]
    );
    assert_eq!(all[0].payload["text"], "one");
}

#[tokio::test]
async fn limit_is_respected() {
    let Some(pool) = pool().await else { return };
    let usecase = MatosUseCase::with_test_policies(pool.clone());
    let org = organization(&pool).await;
    let run_id = run(&pool, org).await;
    usecase
        .append_run_events(org, run_id, batch())
        .await
        .unwrap();
    let page = usecase
        .list_run_events(admin(org), run_id, 0, 2)
        .await
        .unwrap();
    assert_eq!(page.iter().map(|e| e.seq).collect::<Vec<_>>(), [1, 2]);
    let clamped = usecase
        .list_run_events(admin(org), run_id, 0, 0)
        .await
        .unwrap();
    assert_eq!(clamped.len(), 1);
}

#[tokio::test]
async fn another_organization_sees_nothing() {
    let Some(pool) = pool().await else { return };
    let usecase = MatosUseCase::with_test_policies(pool.clone());
    let org = organization(&pool).await;
    let other = organization(&pool).await;
    let run_id = run(&pool, org).await;
    usecase
        .append_run_events(org, run_id, batch())
        .await
        .unwrap();

    let result = usecase.list_run_events(admin(other), run_id, 0, 100).await;
    assert!(matches!(result, Err(CoreError::NotFound)));

    let other_run = run(&pool, other).await;
    let own = usecase
        .list_run_events(admin(other), other_run, 0, 100)
        .await
        .unwrap();
    assert!(own.is_empty());
}
