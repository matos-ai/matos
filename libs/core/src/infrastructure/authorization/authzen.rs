use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use matos_common::{CoreError, OrganizationId};
use reqwest::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::sync::Mutex;

use super::evaluation_of;
use crate::{
    domain::{
        agent::SecretToken,
        authorization::{AccessRequest, Decision, ports::Authorizer},
    },
    infrastructure::postgres::map_sqlx_error,
};

pub const PDP_UNAVAILABLE: &str = "pdp_unavailable";

const HTTP_TIMEOUT: Duration = Duration::from_secs(5);
const TOKEN_MARGIN: Duration = Duration::from_secs(30);
const DEFAULT_TOKEN_LIFETIME: u64 = 60;
const MAX_BATCH: usize = 100;
const GRANT_CLIENT_CREDENTIALS: &str = "client_credentials";

#[derive(Clone)]
pub struct PdpCredentials {
    pub client_id: String,
    pub client_secret: SecretToken,
}

impl std::fmt::Debug for PdpCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PdpCredentials")
            .field("client_id", &self.client_id)
            .field("client_secret", &"***")
            .finish()
    }
}

pub trait Issuers: Send + Sync {
    fn issuer_of(
        &self,
        organization_id: OrganizationId,
    ) -> impl Future<Output = Result<String, CoreError>> + Send;
}

#[derive(Clone)]
pub struct PgIssuers {
    pool: PgPool,
}

impl PgIssuers {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl Issuers for PgIssuers {
    async fn issuer_of(&self, organization_id: OrganizationId) -> Result<String, CoreError> {
        sqlx::query_scalar::<_, String>("SELECT issuer_url FROM organizations WHERE id = $1")
            .bind(organization_id.0)
            .fetch_optional(&self.pool)
            .await
            .map_err(map_sqlx_error)?
            .ok_or(CoreError::NotFound)
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: Option<u64>,
}

#[derive(Deserialize)]
struct Verdict {
    decision: bool,
    #[serde(default)]
    context: Option<VerdictContext>,
}

#[derive(Deserialize)]
struct VerdictContext {
    #[serde(default)]
    error: Option<Value>,
}

#[derive(Deserialize)]
struct Verdicts {
    evaluations: Vec<Verdict>,
}

struct CachedToken {
    token: SecretToken,
    expires_at: Instant,
}

struct Inner<I> {
    issuers: I,
    credentials: PdpCredentials,
    http: reqwest::Client,
    tokens: Mutex<HashMap<String, CachedToken>>,
}

pub struct AuthzenAuthorizer<I> {
    inner: Arc<Inner<I>>,
}

impl<I> Clone for AuthzenAuthorizer<I> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

fn unavailable(what: &str, error: impl std::fmt::Display) -> CoreError {
    CoreError::Internal(format!("policy decision point: {what}: {error}"))
}

fn deny_unavailable() -> Decision {
    Decision::Deny {
        reason: PDP_UNAVAILABLE.to_owned(),
    }
}

fn decision_of(request: &AccessRequest, verdict: &Verdict) -> Decision {
    if verdict.decision {
        return Decision::Allow;
    }
    if let Some(error) = verdict
        .context
        .as_ref()
        .and_then(|context| context.error.as_ref())
    {
        tracing::warn!(%error, action = request.action.as_str(), "the policy decision point rejected the request");
    }
    Decision::Deny {
        reason: format!(
            "{} denied by the policy decision point",
            request.action.as_str()
        ),
    }
}

impl<I: Issuers> AuthzenAuthorizer<I> {
    pub fn new(issuers: I, credentials: PdpCredentials) -> Self {
        let http = reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .build()
            .unwrap_or_default();
        Self {
            inner: Arc::new(Inner {
                issuers,
                credentials,
                http,
                tokens: Mutex::new(HashMap::new()),
            }),
        }
    }

    async fn token(&self, issuer: &str) -> Result<SecretToken, CoreError> {
        let mut tokens = self.inner.tokens.lock().await;
        if let Some(cached) = tokens.get(issuer)
            && cached.expires_at > Instant::now()
        {
            return Ok(cached.token.clone());
        }
        let response = self
            .inner
            .http
            .post(format!("{issuer}/protocol/openid-connect/token"))
            .form(&[
                ("grant_type", GRANT_CLIENT_CREDENTIALS),
                ("client_id", &self.inner.credentials.client_id),
                (
                    "client_secret",
                    self.inner.credentials.client_secret.expose(),
                ),
            ])
            .send()
            .await
            .map_err(|e| unavailable("token", e.without_url()))?;
        if !response.status().is_success() {
            return Err(unavailable("token", response.status()));
        }
        let body: TokenResponse = response
            .json()
            .await
            .map_err(|e| unavailable("token", e.without_url()))?;
        let lifetime = Duration::from_secs(body.expires_in.unwrap_or(DEFAULT_TOKEN_LIFETIME));
        let token = SecretToken::new(body.access_token);
        tokens.insert(
            issuer.to_owned(),
            CachedToken {
                token: token.clone(),
                expires_at: Instant::now() + lifetime.saturating_sub(TOKEN_MARGIN),
            },
        );
        Ok(token)
    }

    async fn forget_token(&self, issuer: &str) {
        self.inner.tokens.lock().await.remove(issuer);
    }

    async fn post(&self, issuer: &str, path: &str, body: &Value) -> Result<Value, CoreError> {
        let token = self.token(issuer).await?;
        let response = self
            .inner
            .http
            .post(format!("{issuer}/authzen/access/v1/{path}"))
            .bearer_auth(token.expose())
            .json(body)
            .send()
            .await
            .map_err(|e| unavailable(path, e.without_url()))?;
        let status = response.status();
        if status == StatusCode::UNAUTHORIZED {
            self.forget_token(issuer).await;
        }
        if status != StatusCode::OK {
            return Err(unavailable(path, status));
        }
        response
            .json()
            .await
            .map_err(|e| unavailable(path, e.without_url()))
    }

    async fn issuer(&self, request: &AccessRequest) -> Result<String, CoreError> {
        let issuer = self
            .inner
            .issuers
            .issuer_of(request.subject.organization_id)
            .await
            .map_err(|e| unavailable("issuer", e))?;
        Ok(issuer.trim_end_matches('/').to_owned())
    }

    async fn ask(&self, request: &AccessRequest) -> Result<Decision, CoreError> {
        let issuer = self.issuer(request).await?;
        let answer = self
            .post(&issuer, "evaluation", &evaluation_of(request))
            .await?;
        let verdict: Verdict =
            serde_json::from_value(answer).map_err(|e| unavailable("evaluation", e))?;
        Ok(decision_of(request, &verdict))
    }

    async fn ask_batch(
        &self,
        issuer: &str,
        requests: &[&AccessRequest],
    ) -> Result<Vec<Decision>, CoreError> {
        let body = json!({
            "evaluations": requests.iter().map(|r| evaluation_of(r)).collect::<Vec<_>>(),
            "options": { "evaluations_semantic": "execute_all" },
        });
        let answer = self.post(issuer, "evaluations", &body).await?;
        let verdicts: Verdicts =
            serde_json::from_value(answer).map_err(|e| unavailable("evaluations", e))?;
        if verdicts.evaluations.len() != requests.len() {
            return Err(unavailable(
                "evaluations",
                format!(
                    "{} answers for {} requests",
                    verdicts.evaluations.len(),
                    requests.len()
                ),
            ));
        }
        Ok(requests
            .iter()
            .zip(&verdicts.evaluations)
            .map(|(request, verdict)| decision_of(request, verdict))
            .collect())
    }

    async fn ask_all(&self, requests: &[AccessRequest]) -> Vec<Decision> {
        let mut decisions = vec![deny_unavailable(); requests.len()];
        let mut by_issuer: Vec<(String, Vec<usize>)> = Vec::new();
        for (index, request) in requests.iter().enumerate() {
            let issuer = match self.issuer(request).await {
                Ok(issuer) => issuer,
                Err(error) => {
                    tracing::warn!(%error, "the policy decision point is unavailable");
                    continue;
                }
            };
            match by_issuer.iter_mut().find(|(known, _)| *known == issuer) {
                Some((_, indexes)) => indexes.push(index),
                None => by_issuer.push((issuer, vec![index])),
            }
        }
        for (issuer, indexes) in &by_issuer {
            for chunk in indexes.chunks(MAX_BATCH) {
                let batch: Vec<&AccessRequest> = chunk.iter().map(|&i| &requests[i]).collect();
                match self.ask_batch(issuer, &batch).await {
                    Ok(answers) => {
                        for (&index, answer) in chunk.iter().zip(answers) {
                            decisions[index] = answer;
                        }
                    }
                    Err(error) => {
                        tracing::warn!(%error, "the policy decision point is unavailable");
                    }
                }
            }
        }
        decisions
    }
}

impl<I: Issuers> Authorizer for AuthzenAuthorizer<I> {
    async fn evaluate(&self, request: &AccessRequest) -> Result<Decision, CoreError> {
        match self.ask(request).await {
            Ok(decision) => Ok(decision),
            Err(error) => {
                tracing::warn!(%error, "the policy decision point is unavailable");
                Ok(deny_unavailable())
            }
        }
    }

    async fn evaluate_all(&self, requests: &[AccessRequest]) -> Result<Vec<Decision>, CoreError> {
        Ok(self.ask_all(requests).await)
    }
}

#[cfg(test)]
mod tests {
    use matos_common::OrganizationId;
    use serde_json::json;

    use super::*;
    use crate::{
        domain::{
            authorization::{Action, ResourceKind, Subject},
            card::Column,
        },
        infrastructure::authorization::fake_server::FakeServer,
    };

    struct Fixed(String);

    impl Issuers for Fixed {
        async fn issuer_of(&self, _: OrganizationId) -> Result<String, CoreError> {
            Ok(self.0.clone())
        }
    }

    fn credentials() -> PdpCredentials {
        PdpCredentials {
            client_id: "matos-api".into(),
            client_secret: SecretToken::new("s3cret"),
        }
    }

    fn request(action: Action) -> AccessRequest {
        AccessRequest::new(
            Subject {
                id: "user-1".into(),
                organization_id: OrganizationId::new(),
                actor: None,
                is_admin: false,
            },
            action,
            ResourceKind::Card,
            None,
        )
    }

    fn token_route(expires_in: u64) -> (&'static str, &'static str, u16, String) {
        (
            "POST",
            "/realms/acme/protocol/openid-connect/token",
            200,
            json!({"access_token": "pdp-token", "expires_in": expires_in}).to_string(),
        )
    }

    fn authorizer(server: &FakeServer) -> AuthzenAuthorizer<Fixed> {
        AuthzenAuthorizer::new(Fixed(server.url("/realms/acme")), credentials())
    }

    fn denied_as_unavailable(decision: &Decision) -> bool {
        matches!(decision, Decision::Deny { reason } if reason == PDP_UNAVAILABLE)
    }

    #[tokio::test]
    async fn an_unreachable_pdp_denies_as_unavailable() {
        let closed = Fixed("http://127.0.0.1:1/realms/acme".into());
        let authorizer = AuthzenAuthorizer::new(closed, credentials());
        let decision = authorizer
            .evaluate(&request(Action::CardCreate))
            .await
            .unwrap();
        assert!(denied_as_unavailable(&decision), "{decision:?}");
    }

    #[tokio::test]
    async fn an_unknown_organization_denies_as_unavailable() {
        struct Nobody;
        impl Issuers for Nobody {
            async fn issuer_of(&self, _: OrganizationId) -> Result<String, CoreError> {
                Err(CoreError::NotFound)
            }
        }
        let decision = AuthzenAuthorizer::new(Nobody, credentials())
            .evaluate(&request(Action::CardCreate))
            .await
            .unwrap();
        assert!(denied_as_unavailable(&decision), "{decision:?}");
    }

    #[tokio::test]
    async fn an_invalid_answer_denies_as_unavailable() {
        for body in ["not json", "{}", r#"{"decision":"yes"}"#] {
            let server = FakeServer::start(vec![
                token_route(3600),
                (
                    "POST",
                    "/realms/acme/authzen/access/v1/evaluation",
                    200,
                    body.to_owned(),
                ),
            ]);
            let decision = authorizer(&server)
                .evaluate(&request(Action::CardCreate))
                .await
                .unwrap();
            assert!(denied_as_unavailable(&decision), "{body}: {decision:?}");
        }
    }

    #[tokio::test]
    async fn a_failing_pdp_denies_as_unavailable() {
        for status in [401, 403, 404, 500] {
            let server = FakeServer::start(vec![
                token_route(3600),
                (
                    "POST",
                    "/realms/acme/authzen/access/v1/evaluation",
                    status,
                    "{}".to_owned(),
                ),
            ]);
            let decision = authorizer(&server)
                .evaluate(&request(Action::CardCreate))
                .await
                .unwrap();
            assert!(denied_as_unavailable(&decision), "{status}: {decision:?}");
        }
    }

    #[tokio::test]
    async fn a_token_that_cannot_be_obtained_denies_as_unavailable() {
        let server = FakeServer::start(vec![(
            "POST",
            "/realms/acme/protocol/openid-connect/token",
            401,
            r#"{"error":"invalid_client"}"#.to_owned(),
        )]);
        let decision = authorizer(&server)
            .evaluate(&request(Action::CardCreate))
            .await
            .unwrap();
        assert!(denied_as_unavailable(&decision), "{decision:?}");
        assert!(
            server
                .requests()
                .iter()
                .all(|r| !r.path.contains("authzen"))
        );
    }

    #[tokio::test]
    async fn the_pdp_receives_the_mapped_request_with_the_client_token() {
        let server = FakeServer::start(vec![
            token_route(3600),
            (
                "POST",
                "/realms/acme/authzen/access/v1/evaluation",
                200,
                r#"{"decision":true}"#.to_owned(),
            ),
        ]);
        let asked = request(Action::CardMove {
            from: Column::Proposed,
            to: Column::Todo,
        });
        let decision = authorizer(&server).evaluate(&asked).await.unwrap();
        assert_eq!(decision, Decision::Allow);

        let requests = server.requests();
        let token = &requests[0];
        assert!(token.body.contains("grant_type=client_credentials"));
        assert!(token.body.contains("client_id=matos-api"));
        assert!(token.body.contains("client_secret=s3cret"));
        let evaluation = &requests[1];
        assert_eq!(evaluation.header("authorization"), Some("Bearer pdp-token"));
        let sent: Value = serde_json::from_str(&evaluation.body).unwrap();
        assert_eq!(sent, evaluation_of(&asked));
    }

    #[tokio::test]
    async fn a_refusal_is_a_denial_naming_the_action() {
        let server = FakeServer::start(vec![
            token_route(3600),
            (
                "POST",
                "/realms/acme/authzen/access/v1/evaluation",
                200,
                r#"{"decision":false,"context":{"reason_admin":{"policies":["p"]}}}"#.to_owned(),
            ),
        ]);
        let decision = authorizer(&server)
            .evaluate(&request(Action::CardCreate))
            .await
            .unwrap();
        let Decision::Deny { reason } = decision else {
            panic!("expected a denial");
        };
        assert!(reason.contains("card.create"), "{reason}");
        assert!(!reason.contains("pdp_unavailable"), "{reason}");
    }

    #[tokio::test]
    async fn the_token_is_fetched_once_and_reused_until_it_expires() {
        let evaluation = (
            "POST",
            "/realms/acme/authzen/access/v1/evaluation",
            200,
            r#"{"decision":true}"#.to_owned(),
        );
        let server = FakeServer::start(vec![token_route(3600), evaluation.clone()]);
        let authorizer = authorizer(&server);
        for _ in 0..3 {
            authorizer
                .evaluate(&request(Action::CardCreate))
                .await
                .unwrap();
        }
        assert_eq!(
            server.count("/realms/acme/protocol/openid-connect/token"),
            1
        );

        let short = FakeServer::start(vec![token_route(1), evaluation]);
        let authorizer = self::authorizer(&short);
        for _ in 0..2 {
            authorizer
                .evaluate(&request(Action::CardCreate))
                .await
                .unwrap();
        }
        assert_eq!(short.count("/realms/acme/protocol/openid-connect/token"), 2);
    }

    #[tokio::test]
    async fn a_batch_is_one_evaluations_call_answered_in_order() {
        let server = FakeServer::start(vec![
            token_route(3600),
            (
                "POST",
                "/realms/acme/authzen/access/v1/evaluations",
                200,
                json!({"evaluations": [
                    {"decision": true},
                    {"decision": false},
                    {"decision": true, "context": {"reason_admin": {"policies": ["p"]}}},
                ]})
                .to_string(),
            ),
        ]);
        let org = OrganizationId::new();
        let mut asked = vec![
            request(Action::CardCreate),
            request(Action::CardComment),
            request(Action::CardRead),
        ];
        for r in &mut asked {
            r.subject.organization_id = org;
            r.resource.organization_id = org;
        }
        let decisions = authorizer(&server).evaluate_all(&asked).await.unwrap();

        assert_eq!(decisions.len(), 3);
        assert_eq!(decisions[0], Decision::Allow);
        assert!(
            matches!(&decisions[1], Decision::Deny { reason } if reason.contains("card.comment"))
        );
        assert_eq!(decisions[2], Decision::Allow);
        assert_eq!(
            server.count("/realms/acme/authzen/access/v1/evaluations"),
            1
        );
        assert_eq!(server.count("/realms/acme/authzen/access/v1/evaluation"), 0);
        let requests = server.requests();
        let body: Value = serde_json::from_str(&requests[1].body).unwrap();
        let sent = body["evaluations"].as_array().unwrap();
        assert_eq!(sent.len(), 3);
        for (sent, asked) in sent.iter().zip(&asked) {
            assert_eq!(sent, &evaluation_of(asked));
        }
        assert_eq!(
            requests[1].header("authorization"),
            Some("Bearer pdp-token")
        );
    }

    #[tokio::test]
    async fn a_batch_with_a_wrong_number_of_answers_denies_all_as_unavailable() {
        let server = FakeServer::start(vec![
            token_route(3600),
            (
                "POST",
                "/realms/acme/authzen/access/v1/evaluations",
                200,
                json!({"evaluations": [{"decision": true}]}).to_string(),
            ),
        ]);
        let asked = [request(Action::CardCreate), request(Action::CardComment)];
        let decisions = authorizer(&server).evaluate_all(&asked).await.unwrap();
        assert!(decisions.iter().all(denied_as_unavailable), "{decisions:?}");
    }

    #[tokio::test]
    async fn an_empty_batch_makes_no_call() {
        let server = FakeServer::start(vec![]);
        let decisions = authorizer(&server).evaluate_all(&[]).await.unwrap();
        assert!(decisions.is_empty());
        assert!(server.requests().is_empty());
    }

    #[tokio::test]
    async fn an_unreachable_pdp_denies_every_write_of_a_batch() {
        let closed = Fixed("http://127.0.0.1:1/realms/acme".into());
        let authorizer = AuthzenAuthorizer::new(closed, credentials());
        let asked = [request(Action::CardCreate), request(Action::CardComment)];
        let decisions = authorizer.evaluate_all(&asked).await.unwrap();
        assert_eq!(decisions.len(), 2);
        assert!(decisions.iter().all(denied_as_unavailable), "{decisions:?}");
    }

    fn real_pdp() -> Option<AuthzenAuthorizer<Fixed>> {
        let issuer = std::env::var("MATOS_PDP_ISSUER").ok()?;
        let secret = std::env::var("MATOS_PDP_CLIENT_SECRET").ok()?;
        Some(AuthzenAuthorizer::new(
            Fixed(issuer),
            PdpCredentials {
                client_id: "matos-api".into(),
                client_secret: SecretToken::new(secret),
            },
        ))
    }

    #[tokio::test]
    #[ignore = "needs a FerrisKey with the Matos policies: MATOS_PDP_ISSUER and MATOS_PDP_CLIENT_SECRET"]
    async fn the_real_pdp_decides_like_the_local_engine() {
        let Some(pdp) = real_pdp() else {
            panic!("MATOS_PDP_ISSUER and MATOS_PDP_CLIENT_SECRET are required");
        };
        let org = OrganizationId::new();
        let in_org = |action: Action, actor: Option<&str>| {
            let mut asked = request(action);
            asked.subject.organization_id = org;
            asked.resource.organization_id = org;
            asked.subject.actor = actor.map(str::to_owned);
            asked
        };
        let create = in_org(Action::CardCreate, None);
        let agent_move = in_org(
            Action::CardMove {
                from: Column::Proposed,
                to: Column::Todo,
            },
            Some("matos-agent-captain"),
        );
        let human_move = in_org(
            Action::CardMove {
                from: Column::Proposed,
                to: Column::Todo,
            },
            None,
        );
        let mut foreign = in_org(Action::CardRead, None);
        foreign.resource.organization_id = OrganizationId::new();

        assert_eq!(pdp.evaluate(&create).await.unwrap(), Decision::Allow);
        assert!(matches!(
            pdp.evaluate(&agent_move).await.unwrap(),
            Decision::Deny { reason } if reason != PDP_UNAVAILABLE
        ));
        let decisions = pdp
            .evaluate_all(&[create, agent_move, human_move, foreign])
            .await
            .unwrap();
        assert_eq!(decisions[0], Decision::Allow);
        assert!(matches!(&decisions[1], Decision::Deny { reason } if reason != PDP_UNAVAILABLE));
        assert_eq!(decisions[2], Decision::Allow);
        assert!(matches!(&decisions[3], Decision::Deny { reason } if reason != PDP_UNAVAILABLE));
    }
}
