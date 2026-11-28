#![allow(clippy::unwrap_used)]

use chrono::Utc;
use matos_common::{CardId, OrganizationId, RunId};
use matos_core::{
    MatosUseCase,
    domain::{
        agent::SecretToken,
        agent_task::{ExtraRepositories, RepositoryTarget, StartAgentRun},
        authorization::Subject,
        card::{
            Card, Column,
            edit::EditCard,
            service::{CreateCard, MoveCard},
        },
        client::links::Change,
        crew::service::CreateAgent,
        execution::{ExecutorStatus, GithubAppCredentials},
        provider::ProviderKind,
        run::{Run, RunSubject},
        user_provider::service::PutUserProvider,
    },
    infrastructure::{crypto::SecretCipher, postgres},
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

fn member(organization_id: OrganizationId, id: &str) -> Subject {
    Subject {
        id: id.into(),
        organization_id,
        actor: None,
        is_admin: false,
    }
}

fn admin(organization_id: OrganizationId) -> Subject {
    Subject {
        is_admin: true,
        ..member(organization_id, "admin-1")
    }
}

fn fresh_id() -> i64 {
    (Uuid::now_v7().as_u128() & 0x3FFF_FFFF_FFFF_FFFF) as i64
}

struct World {
    organization_id: OrganizationId,
    pool: PgPool,
    usecase: MatosUseCase,
    run: Run,
    card: Card,
}

async fn provider_for(usecase: &MatosUseCase, organization_id: OrganizationId, id: &str) {
    usecase
        .put_user_provider(PutUserProvider {
            subject: member(organization_id, id),
            kind: ProviderKind::Anthropic,
            secret: Some(SecretToken::new("sk-ant-api03-abcdefgh1234")),
            base_url: None,
            default_model: "claude-haiku-5-5".into(),
            is_default: None,
        })
        .await
        .unwrap();
}

async fn world(extras: bool) -> Option<World> {
    let url = database_url().or_else(|| {
        eprintln!("skipped: DATABASE_URL is unset");
        None
    })?;
    let pool = postgres::connect(&url, 4).await.unwrap();
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO organizations (id, slug, name, issuer_url) VALUES ($1, $2, 'Test', $3)",
    )
    .bind(id)
    .bind(format!("test-{}", id.simple()))
    .bind(format!("http://idp.test/realms/{id}"))
    .execute(&pool)
    .await
    .unwrap();
    let organization_id = OrganizationId(id);
    let usecase = MatosUseCase::with_test_policies(pool.clone())
        .with_cipher(SecretCipher::new(&[7u8; 32]).unwrap())
        .with_secrets_cipher(SecretCipher::new(&[9u8; 32]).unwrap());
    usecase
        .save_github_app(
            admin(organization_id),
            GithubAppCredentials {
                organization_id,
                app_id: fresh_id(),
                slug: "matos-test".into(),
                client_id: "Iv1.test".into(),
                client_secret: SecretToken::new("oauth-secret"),
                private_key_pem: SecretToken::new("pem"),
                webhook_secret: SecretToken::new("hook"),
                html_url: "https://github.com/apps/matos-test".into(),
                owner_login: "acme".into(),
                owner_type: "Organization".into(),
                created_by: "admin-1".into(),
                created_at: Utc::now(),
            },
        )
        .await
        .unwrap();
    usecase
        .connect_github(admin(organization_id), fresh_id(), "acme".into())
        .await
        .unwrap();
    provider_for(&usecase, organization_id, "user-1").await;
    provider_for(&usecase, organization_id, "user-2").await;
    usecase
        .record_executor_status(&ExecutorStatus {
            enabled: true,
            reason: None,
        })
        .await
        .unwrap();
    let agent = usecase
        .create_crew_agent(
            member(organization_id, "user-1"),
            CreateAgent {
                slug: "auditrice".into(),
                name: "Auditrice".into(),
                role: String::new(),
                description: String::new(),
                model: "claude-opus-5".into(),
                hue: 0,
                reports_to: None,
                instructions: "Tu audites.".into(),
                connector_grants: vec![],
                repository_grants: vec!["acme/web".into(), "acme/api".into()],
            },
        )
        .await
        .unwrap();
    let card = usecase
        .create_card(
            member(organization_id, "user-1"),
            CreateCard {
                title: "Audite le dépôt".into(),
                description: "Cherche les failles".into(),
                provenance: None,
                run_id: None,
                attributes: Default::default(),
            },
        )
        .await
        .unwrap();
    let mut edit = EditCard::new(card.version);
    edit.assignee_agent = Change::Set(agent.id);
    let card = usecase
        .edit_card(member(organization_id, "user-1"), card.id, edit)
        .await
        .unwrap();
    let card = usecase
        .move_card(
            member(organization_id, "user-1"),
            card.id,
            MoveCard {
                to: Column::Todo,
                expected_version: card.version,
                run_id: None,
            },
        )
        .await
        .unwrap();
    let primary = "acme/web".parse().unwrap();
    let mut target =
        RepositoryTarget::new("acme/web".parse().unwrap(), Some("develop".into())).unwrap();
    if extras {
        target.extra_repositories =
            ExtraRepositories::new(&primary, vec!["acme/api".parse().unwrap()]).unwrap();
    }
    let started = usecase
        .start_agent_repository_run(
            member(organization_id, "user-1"),
            card.id,
            StartAgentRun {
                expected_version: card.version,
                allow_writes: true,
            },
            target,
        )
        .await
        .unwrap();
    let run = usecase
        .get_run(member(organization_id, "user-1"), started.run_id)
        .await
        .unwrap()
        .run;
    Some(World {
        organization_id,
        pool,
        usecase,
        run,
        card,
    })
}

async fn finish(world: &World, status: &str) {
    sqlx::query("UPDATE runs SET status = $2, finished_at = now() WHERE id = $1")
        .bind(world.run.id.0)
        .bind(status)
        .execute(&world.pool)
        .await
        .unwrap();
}

async fn reply(world: &World, author: Subject, body: &str) {
    world
        .usecase
        .comment_card(author, world.card.id, body.into(), None)
        .await
        .unwrap();
}

async fn next_runs(world: &World) -> Vec<(RunId, String)> {
    let rows: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT id, initiated_by FROM runs WHERE card_id = $1 AND id <> $2 ORDER BY id",
    )
    .bind(world.card.id.0)
    .bind(world.run.id.0)
    .fetch_all(&world.pool)
    .await
    .unwrap();
    rows.into_iter().map(|(id, by)| (RunId(id), by)).collect()
}

async fn allow_writes(world: &World, run_id: RunId) -> bool {
    sqlx::query_scalar("SELECT allow_writes FROM agent_tasks WHERE run_id = $1")
        .bind(run_id.0)
        .fetch_one(&world.pool)
        .await
        .unwrap()
}

fn card_of(run: &Run) -> CardId {
    match &run.subject {
        RunSubject::Card { card_id, .. } => *card_id,
        _ => panic!("not a card run"),
    }
}

#[tokio::test]
async fn a_human_reply_relaunches_the_run_on_the_same_target_read_only_as_the_replier() {
    let Some(world) = world(true).await else {
        return;
    };
    finish(&world, "succeeded").await;
    reply(
        &world,
        member(world.organization_id, "user-2"),
        "Et le dépôt api ?",
    )
    .await;

    let started = world
        .usecase
        .relaunch_repository_on_reply(&world.run)
        .await
        .unwrap()
        .unwrap();

    let next = next_runs(&world).await;
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].0, started.run_id);
    assert_eq!(next[0].1, "user-2");
    assert!(!allow_writes(&world, started.run_id).await);
    let relaunched = world
        .usecase
        .get_run(member(world.organization_id, "user-2"), started.run_id)
        .await
        .unwrap()
        .run;
    assert_eq!(card_of(&relaunched), card_of(&world.run));
    assert_eq!(relaunched.subject, world.run.subject);
    let RunSubject::Card {
        base_branch,
        extra_repositories,
        ..
    } = &relaunched.subject
    else {
        panic!("not a card run");
    };
    assert_eq!(base_branch, "develop");
    assert_eq!(extra_repositories.len(), 1);
}

#[tokio::test]
async fn without_a_reply_nothing_starts() {
    let Some(world) = world(false).await else {
        return;
    };
    finish(&world, "succeeded").await;

    let started = world
        .usecase
        .relaunch_repository_on_reply(&world.run)
        .await
        .unwrap();

    assert!(started.is_none());
    assert!(next_runs(&world).await.is_empty());
}

#[tokio::test]
async fn an_agent_comment_alone_starts_nothing() {
    let Some(world) = world(false).await else {
        return;
    };
    finish(&world, "succeeded").await;
    let agent = Subject {
        actor: Some("agent:auditrice".into()),
        ..member(world.organization_id, "user-1")
    };
    reply(&world, agent, "Voici mon rapport").await;

    let started = world
        .usecase
        .relaunch_repository_on_reply(&world.run)
        .await
        .unwrap();

    assert!(started.is_none());
    assert!(next_runs(&world).await.is_empty());
}

#[tokio::test]
async fn a_start_refused_as_a_conflict_is_not_an_error() {
    let Some(world) = world(false).await else {
        return;
    };
    reply(
        &world,
        member(world.organization_id, "user-2"),
        "Une précision",
    )
    .await;

    let started = world
        .usecase
        .relaunch_repository_on_reply(&world.run)
        .await
        .unwrap();

    assert!(started.is_none());
    assert!(next_runs(&world).await.is_empty());
}

#[tokio::test]
async fn a_run_that_is_not_a_card_run_relaunches_nothing() {
    let Some(world) = world(false).await else {
        return;
    };
    finish(&world, "succeeded").await;
    reply(
        &world,
        member(world.organization_id, "user-2"),
        "Une précision",
    )
    .await;
    let mut run = world.run.clone();
    run.subject = RunSubject::AgentTask {
        card_id: world.card.id,
    };

    let started = world
        .usecase
        .relaunch_repository_on_reply(&run)
        .await
        .unwrap();

    assert!(started.is_none());
    assert!(next_runs(&world).await.is_empty());
}
