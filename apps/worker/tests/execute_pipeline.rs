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
            service::{CreateCard, MoveCard},
        },
        delegation::DelegatedToken,
        execution::{
            CommitAuthor, DraftPullRequest, ExecutionMode, ExecutorModel, GithubAppCredentials,
            PullRequestUrl, TaskGrant, TaskLine, TaskMarker, TaskOutcome, TaskSpec,
            harness::Harness,
            ports::{MockCodeHost, MockTaskRunner},
        },
        github_user::{GitIdentity, GithubUser, UserTokens, ports::MockGithubUserOAuth},
        member::Member,
        provider::ModelAssignments,
        run::{
            StepKind,
            step::{Pricing, Step, StepStatus},
        },
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
        }));
    }
    let hash = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, text.as_bytes());
    lines.push(TaskLine::Matos(TaskMarker::ContributionEnd {
        sha256: hash.as_ref().iter().map(|b| format!("{b:02x}")).collect(),
        bytes: text.len() as u64,
        truncated: false,
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
