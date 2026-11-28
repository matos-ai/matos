#![allow(clippy::unwrap_used)]

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use chrono::Utc;
use matos_common::{CardId, CoreError, OrganizationId, RunId};
use matos_core::{
    ExecuteConfig, ExecutePipeline, Executor, MatosUseCase,
    domain::{
        agent::SecretToken,
        authorization::Subject,
        card::{
            Card, Column,
            edit::EditCard,
            service::{CreateCard, MoveCard},
        },
        client::links::Change,
        crew::service::CreateAgent,
        delegation::DelegatedToken,
        execution::{
            CommitAuthor, DraftPullRequest, ExecutionMode, ExecutorModel, ExecutorStatus,
            GithubAppCredentials, PullRequestUrl, TaskGrant, TaskLine, TaskMarker, TaskOutcome,
            TaskSpec,
            harness::Harness,
            ports::{MockCodeHost, MockTaskRunner},
        },
        github_user::{
            GitIdentity, GithubUser, PersonalTokenCheck, UserTokens,
            ports::{MockGithubUserOAuth, MockPersonalTokenValidator},
        },
        member::Member,
        provider::{ModelAssignments, ProviderKind},
        run::{
            StepKind,
            step::{Pricing, Step, StepStatus},
            stream::{RUN_STREAM_CHANNEL, RunStreamNotice},
        },
        user_provider::service::PutUserProvider,
    },
    infrastructure::{crypto::SecretCipher, postgres},
};
use sqlx::PgPool;
use uuid::Uuid;

const SAMPLE: &str = include_str!("../../../libs/core/tests/fixtures/claude-stream.jsonl");
const CODEX_SAMPLE: &str = include_str!("../../../libs/core/tests/fixtures/codex-stream.jsonl");
const VIBE_SAMPLE: &str = include_str!("../../../libs/core/tests/fixtures/vibe-stream.jsonl");
const ANTIGRAVITY_SAMPLE: &str =
    include_str!("../../../libs/core/tests/fixtures/antigravity-stream.jsonl");
const PR_URL: &str = "https://github.com/acme/web/pull/7";

fn database_url() -> Option<String> {
    match std::env::var("DATABASE_URL") {
        Ok(url) => Some(url),
        Err(_) if std::env::var("REQUIRE_DATABASE_URL").as_deref() == Ok("1") => {
            panic!("DATABASE_URL is required when REQUIRE_DATABASE_URL=1")
        }
        Err(_) => None,
    }
}

fn human(organization_id: OrganizationId) -> Subject {
    Subject {
        id: "user-1".into(),
        organization_id,
        actor: None,
        is_admin: false,
    }
}

fn admin(organization_id: OrganizationId) -> Subject {
    Subject {
        is_admin: true,
        ..human(organization_id)
    }
}

fn app_of(organization_id: OrganizationId) -> GithubAppCredentials {
    GithubAppCredentials {
        organization_id,
        app_id: fresh_installation(),
        slug: "matos-test".into(),
        client_id: "Iv1.test".into(),
        client_secret: SecretToken::new("oauth-secret"),
        private_key_pem: SecretToken::new("pem"),
        webhook_secret: SecretToken::new("hook"),
        html_url: "https://github.com/apps/matos-test".into(),
        owner_login: "acme".into(),
        owner_type: "Organization".into(),
        created_by: "user-1".into(),
        created_at: Utc::now(),
    }
}

struct World {
    pool: PgPool,
    usecase: MatosUseCase,
    organization_id: OrganizationId,
    installation_id: i64,
    run_id: RunId,
    card: Card,
}

fn fresh_installation() -> i64 {
    (Uuid::now_v7().as_u128() & 0x3FFF_FFFF_FFFF_FFFF) as i64
}

async fn world() -> Option<World> {
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
    let installation_id = fresh_installation();
    usecase
        .save_github_app(admin(organization_id), app_of(organization_id))
        .await
        .unwrap();
    usecase
        .connect_github(
            Subject {
                is_admin: true,
                ..human(organization_id)
            },
            installation_id,
            "acme".into(),
        )
        .await
        .unwrap();
    let proposed = usecase
        .create_card(
            human(organization_id),
            CreateCard {
                title: "Ajouter un CHANGELOG".into(),
                description: "À la racine".into(),
                provenance: None,
                run_id: None,
                attributes: Default::default(),
            },
        )
        .await
        .unwrap();
    let todo = usecase
        .move_card(
            human(organization_id),
            proposed.id,
            MoveCard {
                to: Column::Todo,
                expected_version: proposed.version,
                run_id: None,
            },
        )
        .await
        .unwrap();
    let started = usecase
        .execute_card(
            human(organization_id),
            todo.id,
            "acme/web".parse().unwrap(),
            "main".into(),
            Vec::new(),
            todo.version,
        )
        .await
        .unwrap();
    Some(World {
        pool,
        usecase,
        organization_id,
        installation_id,
        run_id: started.run_id,
        card: started.card,
    })
}

fn config() -> ExecuteConfig {
    ExecuteConfig {
        max_attempts: 2,
        retry_base: Duration::from_millis(1),
        stale_after: Duration::from_secs(600),
        max_turns: 50,
        deadline: Duration::from_secs(1800),
        progress_every: Duration::from_millis(25),
        console_url: Some("https://console.test".into()),
        default_models: ModelAssignments::defaults("chat", "embed"),
        prices: vec![(
            "claude-sonnet-5-5".into(),
            Pricing {
                input_per_million_micros: 1_000_000,
                output_per_million_micros: 2_000_000,
            },
        )],
    }
}

/// The branch of the fixture card's run: its title, then the run's tail.
fn branch_of(run_id: RunId) -> String {
    let simple = run_id.0.simple().to_string();
    format!(
        "change/ajouter-un-changelog-{}",
        &simple[simple.len() - 6..]
    )
}

fn lines_of(sample: &str) -> Vec<TaskLine> {
    sample
        .lines()
        .map(|line| {
            let value: serde_json::Value = serde_json::from_str(line).unwrap();
            if value["type"] == "matos" {
                TaskLine::Matos(serde_json::from_value(value).unwrap())
            } else {
                TaskLine::Agent(value)
            }
        })
        .collect()
}

#[derive(Clone, Default)]
struct Seen {
    token: Arc<Mutex<Option<String>>>,
    starts: Arc<Mutex<u32>>,
    pull_requests: Arc<Mutex<Vec<DraftPullRequest>>>,
}

const PUSHED_SHA: &str = "3f9c2a7d1e5b48a6c0d7e9f1a2b3c4d5e6f70812";

fn runner_ending(seen: &Seen, lines: Vec<TaskLine>, outcome: TaskOutcome) -> MockTaskRunner {
    let mut runner = MockTaskRunner::new();
    let (token, starts) = (seen.token.clone(), seen.starts.clone());
    runner.expect_start().returning(move |spec| {
        *token.lock().unwrap() = Some(spec.task_token.expose().to_owned());
        *starts.lock().unwrap() += 1;
        Box::pin(std::future::ready(Ok(())))
    });
    runner.expect_follow().returning(move |_, sender| {
        let lines = lines.clone();
        let outcome = outcome.clone();
        Box::pin(async move {
            for line in lines {
                sender.send(line).await.unwrap();
            }
            Ok(outcome)
        })
    });
    runner
}

fn deleting(mut runner: MockTaskRunner) -> MockTaskRunner {
    runner
        .expect_delete()
        .times(1)
        .returning(|_| Box::pin(std::future::ready(Ok(()))));
    runner
}

fn host(seen: &Seen, organization_id: OrganizationId, installation_id: i64) -> MockCodeHost {
    host_with_config(seen, organization_id, installation_id, None)
}

fn without_config() -> MockCodeHost {
    config_file(MockCodeHost::new(), None)
}

fn config_file(mut host: MockCodeHost, text: Option<&'static str>) -> MockCodeHost {
    host.expect_file_at()
        .returning(move |_, _, repository, git_ref, path| {
            assert_eq!(repository.to_string(), "acme/web");
            assert_eq!(git_ref, "main");
            assert_eq!(path, ".matos.toml");
            Box::pin(std::future::ready(Ok(text.map(str::to_owned))))
        });
    host
}

fn host_with_config(
    seen: &Seen,
    organization_id: OrganizationId,
    installation_id: i64,
    config: Option<&'static str>,
) -> MockCodeHost {
    let mut host = config_file(MockCodeHost::new(), config);
    let pull_requests = seen.pull_requests.clone();
    host.expect_open_draft_pr()
        .returning(move |organization, installation, pull_request| {
            assert_eq!(organization, organization_id);
            assert_eq!(installation, installation_id);
            pull_requests.lock().unwrap().push(pull_request);
            Box::pin(std::future::ready(Ok(PullRequestUrl(PR_URL.into()))))
        });
    host.expect_branch_head()
        .returning(|_, _, repository, branch| {
            assert_eq!(repository.to_string(), "acme/web");
            assert!(branch.starts_with("change/ajouter-un-changelog-"));
            Box::pin(std::future::ready(Ok(Some(PUSHED_SHA.to_owned()))))
        });
    host
}

type Pipeline = ExecutePipeline<MockTaskRunner, MockCodeHost>;

fn pipeline(world: &World, runner: MockTaskRunner, host: MockCodeHost) -> Pipeline {
    ExecutePipeline::new(
        world.usecase.clone(),
        Executor::Ready { runner, host },
        config(),
    )
}

async fn run_row(pool: &PgPool, run_id: RunId) -> (String, Option<String>) {
    sqlx::query_as("SELECT status, failure FROM runs WHERE id = $1")
        .bind(run_id.0)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn grants(pool: &PgPool, run_id: RunId) -> (i64, i64) {
    sqlx::query_as("SELECT count(*), count(revoked_at) FROM task_grants WHERE run_id = $1")
        .bind(run_id.0)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn delegations(pool: &PgPool, run_id: RunId) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM run_delegations WHERE run_id = $1")
        .bind(run_id.0)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn step(pool: &PgPool, run_id: RunId) -> (String, String, i64, i64, serde_json::Value) {
    sqlx::query_as(
        "SELECT status, kind, cost_micros, input_tokens, output FROM run_steps WHERE run_id = $1",
    )
    .bind(run_id.0)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn comments(pool: &PgPool, card_id: CardId) -> Vec<(String, Option<String>, String)> {
    sqlx::query_as(
        "SELECT author_subject, author_actor, body FROM card_comments WHERE card_id = $1 ORDER BY created_at",
    )
    .bind(card_id.0)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn assert_closed(world: &World) {
    let (total, revoked) = grants(&world.pool, world.run_id).await;
    assert_eq!(total, revoked, "every task grant is revoked");
    assert_eq!(delegations(&world.pool, world.run_id).await, 0);
}

#[tokio::test]
async fn a_codex_execution_is_priced_from_its_stream_and_opens_the_pull_request() {
    let Some(world) = world().await else { return };
    let seen = Seen::default();
    let runner = runner_ending(&seen, lines_of(CODEX_SAMPLE), TaskOutcome::Succeeded);
    let mut codex = config();
    codex.default_models.executor = ExecutorModel::new(Harness::Codex, "gpt-6-luna").unwrap();
    codex.prices = vec![(
        "gpt-6-luna".into(),
        Pricing {
            input_per_million_micros: 100_000,
            output_per_million_micros: 500_000,
        },
    )];
    let pipeline = ExecutePipeline::new(
        world.usecase.clone(),
        Executor::Ready {
            runner,
            host: host(&seen, world.organization_id, world.installation_id),
        },
        codex,
    );

    pipeline.execute(world.run_id, None).await.unwrap();

    assert_eq!(run_row(&world.pool, world.run_id).await.0, "succeeded");
    let pull_requests = seen.pull_requests.lock().unwrap().clone();
    assert_eq!(pull_requests.len(), 1);
    assert!(
        !pull_requests[0].body.contains("27500"),
        "no token count leaves"
    );
    let posted = comments(&world.pool, world.card.id).await;
    assert_eq!(posted.len(), 1);
    assert!(posted[0].2.contains(PR_URL));
    assert!(posted[0].2.contains("I added CHANGELOG.md"));
    let (status, kind, cost, input_tokens, output) = step(&world.pool, world.run_id).await;
    assert_eq!((status.as_str(), kind.as_str()), ("succeeded", "execute"));
    assert_eq!(input_tokens, 27_500);
    assert_eq!(cost, 1_070, "6500 fresh, 21000 cached at 10 %, 420 out");
    assert_eq!(output["pull_request"], PR_URL);
    assert_eq!(output["comment"], "posted");
    assert_eq!(output["tool_calls"].as_array().unwrap().len(), 3);
    let model: String = sqlx::query_scalar("SELECT model FROM task_grants WHERE run_id = $1")
        .bind(world.run_id.0)
        .fetch_one(&world.pool)
        .await
        .unwrap();
    assert_eq!(model, "gpt-6-luna");
    assert_closed(&world).await;
}

#[tokio::test]
async fn an_antigravity_execution_is_priced_from_its_stream_and_opens_the_pull_request() {
    let Some(world) = world().await else { return };
    let seen = Seen::default();
    let runner = runner_ending(&seen, lines_of(ANTIGRAVITY_SAMPLE), TaskOutcome::Succeeded);
    let mut antigravity = config();
    antigravity.default_models.executor =
        ExecutorModel::new(Harness::Antigravity, "gemini-3.8-flash").unwrap();
    antigravity.prices = vec![(
        "gemini-3.8-flash".into(),
        Pricing {
            input_per_million_micros: 1_500_000,
            output_per_million_micros: 7_500_000,
        },
    )];
    let pipeline = ExecutePipeline::new(
        world.usecase.clone(),
        Executor::Ready {
            runner,
            host: host(&seen, world.organization_id, world.installation_id),
        },
        antigravity,
    );

    pipeline.execute(world.run_id, None).await.unwrap();

    assert_eq!(run_row(&world.pool, world.run_id).await.0, "succeeded");
    let pull_requests = seen.pull_requests.lock().unwrap().clone();
    assert_eq!(pull_requests.len(), 1);
    assert!(
        !pull_requests[0].body.contains("2400"),
        "no token count leaves"
    );
    let posted = comments(&world.pool, world.card.id).await;
    assert_eq!(posted.len(), 1);
    assert!(posted[0].2.contains(PR_URL));
    assert!(posted[0].2.contains("Done: I ran echo."));
    let (status, kind, cost, input_tokens, output) = step(&world.pool, world.run_id).await;
    assert_eq!((status.as_str(), kind.as_str()), ("succeeded", "execute"));
    assert_eq!(input_tokens, 2_400);
    assert_eq!(cost, 1_965, "800 fresh, 1600 cached at 10 %, 70 out");
    assert_eq!(output["pull_request"], PR_URL);
    assert_eq!(output["comment"], "posted");
    assert_eq!(output["tool_calls"].as_array().unwrap().len(), 1);
    let model: String = sqlx::query_scalar("SELECT model FROM task_grants WHERE run_id = $1")
        .bind(world.run_id.0)
        .fetch_one(&world.pool)
        .await
        .unwrap();
    assert_eq!(model, "gemini-3.8-flash");
    assert_closed(&world).await;
}

#[tokio::test]
async fn a_mistral_vibe_execution_is_priced_from_its_stream_and_opens_the_pull_request() {
    let Some(world) = world().await else { return };
    let seen = Seen::default();
    let runner = runner_ending(&seen, lines_of(VIBE_SAMPLE), TaskOutcome::Succeeded);
    let mut vibe = config();
    vibe.default_models.executor =
        ExecutorModel::new(Harness::MistralVibe, "mistral-medium-3.5").unwrap();
    vibe.prices = vec![(
        "mistral-medium-3.5".into(),
        Pricing {
            input_per_million_micros: 1_500_000,
            output_per_million_micros: 7_500_000,
        },
    )];
    let pipeline = ExecutePipeline::new(
        world.usecase.clone(),
        Executor::Ready {
            runner,
            host: host(&seen, world.organization_id, world.installation_id),
        },
        vibe,
    );

    pipeline.execute(world.run_id, None).await.unwrap();

    assert_eq!(run_row(&world.pool, world.run_id).await.0, "succeeded");
    let pull_requests = seen.pull_requests.lock().unwrap().clone();
    assert_eq!(pull_requests.len(), 1);
    assert!(
        !pull_requests[0].body.contains("2000"),
        "no token count leaves"
    );
    let posted = comments(&world.pool, world.card.id).await;
    assert_eq!(posted.len(), 1);
    assert!(posted[0].2.contains(PR_URL));
    assert!(
        posted[0]
            .2
            .contains("Done: the command printed VIBE_LAB_OK.")
    );
    let (status, kind, cost, input_tokens, output) = step(&world.pool, world.run_id).await;
    assert_eq!((status.as_str(), kind.as_str()), ("succeeded", "execute"));
    assert_eq!(input_tokens, 2_000);
    assert_eq!(cost, 2_670, "1200 fresh, 800 cached at 10 %, 100 out");
    assert_eq!(output["pull_request"], PR_URL);
    assert_eq!(output["comment"], "posted");
    assert_eq!(output["tool_calls"].as_array().unwrap().len(), 1);
    let model: String = sqlx::query_scalar("SELECT model FROM task_grants WHERE run_id = $1")
        .bind(world.run_id.0)
        .fetch_one(&world.pool)
        .await
        .unwrap();
    assert_eq!(model, "mistral-medium-3.5");
    assert_closed(&world).await;
}

#[tokio::test]
async fn a_pushed_branch_opens_a_draft_pull_request_and_comments_the_card() {
    let Some(world) = world().await else { return };
    let seen = Seen::default();
    let lines = lines_of(SAMPLE);
    let token_probe = seen.token.clone();
    let mut runner = MockTaskRunner::new();
    let (token, starts) = (seen.token.clone(), seen.starts.clone());
    runner.expect_start().returning(move |spec| {
        *token.lock().unwrap() = Some(spec.task_token.expose().to_owned());
        *starts.lock().unwrap() += 1;
        assert_eq!(spec.branch, branch_of(spec.run_id));
        assert_eq!(spec.model.as_str(), "claude-sonnet-5-5");
        assert_eq!(spec.max_turns, 50);
        assert!(spec.prompt.contains("Ajouter un CHANGELOG"));
        Box::pin(std::future::ready(Ok(())))
    });
    runner.expect_follow().returning(move |_, sender| {
        let lines = lines.clone();
        let token = token_probe.lock().unwrap().clone().unwrap();
        Box::pin(async move {
            sender
                .send(TaskLine::Agent(serde_json::json!({
                    "type": "assistant",
                    "message": {"content": [{
                        "type": "tool_use", "id": "toolu_leak", "name": "Bash",
                        "input": {"command": format!("echo {token}")}
                    }]}
                })))
                .await
                .unwrap();
            sender
                .send(TaskLine::Agent(serde_json::json!({
                    "type": "user",
                    "message": {"content": [{
                        "type": "tool_result", "tool_use_id": "toolu_leak",
                        "content": format!("{token}\n"), "is_error": false
                    }]}
                })))
                .await
                .unwrap();
            for line in lines {
                sender.send(line).await.unwrap();
            }
            Ok(TaskOutcome::Succeeded)
        })
    });
    let pipeline = pipeline(
        &world,
        runner,
        host(&seen, world.organization_id, world.installation_id),
    );

    pipeline.execute(world.run_id, None).await.unwrap();

    assert_eq!(run_row(&world.pool, world.run_id).await.0, "succeeded");
    assert_eq!(*seen.starts.lock().unwrap(), 1);
    let pull_requests = seen.pull_requests.lock().unwrap().clone();
    assert_eq!(pull_requests.len(), 1);
    let pull_request = &pull_requests[0];
    assert_eq!(pull_request.base, "main");
    assert_eq!(pull_request.head, branch_of(world.run_id));
    assert_eq!(pull_request.title, "Ajouter un CHANGELOG");
    assert!(pull_request.body.contains("I added CHANGELOG.md"));
    for internal in [
        "console.test",
        "claude-sonnet-5-5",
        "22174",
        "Coût",
        "Modèle",
    ] {
        assert!(!pull_request.body.contains(internal), "{internal}");
    }
    let posted = comments(&world.pool, world.card.id).await;
    assert_eq!(posted.len(), 1);
    assert_eq!(posted[0].0, "user-1");
    assert_eq!(posted[0].1.as_deref(), Some("matos-agent-executor"));
    assert!(posted[0].2.contains(PR_URL));
    assert!(posted[0].2.contains("I added CHANGELOG.md"));
    let audited: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM authorization_decisions WHERE run_id = $1 AND action = 'card.comment' AND actor = 'matos-agent-executor' AND allowed",
    )
    .bind(world.run_id.0)
    .fetch_one(&world.pool)
    .await
    .unwrap();
    assert_eq!(audited, 1);

    let (status, kind, cost, input_tokens, output) = step(&world.pool, world.run_id).await;
    assert_eq!((status.as_str(), kind.as_str()), ("succeeded", "execute"));
    assert_eq!(input_tokens, 22_174);
    assert_eq!(cost, 9_467, "cache reads at 10 %, cache writes at 125 %");
    assert_eq!(output["pull_request"], PR_URL);
    assert_eq!(output["comment"], "posted");
    assert!(output["comment_id"].is_string());
    assert_eq!(output["job_started"], true);
    let model: String = sqlx::query_scalar("SELECT model FROM task_grants WHERE run_id = $1")
        .bind(world.run_id.0)
        .fetch_one(&world.pool)
        .await
        .unwrap();
    assert_eq!(model, "claude-sonnet-5-5");
    assert_eq!(output["tool_calls"].as_array().unwrap().len(), 4);

    assert_closed(&world).await;
    let token = seen.token.lock().unwrap().clone().unwrap();
    let hashed: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM task_grants WHERE run_id = $1 AND token_sha256 = sha256($2::bytea)",
    )
    .bind(world.run_id.0)
    .bind(token.as_bytes())
    .fetch_one(&world.pool)
    .await
    .unwrap();
    assert_eq!(hashed, 1, "only the hash of the token is stored");
    let stored = format!(
        "{output} {} {} {}",
        pull_request.body, posted[0].2, pull_request.title
    );
    assert!(!stored.contains(&token), "the task token reached storage");
    assert!(output.to_string().contains("***"));
}

/// The sample's lines with a contribution text sent before its last marker.
fn lines_with_contribution(text: &str) -> Vec<TaskLine> {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let mut lines = lines_of(SAMPLE);
    let pushed = lines.pop().unwrap();
    for chunk in STANDARD.encode(text).as_bytes().chunks(12) {
        lines.push(TaskLine::Matos(TaskMarker::ContributionChunk {
            data: String::from_utf8(chunk.to_vec()).unwrap(),
            repository: None,
        }));
    }
    let hash = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, text.as_bytes());
    lines.push(TaskLine::Matos(TaskMarker::ContributionEnd {
        sha256: hash.as_ref().iter().map(|b| format!("{b:02x}")).collect(),
        bytes: text.len() as u64,
        truncated: false,
        repository: None,
    }));
    lines.push(pushed);
    lines
}

fn recording_runner(lines: Vec<TaskLine>, specs: Arc<Mutex<Vec<TaskSpec>>>) -> MockTaskRunner {
    let mut runner = MockTaskRunner::new();
    runner.expect_start().returning(move |spec| {
        specs.lock().unwrap().push(spec.clone());
        Box::pin(std::future::ready(Ok(())))
    });
    runner.expect_follow().returning(move |_, sender| {
        let lines = lines.clone();
        Box::pin(async move {
            for line in lines {
                sender.send(line).await.unwrap();
            }
            Ok(TaskOutcome::Succeeded)
        })
    });
    runner
}

#[tokio::test]
async fn the_run_writes_the_pull_request_and_the_member_who_started_it_authors_the_commit() {
    let Some(world) = world().await else { return };
    world
        .usecase
        .record_member(Member {
            organization_id: world.organization_id,
            subject: "user-1".into(),
            name: "Ada Lovelace".into(),
            email: Some("ada@example.test".into()),
            last_seen_at: Utc::now(),
        })
        .await
        .unwrap();
    let seen = Seen::default();
    let specs = Arc::new(Mutex::new(Vec::new()));
    let runner = recording_runner(
        lines_with_contribution(
            "# docs: add a changelog\n\nStarts CHANGELOG.md with an Unreleased section.\n",
        ),
        specs.clone(),
    );
    let pipeline = pipeline(
        &world,
        runner,
        host(&seen, world.organization_id, world.installation_id),
    );

    pipeline.execute(world.run_id, None).await.unwrap();

    let spec = specs.lock().unwrap()[0].clone();
    assert_eq!(
        spec.commit.author,
        CommitAuthor {
            name: "Ada Lovelace".into(),
            email: "ada@example.test".into()
        }
    );
    assert_eq!(spec.commit.fallback_message, "Ajouter un CHANGELOG");
    let terms: Vec<&str> = spec.commit.leak_terms.lines().collect();
    for term in [
        "claude-sonnet-5-5",
        "matos",
        "console.test",
        world.run_id.0.to_string().as_str(),
        world.card.id.0.to_string().as_str(),
    ] {
        assert!(terms.contains(&term), "{term}");
    }
    assert!(spec.prompt.contains("/workspace/.matos/contribution.md"));
    let pull_request = seen.pull_requests.lock().unwrap()[0].clone();
    assert_eq!(pull_request.title, "docs: add a changelog");
    assert_eq!(
        pull_request.body,
        "Starts CHANGELOG.md with an Unreleased section.\n\n## Checks\n\nLint: not run, the toolchain is not available"
    );
    let (_, _, _, _, output) = step(&world.pool, world.run_id).await;
    assert_eq!(output["contribution"], "run");
    let posted = comments(&world.pool, world.card.id).await;
    assert!(!posted[0].2.contains("secours"));
}

#[tokio::test]
async fn a_contribution_naming_the_model_is_replaced_and_the_card_says_why() {
    let Some(world) = world().await else { return };
    let seen = Seen::default();
    let specs = Arc::new(Mutex::new(Vec::new()));
    let runner = recording_runner(
        lines_with_contribution(
            "docs: add a changelog\n\nWritten by claude-sonnet-5-5.\n\nCo-Authored-By: a bot\n",
        ),
        specs.clone(),
    );
    let pipeline = pipeline(
        &world,
        runner,
        host(&seen, world.organization_id, world.installation_id),
    );

    pipeline.execute(world.run_id, None).await.unwrap();

    let spec = specs.lock().unwrap()[0].clone();
    assert_eq!(
        spec.commit.author,
        CommitAuthor::app_bot("matos-test"),
        "no member is known for the initiator"
    );
    let pull_request = seen.pull_requests.lock().unwrap()[0].clone();
    assert_eq!(pull_request.title, "Ajouter un CHANGELOG");
    assert!(pull_request.body.starts_with("I added CHANGELOG.md"));
    let (_, _, _, _, output) = step(&world.pool, world.run_id).await;
    let source = output["contribution"].as_str().unwrap();
    assert!(source.starts_with("fallback: "), "{source}");
    let posted = comments(&world.pool, world.card.id).await;
    assert!(posted[0].2.contains("Titre et description de secours"));
    assert!(posted[0].2.contains("« claude-sonnet-5-5 »"));
    assert!(posted[0].2.contains("« co-authored-by: »"));
}

#[tokio::test]
async fn without_changes_no_pull_request_is_opened_and_the_card_says_so() {
    let Some(world) = world().await else { return };
    let seen = Seen::default();
    let mut lines = lines_of(SAMPLE);
    lines.pop();
    lines.push(TaskLine::Matos(TaskMarker::NoChanges));
    let runner = runner_ending(&seen, lines, TaskOutcome::Succeeded);
    let pipeline = pipeline(&world, runner, without_config());

    pipeline.execute(world.run_id, None).await.unwrap();

    assert_eq!(run_row(&world.pool, world.run_id).await.0, "succeeded");
    assert!(seen.pull_requests.lock().unwrap().is_empty());
    let posted = comments(&world.pool, world.card.id).await;
    assert!(posted[0].2.starts_with("Aucun changement"));
    assert!(posted[0].2.contains("I added CHANGELOG.md"));
    let (status, _, _, _, output) = step(&world.pool, world.run_id).await;
    assert_eq!(status, "succeeded");
    assert_eq!(output["no_changes"], true);
    assert_closed(&world).await;
}

#[tokio::test]
async fn a_task_past_its_deadline_fails_the_run_and_revokes_the_token() {
    let Some(world) = world().await else { return };
    let seen = Seen::default();
    let runner = deleting(runner_ending(
        &seen,
        Vec::new(),
        TaskOutcome::DeadlineExceeded,
    ));
    let pipeline = pipeline(&world, runner, without_config());

    pipeline.execute(world.run_id, None).await.unwrap();

    let (status, failure) = run_row(&world.pool, world.run_id).await;
    assert_eq!(status, "failed");
    assert_eq!(failure.as_deref(), Some("durée maximale dépassée"));
    assert_eq!(step(&world.pool, world.run_id).await.0, "failed");
    assert!(
        comments(&world.pool, world.card.id).await[0]
            .2
            .contains("durée maximale dépassée")
    );
    assert_closed(&world).await;
}

#[tokio::test]
async fn a_failed_task_fails_the_run_with_its_reason() {
    let Some(world) = world().await else { return };
    let seen = Seen::default();
    let runner = deleting(runner_ending(
        &seen,
        Vec::new(),
        TaskOutcome::Failed {
            reason: "OOMKilled".into(),
        },
    ));
    let pipeline = pipeline(&world, runner, without_config());

    pipeline.execute(world.run_id, None).await.unwrap();

    let (status, failure) = run_row(&world.pool, world.run_id).await;
    assert_eq!(status, "failed");
    assert_eq!(failure.as_deref(), Some("OOMKilled"));
    assert_closed(&world).await;
}

#[tokio::test]
async fn a_runner_that_keeps_failing_is_retried_then_fails_the_run_and_revokes() {
    let Some(world) = world().await else { return };
    let seen = Seen::default();
    let mut runner = MockTaskRunner::new();
    let starts = seen.starts.clone();
    runner.expect_start().times(2).returning(move |_| {
        *starts.lock().unwrap() += 1;
        Box::pin(std::future::ready(Err(CoreError::Internal(
            "the cluster is unreachable".into(),
        ))))
    });
    let pipeline = pipeline(&world, runner, without_config());

    pipeline.execute(world.run_id, None).await.unwrap();

    let (status, failure) = run_row(&world.pool, world.run_id).await;
    assert_eq!(status, "failed");
    assert_eq!(failure.as_deref(), Some("the cluster is unreachable"));
    assert_eq!(*seen.starts.lock().unwrap(), 2);
    assert_closed(&world).await;
}

#[tokio::test]
async fn a_transient_runner_error_is_retried_and_the_run_succeeds() {
    let Some(world) = world().await else { return };
    let mut lines = lines_of(SAMPLE);
    lines.pop();
    lines.push(TaskLine::Matos(TaskMarker::NoChanges));
    let mut runner = MockTaskRunner::new();
    let calls = Arc::new(Mutex::new(0u32));
    runner.expect_start().times(2).returning(move |_| {
        let mut calls = calls.lock().unwrap();
        *calls += 1;
        let result = if *calls == 1 {
            Err(CoreError::Internal("try again".into()))
        } else {
            Ok(())
        };
        Box::pin(std::future::ready(result))
    });
    runner.expect_follow().returning(move |_, sender| {
        let lines = lines.clone();
        Box::pin(async move {
            for line in lines {
                sender.send(line).await.unwrap();
            }
            Ok(TaskOutcome::Succeeded)
        })
    });
    let pipeline = pipeline(&world, runner, without_config());

    pipeline.execute(world.run_id, None).await.unwrap();

    assert_eq!(run_row(&world.pool, world.run_id).await.0, "succeeded");
    assert_closed(&world).await;
}

fn no_changes_lines() -> Vec<TaskLine> {
    let mut lines = lines_of(SAMPLE);
    lines.pop();
    lines.push(TaskLine::Matos(TaskMarker::NoChanges));
    lines
}

fn following(lines: Vec<TaskLine>, runner: &mut MockTaskRunner) {
    runner.expect_follow().times(1).returning(move |_, sender| {
        let lines = lines.clone();
        Box::pin(async move {
            for line in lines {
                sender.send(line).await.unwrap();
            }
            Ok(TaskOutcome::Succeeded)
        })
    });
}

async fn crashed_attempt(world: &World, token: &str, output: Option<serde_json::Value>) {
    let previous = world
        .usecase
        .claim_execution(world.run_id, Utc::now() - chrono::Duration::hours(1), None)
        .await
        .unwrap()
        .unwrap();
    assert!(!previous.resumed);
    let expires_at = Utc::now() + chrono::Duration::minutes(30);
    world
        .usecase
        .issue_task_token(
            world.run_id,
            DelegatedToken {
                token: SecretToken::new(token),
                expires_at,
            },
            TaskGrant {
                run_id: world.run_id,
                organization_id: world.organization_id,
                repository: "acme/web".parse().unwrap(),
                branch: branch_of(world.run_id),
                model: "claude-sonnet-5-5".into(),
                mode: ExecutionMode::Change,
                companions: Vec::new(),
                writable_repositories: Vec::new(),
                initiated_by: None,
                git_identity: GitIdentity::App,
                expires_at,
                revoked_at: None,
            },
        )
        .await
        .unwrap();
    if let Some(output) = output {
        let step = Step {
            id: matos_common::StepId::new(),
            run_id: world.run_id,
            organization_id: world.organization_id,
            seq: 0,
            kind: StepKind::Execute,
            agent: Some("matos-agent-executor".into()),
            status: StepStatus::Running,
            attempt: 1,
            output: Some(output),
            usage: Default::default(),
            cost_micros: 0,
            latency_ms: None,
            error: None,
            started_at: Utc::now(),
            finished_at: None,
        };
        world.usecase.record_step(previous.run, step).await.unwrap();
    }
    stale_heartbeat(world).await;
}

async fn stale_heartbeat(world: &World) {
    sqlx::query("UPDATE runs SET heartbeat_at = now() - interval '2 hours' WHERE id = $1")
        .bind(world.run_id.0)
        .execute(&world.pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_resumed_run_attaches_to_its_job_with_the_same_token() {
    let Some(world) = world().await else { return };
    let seen = Seen::default();
    let first_token = format!("first-attempt-{}", world.run_id);
    crashed_attempt(&world, &first_token, None).await;
    let mut runner = MockTaskRunner::new();
    runner
        .expect_attach()
        .times(1)
        .returning(|_| Box::pin(std::future::ready(Ok(true))));
    following(no_changes_lines(), &mut runner);
    let pipeline = pipeline(&world, runner, without_config());

    pipeline.execute(world.run_id, None).await.unwrap();

    assert_eq!(*seen.starts.lock().unwrap(), 0, "no second Job");
    assert_eq!(run_row(&world.pool, world.run_id).await.0, "succeeded");
    let (total, revoked) = grants(&world.pool, world.run_id).await;
    assert_eq!((total, revoked), (1, 1), "no second grant was issued");
    assert_closed(&world).await;
}

#[tokio::test]
async fn a_resume_without_a_job_fails_and_starts_nothing() {
    let Some(world) = world().await else { return };
    crashed_attempt(&world, &format!("first-{}", world.run_id), None).await;
    let mut runner = MockTaskRunner::new();
    runner
        .expect_attach()
        .times(1)
        .returning(|_| Box::pin(std::future::ready(Ok(false))));
    let pipeline = pipeline(&world, runner, without_config());

    pipeline.execute(world.run_id, None).await.unwrap();

    let (status, failure) = run_row(&world.pool, world.run_id).await;
    assert_eq!(status, "failed");
    assert_eq!(failure.as_deref(), Some("tâche introuvable à la reprise"));
    assert_closed(&world).await;
}

#[tokio::test]
async fn a_run_that_crashed_before_its_job_started_may_start_it() {
    let Some(world) = world().await else { return };
    let seen = Seen::default();
    crashed_attempt(
        &world,
        &format!("first-{}", world.run_id),
        Some(serde_json::json!({"job_started": false})),
    )
    .await;
    let mut runner = runner_ending(&seen, no_changes_lines(), TaskOutcome::Succeeded);
    runner
        .expect_attach()
        .times(1)
        .returning(|_| Box::pin(std::future::ready(Ok(false))));
    let pipeline = pipeline(&world, runner, without_config());

    pipeline.execute(world.run_id, None).await.unwrap();

    assert_eq!(*seen.starts.lock().unwrap(), 1);
    assert_eq!(run_row(&world.pool, world.run_id).await.0, "succeeded");
    assert_closed(&world).await;
}

#[tokio::test]
async fn a_pull_request_opened_before_a_crash_is_not_opened_again_after_two_resumes() {
    let Some(world) = world().await else { return };
    let seen = Seen::default();
    let first_token = format!("first-{}", world.run_id);
    crashed_attempt(
        &world,
        &first_token,
        Some(serde_json::json!({
            "job_started": true,
            "branch": branch_of(world.run_id),
            "sha": PUSHED_SHA,
            "pull_request": PR_URL,
        })),
    )
    .await;

    let mut hanging = MockTaskRunner::new();
    hanging
        .expect_attach()
        .returning(|_| Box::pin(std::future::ready(Ok(true))));
    hanging
        .expect_follow()
        .returning(|_, _| Box::pin(std::future::pending()));
    let interrupted = pipeline(&world, hanging, without_config());
    let crash = tokio::time::timeout(
        Duration::from_millis(600),
        interrupted.execute(world.run_id, None),
    )
    .await;
    assert!(crash.is_err(), "the first resume never ended");
    let carried: serde_json::Value =
        sqlx::query_scalar("SELECT output FROM run_steps WHERE run_id = $1")
            .bind(world.run_id.0)
            .fetch_one(&world.pool)
            .await
            .unwrap();
    assert_eq!(carried["pull_request"], PR_URL);
    assert_eq!(carried["sha"], PUSHED_SHA);
    stale_heartbeat(&world).await;

    let mut runner = MockTaskRunner::new();
    runner
        .expect_attach()
        .times(1)
        .returning(|_| Box::pin(std::future::ready(Ok(true))));
    following(lines_of(SAMPLE), &mut runner);
    let pipeline = pipeline(&world, runner, without_config());
    pipeline.execute(world.run_id, None).await.unwrap();

    assert_eq!(run_row(&world.pool, world.run_id).await.0, "succeeded");
    assert!(seen.pull_requests.lock().unwrap().is_empty());
    let (_, _, _, _, output) = step(&world.pool, world.run_id).await;
    assert_eq!(output["pull_request"], PR_URL);
    let posted = comments(&world.pool, world.card.id).await;
    assert_eq!(posted.len(), 1);
    assert!(posted[0].2.contains(PR_URL));
}

#[tokio::test]
async fn a_comment_posted_before_a_crash_is_not_posted_again() {
    let Some(world) = world().await else { return };
    crashed_attempt(
        &world,
        &format!("first-{}", world.run_id),
        Some(serde_json::json!({"job_started": true})),
    )
    .await;
    let already = world
        .usecase
        .comment_card_as_executor(world.run_id, world.card.id, None, "déjà dit".into())
        .await
        .unwrap();
    let mut runner = MockTaskRunner::new();
    runner
        .expect_attach()
        .returning(|_| Box::pin(std::future::ready(Ok(true))));
    following(no_changes_lines(), &mut runner);
    let pipeline = pipeline(&world, runner, without_config());

    pipeline.execute(world.run_id, None).await.unwrap();

    let posted = comments(&world.pool, world.card.id).await;
    assert_eq!(posted.len(), 1);
    assert_eq!(posted[0].2, "déjà dit");
    let (_, _, _, _, output) = step(&world.pool, world.run_id).await;
    assert_eq!(output["comment_id"], already.0.to_string());
}

#[tokio::test]
async fn a_run_alive_on_another_worker_is_left_alone() {
    let Some(world) = world().await else { return };
    world
        .usecase
        .claim_execution(world.run_id, Utc::now() - chrono::Duration::hours(1), None)
        .await
        .unwrap()
        .unwrap();
    let pipeline = pipeline(&world, MockTaskRunner::new(), without_config());

    pipeline.execute(world.run_id, None).await.unwrap();

    assert_eq!(run_row(&world.pool, world.run_id).await.0, "running");
}

#[tokio::test]
async fn text_the_agent_wrote_cannot_mention_anyone() {
    let Some(world) = world().await else { return };
    let seen = Seen::default();
    let mut lines = lines_of(SAMPLE);
    for line in &mut lines {
        if let TaskLine::Agent(value) = line
            && value["type"] == "result"
        {
            value["result"] = serde_json::json!("Merci @octocat, cc @acme/security");
        }
    }
    let runner = runner_ending(&seen, lines, TaskOutcome::Succeeded);
    let pipeline = pipeline(
        &world,
        runner,
        host(&seen, world.organization_id, world.installation_id),
    );

    pipeline.execute(world.run_id, None).await.unwrap();

    let pull_requests = seen.pull_requests.lock().unwrap().clone();
    let posted = comments(&world.pool, world.card.id).await;
    for text in [&pull_requests[0].body, &posted[0].2] {
        assert!(text.contains("@\u{200B}octocat"), "{text}");
        assert!(
            !text.contains("@octocat") && !text.contains("@acme"),
            "{text}"
        );
    }
}

#[tokio::test]
async fn a_branch_that_is_not_where_the_task_says_fails_the_run_before_any_pull_request() {
    let Some(world) = world().await else { return };
    let seen = Seen::default();
    let runner = deleting(runner_ending(
        &seen,
        lines_of(SAMPLE),
        TaskOutcome::Succeeded,
    ));
    let mut host = config_file(MockCodeHost::new(), None);
    host.expect_branch_head().returning(|_, _, _, _| {
        Box::pin(std::future::ready(Ok(Some(
            "0000000000000000000000000000000000000000".to_owned(),
        ))))
    });
    let pipeline = pipeline(&world, runner, host);

    pipeline.execute(world.run_id, None).await.unwrap();

    let (status, failure) = run_row(&world.pool, world.run_id).await;
    assert_eq!(status, "failed");
    assert_eq!(failure.as_deref(), Some("branche absente ou différente"));
    assert!(seen.pull_requests.lock().unwrap().is_empty());
    assert_closed(&world).await;
}

#[tokio::test]
async fn a_missing_branch_fails_the_run_too() {
    let Some(world) = world().await else { return };
    let seen = Seen::default();
    let runner = deleting(runner_ending(
        &seen,
        lines_of(SAMPLE),
        TaskOutcome::Succeeded,
    ));
    let mut host = config_file(MockCodeHost::new(), None);
    host.expect_branch_head()
        .returning(|_, _, _, _| Box::pin(std::future::ready(Ok(None))));
    let pipeline = pipeline(&world, runner, host);

    pipeline.execute(world.run_id, None).await.unwrap();

    let (_, failure) = run_row(&world.pool, world.run_id).await;
    assert_eq!(failure.as_deref(), Some("branche absente ou différente"));
}

#[tokio::test]
async fn an_agent_that_ran_out_of_turns_without_pushing_fails_the_run() {
    let Some(world) = world().await else { return };
    let seen = Seen::default();
    let mut lines = no_changes_lines();
    for line in &mut lines {
        if let TaskLine::Agent(value) = line
            && value["type"] == "result"
        {
            value["subtype"] = serde_json::json!("error_max_turns");
        }
    }
    let runner = deleting(runner_ending(&seen, lines, TaskOutcome::Succeeded));
    let pipeline = pipeline(&world, runner, without_config());

    pipeline.execute(world.run_id, None).await.unwrap();

    let (status, failure) = run_row(&world.pool, world.run_id).await;
    assert_eq!(status, "failed");
    assert_eq!(
        failure.as_deref(),
        Some("arrêté : nombre maximal de tours atteint")
    );
    let posted = comments(&world.pool, world.card.id).await;
    assert!(
        posted[0]
            .2
            .contains("arrêté : nombre maximal de tours atteint")
    );
    assert!(!posted[0].2.starts_with("Aucun changement"));
}

#[tokio::test]
async fn a_model_without_a_known_price_costs_what_claude_code_reported() {
    let Some(world) = world().await else { return };
    let seen = Seen::default();
    let runner = runner_ending(&seen, no_changes_lines(), TaskOutcome::Succeeded);
    let mut unpriced = config();
    unpriced.prices.clear();
    let pipeline = ExecutePipeline::new(
        world.usecase.clone(),
        Executor::Ready {
            runner,
            host: without_config(),
        },
        unpriced,
    );

    pipeline.execute(world.run_id, None).await.unwrap();

    let (_, _, cost, _, _) = step(&world.pool, world.run_id).await;
    assert_eq!(cost, 29_100);
}

#[tokio::test]
async fn a_disabled_executor_fails_the_run_at_once_with_a_clear_reason() {
    let Some(world) = world().await else { return };
    let pipeline: Pipeline = ExecutePipeline::new(
        world.usecase.clone(),
        Executor::Disabled {
            reason: "les exécutions sont désactivées (EXECUTOR_NAMESPACE)".into(),
        },
        config(),
    );

    pipeline.execute(world.run_id, None).await.unwrap();

    let (status, failure) = run_row(&world.pool, world.run_id).await;
    assert_eq!(status, "failed");
    assert_eq!(
        failure.as_deref(),
        Some("les exécutions sont désactivées (EXECUTOR_NAMESPACE)")
    );
    assert_eq!(delegations(&world.pool, world.run_id).await, 0);
}

#[tokio::test]
async fn a_disconnected_github_fails_the_run_before_any_job_starts() {
    let Some(world) = world().await else { return };
    world
        .usecase
        .disconnect_github(Subject {
            is_admin: true,
            ..human(world.organization_id)
        })
        .await
        .unwrap();
    let pipeline = pipeline(&world, MockTaskRunner::new(), without_config());

    pipeline.execute(world.run_id, None).await.unwrap();

    let (status, failure) = run_row(&world.pool, world.run_id).await;
    assert_eq!(status, "failed");
    assert!(failure.unwrap().contains("GitHub"));
    assert_closed(&world).await;
}

#[tokio::test]
async fn an_organization_without_a_github_app_fails_the_run_before_any_job_starts() {
    let Some(world) = world().await else { return };
    world
        .usecase
        .delete_github_app(admin(world.organization_id))
        .await
        .unwrap();
    let pipeline = pipeline(&world, MockTaskRunner::new(), without_config());

    pipeline.execute(world.run_id, None).await.unwrap();

    let (status, failure) = run_row(&world.pool, world.run_id).await;
    assert_eq!(status, "failed");
    assert_eq!(failure.as_deref(), Some("application GitHub absente"));
    assert_closed(&world).await;
}

#[tokio::test]
async fn the_timeline_is_saved_while_the_task_runs() {
    let Some(world) = world().await else { return };
    let lines = no_changes_lines();
    let live = Arc::new(Mutex::new(None));
    let mut runner = MockTaskRunner::new();
    runner
        .expect_start()
        .returning(|_| Box::pin(std::future::ready(Ok(()))));
    let (pool, run_id, observed) = (world.pool.clone(), world.run_id, live.clone());
    runner.expect_follow().returning(move |_, sender| {
        let lines = lines.clone();
        let (pool, observed) = (pool.clone(), observed.clone());
        Box::pin(async move {
            for line in lines {
                sender.send(line).await.unwrap();
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
            let output: Option<serde_json::Value> =
                sqlx::query_scalar("SELECT output FROM run_steps WHERE run_id = $1")
                    .bind(run_id.0)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            let status: String =
                sqlx::query_scalar("SELECT status FROM run_steps WHERE run_id = $1")
                    .bind(run_id.0)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            *observed.lock().unwrap() = Some((status, output));
            Ok(TaskOutcome::Succeeded)
        })
    });
    let pipeline = pipeline(&world, runner, without_config());

    pipeline.execute(world.run_id, None).await.unwrap();

    let (status, output) = live.lock().unwrap().clone().unwrap();
    assert_eq!(status, "running");
    assert_eq!(output.unwrap()["tool_calls"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn the_meeting_pipeline_leaves_card_runs_to_the_executor() {
    let Some(world) = world().await else { return };

    assert!(world.usecase.is_execution_run(world.run_id).await.unwrap());
    let claimed = world
        .usecase
        .claim_run(world.run_id, Utc::now(), None)
        .await
        .unwrap();

    assert!(claimed.is_none());
    assert_eq!(run_row(&world.pool, world.run_id).await.0, "queued");
    assert!(
        !world
            .usecase
            .is_execution_run(RunId(Uuid::now_v7()))
            .await
            .unwrap()
    );
}

fn with_markers(markers: Vec<TaskMarker>) -> Vec<TaskLine> {
    let mut lines = lines_of(SAMPLE);
    let pushed = lines.pop().unwrap();
    lines.extend(markers.into_iter().map(TaskLine::Matos));
    lines.push(pushed);
    lines
}

#[tokio::test]
async fn the_rules_of_the_base_branch_config_reach_the_task() {
    let Some(world) = world().await else { return };
    let seen = Seen::default();
    let specs = Arc::new(Mutex::new(Vec::new()));
    let runner = recording_runner(lines_of(SAMPLE), specs.clone());
    let host = host_with_config(
        &seen,
        world.organization_id,
        world.installation_id,
        Some("[guard]\nallow_paths = [\".github/workflows/\"]\nmax_files = 80\n"),
    );
    let pipeline = pipeline(&world, runner, host);

    pipeline.execute(world.run_id, None).await.unwrap();

    let guard = specs.lock().unwrap()[0].push_guard.clone();
    assert_eq!(guard.allow_paths, vec![".github/workflows/"]);
    assert_eq!((guard.max_files, guard.max_lines), (80, 2000));
    assert!(guard.deny_paths.contains(&".matos.toml".to_owned()));
    let posted = comments(&world.pool, world.card.id).await;
    assert!(!posted[0].2.contains("invalide"));
}

#[tokio::test]
async fn a_card_run_without_an_agent_gets_no_companion_and_the_card_lists_them_as_skipped() {
    let Some(world) = world().await else { return };
    let seen = Seen::default();
    let specs = Arc::new(Mutex::new(Vec::new()));
    let runner = recording_runner(lines_of(SAMPLE), specs.clone());
    let host = host_with_config(
        &seen,
        world.organization_id,
        world.installation_id,
        Some(
            "[[companions]]\nrepository = \"acme/sdk\"\npath = \"../sdk\"\n\n[[companions]]\nrepository = \"acme/docs\"\npath = \"/etc\"\nref = \"main\"\nref_file = \"x\"\n",
        ),
    );
    let pipeline = pipeline(&world, runner, host);

    pipeline.execute(world.run_id, None).await.unwrap();

    assert!(specs.lock().unwrap()[0].companions.is_empty());
    let granted: Vec<String> =
        sqlx::query_scalar("SELECT companions FROM task_grants WHERE run_id = $1")
            .bind(world.run_id.0)
            .fetch_one(&world.pool)
            .await
            .unwrap();
    assert!(granted.is_empty());
    let posted = comments(&world.pool, world.card.id).await;
    assert!(
        posted[0]
            .2
            .contains("acme/sdk (dépôt non autorisé pour l'agent)")
    );
    assert!(posted[0].2.contains("acme/docs (entrée invalide)"));
    assert!(!posted[0].2.contains("invalide :"));
}

#[tokio::test]
async fn a_missing_config_means_the_defaults_without_a_word() {
    let Some(world) = world().await else { return };
    let seen = Seen::default();
    let specs = Arc::new(Mutex::new(Vec::new()));
    let runner = recording_runner(lines_of(SAMPLE), specs.clone());
    let pipeline = pipeline(
        &world,
        runner,
        host(&seen, world.organization_id, world.installation_id),
    );

    pipeline.execute(world.run_id, None).await.unwrap();

    assert_eq!(
        specs.lock().unwrap()[0].push_guard,
        matos_core::domain::execution::PushGuard::default()
    );
    let posted = comments(&world.pool, world.card.id).await;
    assert!(!posted[0].2.contains("invalide"));
}

#[tokio::test]
async fn an_invalid_config_means_the_defaults_and_the_card_says_so() {
    let Some(world) = world().await else { return };
    let seen = Seen::default();
    let specs = Arc::new(Mutex::new(Vec::new()));
    let runner = recording_runner(lines_of(SAMPLE), specs.clone());
    let host = host_with_config(
        &seen,
        world.organization_id,
        world.installation_id,
        Some("[guard]\nallow_path = [\"deploy/\"]\n"),
    );
    let pipeline = pipeline(&world, runner, host);

    pipeline.execute(world.run_id, None).await.unwrap();

    assert_eq!(
        specs.lock().unwrap()[0].push_guard,
        matos_core::domain::execution::PushGuard::default()
    );
    let posted = comments(&world.pool, world.card.id).await;
    assert!(posted[0].2.contains(".matos.toml"));
    assert!(posted[0].2.contains("invalide"));
}

#[tokio::test]
async fn a_blocked_run_fails_with_the_reasons_and_opens_no_pull_request() {
    let Some(world) = world().await else { return };
    let seen = Seen::default();
    let mut lines = lines_of(SAMPLE);
    lines.pop();
    lines.push(TaskLine::Matos(TaskMarker::GuardBlocked {
        reasons: vec![
            "chemin sensible : .github/workflows/ci.yml".into(),
            "secret possible (cle privee) dans src/key.txt".into(),
        ],
    }));
    let runner = deleting(runner_ending(
        &seen,
        lines,
        TaskOutcome::Failed {
            reason: "container exited with 3".into(),
        },
    ));
    let pipeline = pipeline(
        &world,
        runner,
        host(&seen, world.organization_id, world.installation_id),
    );

    pipeline.execute(world.run_id, None).await.unwrap();

    let (status, failure) = run_row(&world.pool, world.run_id).await;
    assert_eq!(status, "failed");
    let failure = failure.unwrap();
    assert!(failure.contains("chemin sensible : .github/workflows/ci.yml"));
    assert!(failure.contains("secret possible (cle privee) dans src/key.txt"));
    assert!(!failure.contains("container exited"));
    assert!(seen.pull_requests.lock().unwrap().is_empty());
    let posted = comments(&world.pool, world.card.id).await;
    assert!(
        posted[0]
            .2
            .contains("chemin sensible : .github/workflows/ci.yml")
    );
    assert_closed(&world).await;
}

#[tokio::test]
async fn the_pull_request_states_the_lint_result() {
    let Some(world) = world().await else { return };
    let seen = Seen::default();
    let runner = recording_runner(
        with_markers(vec![TaskMarker::Quality {
            lint: matos_core::domain::execution::QualityStatus::Failed,
            excerpt: Some("error: unused variable `x`".into()),
            repository: None,
        }]),
        Arc::new(Mutex::new(Vec::new())),
    );
    let pipeline = pipeline(
        &world,
        runner,
        host(&seen, world.organization_id, world.installation_id),
    );

    pipeline.execute(world.run_id, None).await.unwrap();

    let body = seen.pull_requests.lock().unwrap()[0].body.clone();
    assert!(
        body.ends_with("## Checks\n\nLint: failed\n\n```\nerror: unused variable `x`\n```"),
        "{body}"
    );
}

#[tokio::test]
async fn without_a_quality_marker_the_pull_request_says_the_lint_did_not_run() {
    let Some(world) = world().await else { return };
    let seen = Seen::default();
    let runner = recording_runner(lines_of(SAMPLE), Arc::new(Mutex::new(Vec::new())));
    let pipeline = pipeline(
        &world,
        runner,
        host(&seen, world.organization_id, world.installation_id),
    );

    pipeline.execute(world.run_id, None).await.unwrap();

    let body = seen.pull_requests.lock().unwrap()[0].body.clone();
    assert!(
        body.ends_with("Lint: not run, the toolchain is not available"),
        "{body}"
    );
    assert!(!body.contains("claude-sonnet"));
}

#[tokio::test]
async fn a_lint_excerpt_naming_the_model_is_dropped() {
    let Some(world) = world().await else { return };
    let seen = Seen::default();
    let runner = recording_runner(
        with_markers(vec![TaskMarker::Quality {
            lint: matos_core::domain::execution::QualityStatus::Failed,
            excerpt: Some("warning from claude-sonnet-5-5".into()),
            repository: None,
        }]),
        Arc::new(Mutex::new(Vec::new())),
    );
    let pipeline = pipeline(
        &world,
        runner,
        host(&seen, world.organization_id, world.installation_id),
    );

    pipeline.execute(world.run_id, None).await.unwrap();

    let body = seen.pull_requests.lock().unwrap()[0].body.clone();
    assert!(body.ends_with("Lint: failed"), "{body}");
    assert!(!body.contains("claude-sonnet"));
}

const MEMBER_ACCESS: &str = "ghu_member_access_secret";
const MEMBER_REFRESH: &str = "ghr_member_refresh_secret";
const APP_NOTE: &str = "Pull request ouverte par l'application GitHub : connecte ton compte GitHub dans Matos (Paramètres › Intégrations › GitHub) pour qu'elle soit ouverte à ton nom.";

fn user_tokens(access: &str, expires_in: chrono::Duration, refresh: &str) -> UserTokens {
    UserTokens {
        access_token: SecretToken::new(access),
        expires_at: Some(Utc::now() + expires_in),
        refresh_token: Some(SecretToken::new(refresh)),
        refresh_expires_at: Some(Utc::now() + chrono::Duration::days(180)),
    }
}

async fn record_ada(world: &World) {
    world
        .usecase
        .record_member(Member {
            organization_id: world.organization_id,
            subject: "user-1".into(),
            name: "Ada Lovelace".into(),
            email: Some("ada@example.test".into()),
            last_seen_at: Utc::now(),
        })
        .await
        .unwrap();
}

async fn connect_ada(world: &World, tokens: UserTokens) {
    record_ada(world).await;
    let mut oauth = MockGithubUserOAuth::new();
    oauth
        .expect_exchange_code()
        .returning(move |_, _| Box::pin(std::future::ready(Ok(tokens.clone()))));
    oauth.expect_user().returning(|_, _| {
        Box::pin(std::future::ready(Ok(GithubUser {
            id: 583231,
            login: "ada".into(),
        })))
    });
    world
        .usecase
        .connect_github_user(human(world.organization_id), "code", &oauth)
        .await
        .unwrap();
}

#[derive(Clone, Default)]
struct MemberSeen {
    pull_requests: Arc<Mutex<Vec<(String, DraftPullRequest)>>>,
    refreshed_with: Arc<Mutex<Vec<String>>>,
}

struct MemberHost {
    can_push: bool,
    pull_request: Result<(), &'static str>,
    refresh: Option<UserTokens>,
}

impl Default for MemberHost {
    fn default() -> Self {
        Self {
            can_push: true,
            pull_request: Ok(()),
            refresh: None,
        }
    }
}

fn member_host(
    seen: &Seen,
    member_seen: &MemberSeen,
    world: &World,
    behaviour: MemberHost,
) -> MockCodeHost {
    let mut host = host(seen, world.organization_id, world.installation_id);
    let can_push = behaviour.can_push;
    host.expect_member_can_push()
        .returning(move |_, _, repository| {
            assert_eq!(repository.to_string(), "acme/web");
            Box::pin(std::future::ready(Ok(can_push)))
        });
    let recorded = member_seen.pull_requests.clone();
    let outcome = behaviour.pull_request;
    host.expect_open_draft_pr_as_member()
        .returning(move |_, token, pull_request| {
            recorded
                .lock()
                .unwrap()
                .push((token.expose().to_owned(), pull_request));
            Box::pin(std::future::ready(match outcome {
                Ok(()) => Ok(PullRequestUrl(PR_URL.into())),
                Err(message) => Err(CoreError::Invalid(message.into())),
            }))
        });
    let refreshed_with = member_seen.refreshed_with.clone();
    let fresh = behaviour.refresh;
    host.expect_refresh_user_token()
        .returning(move |_, refresh_token| {
            refreshed_with
                .lock()
                .unwrap()
                .push(refresh_token.expose().to_owned());
            Box::pin(std::future::ready(
                fresh
                    .clone()
                    .ok_or_else(|| CoreError::Invalid("bad_refresh_token".into())),
            ))
        });
    host
}

async fn git_identity_of(world: &World) -> String {
    sqlx::query_scalar("SELECT git_identity FROM task_grants WHERE run_id = $1")
        .bind(world.run_id.0)
        .fetch_one(&world.pool)
        .await
        .unwrap()
}

async fn run_with_host(world: &World, host: MockCodeHost, seen: &Seen) {
    let runner = runner_ending(seen, lines_of(SAMPLE), TaskOutcome::Succeeded);
    pipeline(world, runner, host)
        .execute(world.run_id, None)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_connected_member_has_the_pull_request_opened_with_their_own_token() {
    let Some(world) = world().await else { return };
    connect_ada(
        &world,
        user_tokens(MEMBER_ACCESS, chrono::Duration::hours(8), MEMBER_REFRESH),
    )
    .await;
    let (seen, member_seen) = (Seen::default(), MemberSeen::default());
    let host = member_host(&seen, &member_seen, &world, MemberHost::default());

    run_with_host(&world, host, &seen).await;

    assert_eq!(run_row(&world.pool, world.run_id).await.0, "succeeded");
    assert_eq!(git_identity_of(&world).await, "member");
    let opened = member_seen.pull_requests.lock().unwrap().clone();
    assert_eq!(opened.len(), 1);
    assert_eq!(opened[0].0, MEMBER_ACCESS);
    assert_eq!(opened[0].1.head, branch_of(world.run_id));
    assert!(seen.pull_requests.lock().unwrap().is_empty());
    assert!(member_seen.refreshed_with.lock().unwrap().is_empty());
    let posted = comments(&world.pool, world.card.id).await;
    assert!(posted[0].2.contains(PR_URL));
    assert!(!posted[0].2.contains(APP_NOTE));
    let (_, _, _, _, output) = step(&world.pool, world.run_id).await;
    for text in [posted[0].2.clone(), output.to_string()] {
        assert!(!text.contains(MEMBER_ACCESS) && !text.contains(MEMBER_REFRESH));
    }
    assert_closed(&world).await;
}

#[tokio::test]
async fn a_member_who_never_connected_gets_the_app_and_a_note_on_the_card() {
    let Some(world) = world().await else { return };
    record_ada(&world).await;
    let (seen, member_seen) = (Seen::default(), MemberSeen::default());
    let host = member_host(&seen, &member_seen, &world, MemberHost::default());

    run_with_host(&world, host, &seen).await;

    assert_eq!(run_row(&world.pool, world.run_id).await.0, "succeeded");
    assert_eq!(git_identity_of(&world).await, "app");
    assert_eq!(seen.pull_requests.lock().unwrap().len(), 1);
    assert!(member_seen.pull_requests.lock().unwrap().is_empty());
    let posted = comments(&world.pool, world.card.id).await;
    assert!(posted[0].2.contains(PR_URL));
    assert!(posted[0].2.ends_with(APP_NOTE), "{}", posted[0].2);
}

#[tokio::test]
async fn an_initiator_who_is_no_member_gets_the_app_without_a_note() {
    let Some(world) = world().await else { return };
    let seen = Seen::default();
    let host = host(&seen, world.organization_id, world.installation_id);

    run_with_host(&world, host, &seen).await;

    assert_eq!(run_row(&world.pool, world.run_id).await.0, "succeeded");
    assert_eq!(git_identity_of(&world).await, "app");
    assert_eq!(seen.pull_requests.lock().unwrap().len(), 1);
    let posted = comments(&world.pool, world.card.id).await;
    assert!(!posted[0].2.contains("connecte ton compte GitHub"));
}

#[tokio::test]
async fn an_expired_token_is_refreshed_when_the_run_is_prepared_and_the_new_pair_is_stored() {
    let Some(world) = world().await else { return };
    connect_ada(
        &world,
        user_tokens(MEMBER_ACCESS, chrono::Duration::minutes(-5), MEMBER_REFRESH),
    )
    .await;
    let (seen, member_seen) = (Seen::default(), MemberSeen::default());
    let host = member_host(
        &seen,
        &member_seen,
        &world,
        MemberHost {
            refresh: Some(user_tokens(
                "ghu_member_access_2",
                chrono::Duration::hours(8),
                "ghr_member_refresh_2",
            )),
            ..MemberHost::default()
        },
    );

    run_with_host(&world, host, &seen).await;

    assert_eq!(
        *member_seen.refreshed_with.lock().unwrap(),
        [MEMBER_REFRESH]
    );
    let opened = member_seen.pull_requests.lock().unwrap().clone();
    assert_eq!(opened[0].0, "ghu_member_access_2");
    assert_eq!(git_identity_of(&world).await, "member");
    let stored = world
        .usecase
        .member_git_access_token(world.organization_id, "user-1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.expose(), "ghu_member_access_2");
}

#[tokio::test]
async fn a_refused_refresh_gives_the_app_and_the_note_without_failing_the_run() {
    let Some(world) = world().await else { return };
    connect_ada(
        &world,
        user_tokens(MEMBER_ACCESS, chrono::Duration::minutes(-5), MEMBER_REFRESH),
    )
    .await;
    let (seen, member_seen) = (Seen::default(), MemberSeen::default());
    let host = member_host(&seen, &member_seen, &world, MemberHost::default());

    run_with_host(&world, host, &seen).await;

    assert_eq!(
        run_row(&world.pool, world.run_id).await,
        ("succeeded".into(), None)
    );
    assert_eq!(git_identity_of(&world).await, "app");
    assert_eq!(seen.pull_requests.lock().unwrap().len(), 1);
    assert!(member_seen.pull_requests.lock().unwrap().is_empty());
    let posted = comments(&world.pool, world.card.id).await;
    assert!(posted[0].2.ends_with(APP_NOTE), "{}", posted[0].2);
}

#[tokio::test]
async fn a_member_without_write_access_to_the_repository_gets_the_app_and_the_note() {
    let Some(world) = world().await else { return };
    connect_ada(
        &world,
        user_tokens(MEMBER_ACCESS, chrono::Duration::hours(8), MEMBER_REFRESH),
    )
    .await;
    let (seen, member_seen) = (Seen::default(), MemberSeen::default());
    let host = member_host(
        &seen,
        &member_seen,
        &world,
        MemberHost {
            can_push: false,
            ..MemberHost::default()
        },
    );

    run_with_host(&world, host, &seen).await;

    assert_eq!(git_identity_of(&world).await, "app");
    assert_eq!(seen.pull_requests.lock().unwrap().len(), 1);
    assert!(member_seen.pull_requests.lock().unwrap().is_empty());
    let posted = comments(&world.pool, world.card.id).await;
    assert!(posted[0].2.ends_with(APP_NOTE), "{}", posted[0].2);
}

#[tokio::test]
async fn a_pull_request_refused_with_the_members_token_is_opened_by_the_app_with_the_note() {
    let Some(world) = world().await else { return };
    connect_ada(
        &world,
        user_tokens(MEMBER_ACCESS, chrono::Duration::hours(8), MEMBER_REFRESH),
    )
    .await;
    let (seen, member_seen) = (Seen::default(), MemberSeen::default());
    let host = member_host(
        &seen,
        &member_seen,
        &world,
        MemberHost {
            pull_request: Err("Resource not accessible by personal access token"),
            ..MemberHost::default()
        },
    );

    run_with_host(&world, host, &seen).await;

    assert_eq!(
        run_row(&world.pool, world.run_id).await,
        ("succeeded".into(), None)
    );
    assert_eq!(git_identity_of(&world).await, "member");
    assert_eq!(member_seen.pull_requests.lock().unwrap().len(), 1);
    assert_eq!(seen.pull_requests.lock().unwrap().len(), 1);
    let posted = comments(&world.pool, world.card.id).await;
    assert!(posted[0].2.contains(PR_URL));
    assert!(posted[0].2.ends_with(APP_NOTE), "{}", posted[0].2);
    let (_, _, _, _, output) = step(&world.pool, world.run_id).await;
    assert_eq!(output["pull_request"], PR_URL);
}

#[tokio::test]
async fn a_pull_request_that_already_exists_is_not_retried_with_the_app() {
    let Some(world) = world().await else { return };
    connect_ada(
        &world,
        user_tokens(MEMBER_ACCESS, chrono::Duration::hours(8), MEMBER_REFRESH),
    )
    .await;
    let (seen, member_seen) = (Seen::default(), MemberSeen::default());
    let host = member_host(
        &seen,
        &member_seen,
        &world,
        MemberHost {
            pull_request: Err("Validation Failed: A pull request already exists for acme:x."),
            ..MemberHost::default()
        },
    );

    let runner = deleting(runner_ending(
        &seen,
        lines_of(SAMPLE),
        TaskOutcome::Succeeded,
    ));
    pipeline(&world, runner, host)
        .execute(world.run_id, None)
        .await
        .unwrap();

    assert_eq!(run_row(&world.pool, world.run_id).await.0, "failed");
    assert!(seen.pull_requests.lock().unwrap().is_empty());
}

const MEMBER_PAT: &str = "github_pat_member_personal_secret";

async fn save_ada_personal_token(world: &World, expires_at: Option<chrono::DateTime<Utc>>) {
    let mut validator = MockPersonalTokenValidator::new();
    validator.expect_validate().returning(move |_| {
        Box::pin(std::future::ready(Ok(PersonalTokenCheck {
            user: GithubUser {
                id: 583231,
                login: "ada".into(),
            },
            expires_at,
        })))
    });
    world
        .usecase
        .save_github_personal_token(
            human(world.organization_id),
            SecretToken::new(MEMBER_PAT),
            &validator,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn a_personal_token_opens_the_pull_request_even_when_the_apps_authorization_exists() {
    let Some(world) = world().await else { return };
    connect_ada(
        &world,
        user_tokens(MEMBER_ACCESS, chrono::Duration::hours(8), MEMBER_REFRESH),
    )
    .await;
    save_ada_personal_token(&world, None).await;
    let (seen, member_seen) = (Seen::default(), MemberSeen::default());
    let host = member_host(&seen, &member_seen, &world, MemberHost::default());

    run_with_host(&world, host, &seen).await;

    assert_eq!(run_row(&world.pool, world.run_id).await.0, "succeeded");
    assert_eq!(git_identity_of(&world).await, "member");
    let opened = member_seen.pull_requests.lock().unwrap().clone();
    assert_eq!(opened.len(), 1);
    assert_eq!(opened[0].0, MEMBER_PAT);
    assert!(seen.pull_requests.lock().unwrap().is_empty());
    assert!(member_seen.refreshed_with.lock().unwrap().is_empty());
    let posted = comments(&world.pool, world.card.id).await;
    assert!(!posted[0].2.contains(APP_NOTE));
    let (_, _, _, _, output) = step(&world.pool, world.run_id).await;
    for text in [posted[0].2.clone(), output.to_string()] {
        assert!(!text.contains(MEMBER_PAT));
    }
}

#[tokio::test]
async fn an_expired_personal_token_falls_back_to_the_apps_authorization() {
    let Some(world) = world().await else { return };
    connect_ada(
        &world,
        user_tokens(MEMBER_ACCESS, chrono::Duration::hours(8), MEMBER_REFRESH),
    )
    .await;
    save_ada_personal_token(&world, Some(Utc::now() - chrono::Duration::minutes(1))).await;
    let (seen, member_seen) = (Seen::default(), MemberSeen::default());
    let host = member_host(&seen, &member_seen, &world, MemberHost::default());

    run_with_host(&world, host, &seen).await;

    assert_eq!(git_identity_of(&world).await, "member");
    let opened = member_seen.pull_requests.lock().unwrap().clone();
    assert_eq!(opened[0].0, MEMBER_ACCESS);
}

async fn with_extra_repositories(world: &World, extras: &[&str]) {
    let extras: Vec<String> = extras.iter().map(|extra| (*extra).to_owned()).collect();
    sqlx::query("UPDATE runs SET extra_repositories = $2 WHERE id = $1")
        .bind(world.run_id.0)
        .bind(&extras)
        .execute(&world.pool)
        .await
        .unwrap();
}

const API_CONFIG: &str = "[guard]\nallow_paths = [\"deploy/\"]\nmax_files = 7\nmax_lines = 70\n";

#[derive(Clone, Default)]
struct ExtraReads {
    default_branches: Arc<Mutex<Vec<String>>>,
    files: Arc<Mutex<Vec<(String, String)>>>,
}

fn extras_host(
    seen: &Seen,
    reads: &ExtraReads,
    can_push: Option<fn(&str) -> bool>,
) -> MockCodeHost {
    let mut host = MockCodeHost::new();
    let files = reads.files.clone();
    host.expect_file_at()
        .returning(move |_, _, repository, git_ref, path| {
            assert_eq!(path, ".matos.toml");
            files
                .lock()
                .unwrap()
                .push((repository.to_string(), git_ref.to_owned()));
            let text = (repository.to_string() == "acme/api").then(|| API_CONFIG.to_owned());
            Box::pin(std::future::ready(Ok(text)))
        });
    let branches = reads.default_branches.clone();
    host.expect_default_branch()
        .returning(move |_, _, repository| {
            branches.lock().unwrap().push(repository.to_string());
            Box::pin(std::future::ready(Ok("develop".to_owned())))
        });
    host.expect_branch_head()
        .returning(|_, _, _, _| Box::pin(std::future::ready(Ok(Some(PUSHED_SHA.to_owned())))));
    let pull_requests = seen.pull_requests.clone();
    host.expect_open_draft_pr()
        .returning(move |_, _, pull_request| {
            pull_requests.lock().unwrap().push(pull_request);
            Box::pin(std::future::ready(Ok(PullRequestUrl(PR_URL.into()))))
        });
    if let Some(can_push) = can_push {
        host.expect_member_can_push()
            .returning(move |_, _, repository| {
                Box::pin(std::future::ready(Ok(can_push(&repository.to_string()))))
            });
        host.expect_open_draft_pr_as_member()
            .returning(|_, _, _| Box::pin(std::future::ready(Ok(PullRequestUrl(PR_URL.into())))));
    }
    host
}

#[tokio::test]
async fn an_extra_repository_gets_its_default_branch_its_own_guard_a_path_and_a_writable_grant() {
    let Some(world) = world().await else { return };
    with_extra_repositories(&world, &["acme/api"]).await;
    let (seen, reads) = (Seen::default(), ExtraReads::default());
    let specs = Arc::new(Mutex::new(Vec::new()));
    let runner = recording_runner(lines_of(SAMPLE), specs.clone());
    let host = extras_host(&seen, &reads, None);

    pipeline(&world, runner, host)
        .execute(world.run_id, None)
        .await
        .unwrap();

    let spec = specs.lock().unwrap()[0].clone();
    assert_eq!(spec.extra_repositories.len(), 1);
    let extra = &spec.extra_repositories[0];
    assert_eq!(extra.repository.to_string(), "acme/api");
    assert_eq!(extra.path, "/workspace/api");
    assert_eq!(extra.base_branch, "develop");
    assert_eq!(extra.guard.allow_paths, vec!["deploy/"]);
    assert_eq!((extra.guard.max_files, extra.guard.max_lines), (7, 70));
    assert!(spec.push_guard.allow_paths.is_empty());
    assert!(spec.prompt.contains("acme/api at /workspace/api"));
    assert!(
        reads
            .files
            .lock()
            .unwrap()
            .contains(&("acme/api".to_owned(), "develop".to_owned()))
    );
    let writable: Vec<String> =
        sqlx::query_scalar("SELECT writable_repositories FROM task_grants WHERE run_id = $1")
            .bind(world.run_id.0)
            .fetch_one(&world.pool)
            .await
            .unwrap();
    assert_eq!(writable, ["acme/api"]);
    let (_, _, _, _, output) = step(&world.pool, world.run_id).await;
    assert_eq!(output["extra_repositories"][0]["repository"], "acme/api");
    assert_eq!(output["extra_repositories"][0]["base_branch"], "develop");
}

#[tokio::test]
async fn a_resumed_run_keeps_the_extra_repositories_it_read_instead_of_reading_them_again() {
    let Some(world) = world().await else { return };
    with_extra_repositories(&world, &["acme/api"]).await;
    let kept = serde_json::json!([{
        "repository": "acme/api",
        "path": "/workspace/api",
        "base_branch": "release",
        "guard": {"deny_paths": [".matos.toml"], "allow_paths": [], "max_files": 3, "max_lines": 30}
    }]);
    crashed_attempt(
        &world,
        &format!("first-{}", world.run_id),
        Some(serde_json::json!({"job_started": true, "extra_repositories": kept})),
    )
    .await;
    let (seen, reads) = (Seen::default(), ExtraReads::default());
    let mut runner = MockTaskRunner::new();
    runner
        .expect_attach()
        .returning(|_| Box::pin(std::future::ready(Ok(true))));
    following(no_changes_lines(), &mut runner);
    let host = extras_host(&seen, &reads, None);

    pipeline(&world, runner, host)
        .execute(world.run_id, None)
        .await
        .unwrap();

    assert!(reads.default_branches.lock().unwrap().is_empty());
    assert!(
        reads
            .files
            .lock()
            .unwrap()
            .iter()
            .all(|(repository, _)| repository == "acme/web")
    );
    let (_, _, _, _, output) = step(&world.pool, world.run_id).await;
    assert_eq!(output["extra_repositories"][0]["base_branch"], "release");
    assert_eq!(output["extra_repositories"][0]["guard"]["max_files"], 3);
}

#[tokio::test]
async fn a_member_who_cannot_push_to_one_extra_repository_makes_the_whole_run_use_the_app() {
    let Some(world) = world().await else { return };
    connect_ada(
        &world,
        user_tokens(MEMBER_ACCESS, chrono::Duration::hours(8), MEMBER_REFRESH),
    )
    .await;
    with_extra_repositories(&world, &["acme/api"]).await;
    let seen = Seen::default();
    let host = extras_host(
        &seen,
        &ExtraReads::default(),
        Some(|repository| repository == "acme/web"),
    );

    run_with_host(&world, host, &seen).await;

    assert_eq!(git_identity_of(&world).await, "app");
}

#[tokio::test]
async fn a_member_who_can_push_to_every_repository_keeps_the_member_identity() {
    let Some(world) = world().await else { return };
    connect_ada(
        &world,
        user_tokens(MEMBER_ACCESS, chrono::Duration::hours(8), MEMBER_REFRESH),
    )
    .await;
    with_extra_repositories(&world, &["acme/api"]).await;
    let seen = Seen::default();
    let host = extras_host(&seen, &ExtraReads::default(), Some(|_| true));

    run_with_host(&world, host, &seen).await;

    assert_eq!(git_identity_of(&world).await, "member");
}

const API_PR_URL: &str = "https://github.com/acme/api/pull/3";
const API_SHA: &str = "9a8b7c6d5e4f30211203f4e5d6c7b8a9908172635";
const LINKED_HEADING: &str = "## Pull requests liées";

#[derive(Clone, Default)]
struct Linking {
    heads: Arc<Mutex<Vec<(String, String)>>>,
    updates: Arc<Mutex<Vec<(String, String, String)>>>,
    members: Arc<Mutex<Vec<(String, String)>>>,
}

fn url_of(repository: &str) -> &'static str {
    match repository {
        "acme/web" => PR_URL,
        _ => API_PR_URL,
    }
}

fn linking_host(seen: &Seen, linking: &Linking, member: bool) -> MockCodeHost {
    let mut host = MockCodeHost::new();
    host.expect_file_at()
        .returning(|_, _, _, _, _| Box::pin(std::future::ready(Ok(None))));
    host.expect_default_branch()
        .returning(|_, _, _| Box::pin(std::future::ready(Ok("develop".to_owned()))));
    let heads = linking.heads.clone();
    host.expect_branch_head()
        .returning(move |_, _, repository, branch| {
            heads
                .lock()
                .unwrap()
                .push((repository.to_string(), branch.to_owned()));
            let sha = if repository.to_string() == "acme/web" {
                PUSHED_SHA
            } else {
                API_SHA
            };
            Box::pin(std::future::ready(Ok(Some(sha.to_owned()))))
        });
    let pull_requests = seen.pull_requests.clone();
    host.expect_open_draft_pr()
        .returning(move |_, _, pull_request| {
            let url = url_of(&pull_request.repository.to_string());
            pull_requests.lock().unwrap().push(pull_request);
            Box::pin(std::future::ready(Ok(PullRequestUrl(url.into()))))
        });
    let updates = linking.updates.clone();
    host.expect_update_pull_request_body()
        .returning(move |_, _, repository, url, body| {
            updates
                .lock()
                .unwrap()
                .push((repository.to_string(), url.0.clone(), body.to_owned()));
            Box::pin(std::future::ready(Ok(())))
        });
    if member {
        host.expect_member_can_push()
            .returning(|_, _, _| Box::pin(std::future::ready(Ok(true))));
        let members = linking.members.clone();
        host.expect_open_draft_pr_as_member()
            .returning(move |_, token, pull_request| {
                let url = url_of(&pull_request.repository.to_string());
                members.lock().unwrap().push((
                    token.expose().to_owned(),
                    pull_request.repository.to_string(),
                ));
                Box::pin(std::future::ready(Ok(PullRequestUrl(url.into()))))
            });
    }
    host
}

fn api_contribution_lines(text: &str) -> Vec<TaskLine> {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let mut lines = Vec::new();
    for chunk in STANDARD.encode(text).as_bytes().chunks(12) {
        lines.push(TaskLine::Matos(TaskMarker::ContributionChunk {
            data: String::from_utf8(chunk.to_vec()).unwrap(),
            repository: Some("acme/api".into()),
        }));
    }
    let hash = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, text.as_bytes());
    lines.push(TaskLine::Matos(TaskMarker::ContributionEnd {
        sha256: hash.as_ref().iter().map(|b| format!("{b:02x}")).collect(),
        bytes: text.len() as u64,
        truncated: false,
        repository: Some("acme/api".into()),
    }));
    lines
}

fn both_pushed_lines() -> Vec<TaskLine> {
    let mut markers = api_contribution_lines("Aligner l'API\n\nLe contrat change aussi côté api.");
    markers.push(TaskLine::Matos(TaskMarker::Quality {
        lint: matos_core::domain::execution::QualityStatus::Passed,
        excerpt: None,
        repository: Some("acme/api".into()),
    }));
    markers.push(TaskLine::Matos(TaskMarker::ExtraRepositories {
        cloned: vec!["acme/api".into()],
        failed: Vec::new(),
    }));
    markers.push(TaskLine::Matos(TaskMarker::Pushed {
        sha: API_SHA.into(),
        repository: Some("acme/api".into()),
    }));
    let mut lines = lines_of(SAMPLE);
    let pushed = lines.pop().unwrap();
    lines.extend(markers);
    lines.push(pushed);
    lines
}

fn body_of(seen: &Seen, repository: &str) -> String {
    seen.pull_requests
        .lock()
        .unwrap()
        .iter()
        .find(|pull_request| pull_request.repository.to_string() == repository)
        .unwrap()
        .body
        .clone()
}

#[tokio::test]
async fn a_change_in_two_repositories_opens_two_pull_requests_that_cite_each_other() {
    let Some(world) = world().await else { return };
    with_extra_repositories(&world, &["acme/api"]).await;
    let (seen, linking) = (Seen::default(), Linking::default());
    let runner = recording_runner(both_pushed_lines(), Arc::new(Mutex::new(Vec::new())));

    pipeline(&world, runner, linking_host(&seen, &linking, false))
        .execute(world.run_id, None)
        .await
        .unwrap();

    assert_eq!(run_row(&world.pool, world.run_id).await.0, "succeeded");
    let opened = seen.pull_requests.lock().unwrap().clone();
    let order: Vec<String> = opened
        .iter()
        .map(|pull_request| pull_request.repository.to_string())
        .collect();
    assert_eq!(order, ["acme/web", "acme/api"]);
    assert!(
        opened
            .iter()
            .all(|pull_request| pull_request.head == branch_of(world.run_id))
    );
    assert_eq!(
        opened
            .iter()
            .map(|pull_request| pull_request.base.as_str())
            .collect::<Vec<_>>(),
        ["main", "develop"]
    );
    assert_eq!(opened[1].title, "Aligner l'API");
    assert!(opened[1].body.contains("Le contrat change aussi côté api."));
    assert!(opened[1].body.ends_with("Lint: passed"));
    assert_eq!(opened[0].title, "Ajouter un CHANGELOG");
    assert!(
        opened[0]
            .body
            .ends_with("Lint: not run, the toolchain is not available")
    );
    let branch = branch_of(world.run_id);
    assert_eq!(
        *linking.heads.lock().unwrap(),
        [
            ("acme/web".to_owned(), branch.clone()),
            ("acme/api".to_owned(), branch)
        ]
    );
    let updates = linking.updates.lock().unwrap().clone();
    assert_eq!(updates.len(), 2);
    assert_eq!(updates[0].0, "acme/web");
    assert_eq!(updates[0].1, PR_URL);
    assert!(updates[0].2.starts_with(&opened[0].body));
    assert!(
        updates[0]
            .2
            .ends_with(&format!("\n\n{LINKED_HEADING}\n\n- {API_PR_URL}"))
    );
    assert_eq!(updates[1].0, "acme/api");
    assert!(
        updates[1]
            .2
            .ends_with(&format!("\n\n{LINKED_HEADING}\n\n- {PR_URL}"))
    );
    let (_, _, _, _, output) = step(&world.pool, world.run_id).await;
    assert_eq!(output["pull_request"], PR_URL);
    assert_eq!(output["pull_requests"]["acme/web"], PR_URL);
    assert_eq!(output["pull_requests"]["acme/api"], API_PR_URL);
    assert_eq!(output["linked"], true);
    assert_eq!(output["sha"], PUSHED_SHA);
    let posted = comments(&world.pool, world.card.id).await;
    assert!(posted[0].2.starts_with(&format!(
        "Pull request ouverte en brouillon sur acme/web : {PR_URL}\nPull request ouverte en brouillon sur acme/api : {API_PR_URL}"
    )), "{}", posted[0].2);
    assert_closed(&world).await;
}

#[tokio::test]
async fn an_extra_repository_left_unchanged_gets_no_pull_request_and_no_linked_section() {
    let Some(world) = world().await else { return };
    with_extra_repositories(&world, &["acme/api"]).await;
    let (seen, linking) = (Seen::default(), Linking::default());
    let runner = recording_runner(lines_of(SAMPLE), Arc::new(Mutex::new(Vec::new())));

    pipeline(&world, runner, linking_host(&seen, &linking, false))
        .execute(world.run_id, None)
        .await
        .unwrap();

    assert_eq!(run_row(&world.pool, world.run_id).await.0, "succeeded");
    assert_eq!(seen.pull_requests.lock().unwrap().len(), 1);
    assert!(!body_of(&seen, "acme/web").contains(LINKED_HEADING));
    assert!(linking.updates.lock().unwrap().is_empty());
    assert_eq!(
        *linking.heads.lock().unwrap(),
        [("acme/web".to_owned(), branch_of(world.run_id))]
    );
    let (_, _, _, _, output) = step(&world.pool, world.run_id).await;
    assert_eq!(output["pull_requests"].as_object().unwrap().len(), 1);
    assert_eq!(output["linked"], false);
    let posted = comments(&world.pool, world.card.id).await;
    assert!(posted[0].2.starts_with(&format!(
        "Pull request ouverte en brouillon sur acme/web : {PR_URL}"
    )));
    assert!(!posted[0].2.contains("acme/api"));
}

#[tokio::test]
async fn a_run_without_extra_repositories_keeps_the_single_line_comment() {
    let Some(world) = world().await else { return };
    let (seen, linking) = (Seen::default(), Linking::default());
    let runner = recording_runner(lines_of(SAMPLE), Arc::new(Mutex::new(Vec::new())));

    pipeline(&world, runner, linking_host(&seen, &linking, false))
        .execute(world.run_id, None)
        .await
        .unwrap();

    let posted = comments(&world.pool, world.card.id).await;
    assert!(
        posted[0]
            .2
            .starts_with(&format!("Pull request ouverte en brouillon : {PR_URL}"))
    );
    assert!(linking.updates.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_guard_blocking_one_repository_pushes_none_and_the_reasons_name_it() {
    let Some(world) = world().await else { return };
    with_extra_repositories(&world, &["acme/api"]).await;
    let (seen, linking) = (Seen::default(), Linking::default());
    let mut lines = lines_of(SAMPLE);
    lines.pop();
    lines.push(TaskLine::Matos(TaskMarker::GuardBlocked {
        reasons: vec!["acme/api: chemin sensible : .github/workflows/ci.yml".into()],
    }));
    let runner = deleting(runner_ending(
        &seen,
        lines,
        TaskOutcome::Failed {
            reason: "container exited with 3".into(),
        },
    ));

    pipeline(&world, runner, linking_host(&seen, &linking, false))
        .execute(world.run_id, None)
        .await
        .unwrap();

    let (status, failure) = run_row(&world.pool, world.run_id).await;
    assert_eq!(status, "failed");
    assert!(
        failure
            .unwrap()
            .contains("acme/api: chemin sensible : .github/workflows/ci.yml")
    );
    assert!(seen.pull_requests.lock().unwrap().is_empty());
    assert!(linking.heads.lock().unwrap().is_empty());
    assert!(linking.updates.lock().unwrap().is_empty());
    assert_closed(&world).await;
}

#[tokio::test]
async fn a_crash_between_two_openings_reopens_nothing_opens_the_next_and_links_both() {
    let Some(world) = world().await else { return };
    with_extra_repositories(&world, &["acme/api"]).await;
    crashed_attempt(
        &world,
        &format!("first-{}", world.run_id),
        Some(serde_json::json!({
            "job_started": true,
            "branch": branch_of(world.run_id),
            "sha": PUSHED_SHA,
            "pull_request": PR_URL,
            "pull_requests": {"acme/web": PR_URL},
        })),
    )
    .await;
    let (seen, linking) = (Seen::default(), Linking::default());
    let mut runner = MockTaskRunner::new();
    runner
        .expect_attach()
        .times(1)
        .returning(|_| Box::pin(std::future::ready(Ok(true))));
    following(both_pushed_lines(), &mut runner);

    pipeline(&world, runner, linking_host(&seen, &linking, false))
        .execute(world.run_id, None)
        .await
        .unwrap();

    assert_eq!(run_row(&world.pool, world.run_id).await.0, "succeeded");
    let opened = seen.pull_requests.lock().unwrap().clone();
    assert_eq!(opened.len(), 1);
    assert_eq!(opened[0].repository.to_string(), "acme/api");
    assert_eq!(
        *linking.heads.lock().unwrap(),
        [("acme/api".to_owned(), branch_of(world.run_id))],
        "the pull request already opened is not verified again"
    );
    let updates = linking.updates.lock().unwrap().clone();
    assert_eq!(updates.len(), 2);
    assert!(updates[0].2.contains(API_PR_URL) && updates[1].2.contains(PR_URL));
    let (_, _, _, _, output) = step(&world.pool, world.run_id).await;
    assert_eq!(output["pull_request"], PR_URL);
    assert_eq!(output["pull_requests"]["acme/api"], API_PR_URL);
    assert_eq!(output["linked"], true);
    let posted = comments(&world.pool, world.card.id).await;
    assert!(posted[0].2.contains(&format!("sur acme/web : {PR_URL}")));
    assert!(
        posted[0]
            .2
            .contains(&format!("sur acme/api : {API_PR_URL}"))
    );
}

#[tokio::test]
async fn pull_requests_already_linked_are_not_edited_again_after_a_resume() {
    let Some(world) = world().await else { return };
    with_extra_repositories(&world, &["acme/api"]).await;
    crashed_attempt(
        &world,
        &format!("first-{}", world.run_id),
        Some(serde_json::json!({
            "job_started": true,
            "sha": PUSHED_SHA,
            "pull_request": PR_URL,
            "pull_requests": {"acme/web": PR_URL, "acme/api": API_PR_URL},
            "linked": true,
        })),
    )
    .await;
    let (seen, linking) = (Seen::default(), Linking::default());
    let mut runner = MockTaskRunner::new();
    runner
        .expect_attach()
        .returning(|_| Box::pin(std::future::ready(Ok(true))));
    following(both_pushed_lines(), &mut runner);

    pipeline(&world, runner, linking_host(&seen, &linking, false))
        .execute(world.run_id, None)
        .await
        .unwrap();

    assert!(seen.pull_requests.lock().unwrap().is_empty());
    assert!(linking.heads.lock().unwrap().is_empty());
    assert!(linking.updates.lock().unwrap().is_empty());
    assert_eq!(run_row(&world.pool, world.run_id).await.0, "succeeded");
}

#[tokio::test]
async fn every_pull_request_of_a_run_is_opened_with_the_identity_the_run_decided() {
    let Some(world) = world().await else { return };
    connect_ada(
        &world,
        user_tokens(MEMBER_ACCESS, chrono::Duration::hours(8), MEMBER_REFRESH),
    )
    .await;
    with_extra_repositories(&world, &["acme/api"]).await;
    let (seen, linking) = (Seen::default(), Linking::default());
    let runner = recording_runner(both_pushed_lines(), Arc::new(Mutex::new(Vec::new())));

    pipeline(&world, runner, linking_host(&seen, &linking, true))
        .execute(world.run_id, None)
        .await
        .unwrap();

    assert_eq!(git_identity_of(&world).await, "member");
    assert_eq!(
        *linking.members.lock().unwrap(),
        [
            (MEMBER_ACCESS.to_owned(), "acme/web".to_owned()),
            (MEMBER_ACCESS.to_owned(), "acme/api".to_owned())
        ]
    );
    assert!(seen.pull_requests.lock().unwrap().is_empty());
    assert_eq!(linking.updates.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn a_pushed_repository_the_run_does_not_list_fails_the_run_before_any_pull_request() {
    let Some(world) = world().await else { return };
    let (seen, linking) = (Seen::default(), Linking::default());
    let mut lines = lines_of(SAMPLE);
    let pushed = lines.pop().unwrap();
    lines.push(TaskLine::Matos(TaskMarker::Pushed {
        sha: API_SHA.into(),
        repository: Some("acme/other".into()),
    }));
    lines.push(pushed);
    let runner = deleting(runner_ending(&seen, lines, TaskOutcome::Succeeded));

    pipeline(&world, runner, linking_host(&seen, &linking, false))
        .execute(world.run_id, None)
        .await
        .unwrap();

    let (status, failure) = run_row(&world.pool, world.run_id).await;
    assert_eq!(status, "failed");
    assert!(failure.unwrap().contains("acme/other"));
    assert!(seen.pull_requests.lock().unwrap().is_empty());
}

const EXIT_THREE: &str = "container exited with code 3: Error";
const LIMIT_NOTE: &str =
    "L'agent s'est arrêté à la limite de 50 tours : ce changement est partiel.";
const RESUME_ON_CARD: &str = "Réponds sur la carte pour qu'il reprenne.";

fn stopped_on_limit(mut lines: Vec<TaskLine>) -> Vec<TaskLine> {
    for line in &mut lines {
        if let TaskLine::Agent(value) = line
            && value["type"] == "result"
        {
            value["subtype"] = serde_json::json!("error_max_turns");
        }
    }
    lines
}

fn exited_with_three() -> TaskOutcome {
    TaskOutcome::Failed {
        reason: EXIT_THREE.into(),
    }
}

#[tokio::test]
async fn an_agent_stopped_on_its_turn_limit_after_pushing_still_opens_the_pull_request_with_a_note()
{
    let Some(world) = world().await else { return };
    let seen = Seen::default();
    let runner = runner_ending(
        &seen,
        stopped_on_limit(lines_of(SAMPLE)),
        exited_with_three(),
    );
    let pipeline = pipeline(
        &world,
        runner,
        host(&seen, world.organization_id, world.installation_id),
    );

    pipeline.execute(world.run_id, None).await.unwrap();

    let (status, failure) = run_row(&world.pool, world.run_id).await;
    assert_eq!((status.as_str(), failure), ("succeeded", None));
    let opened = seen.pull_requests.lock().unwrap().clone();
    assert_eq!(opened.len(), 1);
    let body = &opened[0].body;
    let section = body.find("## Travail incomplet").unwrap();
    assert!(body.contains(LIMIT_NOTE));
    assert!(section < body.find("## Checks").unwrap());
    let (_, _, _, _, output) = step(&world.pool, world.run_id).await;
    assert_eq!(output["stopped"], "error_max_turns");
    assert_eq!(output["incomplete"], LIMIT_NOTE);
    assert_eq!(output["pull_request"], PR_URL);
    let posted = comments(&world.pool, world.card.id).await;
    assert!(posted[0].2.contains(PR_URL));
    assert!(
        posted[0]
            .2
            .contains(&format!("{LIMIT_NOTE} {RESUME_ON_CARD}"))
    );
    assert!(!posted[0].2.contains("L'exécution a échoué"));
    assert_closed(&world).await;
}

#[tokio::test]
async fn an_agent_that_exited_in_error_after_pushing_opens_the_pull_request_and_says_why() {
    let Some(world) = world().await else { return };
    let seen = Seen::default();
    let runner = runner_ending(&seen, lines_of(SAMPLE), exited_with_three());
    let pipeline = pipeline(
        &world,
        runner,
        host(&seen, world.organization_id, world.installation_id),
    );

    pipeline.execute(world.run_id, None).await.unwrap();

    assert_eq!(run_row(&world.pool, world.run_id).await.0, "succeeded");
    let note = format!(
        "L'agent s'est arrêté en erreur ({EXIT_THREE}) : ce changement est peut-être partiel."
    );
    let opened = seen.pull_requests.lock().unwrap().clone();
    assert!(
        opened[0]
            .body
            .contains(&format!("## Travail incomplet\n\n{note}"))
    );
    let posted = comments(&world.pool, world.card.id).await;
    assert!(posted[0].2.contains(&format!("{note} {RESUME_ON_CARD}")));
}

#[tokio::test]
async fn a_turn_limit_with_nothing_pushed_fails_the_run_with_the_limit_as_reason() {
    let Some(world) = world().await else { return };
    let seen = Seen::default();
    let runner = deleting(runner_ending(
        &seen,
        stopped_on_limit(no_changes_lines()),
        exited_with_three(),
    ));
    let pipeline = pipeline(&world, runner, without_config());

    pipeline.execute(world.run_id, None).await.unwrap();

    let (status, failure) = run_row(&world.pool, world.run_id).await;
    assert_eq!(status, "failed");
    assert_eq!(
        failure.as_deref(),
        Some("L'agent a atteint la limite de 50 tours sans terminer.")
    );
    assert!(seen.pull_requests.lock().unwrap().is_empty());
    let posted = comments(&world.pool, world.card.id).await;
    assert!(posted[0].2.contains("limite de 50 tours sans terminer"));
    assert!(!posted[0].2.contains("container exited"));
}

#[tokio::test]
async fn a_failure_without_a_push_nor_a_limit_keeps_the_container_reason() {
    let Some(world) = world().await else { return };
    let seen = Seen::default();
    let runner = deleting(runner_ending(
        &seen,
        no_changes_lines(),
        exited_with_three(),
    ));
    let pipeline = pipeline(&world, runner, without_config());

    pipeline.execute(world.run_id, None).await.unwrap();

    let (status, failure) = run_row(&world.pool, world.run_id).await;
    assert_eq!(status, "failed");
    assert_eq!(failure.as_deref(), Some(EXIT_THREE));
}

#[tokio::test]
async fn a_blocked_diff_on_a_failed_exit_stays_blocked_even_with_a_pushed_marker() {
    let Some(world) = world().await else { return };
    let seen = Seen::default();
    let mut lines = stopped_on_limit(lines_of(SAMPLE));
    lines.push(TaskLine::Matos(TaskMarker::GuardBlocked {
        reasons: vec!["chemin sensible : .github/workflows/ci.yml".into()],
    }));
    let runner = deleting(runner_ending(&seen, lines, exited_with_three()));
    let pipeline = pipeline(
        &world,
        runner,
        host(&seen, world.organization_id, world.installation_id),
    );

    pipeline.execute(world.run_id, None).await.unwrap();

    let (status, failure) = run_row(&world.pool, world.run_id).await;
    assert_eq!(status, "failed");
    let failure = failure.unwrap();
    assert!(failure.contains("chemin sensible : .github/workflows/ci.yml"));
    assert!(!failure.contains("limite de 50 tours"));
    assert!(seen.pull_requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_resumed_run_keeps_the_incomplete_note_for_the_pull_request_it_still_opens() {
    let Some(world) = world().await else { return };
    with_extra_repositories(&world, &["acme/api"]).await;
    crashed_attempt(
        &world,
        &format!("first-{}", world.run_id),
        Some(serde_json::json!({
            "job_started": true,
            "branch": branch_of(world.run_id),
            "sha": PUSHED_SHA,
            "pull_request": PR_URL,
            "pull_requests": {"acme/web": PR_URL},
            "incomplete": LIMIT_NOTE,
        })),
    )
    .await;
    let (seen, linking) = (Seen::default(), Linking::default());
    let mut runner = MockTaskRunner::new();
    runner
        .expect_attach()
        .times(1)
        .returning(|_| Box::pin(std::future::ready(Ok(true))));
    following(both_pushed_lines(), &mut runner);

    pipeline(&world, runner, linking_host(&seen, &linking, false))
        .execute(world.run_id, None)
        .await
        .unwrap();

    assert_eq!(run_row(&world.pool, world.run_id).await.0, "succeeded");
    assert!(body_of(&seen, "acme/api").contains(&format!("## Travail incomplet\n\n{LIMIT_NOTE}")));
    let updates = linking.updates.lock().unwrap().clone();
    assert!(updates.iter().all(|update| update.2.contains(LIMIT_NOTE)));
    let (_, _, _, _, output) = step(&world.pool, world.run_id).await;
    assert_eq!(output["incomplete"], LIMIT_NOTE);
    let posted = comments(&world.pool, world.card.id).await;
    assert!(
        posted[0]
            .2
            .contains(&format!("{LIMIT_NOTE} {RESUME_ON_CARD}"))
    );
}

#[tokio::test]
async fn a_multi_repository_run_that_failed_after_pushing_puts_the_note_in_every_pull_request() {
    let Some(world) = world().await else { return };
    with_extra_repositories(&world, &["acme/api"]).await;
    let (seen, linking) = (Seen::default(), Linking::default());
    let runner = runner_ending(
        &seen,
        stopped_on_limit(both_pushed_lines()),
        exited_with_three(),
    );

    pipeline(&world, runner, linking_host(&seen, &linking, false))
        .execute(world.run_id, None)
        .await
        .unwrap();

    assert_eq!(run_row(&world.pool, world.run_id).await.0, "succeeded");
    for repository in ["acme/web", "acme/api"] {
        assert!(
            body_of(&seen, repository).contains(LIMIT_NOTE),
            "{repository}"
        );
    }
    assert_eq!(linking.updates.lock().unwrap().len(), 2);
}

fn scripted_conversation() -> Vec<TaskLine> {
    vec![
        TaskLine::Agent(serde_json::json!({
            "type": "assistant",
            "message": {"content": [{"type": "text", "text": "Je regarde le dépôt."}]}
        })),
        TaskLine::Agent(serde_json::json!({
            "type": "assistant",
            "message": {"content": [{
                "type": "tool_use", "id": "toolu_ls", "name": "Bash",
                "input": {"command": "ls"}
            }]}
        })),
        TaskLine::Agent(serde_json::json!({
            "type": "user",
            "message": {"content": [{
                "type": "tool_result", "tool_use_id": "toolu_ls",
                "content": "README.md\n", "is_error": false
            }]}
        })),
        TaskLine::Matos(TaskMarker::NoChanges),
    ]
}

async fn run_events(pool: &PgPool, run_id: RunId) -> Vec<(i64, String, serde_json::Value)> {
    sqlx::query_as("SELECT seq, kind, payload FROM run_events WHERE run_id = $1 ORDER BY seq")
        .bind(run_id.0)
        .fetch_all(pool)
        .await
        .unwrap()
}

fn assert_conversation(events: &[(i64, String, serde_json::Value)]) {
    let seqs: Vec<i64> = events.iter().map(|event| event.0).collect();
    assert_eq!(seqs, (1..=3).collect::<Vec<i64>>());
    let kinds: Vec<&str> = events.iter().map(|event| event.1.as_str()).collect();
    assert_eq!(kinds, ["assistant_text", "tool_use", "tool_result"]);
    assert_eq!(events[0].2["text"], "Je regarde le dépôt.");
    assert_eq!(events[1].2["id"], "toolu_ls");
    assert_eq!(events[1].2["tool"], "Bash");
    assert_eq!(events[2].2["id"], "toolu_ls");
    assert_eq!(events[2].2["succeeded"], true);
    assert_eq!(events[2].2["output"], "README.md\n");
}

#[tokio::test]
async fn the_agent_conversation_is_written_to_the_run_events_in_order() {
    let Some(world) = world().await else { return };
    let seen = Seen::default();
    let runner = runner_ending(&seen, scripted_conversation(), TaskOutcome::Succeeded);
    let pipeline = pipeline(&world, runner, without_config());

    pipeline.execute(world.run_id, None).await.unwrap();

    assert_eq!(run_row(&world.pool, world.run_id).await.0, "succeeded");
    assert_conversation(&run_events(&world.pool, world.run_id).await);
}

#[tokio::test]
async fn following_the_same_lines_again_adds_no_duplicate_events() {
    let Some(world) = world().await else { return };
    let mut runner = MockTaskRunner::new();
    runner
        .expect_start()
        .returning(|_| Box::pin(std::future::ready(Ok(()))));
    let calls = Arc::new(Mutex::new(0u32));
    runner.expect_follow().times(2).returning(move |_, sender| {
        let first = {
            let mut calls = calls.lock().unwrap();
            *calls += 1;
            *calls == 1
        };
        let lines = scripted_conversation();
        Box::pin(async move {
            for line in lines {
                sender.send(line).await.unwrap();
            }
            if first {
                Err(CoreError::Internal("try again".into()))
            } else {
                Ok(TaskOutcome::Succeeded)
            }
        })
    });
    let pipeline = pipeline(&world, runner, without_config());

    pipeline.execute(world.run_id, None).await.unwrap();

    assert_eq!(run_row(&world.pool, world.run_id).await.0, "succeeded");
    assert_conversation(&run_events(&world.pool, world.run_id).await);
}

#[tokio::test]
async fn events_are_visible_while_the_agent_is_still_running() {
    let Some(world) = world().await else { return };
    let release = Arc::new(tokio::sync::Notify::new());
    let mut runner = MockTaskRunner::new();
    runner
        .expect_start()
        .returning(|_| Box::pin(std::future::ready(Ok(()))));
    let gate = release.clone();
    runner.expect_follow().returning(move |_, sender| {
        let gate = gate.clone();
        let lines = scripted_conversation();
        Box::pin(async move {
            for line in lines {
                sender.send(line).await.unwrap();
            }
            gate.notified().await;
            Ok(TaskOutcome::Succeeded)
        })
    });
    let pipeline = pipeline(&world, runner, without_config());
    let run_id = world.run_id;
    let handle = tokio::spawn(async move { pipeline.execute(run_id, None).await });

    let mut live = Vec::new();
    for _ in 0..100 {
        live = run_events(&world.pool, run_id).await;
        if live.len() == 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!handle.is_finished(), "the follow is still pending");
    assert_conversation(&live);
    assert_eq!(run_row(&world.pool, run_id).await.0, "running");

    release.notify_one();
    handle.await.unwrap().unwrap();
    assert_eq!(run_row(&world.pool, run_id).await.0, "succeeded");
    assert_conversation(&run_events(&world.pool, run_id).await);
}

fn text_delta_line(text: &str) -> TaskLine {
    TaskLine::Agent(serde_json::json!({
        "type": "stream_event",
        "event": {
            "type": "content_block_delta", "index": 0,
            "delta": {"type": "text_delta", "text": text}
        }
    }))
}

#[tokio::test]
async fn live_text_and_stored_events_are_published_in_order_on_the_stream_channel() {
    let Some(world) = world().await else { return };
    let mut listener = sqlx::postgres::PgListener::connect_with(&world.pool)
        .await
        .unwrap();
    listener.listen(RUN_STREAM_CHANNEL).await.unwrap();
    let mut runner = MockTaskRunner::new();
    runner
        .expect_start()
        .returning(|_| Box::pin(std::future::ready(Ok(()))));
    runner.expect_follow().returning(move |_, sender| {
        Box::pin(async move {
            sender.send(text_delta_line("Je regarde ")).await.unwrap();
            sender.send(text_delta_line("le dépôt.")).await.unwrap();
            tokio::time::sleep(Duration::from_millis(400)).await;
            sender
                .send(TaskLine::Agent(serde_json::json!({
                    "type": "assistant",
                    "message": {"content": [{"type": "text", "text": "Je regarde le dépôt."}]}
                })))
                .await
                .unwrap();
            sender.send(text_delta_line("Fini.")).await.unwrap();
            sender
                .send(TaskLine::Matos(TaskMarker::NoChanges))
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(400)).await;
            Ok(TaskOutcome::Succeeded)
        })
    });
    let pipeline = pipeline(&world, runner, without_config());

    pipeline.execute(world.run_id, None).await.unwrap();

    let mut notices = Vec::new();
    while let Ok(Ok(notification)) =
        tokio::time::timeout(Duration::from_millis(500), listener.recv()).await
    {
        let notice = RunStreamNotice::decode(notification.payload()).unwrap();
        if notice.run() == world.run_id.0 {
            notices.push(notice);
        }
    }
    let stored = run_events(&world.pool, world.run_id).await;
    let last_seq = stored.last().unwrap().0;
    let mut first = String::new();
    let mut second = String::new();
    let mut events_at = None;
    let mut last_event = 0;
    for (index, notice) in notices.iter().enumerate() {
        match notice {
            RunStreamNotice::Delta { after: 0, text, .. } => {
                assert!(events_at.is_none(), "a delta after 0 came after the events");
                first.push_str(text);
            }
            RunStreamNotice::Delta { after: 1, text, .. } => second.push_str(text),
            RunStreamNotice::Delta { after, .. } => panic!("unexpected after {after}"),
            RunStreamNotice::Events { last, .. } => {
                events_at.get_or_insert(index);
                last_event = *last;
            }
        }
    }
    assert_eq!(first, "Je regarde le dépôt.");
    assert_eq!(second, "Fini.");
    assert!(events_at.is_some());
    assert_eq!(last_event, last_seq);
}

async fn cancel_run_row(pool: &PgPool, run_id: RunId) {
    sqlx::query(
        "UPDATE runs SET status = 'cancelled', finished_at = now(), failure = 'Arrêté par Alice' WHERE id = $1",
    )
    .bind(run_id.0)
    .execute(pool)
    .await
    .unwrap();
}

async fn step_error(pool: &PgPool, run_id: RunId) -> (String, Option<String>) {
    sqlx::query_as("SELECT status, error FROM run_steps WHERE run_id = $1")
        .bind(run_id.0)
        .fetch_one(pool)
        .await
        .unwrap()
}

fn assert_stopped_once(posted: &[(String, Option<String>, String)]) {
    assert_eq!(posted.len(), 1);
    assert!(posted[0].2.starts_with("Arrêté par Alice."));
}

#[tokio::test]
async fn a_run_cancelled_while_the_task_is_followed_stops_the_job_and_leaves_a_trace() {
    let Some(world) = world().await else { return };
    let seen = Seen::default();
    let mut runner = MockTaskRunner::new();
    runner
        .expect_start()
        .returning(|_| Box::pin(std::future::ready(Ok(()))));
    runner.expect_follow().returning(|_, sender| {
        let lines = scripted_conversation();
        Box::pin(async move {
            for line in lines {
                sender.send(line).await.unwrap();
            }
            std::future::pending().await
        })
    });
    let deleted = Arc::new(Mutex::new(Vec::new()));
    let record = deleted.clone();
    runner.expect_delete().times(1).returning(move |run_id| {
        record.lock().unwrap().push(run_id);
        Box::pin(std::future::ready(Ok(())))
    });
    let pipeline = pipeline(
        &world,
        runner,
        host(&seen, world.organization_id, world.installation_id),
    );
    let stopper = async {
        tokio::time::sleep(Duration::from_millis(700)).await;
        cancel_run_row(&world.pool, world.run_id).await;
        std::time::Instant::now()
    };

    let (ended, cancelled_at) = tokio::time::timeout(Duration::from_secs(10), async {
        let run = async {
            pipeline.execute(world.run_id, None).await.unwrap();
            std::time::Instant::now()
        };
        tokio::join!(run, stopper)
    })
    .await
    .unwrap();

    assert!(ended.duration_since(cancelled_at) < Duration::from_secs(2));
    assert_eq!(
        run_row(&world.pool, world.run_id).await,
        ("cancelled".into(), Some("Arrêté par Alice".into()))
    );
    assert_eq!(*deleted.lock().unwrap(), [world.run_id]);
    assert_eq!(
        step_error(&world.pool, world.run_id).await,
        ("failed".into(), Some("cancelled".into()))
    );
    let events = run_events(&world.pool, world.run_id).await;
    let kinds: Vec<&str> = events.iter().map(|event| event.1.as_str()).collect();
    assert_eq!(
        kinds,
        ["assistant_text", "tool_use", "tool_result", "system"]
    );
    assert_eq!(events[3].0, 4);
    assert_eq!(events[3].2["text"], "Arrêté par Alice");
    let posted = comments(&world.pool, world.card.id).await;
    assert_stopped_once(&posted);
    assert!(posted[0].2.contains("\n\nJe regarde le dépôt."));
    assert!(seen.pull_requests.lock().unwrap().is_empty());
    assert_closed(&world).await;
}

#[tokio::test]
async fn a_run_cancelled_before_it_is_claimed_starts_no_job() {
    let Some(world) = world().await else { return };
    cancel_run_row(&world.pool, world.run_id).await;
    let pipeline = pipeline(&world, MockTaskRunner::new(), MockCodeHost::new());

    pipeline.execute(world.run_id, None).await.unwrap();

    assert_eq!(run_row(&world.pool, world.run_id).await.0, "cancelled");
}

#[tokio::test]
async fn a_run_cancelled_while_it_is_prepared_never_starts_a_job() {
    let Some(world) = world().await else { return };
    let (pool, run_id) = (world.pool.clone(), world.run_id);
    let mut host = MockCodeHost::new();
    host.expect_file_at().returning(move |_, _, _, _, _| {
        let pool = pool.clone();
        Box::pin(async move {
            cancel_run_row(&pool, run_id).await;
            Ok(None)
        })
    });
    let pipeline = pipeline(&world, MockTaskRunner::new(), host);

    pipeline.execute(world.run_id, None).await.unwrap();

    assert_eq!(
        run_row(&world.pool, world.run_id).await,
        ("cancelled".into(), Some("Arrêté par Alice".into()))
    );
    assert_eq!(
        step_error(&world.pool, world.run_id).await,
        ("failed".into(), Some("cancelled".into()))
    );
    let events = run_events(&world.pool, world.run_id).await;
    assert_eq!(events.len(), 1);
    assert_eq!((events[0].0, events[0].1.as_str()), (1, "system"));
    assert_stopped_once(&comments(&world.pool, world.card.id).await);
    assert_closed(&world).await;
}

#[tokio::test]
async fn a_run_cancelled_after_the_agent_ended_pushes_nothing() {
    let Some(world) = world().await else { return };
    let seen = Seen::default();
    let mut runner = MockTaskRunner::new();
    runner
        .expect_start()
        .returning(|_| Box::pin(std::future::ready(Ok(()))));
    let (pool, run_id) = (world.pool.clone(), world.run_id);
    let lines = lines_of(SAMPLE);
    runner.expect_follow().returning(move |_, sender| {
        let (pool, lines) = (pool.clone(), lines.clone());
        Box::pin(async move {
            for line in lines {
                sender.send(line).await.unwrap();
            }
            cancel_run_row(&pool, run_id).await;
            Ok(TaskOutcome::Succeeded)
        })
    });
    let pipeline = pipeline(
        &world,
        deleting(runner),
        host(&seen, world.organization_id, world.installation_id),
    );

    pipeline.execute(world.run_id, None).await.unwrap();

    assert_eq!(
        run_row(&world.pool, world.run_id).await,
        ("cancelled".into(), Some("Arrêté par Alice".into()))
    );
    assert!(seen.pull_requests.lock().unwrap().is_empty());
    assert_stopped_once(&comments(&world.pool, world.card.id).await);
    assert_closed(&world).await;
}

async fn ready_for_relaunch(world: &World) {
    for id in ["user-1", "user-2"] {
        world
            .usecase
            .put_user_provider(PutUserProvider {
                subject: Subject {
                    id: id.into(),
                    ..human(world.organization_id)
                },
                kind: ProviderKind::Anthropic,
                secret: Some(SecretToken::new("sk-ant-api03-abcdefgh1234")),
                base_url: None,
                default_model: "claude-haiku-5-5".into(),
                is_default: None,
            })
            .await
            .unwrap();
    }
    world
        .usecase
        .record_executor_status(&ExecutorStatus {
            enabled: true,
            reason: None,
        })
        .await
        .unwrap();
    let agent = world
        .usecase
        .create_crew_agent(
            human(world.organization_id),
            CreateAgent {
                slug: "relanceuse".into(),
                name: "Relanceuse".into(),
                role: String::new(),
                description: String::new(),
                model: "claude-opus-5".into(),
                hue: 0,
                reports_to: None,
                instructions: "Tu réponds.".into(),
                connector_grants: vec![],
                repository_grants: vec!["acme/web".into()],
            },
        )
        .await
        .unwrap();
    let mut edit = EditCard::new(world.card.version);
    edit.assignee_agent = Change::Set(agent.id);
    world
        .usecase
        .edit_card(human(world.organization_id), world.card.id, edit)
        .await
        .unwrap();
}

async fn reply_as_user_2(world: &World) {
    world
        .usecase
        .comment_card(
            Subject {
                id: "user-2".into(),
                ..human(world.organization_id)
            },
            world.card.id,
            "Et le fichier de config ?".into(),
            None,
        )
        .await
        .unwrap();
}

type RelaunchRow = (Uuid, String, String, Option<String>, Option<bool>);

async fn relaunched(world: &World) -> Vec<(RunId, String, String, Option<String>, bool)> {
    let rows: Vec<RelaunchRow> = sqlx::query_as(
        "SELECT r.id, r.initiated_by, r.base_branch, r.repository, t.allow_writes
         FROM runs r LEFT JOIN agent_tasks t ON t.run_id = r.id
         WHERE r.card_id = $1 AND r.id <> $2 ORDER BY r.id",
    )
    .bind(world.card.id.0)
    .bind(world.run_id.0)
    .fetch_all(&world.pool)
    .await
    .unwrap();
    rows.into_iter()
        .map(|(id, by, branch, repository, writes)| {
            (RunId(id), by, branch, repository, writes.unwrap_or(true))
        })
        .collect()
}

#[tokio::test]
async fn a_failed_run_with_a_reply_relaunches_read_only_as_the_replier() {
    let Some(world) = world().await else { return };
    ready_for_relaunch(&world).await;
    reply_as_user_2(&world).await;
    let seen = Seen::default();
    let runner = deleting(runner_ending(
        &seen,
        Vec::new(),
        TaskOutcome::Failed {
            reason: "OOMKilled".into(),
        },
    ));
    let pipeline = pipeline(&world, runner, without_config());

    pipeline.execute(world.run_id, None).await.unwrap();

    assert_eq!(run_row(&world.pool, world.run_id).await.0, "failed");
    let next = relaunched(&world).await;
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].1, "user-2");
    assert_eq!(next[0].2, "main");
    assert_eq!(next[0].3.as_deref(), Some("acme/web"));
    assert!(!next[0].4);
}

#[tokio::test]
async fn a_refused_relaunch_leaves_the_finished_run_untouched() {
    let Some(world) = world().await else { return };
    reply_as_user_2(&world).await;
    let seen = Seen::default();
    let mut lines = lines_of(SAMPLE);
    lines.pop();
    lines.push(TaskLine::Matos(TaskMarker::NoChanges));
    let runner = runner_ending(&seen, lines, TaskOutcome::Succeeded);
    let pipeline = pipeline(&world, runner, without_config());

    pipeline.execute(world.run_id, None).await.unwrap();

    assert_eq!(run_row(&world.pool, world.run_id).await.0, "succeeded");
    assert!(relaunched(&world).await.is_empty());
}

#[tokio::test]
async fn a_cancelled_run_is_not_relaunched_even_with_a_reply() {
    let Some(world) = world().await else { return };
    ready_for_relaunch(&world).await;
    let seen = Seen::default();
    let mut runner = MockTaskRunner::new();
    runner
        .expect_start()
        .returning(|_| Box::pin(std::future::ready(Ok(()))));
    runner.expect_follow().returning(|_, sender| {
        let lines = scripted_conversation();
        Box::pin(async move {
            for line in lines {
                sender.send(line).await.unwrap();
            }
            std::future::pending().await
        })
    });
    runner
        .expect_delete()
        .returning(|_| Box::pin(std::future::ready(Ok(()))));
    let pipeline = pipeline(
        &world,
        runner,
        host(&seen, world.organization_id, world.installation_id),
    );
    let stopper = async {
        tokio::time::sleep(Duration::from_millis(700)).await;
        reply_as_user_2(&world).await;
        cancel_run_row(&world.pool, world.run_id).await;
    };

    tokio::time::timeout(Duration::from_secs(10), async {
        let run = pipeline.execute(world.run_id, None);
        let (ended, ()) = tokio::join!(run, stopper);
        ended.unwrap();
    })
    .await
    .unwrap();

    assert_eq!(run_row(&world.pool, world.run_id).await.0, "cancelled");
    assert!(relaunched(&world).await.is_empty());
}
