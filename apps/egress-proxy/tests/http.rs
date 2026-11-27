#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{
    collections::HashMap,
    convert::Infallible,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{
    Router,
    body::{Body, Bytes},
    extract::{Request, State},
    http::{HeaderMap, Method, StatusCode},
    response::Response,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use chrono::Utc;
use futures_util::stream;
use matos_common::{OrganizationId, RunId};
use matos_core::domain::{
    agent::SecretToken,
    execution::{ExecutionMode, RepositoryRef, TaskGrant},
    github_user::GitIdentity,
    provider::ProviderKind,
};
use matos_egress_proxy::{
    Credentials, CredentialsError, Proxy, ProxyConfig, RegistryConfig, router,
};
use tokio::{net::TcpListener, sync::mpsc};

const TASK_TOKEN: &str = "task-token-0001";
const ANTHROPIC_KEY: &str = "sk-ant-real-key";
const OPENAI_KEY: &str = "sk-openai-real-key";
const GOOGLE_KEY: &str = "AIza-real-key";
const CODEX_MODEL: &str = "gpt-6-luna";
const AGY_MODEL: &str = "gemini-3.1-pro";
const AGY_WIRE: &str = "gemini-3.1-pro-preview";
const HELPER_WIRE: &str = "gemini-3.1-flash-lite-preview";
const MISTRAL_KEY: &str = "mistral-real-key";
const VIBE_MODEL: &str = "mistral-medium-3.5";
const VIBE_LARGE: &str = "mistral-large-4-0";
const VIBE_MEDIUM_WIRE: &str = "mistral-vibe-cli-latest";
const VIBE_BODY: &str = include_str!("fixtures/vibe-request-body.json");
const AGY_BODY: &str = include_str!("fixtures/agy-request-body.json");
const INSTALLATION_TOKEN: &str = "ghs_installation";
const READ_ONLY_TOKEN: &str = "ghs_read_only";
const INITIATOR: &str = "user-1";
const MEMBER_TOKEN: &str = "ghu_member";
const INITIATOR_KEY: &str = "sk-ant-initiator-key";
const MODEL: &str = "claude-sonnet-5-5";
const BRANCH: &str = "matos/0a1b2c3d4e5f";
const OLD: &str = "1111111111111111111111111111111111111111";
const NEW: &str = "2222222222222222222222222222222222222222";
const MESSAGE: &str = r#"{"model":"claude-sonnet-5-5","max_tokens":1024}"#;
const ZERO: &str = "0000000000000000000000000000000000000000";

fn no_registries() -> RegistryConfig {
    RegistryConfig {
        crates_index_base: "http://127.0.0.1:1".into(),
        crates_static_base: "http://127.0.0.1:1".into(),
        npm_base: "http://127.0.0.1:1".into(),
        public_base: None,
    }
}

fn sha256(token: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(
        aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, token.as_bytes()).as_ref(),
    );
    out
}

fn codex_grant() -> TaskGrant {
    TaskGrant {
        model: CODEX_MODEL.into(),
        ..grant()
    }
}

fn agy_grant() -> TaskGrant {
    TaskGrant {
        model: AGY_MODEL.into(),
        ..grant()
    }
}

fn vibe_grant() -> TaskGrant {
    TaskGrant {
        model: VIBE_LARGE.into(),
        ..grant()
    }
}

fn grant() -> TaskGrant {
    TaskGrant {
        run_id: RunId::new(),
        organization_id: OrganizationId(uuid::Uuid::nil()),
        repository: "acme/web".parse().unwrap(),
        branch: BRANCH.into(),
        model: MODEL.into(),
        mode: ExecutionMode::Change,
        companions: Vec::new(),
        writable_repositories: Vec::new(),
        initiated_by: Some(INITIATOR.into()),
        git_identity: GitIdentity::App,
        expires_at: Utc::now() + chrono::Duration::minutes(30),
        revoked_at: None,
    }
}

struct Fake {
    grants: HashMap<[u8; 32], TaskGrant>,
    anthropic_key: Option<&'static str>,
    openai_key: Option<&'static str>,
    google_key: Option<&'static str>,
    mistral_key: Option<&'static str>,
    user_keys: HashMap<&'static str, &'static str>,
    github_token: Option<&'static str>,
    member_tokens: HashMap<&'static str, &'static str>,
    lookups: Arc<AtomicUsize>,
    token_modes: Arc<Mutex<Vec<ExecutionMode>>>,
}

impl Fake {
    fn with(grant: TaskGrant) -> Self {
        Self {
            grants: HashMap::from([(sha256(TASK_TOKEN), grant)]),
            anthropic_key: Some(ANTHROPIC_KEY),
            openai_key: Some(OPENAI_KEY),
            google_key: Some(GOOGLE_KEY),
            mistral_key: Some(MISTRAL_KEY),
            user_keys: HashMap::new(),
            github_token: Some(INSTALLATION_TOKEN),
            member_tokens: HashMap::new(),
            lookups: Arc::new(AtomicUsize::new(0)),
            token_modes: Arc::default(),
        }
    }
}

impl Credentials for Fake {
    async fn grant(&self, token_sha256: [u8; 32]) -> Result<Option<TaskGrant>, CredentialsError> {
        self.lookups.fetch_add(1, Ordering::SeqCst);
        Ok(self.grants.get(&token_sha256).cloned())
    }

    async fn executor_key(
        &self,
        provider: ProviderKind,
        _: OrganizationId,
        initiated_by: Option<&str>,
    ) -> Result<Option<SecretToken>, CredentialsError> {
        let key = match provider {
            ProviderKind::Anthropic => {
                let own = initiated_by.and_then(|subject| self.user_keys.get(subject).copied());
                own.or(self.anthropic_key)
            }
            ProviderKind::OpenAi => self.openai_key,
            ProviderKind::Google => self.google_key,
            ProviderKind::Mistral => self.mistral_key,
            _ => None,
        };
        Ok(key.map(SecretToken::new))
    }

    async fn github_token(
        &self,
        _: OrganizationId,
        _: &RepositoryRef,
        mode: ExecutionMode,
    ) -> Result<Option<SecretToken>, CredentialsError> {
        self.token_modes.lock().unwrap().push(mode);
        Ok(self.github_token.map(|token| match mode {
            ExecutionMode::Change => SecretToken::new(token),
            ExecutionMode::Audit => SecretToken::new(READ_ONLY_TOKEN),
        }))
    }

    async fn member_github_token(
        &self,
        _: OrganizationId,
        subject: &str,
    ) -> Result<Option<SecretToken>, CredentialsError> {
        Ok(self
            .member_tokens
            .get(subject)
            .copied()
            .map(SecretToken::new))
    }
}

#[derive(Debug, Clone)]
struct Captured {
    method: Method,
    uri: String,
    headers: HeaderMap,
    body: Bytes,
}

type Log = Arc<Mutex<Vec<Captured>>>;

async fn capture(State(log): State<Log>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let body = axum::body::to_bytes(body, usize::MAX).await.unwrap();
    log.lock().unwrap().push(Captured {
        method: parts.method,
        uri: parts.uri.to_string(),
        headers: parts.headers,
        body,
    });
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/x-test")
        .body(Body::from("upstream-ok"))
        .unwrap()
}

async fn serve(app: Router) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

async fn capturing_upstream() -> (String, Log) {
    let log = Log::default();
    let app = Router::new().fallback(capture).with_state(log.clone());
    (serve(app).await, log)
}

struct Harness {
    url: String,
    anthropic: Log,
    openai: Log,
    google: Log,
    mistral: Log,
    github: Log,
    client: reqwest::Client,
    lookups: Arc<AtomicUsize>,
}

fn plain_client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

async fn harness_with(fake: Fake) -> Harness {
    let (anthropic_url, anthropic) = capturing_upstream().await;
    let (github_url, github) = capturing_upstream().await;
    start(fake, anthropic_url, github_url, anthropic, github).await
}

async fn start_openai(fake: Fake, openai_url: String, openai: Log) -> Harness {
    let (anthropic_url, anthropic) = capturing_upstream().await;
    let (github_url, github) = capturing_upstream().await;
    let mut h = start_with(
        fake,
        anthropic_url,
        openai_url,
        github_url,
        anthropic,
        github,
        capturing_upstream().await,
    )
    .await;
    h.openai = openai;
    h
}

async fn start(
    fake: Fake,
    anthropic_url: String,
    github_url: String,
    anthropic: Log,
    github: Log,
) -> Harness {
    let (openai_url, openai) = capturing_upstream().await;
    let mut h = start_with(
        fake,
        anthropic_url,
        openai_url,
        github_url,
        anthropic,
        github,
        capturing_upstream().await,
    )
    .await;
    h.openai = openai;
    h
}

async fn start_with(
    fake: Fake,
    anthropic_url: String,
    openai_url: String,
    github_url: String,
    anthropic: Log,
    github: Log,
    google: (String, Log),
) -> Harness {
    let lookups = fake.lookups.clone();
    let (google_url, google) = google;
    let (mistral_url, mistral) = capturing_upstream().await;
    let proxy = Proxy::with_http(
        fake,
        ProxyConfig {
            anthropic_base: anthropic_url,
            openai_base: openai_url,
            google_base: google_url,
            mistral_base: mistral_url,
            github_base: github_url,
            registries: no_registries(),
        },
        plain_client(),
    );
    Harness {
        url: serve(router(proxy)).await,
        anthropic,
        openai: Log::default(),
        google,
        mistral,
        github,
        client: plain_client(),
        lookups,
    }
}

async fn harness() -> Harness {
    harness_with(Fake::with(grant())).await
}

fn basic(token: &str) -> String {
    format!(
        "Basic {}",
        STANDARD.encode(format!("x-access-token:{token}"))
    )
}

fn pkt(payload: &[u8]) -> Vec<u8> {
    let mut out = format!("{:04x}", payload.len() + 4).into_bytes();
    out.extend_from_slice(payload);
    out
}

fn push_body(commands: &[(&str, &str, &str)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (index, (old, new, refname)) in commands.iter().enumerate() {
        let mut line = format!("{old} {new} {refname}").into_bytes();
        if index == 0 {
            line.extend_from_slice(b"\0report-status agent=git/2.45");
        }
        out.extend(pkt(&line));
    }
    out.extend_from_slice(b"0000PACK-the-pack-data");
    out
}

fn branch_ref() -> String {
    format!("refs/heads/{BRANCH}")
}

impl Harness {
    async fn messages(&self, path: &str, token: &str) -> reqwest::Response {
        self.client
            .post(format!("{}/anthropic{path}", self.url))
            .header("x-api-key", token)
            .header("anthropic-version", "2023-06-01")
            .header("anthropic-beta", "prompt-caching-2024-07-31, tools-2024")
            .header("content-type", "application/json")
            .body(MESSAGE)
            .send()
            .await
            .unwrap()
    }

    async fn messages_with(&self, body: serde_json::Value) -> reqwest::Response {
        self.client
            .post(format!("{}/anthropic/v1/messages", self.url))
            .header("x-api-key", TASK_TOKEN)
            .header("content-type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .unwrap()
    }

    async fn responses(&self, token: &str, body: serde_json::Value) -> reqwest::Response {
        self.client
            .post(format!("{}/openai/v1/responses", self.url))
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
            .header("openai-organization", "org-container")
            .header("openai-project", "proj-container")
            .body(body.to_string())
            .send()
            .await
            .unwrap()
    }

    async fn generate(
        &self,
        path: &str,
        token_header: (&str, &str),
        body: &str,
    ) -> reqwest::Response {
        self.client
            .post(format!("{}/google{path}", self.url))
            .header(token_header.0, token_header.1)
            .header("content-type", "application/json")
            .header("accept-encoding", "x-agy-coding")
            .header("x-goog-api-client", "gl-go/1.0 container-token")
            .header("user-agent", "agy/1.5.3")
            .body(body.to_owned())
            .send()
            .await
            .unwrap()
    }

    async fn agy(&self, body: &str) -> reqwest::Response {
        self.generate(&stream_path(AGY_WIRE), ("x-goog-api-key", TASK_TOKEN), body)
            .await
    }

    async fn vibe_at(&self, path: &str, token: &str, body: &str) -> reqwest::Response {
        self.client
            .post(format!("{}/mistral{path}", self.url))
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
            .header("accept-encoding", "x-vibe-coding")
            .header("user-agent", "Mistral-Vibe/3.1.4")
            .header("x-affinity", "container-session")
            .body(body.to_owned())
            .send()
            .await
            .unwrap()
    }

    async fn vibe(&self, body: &str) -> reqwest::Response {
        self.vibe_at("/v1/chat/completions", TASK_TOKEN, body).await
    }

    async fn git_get(&self, path: &str, token: &str) -> reqwest::Response {
        self.client
            .get(format!("{}/git/{path}", self.url))
            .header("authorization", basic(token))
            .send()
            .await
            .unwrap()
    }

    async fn git_post(&self, path: &str, token: &str, body: Vec<u8>) -> reqwest::Response {
        self.client
            .post(format!("{}/git/{path}", self.url))
            .header("authorization", basic(token))
            .header("content-type", "application/x-git-receive-pack-request")
            .body(body)
            .send()
            .await
            .unwrap()
    }

    fn received(log: &Log) -> Vec<Captured> {
        log.lock().unwrap().clone()
    }
}

#[tokio::test]
async fn healthz_answers_and_unknown_routes_are_404() {
    let h = harness().await;
    let ok = h
        .client
        .get(format!("{}/healthz", h.url))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), 200);
    let other = h
        .client
        .get(format!("{}/elsewhere", h.url))
        .send()
        .await
        .unwrap();
    assert_eq!(other.status(), 404);
}

#[tokio::test]
async fn an_unknown_token_is_401_with_the_same_body_as_a_revoked_one() {
    let mut revoked = grant();
    revoked.revoked_at = Some(Utc::now());
    let h = harness_with(Fake::with(revoked)).await;

    let unknown = h.messages("/v1/messages", "who-knows").await;
    let unknown_status = unknown.status();
    let unknown_body = unknown.text().await.unwrap();
    let refused = h.messages("/v1/messages", TASK_TOKEN).await;
    assert_eq!(unknown_status, 401);
    assert_eq!(refused.status(), 401);
    assert_eq!(refused.text().await.unwrap(), unknown_body);
    assert!(!unknown_body.contains("who-knows"));
    assert!(Harness::received(&h.anthropic).is_empty());

    let git = h
        .git_get(
            "acme/web.git/info/refs?service=git-upload-pack",
            "who-knows",
        )
        .await;
    assert_eq!(git.status(), 401);
}

#[tokio::test]
async fn git_without_credentials_gets_a_basic_challenge() {
    let h = harness().await;

    let response = h
        .client
        .get(format!(
            "{}/git/acme/web.git/info/refs?service=git-upload-pack",
            h.url
        ))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 401);
    assert_eq!(
        response.headers()["www-authenticate"],
        r#"Basic realm="matos""#
    );
}

#[tokio::test]
async fn a_request_without_credentials_is_401() {
    let h = harness().await;
    let anthropic = h
        .client
        .post(format!("{}/anthropic/v1/messages", h.url))
        .send()
        .await
        .unwrap();
    assert_eq!(anthropic.status(), 401);
    let git = h
        .client
        .get(format!(
            "{}/git/acme/web.git/info/refs?service=git-upload-pack",
            h.url
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(git.status(), 401);
}

#[tokio::test]
async fn git_basic_auth_needs_the_access_token_user() {
    let h = harness().await;
    let response = h
        .client
        .get(format!(
            "{}/git/acme/web.git/info/refs?service=git-upload-pack",
            h.url
        ))
        .header(
            "authorization",
            format!("Basic {}", STANDARD.encode(format!("bob:{TASK_TOKEN}"))),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
}

#[tokio::test]
async fn spec_exec_6_a_revoked_or_expired_token_is_refused() {
    for mutate in [
        (|g: &mut TaskGrant| g.revoked_at = Some(Utc::now())) as fn(&mut TaskGrant),
        |g| g.expires_at = Utc::now() - chrono::Duration::seconds(1),
    ] {
        let mut dead = grant();
        mutate(&mut dead);
        let h = harness_with(Fake::with(dead)).await;
        assert_eq!(h.messages("/v1/messages", TASK_TOKEN).await.status(), 401);
        let git = h
            .git_get("acme/web.git/info/refs?service=git-upload-pack", TASK_TOKEN)
            .await;
        assert_eq!(git.status(), 401);
        assert!(Harness::received(&h.anthropic).is_empty());
        assert!(Harness::received(&h.github).is_empty());
    }
}

#[tokio::test]
async fn a_valid_lookup_is_reused_briefly() {
    let h = harness().await;
    for _ in 0..3 {
        assert_eq!(h.messages("/v1/messages", TASK_TOKEN).await.status(), 200);
    }
    assert_eq!(h.lookups.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn anthropic_messages_are_forwarded_with_the_real_key_only() {
    let h = harness().await;
    let response = h.messages("/v1/messages?beta=true", TASK_TOKEN).await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.text().await.unwrap(), "upstream-ok");

    let seen = Harness::received(&h.anthropic);
    assert_eq!(seen.len(), 1);
    let request = &seen[0];
    assert_eq!(request.method, Method::POST);
    assert_eq!(request.uri, "/v1/messages?beta=true");
    assert_eq!(request.headers["x-api-key"], ANTHROPIC_KEY);
    assert_eq!(request.headers["anthropic-version"], "2023-06-01");
    assert_eq!(
        request.headers["anthropic-beta"],
        "prompt-caching-2024-07-31"
    );
    assert_eq!(request.body, Bytes::from_static(MESSAGE.as_bytes()));
    assert!(request.headers.get("authorization").is_none());
    for (_, value) in &request.headers {
        assert!(!value.to_str().unwrap_or_default().contains(TASK_TOKEN));
    }
}

#[tokio::test]
async fn an_oauth_token_is_forwarded_as_a_bearer_with_the_oauth_beta() {
    const OAUTH_TOKEN: &str = "sk-ant-oat01-real-token";
    let mut fake = Fake::with(grant());
    fake.anthropic_key = Some(OAUTH_TOKEN);
    let h = harness_with(fake).await;
    assert_eq!(h.messages("/v1/messages", TASK_TOKEN).await.status(), 200);

    let seen = Harness::received(&h.anthropic);
    let request = &seen[0];
    assert_eq!(
        request.headers["authorization"],
        format!("Bearer {OAUTH_TOKEN}")
    );
    assert_eq!(
        request.headers["anthropic-beta"],
        "prompt-caching-2024-07-31,oauth-2025-04-20"
    );
    assert!(request.headers.get("x-api-key").is_none());
}

#[tokio::test]
async fn a_bearer_task_token_and_count_tokens_are_accepted() {
    let h = harness().await;
    let response = h
        .client
        .post(format!("{}/anthropic/v1/messages/count_tokens", h.url))
        .bearer_auth(TASK_TOKEN)
        .body(r#"{"model":"claude-sonnet-5-5"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let seen = Harness::received(&h.anthropic);
    assert_eq!(seen[0].uri, "/v1/messages/count_tokens");
    assert_eq!(seen[0].headers["x-api-key"], ANTHROPIC_KEY);
    assert!(seen[0].headers.get("authorization").is_none());
}

#[tokio::test]
async fn other_anthropic_paths_and_methods_are_403() {
    let h = harness().await;
    for path in [
        "/v1/complete",
        "/v1/models",
        "/v1/messages/batches",
        "/v1/organizations/me",
        "/v1/messages/",
    ] {
        let response = h.messages(path, TASK_TOKEN).await;
        assert_eq!(response.status(), 403, "{path}");
        assert!(!response.text().await.unwrap().contains(TASK_TOKEN));
    }
    let get = h
        .client
        .get(format!("{}/anthropic/v1/messages", h.url))
        .header("x-api-key", TASK_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(get.status(), 403);
    assert!(Harness::received(&h.anthropic).is_empty());
}

#[tokio::test]
async fn an_organization_without_an_anthropic_key_gets_a_clear_403() {
    let mut fake = Fake::with(grant());
    fake.anthropic_key = None;
    let h = harness_with(fake).await;
    let response = h.messages("/v1/messages", TASK_TOKEN).await;
    assert_eq!(response.status(), 403);
    assert!(response.text().await.unwrap().contains("Anthropic key"));
    assert!(Harness::received(&h.anthropic).is_empty());
}

#[tokio::test]
async fn upstream_responses_are_streamed_back_as_they_arrive() {
    let (tx, rx) = mpsc::channel::<Bytes>(4);
    let rx = Arc::new(tokio::sync::Mutex::new(Some(rx)));
    let upstream = Router::new()
        .fallback(
            |State(rx): State<Arc<tokio::sync::Mutex<Option<mpsc::Receiver<Bytes>>>>>| async move {
                let rx = rx.lock().await.take().unwrap();
                let body = Body::from_stream(stream::unfold(rx, |mut rx| async {
                    rx.recv()
                        .await
                        .map(|chunk| (Ok::<_, Infallible>(chunk), rx))
                }));
                Response::builder()
                    .header("content-type", "text/event-stream")
                    .body(body)
                    .unwrap()
            },
        )
        .with_state(rx);
    let anthropic_url = serve(upstream).await;
    let (github_url, github) = capturing_upstream().await;
    let h = start(
        Fake::with(grant()),
        anthropic_url,
        github_url,
        Log::default(),
        github,
    )
    .await;

    let mut response = h.messages("/v1/messages", TASK_TOKEN).await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-type"], "text/event-stream");

    tx.send(Bytes::from_static(b"data: one\n\n")).await.unwrap();
    let first = tokio::time::timeout(Duration::from_secs(5), response.chunk())
        .await
        .expect("the first event must arrive before the upstream finishes")
        .unwrap()
        .unwrap();
    assert_eq!(first, Bytes::from_static(b"data: one\n\n"));

    tx.send(Bytes::from_static(b"data: two\n\n")).await.unwrap();
    drop(tx);
    let mut rest = Vec::new();
    while let Some(chunk) = response.chunk().await.unwrap() {
        rest.extend_from_slice(&chunk);
    }
    assert_eq!(rest, b"data: two\n\n");
}

#[tokio::test]
async fn upstream_error_statuses_pass_through() {
    let upstream =
        Router::new().fallback(|| async { (StatusCode::TOO_MANY_REQUESTS, "slow down") });
    let anthropic_url = serve(upstream).await;
    let (github_url, github) = capturing_upstream().await;
    let h = start(
        Fake::with(grant()),
        anthropic_url,
        github_url,
        Log::default(),
        github,
    )
    .await;
    let response = h.messages("/v1/messages", TASK_TOKEN).await;
    assert_eq!(response.status(), 429);
    assert_eq!(response.text().await.unwrap(), "slow down");
}

#[tokio::test]
async fn git_fetch_is_forwarded_with_the_installation_token() {
    let h = harness().await;
    let refs = h
        .git_get("acme/web.git/info/refs?service=git-upload-pack", TASK_TOKEN)
        .await;
    assert_eq!(refs.status(), 200);
    assert_eq!(refs.text().await.unwrap(), "upstream-ok");
    let pack = h
        .git_post(
            "acme/web.git/git-upload-pack",
            TASK_TOKEN,
            b"0032want abc\n0000".to_vec(),
        )
        .await;
    assert_eq!(pack.status(), 200);

    let seen = Harness::received(&h.github);
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].method, Method::GET);
    assert_eq!(
        seen[0].uri,
        "/acme/web.git/info/refs?service=git-upload-pack"
    );
    assert_eq!(seen[1].uri, "/acme/web.git/git-upload-pack");
    assert_eq!(seen[1].body, Bytes::from_static(b"0032want abc\n0000"));
    for request in &seen {
        assert_eq!(
            request.headers["authorization"],
            basic(INSTALLATION_TOKEN).as_str()
        );
        for (_, value) in &request.headers {
            assert!(!value.to_str().unwrap_or_default().contains(TASK_TOKEN));
        }
    }
}

fn read_only() -> Fake {
    Fake::with(TaskGrant {
        mode: ExecutionMode::Audit,
        ..grant()
    })
}

#[tokio::test]
async fn a_read_only_run_fetches_with_a_read_only_token() {
    let fake = read_only();
    let modes = fake.token_modes.clone();
    let h = harness_with(fake).await;
    let refs = h
        .git_get("acme/web.git/info/refs?service=git-upload-pack", TASK_TOKEN)
        .await;
    assert_eq!(refs.status(), 200);
    let pack = h
        .git_post(
            "acme/web.git/git-upload-pack",
            TASK_TOKEN,
            b"0032want abc\n0000".to_vec(),
        )
        .await;
    assert_eq!(pack.status(), 200);
    let seen = Harness::received(&h.github);
    assert_eq!(seen.len(), 2);
    for request in &seen {
        assert_eq!(
            request.headers["authorization"],
            basic(READ_ONLY_TOKEN).as_str()
        );
    }
    assert_eq!(
        *modes.lock().unwrap(),
        [ExecutionMode::Audit, ExecutionMode::Audit]
    );
}

#[tokio::test]
async fn a_read_only_run_cannot_push_at_all() {
    let fake = read_only();
    let modes = fake.token_modes.clone();
    let h = harness_with(fake).await;
    let advertised = h
        .git_get(
            "acme/web.git/info/refs?service=git-receive-pack",
            TASK_TOKEN,
        )
        .await;
    assert_eq!(advertised.status(), 403);
    assert!(advertised.text().await.unwrap().contains("read-only"));
    let push = h
        .git_post(
            "acme/web.git/git-receive-pack",
            TASK_TOKEN,
            push_body(&[(OLD, NEW, &branch_ref())]),
        )
        .await;
    assert_eq!(push.status(), 403);
    assert!(Harness::received(&h.github).is_empty());
    assert!(modes.lock().unwrap().is_empty(), "no token was minted");
}

#[tokio::test]
async fn a_change_run_asks_for_a_writing_token() {
    let fake = Fake::with(grant());
    let modes = fake.token_modes.clone();
    let h = harness_with(fake).await;
    let response = h
        .git_post(
            "acme/web.git/git-receive-pack",
            TASK_TOKEN,
            push_body(&[(OLD, NEW, &branch_ref())]),
        )
        .await;
    assert_eq!(response.status(), 200);
    assert_eq!(*modes.lock().unwrap(), [ExecutionMode::Change]);
}

#[tokio::test]
async fn the_initiators_own_anthropic_key_comes_before_the_organizations() {
    let mut fake = Fake::with(grant());
    fake.user_keys.insert(INITIATOR, INITIATOR_KEY);
    let h = harness_with(fake).await;
    let response = h.messages("/v1/messages", TASK_TOKEN).await;
    assert_eq!(response.status(), 200);
    let seen = Harness::received(&h.anthropic);
    assert_eq!(seen[0].headers["x-api-key"], INITIATOR_KEY);

    let mut someone_else = Fake::with(grant());
    someone_else.user_keys.insert("user-2", INITIATOR_KEY);
    let h = harness_with(someone_else).await;
    assert_eq!(h.messages("/v1/messages", TASK_TOKEN).await.status(), 200);
    assert_eq!(
        Harness::received(&h.anthropic)[0].headers["x-api-key"],
        ANTHROPIC_KEY
    );

    let mut legacy = Fake::with(TaskGrant {
        initiated_by: None,
        ..grant()
    });
    legacy.user_keys.insert(INITIATOR, INITIATOR_KEY);
    let h = harness_with(legacy).await;
    assert_eq!(h.messages("/v1/messages", TASK_TOKEN).await.status(), 200);
    assert_eq!(
        Harness::received(&h.anthropic)[0].headers["x-api-key"],
        ANTHROPIC_KEY
    );
}

#[tokio::test]
async fn spec_exec_5_another_repository_is_403() {
    let h = harness().await;
    for path in [
        "acme/autre.git/info/refs?service=git-upload-pack",
        "other/web.git/info/refs?service=git-upload-pack",
        "acme/web/info/refs?service=git-upload-pack",
        "acme/web.git.git/info/refs?service=git-upload-pack",
    ] {
        let response = h.git_get(path, TASK_TOKEN).await;
        assert_eq!(response.status(), 403, "{path}");
    }
    let push = h
        .git_post(
            "acme/autre.git/git-receive-pack",
            TASK_TOKEN,
            push_body(&[(OLD, NEW, &branch_ref())]),
        )
        .await;
    assert_eq!(push.status(), 403);
    assert!(Harness::received(&h.github).is_empty());
}

#[tokio::test]
async fn spec_exec_5_a_push_to_the_execution_branch_passes() {
    let h = harness().await;
    let body = push_body(&[(OLD, NEW, &branch_ref())]);
    let response = h
        .git_post("acme/web.git/git-receive-pack", TASK_TOKEN, body.clone())
        .await;
    assert_eq!(response.status(), 200);
    let seen = Harness::received(&h.github);
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].uri, "/acme/web.git/git-receive-pack");
    assert_eq!(seen[0].body.as_ref(), body.as_slice());
    assert_eq!(
        seen[0].headers["authorization"],
        basic(INSTALLATION_TOKEN).as_str()
    );
}

#[tokio::test]
async fn spec_exec_5_a_push_arriving_in_small_chunks_passes_intact() {
    let h = harness().await;
    let body = push_body(&[(ZERO, NEW, &branch_ref())]);
    let chunks: Vec<Result<Vec<u8>, Infallible>> =
        body.chunks(7).map(|chunk| Ok(chunk.to_vec())).collect();
    let response = h
        .client
        .post(format!("{}/git/acme/web.git/git-receive-pack", h.url))
        .header("authorization", basic(TASK_TOKEN))
        .body(reqwest::Body::wrap_stream(stream::iter(chunks)))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let seen = Harness::received(&h.github);
    assert_eq!(seen[0].body.as_ref(), body.as_slice());
}

#[tokio::test]
async fn spec_exec_5_a_push_to_main_is_403() {
    let h = harness().await;
    let response = h
        .git_post(
            "acme/web.git/git-receive-pack",
            TASK_TOKEN,
            push_body(&[(OLD, NEW, "refs/heads/main")]),
        )
        .await;
    assert_eq!(response.status(), 403);
    assert!(Harness::received(&h.github).is_empty());
}

#[tokio::test]
async fn a_ref_deletion_is_403() {
    let h = harness().await;
    let response = h
        .git_post(
            "acme/web.git/git-receive-pack",
            TASK_TOKEN,
            push_body(&[(OLD, ZERO, &branch_ref())]),
        )
        .await;
    assert_eq!(response.status(), 403);
    assert!(Harness::received(&h.github).is_empty());
}

#[tokio::test]
async fn one_bad_command_among_several_is_403() {
    let h = harness().await;
    let response = h
        .git_post(
            "acme/web.git/git-receive-pack",
            TASK_TOKEN,
            push_body(&[(OLD, NEW, &branch_ref()), (OLD, NEW, "refs/heads/main")]),
        )
        .await;
    assert_eq!(response.status(), 403);
    assert!(Harness::received(&h.github).is_empty());
}

#[tokio::test]
async fn an_unreadable_push_is_403() {
    let h = harness().await;
    for body in [b"garbage".to_vec(), b"0000".to_vec(), Vec::new()] {
        let response = h
            .git_post("acme/web.git/git-receive-pack", TASK_TOKEN, body)
            .await;
        assert_eq!(response.status(), 403);
    }
    let encoded = h
        .client
        .post(format!("{}/git/acme/web.git/git-receive-pack", h.url))
        .header("authorization", basic(TASK_TOKEN))
        .header("content-encoding", "gzip")
        .body(push_body(&[(OLD, NEW, &branch_ref())]))
        .send()
        .await
        .unwrap();
    assert_eq!(encoded.status(), 403);
    assert!(Harness::received(&h.github).is_empty());
}

#[tokio::test]
async fn other_git_requests_are_403() {
    let h = harness().await;
    for path in [
        "acme/web.git/HEAD",
        "acme/web.git/objects/info/packs",
        "acme/web.git/info/refs",
        "acme/web.git/info/refs?service=git-upload-archive",
        "acme/web.git/info/refs?service=git-upload-pack&x=1",
    ] {
        assert_eq!(h.git_get(path, TASK_TOKEN).await.status(), 403, "{path}");
    }
    let post = h
        .git_post("acme/web.git/git-upload-archive", TASK_TOKEN, Vec::new())
        .await;
    assert_eq!(post.status(), 403);
    assert!(Harness::received(&h.github).is_empty());
}

#[tokio::test]
async fn an_organization_without_github_gets_a_403() {
    let mut fake = Fake::with(grant());
    fake.github_token = None;
    let h = harness_with(fake).await;
    let response = h
        .git_get("acme/web.git/info/refs?service=git-upload-pack", TASK_TOKEN)
        .await;
    assert_eq!(response.status(), 403);
    assert!(Harness::received(&h.github).is_empty());
}

#[tokio::test]
async fn an_unreachable_upstream_is_a_generic_502() {
    let fake = Fake::with(grant());
    let dead = {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        format!("http://{}", listener.local_addr().unwrap())
    };
    let h = start(fake, dead.clone(), dead, Log::default(), Log::default()).await;
    let response = h.messages("/v1/messages", TASK_TOKEN).await;
    assert_eq!(response.status(), 502);
    let body = response.text().await.unwrap();
    assert!(!body.contains("127.0.0.1"));
}

#[tokio::test]
async fn the_original_bytes_are_forwarded_not_a_reserialization() {
    let h = harness().await;
    let body = "{ \"max_tokens\" :  1,   \"model\":\"claude-sonnet-5-5\" ,\"x\":1.0}";
    let response = h
        .client
        .post(format!("{}/anthropic/v1/messages", h.url))
        .header("x-api-key", TASK_TOKEN)
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        Harness::received(&h.anthropic)[0].body.as_ref(),
        body.as_bytes()
    );
}

#[tokio::test]
async fn s1_a_model_other_than_the_grants_is_403() {
    let h = harness().await;
    for body in [
        serde_json::json!({"model": "claude-opus-5", "max_tokens": 10}),
        serde_json::json!({"max_tokens": 10}),
    ] {
        assert_eq!(h.messages_with(body).await.status(), 403);
    }
    assert!(Harness::received(&h.anthropic).is_empty());
}

#[tokio::test]
async fn s1_max_tokens_is_capped_at_128000() {
    let h = harness().await;
    let over = h
        .messages_with(serde_json::json!({"model": MODEL, "max_tokens": 128_001}))
        .await;
    let at = h
        .messages_with(serde_json::json!({"model": MODEL, "max_tokens": 128_000}))
        .await;
    assert_eq!(over.status(), 403);
    assert_eq!(at.status(), 200);
}

#[tokio::test]
async fn s1_remote_mcp_servers_and_containers_are_403() {
    let h = harness().await;
    for extra in [
        serde_json::json!({"mcp_servers": [{"type": "url", "url": "https://x", "name": "x"}]}),
        serde_json::json!({"container": "c1"}),
    ] {
        let mut body = serde_json::json!({"model": MODEL, "max_tokens": 10});
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        assert_eq!(h.messages_with(body).await.status(), 403);
    }
    assert!(Harness::received(&h.anthropic).is_empty());
}

#[tokio::test]
async fn s1_server_side_tools_are_403_and_client_tools_pass() {
    let h = harness().await;
    for kind in [
        "web_search_20250305",
        "web_fetch_20250910",
        "code_execution_20250522",
        "bash_20250124",
        "computer_20250124",
        "text_editor_20250728",
        "mcp_toolset",
    ] {
        let body = serde_json::json!({
            "model": MODEL, "max_tokens": 10,
            "tools": [{"type": kind, "name": "t"}],
        });
        assert_eq!(h.messages_with(body).await.status(), 403, "{kind}");
    }
    assert!(Harness::received(&h.anthropic).is_empty());
    let client_tools = serde_json::json!({
        "model": MODEL, "max_tokens": 10,
        "tools": [{"name": "Bash", "description": "d", "input_schema": {"type": "object"}}],
    });
    assert_eq!(h.messages_with(client_tools).await.status(), 200);
}

#[tokio::test]
async fn s1_only_allow_listed_betas_and_beta_true_are_forwarded() {
    let h = harness().await;
    let response = h
        .client
        .post(format!("{}/anthropic/v1/messages?beta=true&evil=1", h.url))
        .header("x-api-key", TASK_TOKEN)
        .header(
            "anthropic-beta",
            "files-api-2025-04-14,interleaved-thinking-2025-05-14",
        )
        .body(MESSAGE)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let only_unlisted = h
        .client
        .post(format!("{}/anthropic/v1/messages?other=1", h.url))
        .header("x-api-key", TASK_TOKEN)
        .header("anthropic-beta", "files-api-2025-04-14")
        .body(MESSAGE)
        .send()
        .await
        .unwrap();
    assert_eq!(only_unlisted.status(), 200);

    let seen = Harness::received(&h.anthropic);
    assert_eq!(seen[0].uri, "/v1/messages?beta=true");
    assert_eq!(
        seen[0].headers["anthropic-beta"],
        "interleaved-thinking-2025-05-14"
    );
    assert_eq!(seen[1].uri, "/v1/messages");
    assert!(seen[1].headers.get("anthropic-beta").is_none());
}

#[tokio::test]
async fn s1_a_body_over_10_mib_is_413_and_never_forwarded() {
    let h = harness().await;
    let big = format!(
        r#"{{"model":"{MODEL}","max_tokens":1,"pad":"{}"}}"#,
        "x".repeat(10 * 1024 * 1024)
    );
    let declared = h
        .client
        .post(format!("{}/anthropic/v1/messages", h.url))
        .header("x-api-key", TASK_TOKEN)
        .body(big.clone())
        .send()
        .await;
    if let Ok(declared) = declared {
        assert_eq!(declared.status(), 413);
    }
    let chunks: Vec<Result<Vec<u8>, Infallible>> = big
        .as_bytes()
        .chunks(64 * 1024)
        .map(|chunk| Ok(chunk.to_vec()))
        .collect();
    let streamed = h
        .client
        .post(format!("{}/anthropic/v1/messages", h.url))
        .header("x-api-key", TASK_TOKEN)
        .body(reqwest::Body::wrap_stream(stream::iter(chunks)))
        .send()
        .await;
    if let Ok(streamed) = streamed {
        assert_eq!(streamed.status(), 413);
    }
    assert!(Harness::received(&h.anthropic).is_empty());
}

#[tokio::test]
async fn s8_unknown_tokens_are_remembered_for_a_moment() {
    let h = harness().await;
    for _ in 0..4 {
        assert_eq!(h.messages("/v1/messages", "nope").await.status(), 401);
    }
    assert_eq!(h.lookups.load(Ordering::SeqCst), 1);
    assert_eq!(h.messages("/v1/messages", TASK_TOKEN).await.status(), 200);
}

#[tokio::test]
async fn s8_a_grant_serves_at_most_16_requests_at_once() {
    let upstream = Router::new().fallback(|| async {
        tokio::time::sleep(Duration::from_millis(800)).await;
        "slow"
    });
    let anthropic_url = serve(upstream).await;
    let (github_url, github) = capturing_upstream().await;
    let h = Arc::new(
        start(
            Fake::with(grant()),
            anthropic_url,
            github_url,
            Log::default(),
            github,
        )
        .await,
    );
    let calls: Vec<_> = (0..20)
        .map(|_| {
            let h = Arc::clone(&h);
            tokio::spawn(async move { h.messages("/v1/messages", TASK_TOKEN).await.status() })
        })
        .collect();
    let mut statuses = Vec::new();
    for call in calls {
        statuses.push(call.await.unwrap().as_u16());
    }
    assert_eq!(statuses.iter().filter(|s| **s == 200).count(), 16);
    assert_eq!(statuses.iter().filter(|s| **s == 429).count(), 4);
    assert_eq!(h.messages("/v1/messages", TASK_TOKEN).await.status(), 200);
}

fn codex_body() -> serde_json::Value {
    serde_json::json!({
        "model": CODEX_MODEL,
        "stream": true,
        "store": false,
        "input": [{"role": "user", "content": "hi"}],
        "tools": [
            {"type": "function", "name": "shell", "parameters": {}},
            {"type": "custom", "name": "apply_patch"},
        ],
        "tool_choice": "auto",
        "parallel_tool_calls": true,
        "reasoning": {"effort": "high"},
        "include": ["reasoning.encrypted_content"],
        "prompt_cache_key": "k",
        "text": {},
        "client_metadata": {},
    })
}

#[tokio::test]
async fn a_codex_request_is_forwarded_with_the_organizations_openai_key() {
    let sse = "event: response.created\ndata: {}\n\nevent: response.completed\ndata: {}\n\n";
    let upstream = Router::new().fallback(move || async move {
        Response::builder()
            .header("content-type", "text/event-stream")
            .body(Body::from(sse))
            .unwrap()
    });
    let openai_url = serve(upstream).await;
    let (log_url, log) = capturing_upstream().await;
    let capture_h = start_openai(Fake::with(codex_grant()), log_url, log).await;
    let response = capture_h.responses(TASK_TOKEN, codex_body()).await;
    assert_eq!(response.status(), 200);
    let seen = Harness::received(&capture_h.openai);
    assert_eq!(seen.len(), 1);
    let request = &seen[0];
    assert_eq!(request.method, Method::POST);
    assert_eq!(request.uri, "/v1/responses");
    assert_eq!(
        request.headers["authorization"],
        format!("Bearer {OPENAI_KEY}")
    );
    assert_eq!(request.headers["content-type"], "application/json");
    assert_eq!(request.headers["accept"], "text/event-stream");
    assert!(request.headers.get("openai-organization").is_none());
    assert!(request.headers.get("openai-project").is_none());
    assert!(request.headers.get("x-api-key").is_none());
    for (_, value) in &request.headers {
        assert!(!value.to_str().unwrap_or_default().contains(TASK_TOKEN));
    }
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&request.body).unwrap(),
        codex_body()
    );
    assert!(Harness::received(&capture_h.anthropic).is_empty());

    let streaming = start_openai(Fake::with(codex_grant()), openai_url, Log::default()).await;
    let response = streaming.responses(TASK_TOKEN, codex_body()).await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    assert_eq!(response.text().await.unwrap(), sse);
}

#[tokio::test]
async fn the_openai_query_string_is_dropped() {
    let h = harness_with(Fake::with(codex_grant())).await;
    let response = h
        .client
        .post(format!("{}/openai/v1/responses?api-version=x&a=b", h.url))
        .header("authorization", format!("Bearer {TASK_TOKEN}"))
        .header("content-type", "application/json")
        .body(codex_body().to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(Harness::received(&h.openai)[0].uri, "/v1/responses");
}

#[tokio::test]
async fn a_codex_grant_cannot_use_the_anthropic_route() {
    let h = harness_with(Fake::with(codex_grant())).await;
    let response = h
        .messages_with(serde_json::json!({"model": CODEX_MODEL}))
        .await;
    assert_eq!(response.status(), 403);
    assert!(Harness::received(&h.anthropic).is_empty());
    assert!(Harness::received(&h.openai).is_empty());
}

#[tokio::test]
async fn a_claude_grant_cannot_use_the_openai_route() {
    let h = harness().await;
    let response = h
        .responses(TASK_TOKEN, serde_json::json!({"model": MODEL}))
        .await;
    assert_eq!(response.status(), 403);
    assert!(Harness::received(&h.openai).is_empty());
}

#[tokio::test]
async fn openai_bodies_that_escape_the_task_are_refused() {
    let h = harness_with(Fake::with(codex_grant())).await;
    let with = |field: &str, value: serde_json::Value| {
        let mut body = codex_body();
        body[field] = value;
        body
    };
    let refused = [
        serde_json::json!([1]),
        with("model", "gpt-5".into()),
        with("store", true.into()),
        with("previous_response_id", "resp_1".into()),
        with("conversation", "conv_1".into()),
        with("prompt", serde_json::json!({"id": "p"})),
        with("background", true.into()),
        with("max_output_tokens", 128_001.into()),
        with("tools", serde_json::json!([{"type": "web_search"}])),
        with("tools", serde_json::json!([{"name": "untyped"}])),
        with("tools", serde_json::json!([{"type": "image_generation"}])),
    ];
    for body in refused {
        let response = h.responses(TASK_TOKEN, body.clone()).await;
        assert_eq!(response.status(), 400, "{body}");
    }
    let not_json = h
        .client
        .post(format!("{}/openai/v1/responses", h.url))
        .header("authorization", format!("Bearer {TASK_TOKEN}"))
        .body("nope")
        .send()
        .await
        .unwrap();
    assert_eq!(not_json.status(), 400);
    assert!(Harness::received(&h.openai).is_empty());
}

#[tokio::test]
async fn only_post_v1_responses_is_served_on_the_openai_route() {
    let h = harness_with(Fake::with(codex_grant())).await;
    let models = h
        .client
        .get(format!("{}/openai/v1/models", h.url))
        .header("authorization", format!("Bearer {TASK_TOKEN}"))
        .send()
        .await
        .unwrap();
    assert_eq!(models.status(), 403);
    let other = h
        .client
        .post(format!("{}/openai/v1/chat/completions", h.url))
        .header("authorization", format!("Bearer {TASK_TOKEN}"))
        .body(codex_body().to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(other.status(), 403);
    let get = h
        .client
        .get(format!("{}/openai/v1/responses", h.url))
        .header("authorization", format!("Bearer {TASK_TOKEN}"))
        .send()
        .await
        .unwrap();
    assert_eq!(get.status(), 403);
    assert!(Harness::received(&h.openai).is_empty());
}

#[tokio::test]
async fn an_unknown_token_is_401_on_the_openai_route() {
    let h = harness_with(Fake::with(codex_grant())).await;
    let response = h.responses("who-knows", codex_body()).await;
    assert_eq!(response.status(), 401);
    let none = h
        .client
        .post(format!("{}/openai/v1/responses", h.url))
        .body(codex_body().to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(none.status(), 401);
    assert!(Harness::received(&h.openai).is_empty());
}

#[tokio::test]
async fn an_organization_without_an_openai_key_gets_a_clear_403() {
    let mut fake = Fake::with(codex_grant());
    fake.openai_key = None;
    let h = harness_with(fake).await;
    let response = h.responses(TASK_TOKEN, codex_body()).await;
    assert_eq!(response.status(), 403);
    assert!(response.text().await.unwrap().contains("OpenAI key"));
    assert!(Harness::received(&h.openai).is_empty());
}

fn stream_path(model: &str) -> String {
    format!("/v1beta/models/{model}:streamGenerateContent?alt=sse")
}

fn agy_json() -> serde_json::Value {
    serde_json::from_str(AGY_BODY).unwrap()
}

#[tokio::test]
async fn an_agy_request_is_forwarded_with_the_organizations_google_key() {
    let h = harness_with(Fake::with(agy_grant())).await;
    let response = h.agy(AGY_BODY).await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.text().await.unwrap(), "upstream-ok");

    let seen = Harness::received(&h.google);
    assert_eq!(seen.len(), 1);
    let request = &seen[0];
    assert_eq!(request.method, Method::POST);
    assert_eq!(
        request.uri,
        "/v1beta/models/gemini-3.1-pro-preview:streamGenerateContent?alt=sse"
    );
    assert_eq!(request.headers["x-goog-api-key"], GOOGLE_KEY);
    assert_eq!(request.headers["content-type"], "application/json");
    assert!(request.headers.get("authorization").is_none());
    assert!(request.headers.get("x-goog-api-client").is_none());
    assert!(request.headers.get("x-api-key").is_none());
    assert!(request.headers.get("user-agent").is_none());
    assert!(
        !request
            .headers
            .get("accept-encoding")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .contains("x-agy-coding")
    );
    for (_, value) in &request.headers {
        assert!(!value.to_str().unwrap_or_default().contains(TASK_TOKEN));
    }
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&request.body).unwrap(),
        serde_json::from_str::<serde_json::Value>(AGY_BODY).unwrap()
    );
    assert!(Harness::received(&h.anthropic).is_empty());
    assert!(Harness::received(&h.openai).is_empty());
}

async fn start_google(fake: Fake, google_url: String, google: Log) -> Harness {
    let (anthropic_url, anthropic) = capturing_upstream().await;
    let (github_url, github) = capturing_upstream().await;
    let (openai_url, _) = capturing_upstream().await;
    start_with(
        fake,
        anthropic_url,
        openai_url,
        github_url,
        anthropic,
        github,
        (google_url, google),
    )
    .await
}

#[tokio::test]
async fn the_google_stream_is_relayed() {
    let sse = "data: {\"candidates\":[]}\n\ndata: {\"usageMetadata\":{}}\n\n";
    let upstream = Router::new().fallback(move || async move {
        Response::builder()
            .header("content-type", "text/event-stream")
            .body(Body::from(sse))
            .unwrap()
    });
    let h = start_google(
        Fake::with(agy_grant()),
        serve(upstream).await,
        Log::default(),
    )
    .await;
    let response = h.agy(AGY_BODY).await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    assert_eq!(response.text().await.unwrap(), sse);
}

#[tokio::test]
async fn the_grant_model_is_translated_to_the_wire_model_and_the_cli_id_is_refused() {
    let h = harness_with(Fake::with(agy_grant())).await;
    let cli_id = h
        .generate(
            &stream_path(AGY_MODEL),
            ("x-goog-api-key", TASK_TOKEN),
            AGY_BODY,
        )
        .await;
    assert_eq!(cli_id.status(), 403);
    assert!(Harness::received(&h.google).is_empty());
    assert_eq!(h.agy(AGY_BODY).await.status(), 200);
    assert!(
        Harness::received(&h.google)[0]
            .uri
            .starts_with("/v1beta/models/gemini-3.1-pro-preview:")
    );
}

#[tokio::test]
async fn helper_models_are_refused_without_an_upstream_call() {
    let h = harness_with(Fake::with(agy_grant())).await;
    for model in [
        HELPER_WIRE,
        "gemini-3.8-flash",
        "gemini-3.1-pro-preview-x",
        "",
    ] {
        let response = h
            .generate(
                &stream_path(model),
                ("x-goog-api-key", TASK_TOKEN),
                AGY_BODY,
            )
            .await;
        assert_eq!(response.status(), 403, "{model}");
    }
    assert!(Harness::received(&h.google).is_empty());
}

#[tokio::test]
async fn other_google_paths_and_methods_are_refused() {
    let h = harness_with(Fake::with(agy_grant())).await;
    for path in [
        format!("/v1beta/models/{AGY_WIRE}:countTokens"),
        format!("/v1beta/models/{AGY_WIRE}:embedContent"),
        format!("/v1beta/models/{AGY_WIRE}"),
        "/v1beta/models".to_owned(),
        "/v1beta/files".to_owned(),
        format!("/v1beta/models/{AGY_WIRE}:generateContent/extra"),
        format!("/v1beta/models/{AGY_WIRE}%3AgenerateContent"),
        format!("/v1/models/{AGY_WIRE}:generateContent"),
        format!("/v1beta/tunedModels/{AGY_WIRE}:generateContent"),
    ] {
        let response = h
            .generate(&path, ("x-goog-api-key", TASK_TOKEN), AGY_BODY)
            .await;
        assert_eq!(response.status(), 403, "{path}");
    }
    for path in [
        format!("/v1beta/models/{AGY_WIRE}:streamGenerateContent?alt=sse"),
        "/v1beta/models".to_owned(),
    ] {
        let get = h
            .client
            .get(format!("{}/google{path}", h.url))
            .header("x-goog-api-key", TASK_TOKEN)
            .send()
            .await
            .unwrap();
        assert_eq!(get.status(), 403, "{path}");
    }
    assert!(Harness::received(&h.google).is_empty());
}

#[tokio::test]
async fn the_google_query_keeps_only_alt_sse() {
    let h = harness_with(Fake::with(agy_grant())).await;
    let path = format!("/v1beta/models/{AGY_WIRE}:generateContent");
    let with_extras = h
        .generate(
            &format!("{path}?alt=sse&key=stolen&cachedContent=x"),
            ("x-goog-api-key", TASK_TOKEN),
            AGY_BODY,
        )
        .await;
    assert_eq!(with_extras.status(), 200);
    let without = h
        .generate(
            &format!("{path}?key=stolen&alt=json"),
            ("x-goog-api-key", TASK_TOKEN),
            AGY_BODY,
        )
        .await;
    assert_eq!(without.status(), 200);
    let seen = Harness::received(&h.google);
    assert_eq!(
        seen[0].uri,
        "/v1beta/models/gemini-3.1-pro-preview:generateContent?alt=sse"
    );
    assert_eq!(
        seen[1].uri,
        "/v1beta/models/gemini-3.1-pro-preview:generateContent"
    );
}

#[tokio::test]
async fn other_harness_grants_cannot_use_the_google_route() {
    for grant in [grant(), codex_grant()] {
        let h = harness_with(Fake::with(grant)).await;
        assert_eq!(h.agy(AGY_BODY).await.status(), 403);
        assert!(Harness::received(&h.google).is_empty());
    }
}

#[tokio::test]
async fn an_antigravity_grant_cannot_use_the_openai_or_anthropic_routes() {
    let h = harness_with(Fake::with(agy_grant())).await;
    let responses = h
        .responses(TASK_TOKEN, serde_json::json!({"model": AGY_MODEL}))
        .await;
    assert_eq!(responses.status(), 403);
    let messages = h
        .messages_with(serde_json::json!({"model": AGY_MODEL}))
        .await;
    assert_eq!(messages.status(), 403);
    assert!(Harness::received(&h.openai).is_empty());
    assert!(Harness::received(&h.anthropic).is_empty());
}

#[tokio::test]
async fn google_bodies_that_escape_the_task_are_refused() {
    let h = harness_with(Fake::with(agy_grant())).await;
    let with = |field: &str, value: serde_json::Value| {
        let mut body = agy_json();
        body[field] = value;
        body.to_string()
    };
    let remote_part = serde_json::json!([{"role": "user", "parts": [
        {"fileData": {"fileUri": "https://attacker.example/", "mimeType": "image/png"}},
    ]}]);
    let snake_part = serde_json::json!([{"role": "user", "parts": [
        {"file_data": {"file_uri": "gs://bucket/object"}},
    ]}]);
    let refused = [
        "[1]".to_owned(),
        "nope".to_owned(),
        with("cachedContent", "cachedContents/x".into()),
        with("tools", serde_json::json!([{"googleSearch": {}}])),
        with("tools", serde_json::json!([{"codeExecution": {}}])),
        with("tools", serde_json::json!([{"urlContext": {}}])),
        with(
            "tools",
            serde_json::json!([{"functionDeclarations": [], "googleSearch": {}}]),
        ),
        with(
            "generationConfig",
            serde_json::json!({"maxOutputTokens": 128_001}),
        ),
        with("contents", remote_part),
        with("contents", snake_part.clone()),
        with(
            "systemInstruction",
            serde_json::json!({"parts": snake_part[0]["parts"]}),
        ),
    ];
    for body in refused {
        let response = h.agy(&body).await;
        assert_eq!(response.status(), 400, "{body}");
    }
    assert!(Harness::received(&h.google).is_empty());
    let inline = with(
        "contents",
        serde_json::json!([{"role": "user", "parts": [
            {"inlineData": {"mimeType": "image/png", "data": "AAAA"}},
        ]}]),
    );
    assert_eq!(h.agy(&inline).await.status(), 200);
}

#[tokio::test]
async fn an_unknown_token_is_401_on_the_google_route() {
    let h = harness_with(Fake::with(agy_grant())).await;
    let response = h
        .generate(
            &stream_path(AGY_WIRE),
            ("x-goog-api-key", "who-knows"),
            AGY_BODY,
        )
        .await;
    assert_eq!(response.status(), 401);
    let none = h
        .client
        .post(format!("{}/google{}", h.url, stream_path(AGY_WIRE)))
        .body(AGY_BODY)
        .send()
        .await
        .unwrap();
    assert_eq!(none.status(), 401);
    assert!(Harness::received(&h.google).is_empty());
}

#[tokio::test]
async fn an_organization_without_a_google_key_gets_a_clear_403() {
    let mut fake = Fake::with(agy_grant());
    fake.google_key = None;
    let h = harness_with(fake).await;
    let response = h.agy(AGY_BODY).await;
    assert_eq!(response.status(), 403);
    assert!(response.text().await.unwrap().contains("Google key"));
    assert!(Harness::received(&h.google).is_empty());
}

fn vibe_json() -> serde_json::Value {
    serde_json::from_str(VIBE_BODY).unwrap()
}

fn forwarded_vibe_json() -> serde_json::Value {
    let mut body = vibe_json();
    body["max_tokens"] = 128_000.into();
    body
}

fn vibe_with(field: &str, value: serde_json::Value) -> String {
    let mut body = vibe_json();
    body[field] = value;
    body.to_string()
}

#[tokio::test]
async fn a_vibe_request_is_forwarded_with_the_organizations_mistral_key() {
    let h = harness_with(Fake::with(vibe_grant())).await;
    let response = h.vibe(VIBE_BODY).await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.text().await.unwrap(), "upstream-ok");

    let seen = Harness::received(&h.mistral);
    assert_eq!(seen.len(), 1);
    let request = &seen[0];
    assert_eq!(request.method, Method::POST);
    assert_eq!(request.uri, "/v1/chat/completions");
    assert_eq!(
        request.headers["authorization"],
        format!("Bearer {MISTRAL_KEY}")
    );
    assert_eq!(request.headers["content-type"], "application/json");
    assert_eq!(request.headers["accept"], "text/event-stream");
    assert!(request.headers.get("user-agent").is_none());
    assert!(request.headers.get("x-affinity").is_none());
    assert!(request.headers.get("x-api-key").is_none());
    assert!(
        !request
            .headers
            .get("accept-encoding")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .contains("x-vibe-coding")
    );
    for (_, value) in &request.headers {
        assert!(!value.to_str().unwrap_or_default().contains(TASK_TOKEN));
    }
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&request.body).unwrap(),
        forwarded_vibe_json()
    );
    assert!(Harness::received(&h.anthropic).is_empty());
    assert!(Harness::received(&h.openai).is_empty());
    assert!(Harness::received(&h.google).is_empty());
}

#[tokio::test]
async fn the_forwarded_vibe_body_is_the_checked_one() {
    let h = harness_with(Fake::with(vibe_grant())).await;
    let raw = VIBE_BODY.replacen("\"stream\":true", "\"stream\":false,\"stream\":true", 1);
    assert_ne!(raw, VIBE_BODY);
    assert_eq!(h.vibe(&raw).await.status(), 200);
    let seen = Harness::received(&h.mistral);
    let forwarded = std::str::from_utf8(&seen[0].body).unwrap();
    assert_eq!(forwarded.matches("\"stream\":").count(), 1);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(forwarded).unwrap(),
        forwarded_vibe_json()
    );
}

#[tokio::test]
async fn the_vibe_stream_is_relayed() {
    let sse = "data: {\"choices\":[]}\n\ndata: [DONE]\n\n";
    let upstream = Router::new().fallback(move || async move {
        Response::builder()
            .header("content-type", "text/event-stream")
            .body(Body::from(sse))
            .unwrap()
    });
    let mistral_url = serve(upstream).await;
    let fake = Fake::with(vibe_grant());
    let lookups = fake.lookups.clone();
    let (anthropic_url, anthropic) = capturing_upstream().await;
    let (github_url, github) = capturing_upstream().await;
    let (openai_url, openai) = capturing_upstream().await;
    let (google_url, google) = capturing_upstream().await;
    let proxy = Proxy::with_http(
        fake,
        ProxyConfig {
            anthropic_base: anthropic_url,
            openai_base: openai_url,
            google_base: google_url,
            mistral_base: mistral_url,
            github_base: github_url,
            registries: no_registries(),
        },
        plain_client(),
    );
    let h = Harness {
        url: serve(router(proxy)).await,
        anthropic,
        openai,
        google,
        mistral: Log::default(),
        github,
        client: plain_client(),
        lookups,
    };
    let response = h.vibe(VIBE_BODY).await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    assert_eq!(response.text().await.unwrap(), sse);
}

#[tokio::test]
async fn the_vibe_model_is_translated_to_the_wire_model() {
    let mut fake = Fake::with(TaskGrant {
        model: VIBE_MODEL.into(),
        ..grant()
    });
    fake.mistral_key = Some(MISTRAL_KEY);
    let h = harness_with(fake).await;
    let large = h.vibe(VIBE_BODY).await;
    assert_eq!(large.status(), 400);
    let cli_id = h.vibe(&vibe_with("model", VIBE_MODEL.into())).await;
    assert_eq!(cli_id.status(), 400);
    assert!(Harness::received(&h.mistral).is_empty());
    let wire = h.vibe(&vibe_with("model", VIBE_MEDIUM_WIRE.into())).await;
    assert_eq!(wire.status(), 200);
    assert_eq!(Harness::received(&h.mistral).len(), 1);
}

#[tokio::test]
async fn another_model_in_the_vibe_body_is_refused() {
    let h = harness_with(Fake::with(vibe_grant())).await;
    for model in [
        "devstral-2512",
        "mistral-small-latest",
        "",
        "mistral-large-4-0 ",
    ] {
        let response = h.vibe(&vibe_with("model", model.into())).await;
        assert_eq!(response.status(), 400, "{model}");
    }
    assert!(Harness::received(&h.mistral).is_empty());
}

#[tokio::test]
async fn other_mistral_paths_and_methods_are_refused() {
    let h = harness_with(Fake::with(vibe_grant())).await;
    for path in [
        "/v1/embeddings",
        "/v1/files",
        "/v1/models",
        "/v1/fim/completions",
        "/v1/agents/completions",
        "/v1/chat/completions/extra",
        "/v1/chat/completions/",
        "/v1/chat/../files",
        "/v1/chat/completions%2Fx",
        "/v2/chat/completions",
    ] {
        let response = h.vibe_at(path, TASK_TOKEN, VIBE_BODY).await;
        assert_eq!(response.status(), 403, "{path}");
    }
    let get = h
        .client
        .get(format!("{}/mistral/v1/chat/completions", h.url))
        .header("authorization", format!("Bearer {TASK_TOKEN}"))
        .send()
        .await
        .unwrap();
    assert_eq!(get.status(), 403);
    assert!(Harness::received(&h.mistral).is_empty());
}

#[tokio::test]
async fn a_query_string_on_the_mistral_route_is_refused() {
    let h = harness_with(Fake::with(vibe_grant())).await;
    for path in [
        "/v1/chat/completions?x=1",
        "/v1/chat/completions?",
        "/v1/chat/completions?model=other",
    ] {
        let response = h.vibe_at(path, TASK_TOKEN, VIBE_BODY).await;
        assert_eq!(response.status(), 403, "{path}");
    }
    assert!(Harness::received(&h.mistral).is_empty());
}

#[tokio::test]
async fn other_harness_grants_cannot_use_the_mistral_route() {
    for grant in [grant(), codex_grant(), agy_grant()] {
        let h = harness_with(Fake::with(grant)).await;
        assert_eq!(h.vibe(VIBE_BODY).await.status(), 403);
        assert!(Harness::received(&h.mistral).is_empty());
    }
}

#[tokio::test]
async fn a_mistral_grant_cannot_use_the_other_routes() {
    let h = harness_with(Fake::with(vibe_grant())).await;
    let responses = h
        .responses(TASK_TOKEN, serde_json::json!({"model": VIBE_LARGE}))
        .await;
    assert_eq!(responses.status(), 403);
    let messages = h
        .messages_with(serde_json::json!({"model": VIBE_LARGE}))
        .await;
    assert_eq!(messages.status(), 403);
    assert_eq!(h.agy(AGY_BODY).await.status(), 403);
    assert!(Harness::received(&h.openai).is_empty());
    assert!(Harness::received(&h.anthropic).is_empty());
    assert!(Harness::received(&h.google).is_empty());
}

#[tokio::test]
async fn vibe_bodies_that_escape_the_task_are_refused() {
    let h = harness_with(Fake::with(vibe_grant())).await;
    let user = |content: serde_json::Value| {
        vibe_with(
            "messages",
            serde_json::json!([{"role": "user", "content": content}]),
        )
    };
    let refused = [
        "[1]".to_owned(),
        "nope".to_owned(),
        vibe_with("n", 2.into()),
        vibe_with("metadata", serde_json::json!({})),
        vibe_with("max_tokens", 128_001.into()),
        vibe_with("tools", serde_json::json!([{"type": "web_search"}])),
        vibe_with("stream_options", serde_json::json!({"x": true})),
        vibe_with("response_format", serde_json::json!({"type": "grammar"})),
        user(serde_json::json!([
            {"type": "image_url", "image_url": "https://attacker.example/x.png"}
        ])),
        user(serde_json::json!([
            {"type": "image_url", "image_url": {"url": "http://169.254.169.254/"}}
        ])),
        user(serde_json::json!([
            {"type": "document_url", "document_url": "https://attacker.example/x.pdf"}
        ])),
        user(serde_json::json!([{"type": "file", "file_id": "f"}])),
        user(serde_json::json!([{"type": "input_audio", "input_audio": "AAAA"}])),
    ];
    for body in refused {
        let response = h.vibe(&body).await;
        assert_eq!(response.status(), 400, "{body}");
    }
    assert!(Harness::received(&h.mistral).is_empty());
    let inline = user(serde_json::json!([
        {"type": "text", "text": "look"},
        {"type": "image_url", "image_url": "data:image/png;base64,AAAA"}
    ]));
    assert_eq!(h.vibe(&inline).await.status(), 200);
}

#[tokio::test]
async fn an_unknown_token_is_401_on_the_mistral_route() {
    let h = harness_with(Fake::with(vibe_grant())).await;
    let response = h
        .vibe_at("/v1/chat/completions", "who-knows", VIBE_BODY)
        .await;
    assert_eq!(response.status(), 401);
    let none = h
        .client
        .post(format!("{}/mistral/v1/chat/completions", h.url))
        .body(VIBE_BODY)
        .send()
        .await
        .unwrap();
    assert_eq!(none.status(), 401);
    assert!(Harness::received(&h.mistral).is_empty());
}

#[tokio::test]
async fn an_organization_without_a_mistral_key_gets_a_clear_403() {
    let mut fake = Fake::with(vibe_grant());
    fake.mistral_key = None;
    let h = harness_with(fake).await;
    let response = h.vibe(VIBE_BODY).await;
    assert_eq!(response.status(), 403);
    assert!(response.text().await.unwrap().contains("Mistral key"));
    assert!(Harness::received(&h.mistral).is_empty());
}

fn with_companion(companion: &str) -> Fake {
    Fake::with(TaskGrant {
        companions: vec![companion.parse().unwrap()],
        ..grant()
    })
}

#[tokio::test]
async fn a_companion_is_fetched_with_a_read_only_token_over_its_own_repository() {
    let fake = with_companion("acme/sdk");
    let modes = fake.token_modes.clone();
    let h = harness_with(fake).await;
    let refs = h
        .git_get("acme/sdk.git/info/refs?service=git-upload-pack", TASK_TOKEN)
        .await;
    assert_eq!(refs.status(), 200);
    let pack = h
        .git_post("acme/sdk.git/git-upload-pack", TASK_TOKEN, b"0000".to_vec())
        .await;
    assert_eq!(pack.status(), 200);
    let seen = Harness::received(&h.github);
    assert_eq!(seen.len(), 2);
    assert!(seen[0].uri.starts_with("/acme/sdk.git/info/refs"));
    assert_eq!(seen[1].uri, "/acme/sdk.git/git-upload-pack");
    for request in &seen {
        assert_eq!(request.headers["authorization"], basic(READ_ONLY_TOKEN));
        assert!(
            !request.headers["authorization"]
                .to_str()
                .unwrap()
                .contains(INSTALLATION_TOKEN)
        );
    }
    assert_eq!(*modes.lock().unwrap(), vec![ExecutionMode::Audit; 2]);
}

#[tokio::test]
async fn a_companion_matches_without_case_and_the_run_repository_keeps_its_own_token() {
    let fake = with_companion("Acme/SDK");
    let modes = fake.token_modes.clone();
    let h = harness_with(fake).await;
    assert_eq!(
        h.git_get("acme/sdk.git/info/refs?service=git-upload-pack", TASK_TOKEN)
            .await
            .status(),
        200
    );
    assert_eq!(
        h.git_get("acme/web.git/info/refs?service=git-upload-pack", TASK_TOKEN)
            .await
            .status(),
        200
    );
    assert_eq!(
        *modes.lock().unwrap(),
        vec![ExecutionMode::Audit, ExecutionMode::Change]
    );
}

#[tokio::test]
async fn nothing_can_be_pushed_to_a_companion_even_in_a_change_run() {
    let h = harness_with(with_companion("acme/sdk")).await;
    let advert = h
        .git_get(
            "acme/sdk.git/info/refs?service=git-receive-pack",
            TASK_TOKEN,
        )
        .await;
    assert_eq!(advert.status(), 403);
    let push = h
        .git_post(
            "acme/sdk.git/git-receive-pack",
            TASK_TOKEN,
            push_body(&[(OLD, NEW, &format!("refs/heads/{BRANCH}"))]),
        )
        .await;
    assert_eq!(push.status(), 403);
    assert!(Harness::received(&h.github).is_empty());
}

fn with_writable(repository: &str) -> Fake {
    Fake::with(TaskGrant {
        writable_repositories: vec![repository.parse().unwrap()],
        ..grant()
    })
}

#[tokio::test]
async fn a_writable_repository_accepts_a_push_to_the_run_branch_with_the_change_token() {
    let fake = with_writable("acme/api");
    let modes = fake.token_modes.clone();
    let h = harness_with(fake).await;
    let advert = h
        .git_get(
            "acme/api.git/info/refs?service=git-receive-pack",
            TASK_TOKEN,
        )
        .await;
    assert_eq!(advert.status(), 200);
    let body = push_body(&[(OLD, NEW, &branch_ref())]);
    let push = h
        .git_post("acme/api.git/git-receive-pack", TASK_TOKEN, body.clone())
        .await;
    assert_eq!(push.status(), 200);
    let seen = Harness::received(&h.github);
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[1].uri, "/acme/api.git/git-receive-pack");
    assert_eq!(seen[1].body.as_ref(), body.as_slice());
    for request in &seen {
        assert_eq!(request.headers["authorization"], basic(INSTALLATION_TOKEN));
    }
    assert_eq!(*modes.lock().unwrap(), vec![ExecutionMode::Change; 2]);
}

#[tokio::test]
async fn a_writable_repository_refuses_a_push_to_any_other_branch() {
    let h = harness_with(with_writable("acme/api")).await;
    for reference in ["refs/heads/main", "refs/tags/v1"] {
        let push = h
            .git_post(
                "acme/api.git/git-receive-pack",
                TASK_TOKEN,
                push_body(&[(OLD, NEW, reference)]),
            )
            .await;
        assert_eq!(push.status(), 403, "{reference}");
    }
    let deletion = h
        .git_post(
            "acme/api.git/git-receive-pack",
            TASK_TOKEN,
            push_body(&[(OLD, ZERO, &branch_ref())]),
        )
        .await;
    assert_eq!(deletion.status(), 403);
    assert!(Harness::received(&h.github).is_empty());
}

#[tokio::test]
async fn a_writable_repository_matches_without_case_and_is_pushed_with_the_member_token() {
    let mut fake = Fake::with(TaskGrant {
        git_identity: GitIdentity::Member,
        writable_repositories: vec!["Acme/API".parse().unwrap()],
        ..grant()
    });
    fake.member_tokens.insert(INITIATOR, MEMBER_TOKEN);
    let h = harness_with(fake).await;
    let push = h
        .git_post(
            "acme/api.git/git-receive-pack",
            TASK_TOKEN,
            push_body(&[(OLD, NEW, &branch_ref())]),
        )
        .await;
    assert_eq!(push.status(), 200);
    let fetch = h
        .git_get("acme/api.git/info/refs?service=git-upload-pack", TASK_TOKEN)
        .await;
    assert_eq!(fetch.status(), 200);
    let seen = Harness::received(&h.github);
    assert_eq!(
        authorizations(&seen),
        [&basic(MEMBER_TOKEN), &basic(INSTALLATION_TOKEN)]
    );
}

#[tokio::test]
async fn a_companion_stays_read_only_beside_a_writable_repository() {
    let h = harness_with(Fake::with(TaskGrant {
        companions: vec!["acme/sdk".parse().unwrap()],
        writable_repositories: vec!["acme/api".parse().unwrap()],
        ..grant()
    }))
    .await;
    let push = h
        .git_post(
            "acme/sdk.git/git-receive-pack",
            TASK_TOKEN,
            push_body(&[(OLD, NEW, &branch_ref())]),
        )
        .await;
    assert_eq!(push.status(), 403);
    assert!(Harness::received(&h.github).is_empty());
}

#[tokio::test]
async fn a_repository_that_is_neither_the_run_nor_a_companion_is_refused() {
    let h = harness_with(with_companion("acme/sdk")).await;
    for path in [
        "acme/other.git/info/refs?service=git-upload-pack",
        "other/sdk.git/info/refs?service=git-upload-pack",
        "acme/sdk-extra.git/info/refs?service=git-upload-pack",
    ] {
        assert_eq!(h.git_get(path, TASK_TOKEN).await.status(), 403, "{path}");
    }
    assert!(Harness::received(&h.github).is_empty());
}

#[tokio::test]
async fn a_grant_without_companions_refuses_any_other_repository() {
    let h = harness().await;
    assert_eq!(
        h.git_get("acme/sdk.git/info/refs?service=git-upload-pack", TASK_TOKEN)
            .await
            .status(),
        403
    );
}

const PUBLIC: &str = "http://matos-egress-proxy.matos.svc:4130";

struct Registries {
    url: String,
    index: Log,
    crates: Log,
    npm: Log,
    client: reqwest::Client,
}

async fn registries_with(
    index: Router,
    crates: Router,
    npm: Router,
    public_base: Option<&str>,
) -> Registries {
    let (index_url, index_log) = logged(index).await;
    let (crates_url, crates_log) = logged(crates).await;
    let (npm_url, npm_log) = logged(npm).await;
    let proxy = Proxy::with_http(
        Fake::with(grant()),
        ProxyConfig {
            anthropic_base: "http://127.0.0.1:1".into(),
            openai_base: "http://127.0.0.1:1".into(),
            google_base: "http://127.0.0.1:1".into(),
            mistral_base: "http://127.0.0.1:1".into(),
            github_base: "http://127.0.0.1:1".into(),
            registries: RegistryConfig {
                crates_index_base: format!("{index_url}/"),
                crates_static_base: format!("{crates_url}/crates"),
                npm_base: npm_url,
                public_base: public_base.map(str::to_owned),
            },
        },
        plain_client(),
    );
    Registries {
        url: serve(router(proxy)).await,
        index: index_log,
        crates: crates_log,
        npm: npm_log,
        client: plain_client(),
    }
}

async fn logged(app: Router) -> (String, Log) {
    let log = Log::default();
    let recorder = axum::middleware::from_fn_with_state(
        log.clone(),
        |State(log): State<Log>, request: Request, next: axum::middleware::Next| async move {
            log.lock().unwrap().push(Captured {
                method: request.method().clone(),
                uri: request.uri().to_string(),
                headers: request.headers().clone(),
                body: Bytes::new(),
            });
            next.run(request).await
        },
    );
    (serve(app.layer(recorder)).await, log)
}

const CONFIG_JSON: &str =
    r#"{"dl":"https://static.crates.io/crates","api":"https://crates.io","auth-required":false}"#;

async fn registries() -> Registries {
    registries_with(
        Router::new()
            .route("/config.json", axum::routing::get(|| async { CONFIG_JSON }))
            .route(
                "/se/rd/serde",
                axum::routing::get(|| async { "index-line" }),
            ),
        Router::new()
            .route(
                "/crates/serde/1.0.0/download",
                axum::routing::get(|| async { "crate-bytes" }),
            )
            .route(
                "/crates/moved/1.0.0/download",
                axum::routing::get(|| async {
                    (
                        StatusCode::FOUND,
                        [("location", "/crates/serde/1.0.0/download")],
                    )
                }),
            )
            .route(
                "/crates/away/1.0.0/download",
                axum::routing::get(|| async {
                    (
                        StatusCode::FOUND,
                        [("location", "http://127.0.0.1:1/elsewhere")],
                    )
                }),
            ),
        Router::new()
            .route("/left-pad", axum::routing::get(|| async { "packument" }))
            .route(
                "/@babel/core",
                axum::routing::get(|| async { "scoped-packument" }),
            )
            .route(
                "/-/v1/search",
                axum::routing::get(|request: Request| async move {
                    request.uri().query().unwrap_or_default().to_owned()
                }),
            ),
        Some(PUBLIC),
    )
    .await
}

#[tokio::test]
async fn the_cargo_index_configuration_points_downloads_back_at_the_proxy() {
    let r = registries().await;
    let response = r
        .client
        .get(format!("{}/crates-index/config.json", r.url))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let config: serde_json::Value = response.json().await.unwrap();
    assert_eq!(config["dl"], format!("{PUBLIC}/crates"));
    assert!(config.get("api").is_none());
    assert_eq!(config["auth-required"], false);
}

#[tokio::test]
async fn without_a_public_base_the_index_configuration_is_unavailable() {
    let r = registries_with(
        Router::new().route("/config.json", axum::routing::get(|| async { CONFIG_JSON })),
        Router::new(),
        Router::new(),
        None,
    )
    .await;
    let response = r
        .client
        .get(format!("{}/crates-index/config.json", r.url))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 503);
}

#[tokio::test]
async fn the_index_and_the_crates_come_from_their_fixed_upstreams_without_a_token() {
    let r = registries().await;
    let index = r
        .client
        .get(format!("{}/crates-index/se/rd/serde", r.url))
        .header("if-none-match", "\"abc\"")
        .header("authorization", "Bearer should-not-travel")
        .send()
        .await
        .unwrap();
    assert_eq!(index.status(), 200);
    assert_eq!(index.text().await.unwrap(), "index-line");
    let seen = Harness::received(&r.index);
    assert_eq!(seen[0].uri, "/se/rd/serde");
    assert_eq!(seen[0].headers["if-none-match"], "\"abc\"");
    assert!(!seen[0].headers.contains_key("authorization"));

    let krate = r
        .client
        .get(format!("{}/crates/serde/1.0.0/download", r.url))
        .send()
        .await
        .unwrap();
    assert_eq!(krate.status(), 200);
    assert_eq!(krate.text().await.unwrap(), "crate-bytes");
    assert_eq!(
        Harness::received(&r.crates)[0].uri,
        "/crates/serde/1.0.0/download"
    );
}

#[tokio::test]
async fn a_crate_redirect_is_followed_by_the_proxy_inside_the_allowlist_only() {
    let r = registries().await;
    let moved = r
        .client
        .get(format!("{}/crates/moved/1.0.0/download", r.url))
        .send()
        .await
        .unwrap();
    assert_eq!(moved.status(), 200);
    assert_eq!(moved.text().await.unwrap(), "crate-bytes");
    let away = r
        .client
        .get(format!("{}/crates/away/1.0.0/download", r.url))
        .send()
        .await
        .unwrap();
    assert_eq!(away.status(), 502);
    assert!(away.headers().get("location").is_none());
}

#[tokio::test]
async fn npm_packages_scoped_ones_and_queries_reach_the_registry() {
    let r = registries().await;
    let get = |path: &'static str| {
        let url = format!("{}/npm/{path}", r.url);
        let client = r.client.clone();
        async move { client.get(url).send().await.unwrap() }
    };
    assert_eq!(get("left-pad").await.text().await.unwrap(), "packument");
    assert_eq!(
        get("@babel/core").await.text().await.unwrap(),
        "scoped-packument"
    );
    assert_eq!(
        get("-/v1/search?text=left&size=1")
            .await
            .text()
            .await
            .unwrap(),
        "text=left&size=1"
    );
    let missing = get("no-such-package").await;
    assert_eq!(missing.status(), 404);
    assert_eq!(Harness::received(&r.npm).len(), 4);
}

#[tokio::test]
async fn the_registries_serve_head_and_refuse_anything_else() {
    let r = registries().await;
    for path in [
        "crates-index/se/rd/serde",
        "crates/serde/1.0.0/download",
        "npm/left-pad",
    ] {
        let url = format!("{}/{path}", r.url);
        assert_eq!(
            r.client.head(&url).send().await.unwrap().status(),
            200,
            "{path}"
        );
        for request in [
            r.client.post(&url).body("x"),
            r.client.put(&url).body("x"),
            r.client.delete(&url),
            r.client.patch(&url).body("x"),
        ] {
            assert_eq!(request.send().await.unwrap().status(), 405, "{path}");
        }
    }
    assert!(
        Harness::received(&r.npm)
            .iter()
            .all(|request| request.method == Method::HEAD)
    );
}

#[tokio::test]
async fn a_registry_path_cannot_climb_out_of_its_upstream() {
    let r = registries().await;
    for path in [
        "npm/a%2f..%2fb",
        "npm/%2e%2e%2fx",
        "crates/a%2f..%2f..%2fetc",
        "crates-index/a%5cb",
    ] {
        let response = r
            .client
            .get(format!("{}/{path}", r.url))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400, "{path}");
    }
    assert!(Harness::received(&r.npm).is_empty());
    assert!(Harness::received(&r.crates).is_empty());
    assert!(Harness::received(&r.index).is_empty());
}

#[tokio::test]
async fn an_unreachable_registry_is_a_502() {
    let h = harness().await;
    let response = h
        .client
        .get(format!("{}/npm/left-pad", h.url))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 502);
}

fn member_fake(identity: GitIdentity, connected: bool) -> Fake {
    let mut fake = Fake::with(TaskGrant {
        git_identity: identity,
        ..grant()
    });
    if connected {
        fake.member_tokens.insert(INITIATOR, MEMBER_TOKEN);
    }
    fake
}

async fn push_and_fetch(h: &Harness) -> Vec<Captured> {
    let refs = h
        .git_get(
            "acme/web.git/info/refs?service=git-receive-pack",
            TASK_TOKEN,
        )
        .await;
    assert_eq!(refs.status(), 200);
    let push = h
        .git_post(
            "acme/web.git/git-receive-pack",
            TASK_TOKEN,
            push_body(&[(OLD, NEW, &branch_ref())]),
        )
        .await;
    assert_eq!(push.status(), 200);
    let fetch = h
        .git_get("acme/web.git/info/refs?service=git-upload-pack", TASK_TOKEN)
        .await;
    assert_eq!(fetch.status(), 200);
    let pack = h
        .git_post(
            "acme/web.git/git-upload-pack",
            TASK_TOKEN,
            b"0032want abc\n0000".to_vec(),
        )
        .await;
    assert_eq!(pack.status(), 200);
    let seen = Harness::received(&h.github);
    assert_eq!(seen.len(), 4);
    seen
}

fn authorizations(seen: &[Captured]) -> Vec<&str> {
    seen.iter()
        .map(|request| request.headers["authorization"].to_str().unwrap())
        .collect()
}

#[tokio::test]
async fn a_member_identity_pushes_with_the_member_token_and_fetches_with_the_installation_token() {
    let h = harness_with(member_fake(GitIdentity::Member, true)).await;
    let seen = push_and_fetch(&h).await;
    let member = basic(MEMBER_TOKEN);
    let installation = basic(INSTALLATION_TOKEN);
    assert_eq!(
        authorizations(&seen),
        [&member, &member, &installation, &installation]
    );
}

#[tokio::test]
async fn an_app_identity_pushes_with_the_installation_token_even_if_the_member_is_connected() {
    let h = harness_with(member_fake(GitIdentity::App, true)).await;
    let seen = push_and_fetch(&h).await;
    let installation = basic(INSTALLATION_TOKEN);
    assert_eq!(authorizations(&seen), [&installation; 4]);
}

#[tokio::test]
async fn a_member_without_a_token_falls_back_to_the_installation_token() {
    let h = harness_with(member_fake(GitIdentity::Member, false)).await;
    let seen = push_and_fetch(&h).await;
    let installation = basic(INSTALLATION_TOKEN);
    assert_eq!(authorizations(&seen), [&installation; 4]);
}

#[tokio::test]
async fn a_member_identity_without_an_initiator_pushes_with_the_installation_token() {
    let mut fake = Fake::with(TaskGrant {
        git_identity: GitIdentity::Member,
        initiated_by: None,
        ..grant()
    });
    fake.member_tokens.insert(INITIATOR, MEMBER_TOKEN);
    let h = harness_with(fake).await;
    let seen = push_and_fetch(&h).await;
    let installation = basic(INSTALLATION_TOKEN);
    assert_eq!(authorizations(&seen), [&installation; 4]);
}

#[tokio::test]
async fn a_personal_token_the_member_saved_is_what_receive_pack_is_authenticated_with() {
    const PERSONAL_TOKEN: &str = "github_pat_member_personal";
    let mut fake = member_fake(GitIdentity::Member, false);
    fake.member_tokens.insert(INITIATOR, PERSONAL_TOKEN);
    let h = harness_with(fake).await;
    let seen = push_and_fetch(&h).await;
    let personal = basic(PERSONAL_TOKEN);
    let installation = basic(INSTALLATION_TOKEN);
    assert_eq!(
        authorizations(&seen),
        [&personal, &personal, &installation, &installation]
    );
}
