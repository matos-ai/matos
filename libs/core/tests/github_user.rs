#![allow(clippy::unwrap_used)]

use std::sync::Mutex;

use chrono::{Duration, Utc};
use matos_common::{CoreError, OrganizationId};
use matos_core::{
    MatosUseCase,
    domain::{
        agent::SecretToken,
        authorization::Subject,
        execution::ports::CodeHost,
        github_user::{
            GithubUser, PersonalTokenCheck, UserTokens,
            ports::{GithubUserOAuth, PersonalTokenValidator},
        },
    },
    infrastructure::{crypto::SecretCipher, postgres},
};
use sqlx::PgPool;
use uuid::Uuid;

const ACCESS: &str = "ghu_access_secret_1";
const REFRESH: &str = "ghr_refresh_secret_1";

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

fn subject(organization_id: OrganizationId, id: &str) -> Subject {
    Subject {
        id: id.into(),
        organization_id,
        actor: None,
        is_admin: false,
    }
}

fn usecase(pool: &PgPool, key: u8) -> MatosUseCase {
    MatosUseCase::with_test_policies(pool.clone())
        .with_secrets_cipher(SecretCipher::new(&[key; 32]).unwrap())
}

fn tokens(access: &str, expires_in: Option<Duration>, refresh: Option<&str>) -> UserTokens {
    UserTokens {
        access_token: SecretToken::new(access),
        expires_at: expires_in.map(|delay| Utc::now() + delay),
        refresh_token: refresh.map(SecretToken::new),
        refresh_expires_at: refresh.map(|_| Utc::now() + Duration::days(180)),
    }
}

struct FakeGithub {
    exchanged: UserTokens,
    refreshed: Mutex<Option<UserTokens>>,
    refresh_calls: Mutex<Vec<String>>,
    revoked: Mutex<Vec<String>>,
    revoke_fails: bool,
    rival: Option<(MatosUseCase, Subject, UserTokens)>,
}

impl FakeGithub {
    fn new(exchanged: UserTokens) -> Self {
        Self {
            exchanged,
            refreshed: Mutex::new(None),
            refresh_calls: Mutex::new(Vec::new()),
            revoked: Mutex::new(Vec::new()),
            revoke_fails: false,
            rival: None,
        }
    }

    fn refreshing_to(self, fresh: UserTokens) -> Self {
        *self.refreshed.lock().unwrap() = Some(fresh);
        self
    }
}

impl GithubUserOAuth for FakeGithub {
    async fn authorize_url(&self, _: OrganizationId, state: &str) -> Result<String, CoreError> {
        Ok(format!("https://github.test/authorize?state={state}"))
    }

    async fn exchange_code(&self, _: OrganizationId, code: &str) -> Result<UserTokens, CoreError> {
        assert_eq!(code, "the-code");
        Ok(self.exchanged.clone())
    }

    async fn user(&self, _: OrganizationId, access: &SecretToken) -> Result<GithubUser, CoreError> {
        assert_eq!(access.expose(), self.exchanged.access_token.expose());
        Ok(GithubUser {
            id: 583231,
            login: "octocat".into(),
        })
    }

    async fn revoke(&self, _: OrganizationId, access: &SecretToken) -> Result<(), CoreError> {
        self.revoked
            .lock()
            .unwrap()
            .push(access.expose().to_owned());
        if self.revoke_fails {
            return Err(CoreError::Internal("github is down".into()));
        }
        Ok(())
    }
}

impl CodeHost for FakeGithub {
    async fn repositories(
        &self,
        _: OrganizationId,
        _: i64,
    ) -> Result<Vec<matos_core::domain::execution::RepositoryRef>, CoreError> {
        unreachable!()
    }

    async fn repository_listing(
        &self,
        _: OrganizationId,
        _: i64,
    ) -> Result<Vec<matos_core::domain::execution::RepositoryListing>, CoreError> {
        unreachable!()
    }

    async fn open_issues(
        &self,
        _: OrganizationId,
        _: i64,
        _: &matos_core::domain::execution::RepositoryRef,
    ) -> Result<Vec<matos_core::domain::execution::RepositoryIssue>, CoreError> {
        unreachable!()
    }

    async fn branch_head(
        &self,
        _: OrganizationId,
        _: i64,
        _: &matos_core::domain::execution::RepositoryRef,
        _: &str,
    ) -> Result<Option<String>, CoreError> {
        unreachable!()
    }

    async fn file_at(
        &self,
        _: OrganizationId,
        _: i64,
        _: &matos_core::domain::execution::RepositoryRef,
        _: &str,
        _: &str,
    ) -> Result<Option<String>, CoreError> {
        unreachable!()
    }

    async fn open_draft_pr(
        &self,
        _: OrganizationId,
        _: i64,
        _: matos_core::domain::execution::DraftPullRequest,
    ) -> Result<matos_core::domain::execution::PullRequestUrl, CoreError> {
        unreachable!()
    }

    async fn refresh_user_token(
        &self,
        _: OrganizationId,
        refresh_token: &SecretToken,
    ) -> Result<UserTokens, CoreError> {
        self.refresh_calls
            .lock()
            .unwrap()
            .push(refresh_token.expose().to_owned());
        if let Some((usecase, who, won)) = &self.rival {
            usecase
                .connect_github_user(who.clone(), "the-code", &FakeGithub::new(won.clone()))
                .await?;
        }
        self.refreshed
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| CoreError::Invalid("bad_refresh_token".into()))
    }
}

async fn connected(usecase: &MatosUseCase, who: &Subject, exchanged: UserTokens) -> FakeGithub {
    let github = FakeGithub::new(exchanged);
    usecase
        .connect_github_user(who.clone(), "the-code", &github)
        .await
        .unwrap();
    github
}

#[tokio::test]
async fn connecting_stores_the_member_sealed_and_shows_only_the_login() {
    let Some(pool) = pool().await else { return };
    let organization_id = organization(&pool).await;
    let usecase = usecase(&pool, 7);
    let alice = subject(organization_id, "alice");
    assert!(
        !usecase
            .github_connection_status(alice.clone())
            .await
            .unwrap()
            .connected
    );

    let github = FakeGithub::new(tokens(ACCESS, Some(Duration::hours(8)), Some(REFRESH)));
    let status = usecase
        .connect_github_user(alice.clone(), "the-code", &github)
        .await
        .unwrap();
    assert!(status.connected);
    assert_eq!(status.login.as_deref(), Some("octocat"));

    let seen = usecase
        .github_connection_status(alice.clone())
        .await
        .unwrap();
    assert_eq!(seen.login.as_deref(), Some("octocat"));
    assert!(seen.connected_at.is_some());

    let row: (i64, Vec<u8>, Vec<u8>) = sqlx::query_as(
        "SELECT github_user_id, access_ciphertext, refresh_ciphertext FROM github_user_connections WHERE organization_id = $1",
    )
    .bind(organization_id.0)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.0, 583231);
    for sealed in [&row.1, &row.2] {
        for secret in [ACCESS, REFRESH] {
            assert!(!sealed.windows(secret.len()).any(|w| w == secret.as_bytes()));
        }
    }
}

#[tokio::test]
async fn connecting_again_replaces_the_tokens_and_keeps_the_first_connection_date() {
    let Some(pool) = pool().await else { return };
    let organization_id = organization(&pool).await;
    let usecase = usecase(&pool, 7);
    let alice = subject(organization_id, "alice");
    connected(&usecase, &alice, tokens(ACCESS, None, None)).await;
    let first = usecase
        .github_connection_status(alice.clone())
        .await
        .unwrap();

    let github = FakeGithub::new(tokens("ghu_second", None, None));
    usecase
        .connect_github_user(alice.clone(), "the-code", &github)
        .await
        .unwrap();

    let again = usecase
        .github_connection_status(alice.clone())
        .await
        .unwrap();
    assert_eq!(again.connected_at, first.connected_at);
    let token = usecase
        .member_git_access_token(organization_id, "alice")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(token.expose(), "ghu_second");
}

#[tokio::test]
async fn a_member_never_sees_nor_removes_another_members_connection() {
    let Some(pool) = pool().await else { return };
    let acme = organization(&pool).await;
    let other = organization(&pool).await;
    let usecase = usecase(&pool, 7);
    let alice = subject(acme, "alice");
    let github = connected(&usecase, &alice, tokens(ACCESS, None, None)).await;

    for stranger in [subject(acme, "bob"), subject(other, "alice")] {
        assert!(
            !usecase
                .github_connection_status(stranger.clone())
                .await
                .unwrap()
                .connected
        );
        usecase
            .disconnect_github_user(stranger, &github)
            .await
            .unwrap();
    }
    assert!(github.revoked.lock().unwrap().is_empty());
    assert!(
        usecase
            .github_connection_status(alice)
            .await
            .unwrap()
            .connected
    );
    assert!(
        usecase
            .member_git_access_token(acme, "bob")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn disconnecting_deletes_the_row_and_revokes_at_github_without_failing_on_a_refusal() {
    let Some(pool) = pool().await else { return };
    let organization_id = organization(&pool).await;
    let usecase = usecase(&pool, 7);
    let alice = subject(organization_id, "alice");
    let mut github = connected(&usecase, &alice, tokens(ACCESS, None, None)).await;
    github.revoke_fails = true;

    usecase
        .disconnect_github_user(alice.clone(), &github)
        .await
        .unwrap();

    assert_eq!(*github.revoked.lock().unwrap(), [ACCESS]);
    assert!(
        !usecase
            .github_connection_status(alice.clone())
            .await
            .unwrap()
            .connected
    );
    usecase
        .disconnect_github_user(alice, &github)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_ciphertext_moved_to_another_member_or_key_does_not_open() {
    let Some(pool) = pool().await else { return };
    let organization_id = organization(&pool).await;
    let usecase = usecase(&pool, 7);
    let alice = subject(organization_id, "alice");
    connected(&usecase, &alice, tokens(ACCESS, None, Some(REFRESH))).await;

    sqlx::query("UPDATE github_user_connections SET subject = 'bob' WHERE organization_id = $1")
        .bind(organization_id.0)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        usecase
            .member_git_access_token(organization_id, "bob")
            .await
            .is_err()
    );

    sqlx::query("UPDATE github_user_connections SET subject = 'alice' WHERE organization_id = $1")
        .bind(organization_id.0)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        self::usecase(&pool, 8)
            .member_git_access_token(organization_id, "alice")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_token_valid_long_enough_is_returned_without_asking_github() {
    let Some(pool) = pool().await else { return };
    let organization_id = organization(&pool).await;
    let usecase = usecase(&pool, 7);
    let alice = subject(organization_id, "alice");
    let github = connected(
        &usecase,
        &alice,
        tokens(ACCESS, Some(Duration::hours(8)), Some(REFRESH)),
    )
    .await;

    let token = usecase
        .member_git_token(organization_id, "alice", Duration::minutes(40), &github)
        .await
        .unwrap();

    assert_eq!(token.unwrap().expose(), ACCESS);
    assert!(github.refresh_calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_token_without_expiry_is_returned_as_it_is() {
    let Some(pool) = pool().await else { return };
    let organization_id = organization(&pool).await;
    let usecase = usecase(&pool, 7);
    let alice = subject(organization_id, "alice");
    let github = connected(&usecase, &alice, tokens(ACCESS, None, None)).await;

    let token = usecase
        .member_git_token(organization_id, "alice", Duration::days(30), &github)
        .await
        .unwrap();

    assert_eq!(token.unwrap().expose(), ACCESS);
    assert!(github.refresh_calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn an_expired_token_is_refreshed_and_the_new_pair_is_stored() {
    let Some(pool) = pool().await else { return };
    let organization_id = organization(&pool).await;
    let usecase = usecase(&pool, 7);
    let alice = subject(organization_id, "alice");
    let github = connected(
        &usecase,
        &alice,
        tokens(ACCESS, Some(Duration::minutes(-5)), Some(REFRESH)),
    )
    .await
    .refreshing_to(tokens(
        "ghu_access_secret_2",
        Some(Duration::hours(8)),
        Some("ghr_refresh_secret_2"),
    ));

    let token = usecase
        .member_git_token(organization_id, "alice", Duration::minutes(40), &github)
        .await
        .unwrap();

    assert_eq!(token.unwrap().expose(), "ghu_access_secret_2");
    assert_eq!(*github.refresh_calls.lock().unwrap(), [REFRESH]);
    let stored = usecase
        .member_git_access_token(organization_id, "alice")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.expose(), "ghu_access_secret_2");

    let again = usecase
        .member_git_token(organization_id, "alice", Duration::minutes(40), &github)
        .await
        .unwrap();
    assert_eq!(again.unwrap().expose(), "ghu_access_secret_2");
    assert_eq!(github.refresh_calls.lock().unwrap().len(), 1);

    let refresh: (Vec<u8>,) = sqlx::query_as(
        "SELECT refresh_ciphertext FROM github_user_connections WHERE organization_id = $1",
    )
    .bind(organization_id.0)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        !refresh
            .0
            .windows("ghr_refresh_secret_2".len())
            .any(|w| w == b"ghr_refresh_secret_2")
    );
}

#[tokio::test]
async fn a_refused_refresh_gives_no_token_and_keeps_the_connection() {
    let Some(pool) = pool().await else { return };
    let organization_id = organization(&pool).await;
    let usecase = usecase(&pool, 7);
    let alice = subject(organization_id, "alice");
    let github = connected(
        &usecase,
        &alice,
        tokens(ACCESS, Some(Duration::minutes(-5)), Some(REFRESH)),
    )
    .await;

    let token = usecase
        .member_git_token(organization_id, "alice", Duration::minutes(40), &github)
        .await
        .unwrap();

    assert!(token.is_none());
    assert!(
        usecase
            .github_connection_status(alice)
            .await
            .unwrap()
            .connected
    );
}

#[tokio::test]
async fn an_expiring_token_without_a_refresh_token_gives_no_token() {
    let Some(pool) = pool().await else { return };
    let organization_id = organization(&pool).await;
    let usecase = usecase(&pool, 7);
    let alice = subject(organization_id, "alice");
    let github = connected(
        &usecase,
        &alice,
        tokens(ACCESS, Some(Duration::minutes(5)), None),
    )
    .await;

    let token = usecase
        .member_git_token(organization_id, "alice", Duration::minutes(40), &github)
        .await
        .unwrap();

    assert!(token.is_none());
    assert!(github.refresh_calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_member_who_never_connected_has_no_token() {
    let Some(pool) = pool().await else { return };
    let organization_id = organization(&pool).await;
    let usecase = usecase(&pool, 7);
    let github = FakeGithub::new(tokens(ACCESS, None, None));

    assert!(
        usecase
            .member_git_token(organization_id, "alice", Duration::minutes(40), &github)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn the_proxy_reads_the_stored_token_and_never_refreshes_it() {
    let Some(pool) = pool().await else { return };
    let organization_id = organization(&pool).await;
    let usecase = usecase(&pool, 7);
    let github = connected(
        &usecase,
        &subject(organization_id, "alice"),
        tokens(ACCESS, Some(Duration::minutes(5)), Some(REFRESH)),
    )
    .await;
    connected(
        &usecase,
        &subject(organization_id, "bob"),
        tokens(ACCESS, Some(Duration::minutes(-5)), Some(REFRESH)),
    )
    .await;

    let alice = usecase
        .member_git_access_token(organization_id, "alice")
        .await
        .unwrap();
    assert_eq!(alice.unwrap().expose(), ACCESS);
    assert!(
        usecase
            .member_git_access_token(organization_id, "bob")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        usecase
            .member_git_access_token(organization_id, "carol")
            .await
            .unwrap()
            .is_none()
    );
    assert!(github.refresh_calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn the_authorize_url_is_the_ports() {
    let Some(pool) = pool().await else { return };
    let organization_id = organization(&pool).await;
    let usecase = usecase(&pool, 7);
    let github = FakeGithub::new(tokens(ACCESS, None, None));

    let url = usecase
        .github_authorize_url(subject(organization_id, "alice"), "signed", &github)
        .await
        .unwrap();

    assert_eq!(url, "https://github.test/authorize?state=signed");
}

#[tokio::test]
async fn a_refresh_lost_to_a_concurrent_one_uses_the_token_the_winner_stored() {
    let Some(pool) = pool().await else { return };
    let organization_id = organization(&pool).await;
    let usecase = usecase(&pool, 7);
    let alice = subject(organization_id, "alice");
    let mut github = connected(
        &usecase,
        &alice,
        tokens(ACCESS, Some(Duration::minutes(-5)), Some(REFRESH)),
    )
    .await;
    github.rival = Some((
        usecase.clone(),
        alice,
        tokens("ghu_winner", Some(Duration::hours(8)), Some("ghr_winner")),
    ));

    let token = usecase
        .member_git_token(organization_id, "alice", Duration::minutes(40), &github)
        .await
        .unwrap();

    assert_eq!(token.unwrap().expose(), "ghu_winner");
}

const PAT: &str = "github_pat_secret_value_1";

struct FakeValidator {
    expires_at: Option<chrono::DateTime<Utc>>,
    refuses: bool,
}

impl FakeValidator {
    fn accepting() -> Self {
        Self {
            expires_at: None,
            refuses: false,
        }
    }

    fn refusing() -> Self {
        Self {
            expires_at: None,
            refuses: true,
        }
    }
}

impl PersonalTokenValidator for FakeValidator {
    async fn validate(&self, token: &SecretToken) -> Result<PersonalTokenCheck, CoreError> {
        if self.refuses {
            return Err(CoreError::Invalid("Bad credentials".into()));
        }
        assert_eq!(token.expose(), PAT);
        Ok(PersonalTokenCheck {
            user: GithubUser {
                id: 583231,
                login: "octocat-pat".into(),
            },
            expires_at: self.expires_at,
        })
    }
}

async fn with_personal_token(usecase: &MatosUseCase, who: &Subject, validator: &FakeValidator) {
    usecase
        .save_github_personal_token(who.clone(), SecretToken::new(PAT), validator)
        .await
        .unwrap();
}

#[tokio::test]
async fn saving_a_personal_token_stores_it_sealed_and_shows_only_a_preview() {
    let Some(pool) = pool().await else { return };
    let organization_id = organization(&pool).await;
    let usecase = usecase(&pool, 7);
    let alice = subject(organization_id, "alice");
    let expires_at = Utc::now() + Duration::days(30);
    let validator = FakeValidator {
        expires_at: Some(expires_at),
        refuses: false,
    };

    let status = usecase
        .save_github_personal_token(
            alice.clone(),
            SecretToken::new(format!("  {PAT}\n")),
            &validator,
        )
        .await
        .unwrap();

    assert!(!status.connected);
    let shown = status.personal_token.unwrap();
    assert_eq!(shown.login, "octocat-pat");
    assert_eq!(shown.preview, "…ue_1");
    assert!(shown.active);
    assert_eq!(
        shown.expires_at.map(|at| at.timestamp()),
        Some(expires_at.timestamp())
    );
    assert!(!format!("{shown:?}").contains(PAT));

    let row: (Vec<u8>, String) = sqlx::query_as(
        "SELECT token_ciphertext, token_preview FROM github_personal_tokens WHERE organization_id = $1",
    )
    .bind(organization_id.0)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(!row.0.windows(PAT.len()).any(|w| w == PAT.as_bytes()));
    assert!(!row.1.contains("secret"));
    let read = usecase
        .member_git_access_token(organization_id, "alice")
        .await
        .unwrap();
    assert_eq!(read.unwrap().expose(), PAT);
}

#[tokio::test]
async fn a_token_github_refuses_is_not_stored() {
    let Some(pool) = pool().await else { return };
    let organization_id = organization(&pool).await;
    let usecase = usecase(&pool, 7);
    let alice = subject(organization_id, "alice");

    let error = usecase
        .save_github_personal_token(
            alice.clone(),
            SecretToken::new(PAT),
            &FakeValidator::refusing(),
        )
        .await
        .unwrap_err();
    assert!(matches!(error, CoreError::Invalid(_)), "{error:?}");
    let empty = usecase
        .save_github_personal_token(
            alice.clone(),
            SecretToken::new("  "),
            &FakeValidator::accepting(),
        )
        .await
        .unwrap_err();
    assert!(matches!(empty, CoreError::Invalid(_)), "{empty:?}");

    assert!(
        usecase
            .github_connection_status(alice)
            .await
            .unwrap()
            .personal_token
            .is_none()
    );
    let stored: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM github_personal_tokens WHERE organization_id = $1",
    )
    .bind(organization_id.0)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(stored, 0);
}

#[tokio::test]
async fn an_active_personal_token_wins_over_the_apps_authorization_in_both_readers() {
    let Some(pool) = pool().await else { return };
    let organization_id = organization(&pool).await;
    let usecase = usecase(&pool, 7);
    let alice = subject(organization_id, "alice");
    let github = connected(
        &usecase,
        &alice,
        tokens(ACCESS, Some(Duration::minutes(-5)), Some(REFRESH)),
    )
    .await
    .refreshing_to(tokens(
        "ghu_access_secret_2",
        Some(Duration::hours(8)),
        None,
    ));
    with_personal_token(&usecase, &alice, &FakeValidator::accepting()).await;

    let worker = usecase
        .member_git_token(organization_id, "alice", Duration::minutes(40), &github)
        .await
        .unwrap();
    let proxy = usecase
        .member_git_access_token(organization_id, "alice")
        .await
        .unwrap();

    assert_eq!(worker.unwrap().expose(), PAT);
    assert_eq!(proxy.unwrap().expose(), PAT);
    assert!(github.refresh_calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_personal_token_alone_is_enough_without_the_apps_authorization() {
    let Some(pool) = pool().await else { return };
    let organization_id = organization(&pool).await;
    let usecase = usecase(&pool, 7);
    let github = FakeGithub::new(tokens(ACCESS, None, None));
    with_personal_token(
        &usecase,
        &subject(organization_id, "alice"),
        &FakeValidator::accepting(),
    )
    .await;

    let token = usecase
        .member_git_token(organization_id, "alice", Duration::minutes(40), &github)
        .await
        .unwrap();

    assert_eq!(token.unwrap().expose(), PAT);
}

#[tokio::test]
async fn an_inactive_personal_token_gives_way_to_the_apps_authorization() {
    let Some(pool) = pool().await else { return };
    let organization_id = organization(&pool).await;
    let usecase = usecase(&pool, 7);
    let alice = subject(organization_id, "alice");
    let github = connected(
        &usecase,
        &alice,
        tokens(ACCESS, Some(Duration::hours(8)), Some(REFRESH)),
    )
    .await;
    with_personal_token(&usecase, &alice, &FakeValidator::accepting()).await;

    let status = usecase
        .set_github_personal_token_active(alice.clone(), false)
        .await
        .unwrap();
    assert!(!status.personal_token.unwrap().active);
    assert!(status.connected);

    let worker = usecase
        .member_git_token(organization_id, "alice", Duration::minutes(40), &github)
        .await
        .unwrap();
    let proxy = usecase
        .member_git_access_token(organization_id, "alice")
        .await
        .unwrap();
    assert_eq!(worker.unwrap().expose(), ACCESS);
    assert_eq!(proxy.unwrap().expose(), ACCESS);

    let status = usecase
        .set_github_personal_token_active(alice, true)
        .await
        .unwrap();
    assert!(status.personal_token.unwrap().active);
    let again = usecase
        .member_git_access_token(organization_id, "alice")
        .await
        .unwrap();
    assert_eq!(again.unwrap().expose(), PAT);
}

#[tokio::test]
async fn an_expired_personal_token_gives_way_to_the_apps_authorization_and_is_never_refreshed() {
    let Some(pool) = pool().await else { return };
    let organization_id = organization(&pool).await;
    let usecase = usecase(&pool, 7);
    let alice = subject(organization_id, "alice");
    let github = connected(
        &usecase,
        &alice,
        tokens(ACCESS, Some(Duration::hours(8)), Some(REFRESH)),
    )
    .await;
    let validator = FakeValidator {
        expires_at: Some(Utc::now() - Duration::minutes(1)),
        refuses: false,
    };
    with_personal_token(&usecase, &alice, &validator).await;

    let worker = usecase
        .member_git_token(organization_id, "alice", Duration::minutes(40), &github)
        .await
        .unwrap();
    let proxy = usecase
        .member_git_access_token(organization_id, "alice")
        .await
        .unwrap();

    assert_eq!(worker.unwrap().expose(), ACCESS);
    assert_eq!(proxy.unwrap().expose(), ACCESS);
    assert!(github.refresh_calls.lock().unwrap().is_empty());

    let bob = subject(organization_id, "bob");
    with_personal_token(&usecase, &bob, &validator).await;
    assert!(
        usecase
            .member_git_token(organization_id, "bob", Duration::minutes(40), &github)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        usecase
            .member_git_access_token(organization_id, "bob")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn saving_again_replaces_the_token_and_reactivates_it() {
    let Some(pool) = pool().await else { return };
    let organization_id = organization(&pool).await;
    let usecase = usecase(&pool, 7);
    let alice = subject(organization_id, "alice");
    with_personal_token(&usecase, &alice, &FakeValidator::accepting()).await;
    usecase
        .set_github_personal_token_active(alice.clone(), false)
        .await
        .unwrap();

    with_personal_token(&usecase, &alice, &FakeValidator::accepting()).await;

    let status = usecase.github_connection_status(alice).await.unwrap();
    assert!(status.personal_token.unwrap().active);
}

#[tokio::test]
async fn a_member_never_reads_nor_changes_another_members_personal_token() {
    let Some(pool) = pool().await else { return };
    let acme = organization(&pool).await;
    let other = organization(&pool).await;
    let usecase = usecase(&pool, 7);
    let alice = subject(acme, "alice");
    with_personal_token(&usecase, &alice, &FakeValidator::accepting()).await;

    for stranger in [subject(acme, "bob"), subject(other, "alice")] {
        assert!(
            usecase
                .github_connection_status(stranger.clone())
                .await
                .unwrap()
                .personal_token
                .is_none()
        );
        assert!(matches!(
            usecase
                .set_github_personal_token_active(stranger.clone(), false)
                .await,
            Err(CoreError::NotFound)
        ));
        usecase
            .delete_github_personal_token(stranger)
            .await
            .unwrap();
    }
    assert!(
        usecase
            .member_git_access_token(other, "alice")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        usecase
            .github_connection_status(alice)
            .await
            .unwrap()
            .personal_token
            .unwrap()
            .active
    );
}

#[tokio::test]
async fn deleting_removes_the_personal_token_and_is_idempotent() {
    let Some(pool) = pool().await else { return };
    let organization_id = organization(&pool).await;
    let usecase = usecase(&pool, 7);
    let alice = subject(organization_id, "alice");
    with_personal_token(&usecase, &alice, &FakeValidator::accepting()).await;

    usecase
        .delete_github_personal_token(alice.clone())
        .await
        .unwrap();
    usecase
        .delete_github_personal_token(alice.clone())
        .await
        .unwrap();

    assert!(
        usecase
            .github_connection_status(alice)
            .await
            .unwrap()
            .personal_token
            .is_none()
    );
    assert!(
        usecase
            .member_git_access_token(organization_id, "alice")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_personal_token_ciphertext_moved_to_another_member_or_key_does_not_open() {
    let Some(pool) = pool().await else { return };
    let organization_id = organization(&pool).await;
    let usecase = usecase(&pool, 7);
    with_personal_token(
        &usecase,
        &subject(organization_id, "alice"),
        &FakeValidator::accepting(),
    )
    .await;

    sqlx::query("UPDATE github_personal_tokens SET subject = 'bob' WHERE organization_id = $1")
        .bind(organization_id.0)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        usecase
            .member_git_access_token(organization_id, "bob")
            .await
            .unwrap()
            .is_none()
    );

    sqlx::query("UPDATE github_personal_tokens SET subject = 'alice' WHERE organization_id = $1")
        .bind(organization_id.0)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        self::usecase(&pool, 8)
            .member_git_access_token(organization_id, "alice")
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        usecase
            .member_git_access_token(organization_id, "alice")
            .await
            .unwrap()
            .unwrap()
            .expose(),
        PAT
    );
}
