use std::{collections::HashMap, fmt, sync::Arc};

use chrono::{DateTime, Duration, Utc};
use matos_core::domain::{
    agent::SecretToken,
    execution::{
        DraftPullRequest, GithubAppCredentials, PullRequestUrl, RepositoryIssue, RepositoryListing,
        RepositoryRef,
    },
    github_user::{GithubUser, UserTokens},
};
use reqwest::{
    RequestBuilder, Response,
    header::{ACCEPT, AUTHORIZATION, LINK, USER_AGENT},
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::{GithubError, jwt};

pub(crate) const API_VERSION: &str = "2022-11-28";
pub(crate) const AGENT: &str = concat!("matos/", env!("CARGO_PKG_VERSION"));
const REFRESH_MARGIN_MINUTES: i64 = 5;
const ISSUE_PAGES: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GithubBases {
    pub api_base: String,
    pub web_base: String,
}

impl Default for GithubBases {
    fn default() -> Self {
        Self {
            api_base: "https://api.github.com".to_owned(),
            web_base: "https://github.com".to_owned(),
        }
    }
}

#[derive(Clone)]
pub struct GithubApp {
    pub app_id: u64,
    pub private_key_pem: SecretToken,
    pub api_base: String,
    pub web_base: String,
    pub client_id: String,
    pub client_secret: SecretToken,
}

impl GithubApp {
    pub fn new(
        credentials: &GithubAppCredentials,
        bases: &GithubBases,
    ) -> Result<Self, GithubError> {
        let app_id = u64::try_from(credentials.app_id)
            .map_err(|_| GithubError::InvalidApp("the App id is not a positive number".into()))?;
        Ok(Self {
            app_id,
            private_key_pem: credentials.private_key_pem.clone(),
            api_base: bases.api_base.clone(),
            web_base: bases.web_base.clone(),
            client_id: credentials.client_id.clone(),
            client_secret: credentials.client_secret.clone(),
        })
    }
}

impl fmt::Debug for GithubApp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GithubApp")
            .field("app_id", &self.app_id)
            .field("api_base", &self.api_base)
            .field("web_base", &self.web_base)
            .field("client_id", &self.client_id)
            .finish_non_exhaustive()
    }
}

pub struct UserToken {
    token: SecretToken,
    expires_at: Option<DateTime<Utc>>,
    refresh_token: Option<SecretToken>,
    refresh_expires_at: Option<DateTime<Utc>>,
}

impl UserToken {
    pub fn access_token(&self) -> &SecretToken {
        &self.token
    }

    pub fn into_tokens(self) -> UserTokens {
        UserTokens {
            access_token: self.token,
            expires_at: self.expires_at,
            refresh_token: self.refresh_token,
            refresh_expires_at: self.refresh_expires_at,
        }
    }
}

impl fmt::Debug for UserToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UserToken")
            .field("token", &"***")
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub struct InstallationToken {
    pub token: SecretToken,
    pub expires_at: DateTime<Utc>,
}

impl fmt::Debug for InstallationToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InstallationToken")
            .field("token", &"***")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

impl InstallationToken {
    fn is_fresh(&self, now: DateTime<Utc>) -> bool {
        now < self.expires_at - Duration::minutes(REFRESH_MARGIN_MINUTES)
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    token: String,
    expires_at: DateTime<Utc>,
}

#[derive(Deserialize)]
struct OAuthResponse {
    access_token: Option<String>,
    expires_in: Option<i64>,
    refresh_token: Option<String>,
    refresh_token_expires_in: Option<i64>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct UserProfile {
    id: i64,
    login: String,
}

#[derive(Deserialize)]
struct RepositoryPermissions {
    #[serde(default)]
    push: bool,
}

#[derive(Deserialize)]
struct RepositoryAccess {
    permissions: Option<RepositoryPermissions>,
}

#[derive(Deserialize)]
struct UserInstallationsPage {
    installations: Vec<UserInstallationEntry>,
}

#[derive(Deserialize)]
struct UserInstallationEntry {
    id: i64,
}

#[derive(Deserialize)]
struct RepositoriesPage {
    repositories: Vec<RepositoryEntry>,
}

#[derive(Deserialize)]
struct RepositoryEntry {
    full_name: String,
    #[serde(default)]
    private: bool,
    #[serde(default)]
    html_url: Option<String>,
}

#[derive(Deserialize)]
struct IssueEntry {
    number: u64,
    title: String,
    body: Option<String>,
    html_url: String,
    #[serde(default)]
    labels: Vec<IssueLabel>,
    pull_request: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct IssueLabel {
    name: String,
}

#[derive(Deserialize)]
struct RepositoryDetails {
    default_branch: String,
}

#[derive(Deserialize)]
struct InstallationDetails {
    account: AccountEntry,
}

#[derive(Deserialize)]
struct AccountEntry {
    login: String,
    #[serde(rename = "type", default)]
    kind: String,
}

#[derive(Deserialize)]
struct Membership {
    role: String,
    state: String,
}

#[derive(Deserialize)]
struct AuthenticatedUser {
    login: String,
}

#[derive(Deserialize)]
struct RefDetails {
    object: RefObject,
}

#[derive(Deserialize)]
struct RefObject {
    sha: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountKind {
    User,
    Organization,
    Other,
}

/// The account an installation belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallationAccount {
    pub login: String,
    pub kind: AccountKind,
}

#[derive(Deserialize)]
struct PullRequestResponse {
    html_url: String,
}

#[derive(Deserialize)]
struct ErrorBody {
    message: String,
    #[serde(default)]
    errors: Vec<ErrorDetail>,
}

#[derive(Deserialize)]
struct ErrorDetail {
    message: Option<String>,
}

#[derive(Deserialize)]
struct PullRequestListEntry {
    html_url: String,
}

const PULL_REQUEST_EXISTS: &str = "A pull request already exists";

/// What an installation token may reach.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum TokenScope {
    /// Every repository the installation sees, metadata only.
    Installation,
    /// One repository: push its branches and open pull requests.
    Write(RepositoryRef),
    /// One repository, read-only: fetch it.
    Read(RepositoryRef),
}

fn token_request(scope: &TokenScope) -> Value {
    match scope {
        TokenScope::Installation => json!({"permissions": {"metadata": "read"}}),
        TokenScope::Write(repository) => json!({
            "repositories": [repository.name()],
            "permissions": {"contents": "write", "pull_requests": "write"},
        }),
        TokenScope::Read(repository) => json!({
            "repositories": [repository.name()],
            "permissions": {"contents": "read", "metadata": "read"},
        }),
    }
}

type CacheKey = (i64, TokenScope);

#[derive(Clone)]
pub struct GithubClient {
    app: GithubApp,
    http: reqwest::Client,
    cache: Arc<Mutex<HashMap<CacheKey, InstallationToken>>>,
}

impl GithubClient {
    pub fn new(app: GithubApp) -> Result<Self, GithubError> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(GithubError::transport)?;
        Ok(Self::with_http(app, http))
    }

    pub fn with_http(app: GithubApp, http: reqwest::Client) -> Self {
        Self {
            app,
            http,
            cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.app.api_base.trim_end_matches('/'))
    }

    fn request(&self, builder: RequestBuilder) -> RequestBuilder {
        builder
            .header(ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", API_VERSION)
            .header(USER_AGENT, AGENT)
    }

    pub(crate) async fn send(builder: RequestBuilder) -> Result<Response, GithubError> {
        let response = builder.send().await.map_err(GithubError::transport)?;
        if response.status().is_success() {
            return Ok(response);
        }
        let status = response.status();
        let message = response
            .json::<ErrorBody>()
            .await
            .map(|body| {
                let details: Vec<_> = body
                    .errors
                    .into_iter()
                    .filter_map(|detail| detail.message)
                    .collect();
                if details.is_empty() {
                    body.message
                } else {
                    format!("{}: {}", body.message, details.join("; "))
                }
            })
            .unwrap_or_else(|_| {
                status
                    .canonical_reason()
                    .unwrap_or("unknown error")
                    .to_owned()
            });
        Err(GithubError::Status {
            status: status.as_u16(),
            message,
        })
    }

    pub(crate) async fn decode<T: for<'de> Deserialize<'de>>(
        response: Response,
    ) -> Result<T, GithubError> {
        response
            .json()
            .await
            .map_err(|error| GithubError::Decode(error.without_url().to_string()))
    }

    /// `repository` of `None` asks for a token over every repository the
    /// installation can see, which listing repositories requires.
    async fn token_for(
        &self,
        installation_id: i64,
        repository: Option<&RepositoryRef>,
    ) -> Result<InstallationToken, GithubError> {
        let scope = match repository {
            Some(repository) => TokenScope::Write(repository.clone()),
            None => TokenScope::Installation,
        };
        self.scoped_token(installation_id, scope).await
    }

    async fn scoped_token(
        &self,
        installation_id: i64,
        scope: TokenScope,
    ) -> Result<InstallationToken, GithubError> {
        let body = token_request(&scope);
        let key = (installation_id, scope);
        if let Some(token) = self.cache.lock().await.get(&key)
            && token.is_fresh(Utc::now())
        {
            return Ok(token.clone());
        }

        let jwt = jwt::sign(
            self.app.app_id,
            self.app.private_key_pem.expose(),
            Utc::now(),
        )?;
        let response = Self::send(
            self.request(
                self.http
                    .post(self.url(&format!(
                        "/app/installations/{installation_id}/access_tokens"
                    )))
                    .bearer_auth(jwt)
                    .json(&body),
            ),
        )
        .await?;
        let issued: TokenResponse = Self::decode(response).await?;
        let token = InstallationToken {
            token: SecretToken::new(issued.token),
            expires_at: issued.expires_at,
        };
        self.cache.lock().await.insert(key, token.clone());
        Ok(token)
    }

    pub async fn installation_token(
        &self,
        installation_id: i64,
        repository: &RepositoryRef,
    ) -> Result<InstallationToken, GithubError> {
        self.token_for(installation_id, Some(repository)).await
    }

    /// A token that can only fetch `repository`: what a read-only run gets.
    pub async fn read_only_installation_token(
        &self,
        installation_id: i64,
        repository: &RepositoryRef,
    ) -> Result<InstallationToken, GithubError> {
        self.scoped_token(installation_id, TokenScope::Read(repository.clone()))
            .await
    }

    pub async fn default_branch(
        &self,
        installation_id: i64,
        repository: &RepositoryRef,
    ) -> Result<String, GithubError> {
        let token = self.token_for(installation_id, Some(repository)).await?;
        let response = Self::send(
            self.request(
                self.http
                    .get(self.url(&format!(
                        "/repos/{}/{}",
                        repository.owner(),
                        repository.name()
                    )))
                    .header(AUTHORIZATION, format!("Bearer {}", token.token.expose())),
            ),
        )
        .await?;
        Ok(Self::decode::<RepositoryDetails>(response)
            .await?
            .default_branch)
    }

    /// The login of the account the App is installed on; authenticated as the
    /// App itself, since the installation id comes from an untrusted redirect.
    pub async fn installation_account(
        &self,
        installation_id: i64,
    ) -> Result<InstallationAccount, GithubError> {
        let jwt = jwt::sign(
            self.app.app_id,
            self.app.private_key_pem.expose(),
            Utc::now(),
        )?;
        let response = Self::send(
            self.request(
                self.http
                    .get(self.url(&format!("/app/installations/{installation_id}")))
                    .bearer_auth(jwt),
            ),
        )
        .await?;
        let account = Self::decode::<InstallationDetails>(response).await?.account;
        Ok(InstallationAccount {
            kind: match account.kind.as_str() {
                "User" => AccountKind::User,
                "Organization" => AccountKind::Organization,
                _ => AccountKind::Other,
            },
            login: account.login,
        })
    }

    /// Whether the connecting user may bind this installation: an admin of the
    /// organization it is installed on, or the user it is installed on.
    pub async fn user_owns_installation(
        &self,
        token: &UserToken,
        account: &InstallationAccount,
    ) -> Result<bool, GithubError> {
        let authorization = format!("Bearer {}", token.token.expose());
        match account.kind {
            AccountKind::Organization => {
                let response = Self::send(
                    self.request(
                        self.http
                            .get(self.url(&format!("/user/memberships/orgs/{}", account.login)))
                            .header(AUTHORIZATION, authorization),
                    ),
                )
                .await;
                match response {
                    Ok(response) => {
                        let membership: Membership = Self::decode(response).await?;
                        Ok(membership.role == "admin" && membership.state == "active")
                    }
                    Err(GithubError::Status { status: 404, .. }) => Ok(false),
                    Err(GithubError::Status { status: 403, .. }) => {
                        Err(GithubError::MissingPermission("members: read"))
                    }
                    Err(error) => Err(error),
                }
            }
            AccountKind::User => {
                let response = Self::send(
                    self.request(
                        self.http
                            .get(self.url("/user"))
                            .header(AUTHORIZATION, authorization),
                    ),
                )
                .await?;
                let user: AuthenticatedUser = Self::decode(response).await?;
                Ok(user.login.eq_ignore_ascii_case(&account.login))
            }
            AccountKind::Other => Ok(false),
        }
    }

    pub async fn branch_sha(
        &self,
        installation_id: i64,
        repository: &RepositoryRef,
        branch: &str,
    ) -> Result<Option<String>, GithubError> {
        let token = self.token_for(installation_id, Some(repository)).await?;
        let response = Self::send(
            self.request(
                self.http
                    .get(self.url(&format!(
                        "/repos/{}/{}/git/ref/heads/{branch}",
                        repository.owner(),
                        repository.name()
                    )))
                    .header(AUTHORIZATION, format!("Bearer {}", token.token.expose())),
            ),
        )
        .await;
        match response {
            Ok(response) => Ok(Some(Self::decode::<RefDetails>(response).await?.object.sha)),
            Err(GithubError::Status { status: 404, .. }) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// The text of a file at a ref, `None` if there is no such file.
    pub async fn file_text(
        &self,
        installation_id: i64,
        repository: &RepositoryRef,
        git_ref: &str,
        path: &str,
    ) -> Result<Option<String>, GithubError> {
        let token = self.token_for(installation_id, Some(repository)).await?;
        let url = self.url(&format!(
            "/repos/{}/{}/contents/{}?ref={}",
            repository.owner(),
            repository.name(),
            encode_path(path),
            encode_segment(git_ref)
        ));
        let response = Self::send(
            self.http
                .get(url)
                .header(ACCEPT, "application/vnd.github.raw+json")
                .header("X-GitHub-Api-Version", API_VERSION)
                .header(USER_AGENT, AGENT)
                .header(AUTHORIZATION, format!("Bearer {}", token.token.expose())),
        )
        .await;
        match response {
            Ok(response) => response
                .text()
                .await
                .map(Some)
                .map_err(|error| GithubError::Decode(error.without_url().to_string())),
            Err(GithubError::Status { status: 404, .. }) => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn oauth_url(&self, path: &str) -> String {
        format!("{}{path}", self.app.web_base.trim_end_matches('/'))
    }

    async fn request_user_token(&self, grant: Value) -> Result<UserToken, GithubError> {
        let mut body = json!({
            "client_id": self.app.client_id,
            "client_secret": self.app.client_secret.expose(),
        });
        if let (Some(body), Value::Object(grant)) = (body.as_object_mut(), grant) {
            body.extend(grant);
        }
        let response = Self::send(
            self.http
                .post(self.oauth_url("/login/oauth/access_token"))
                .header(ACCEPT, "application/json")
                .header(USER_AGENT, AGENT)
                .json(&body),
        )
        .await?;
        let exchanged: OAuthResponse = Self::decode(response).await?;
        let Some(token) = exchanged.access_token else {
            return Err(GithubError::Status {
                status: 400,
                message: exchanged
                    .error
                    .unwrap_or_else(|| "no access token returned".to_owned()),
            });
        };
        let now = Utc::now();
        let expiry = |seconds: Option<i64>| seconds.map(|seconds| now + Duration::seconds(seconds));
        Ok(UserToken {
            token: SecretToken::new(token),
            expires_at: expiry(exchanged.expires_in),
            refresh_token: exchanged.refresh_token.map(SecretToken::new),
            refresh_expires_at: expiry(exchanged.refresh_token_expires_in),
        })
    }

    pub async fn exchange_user_code(&self, code: &str) -> Result<UserToken, GithubError> {
        self.request_user_token(json!({"code": code})).await
    }

    pub async fn refresh_user_token(&self, refresh_token: &str) -> Result<UserToken, GithubError> {
        self.request_user_token(json!({
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
        }))
        .await
    }

    pub fn authorize_url(&self, state: &str) -> String {
        self.oauth_url(&format!(
            "/login/oauth/authorize?client_id={}&state={}",
            encode_segment(&self.app.client_id),
            encode_segment(state)
        ))
    }

    pub async fn authenticated_user(&self, access_token: &str) -> Result<GithubUser, GithubError> {
        let response = Self::send(
            self.request(
                self.http
                    .get(self.url("/user"))
                    .header(AUTHORIZATION, format!("Bearer {access_token}")),
            ),
        )
        .await?;
        let user: UserProfile = Self::decode(response).await?;
        Ok(GithubUser {
            id: user.id,
            login: user.login,
        })
    }

    pub async fn user_can_push(
        &self,
        access_token: &str,
        repository: &RepositoryRef,
    ) -> Result<bool, GithubError> {
        let response = Self::send(
            self.request(
                self.http
                    .get(self.url(&format!(
                        "/repos/{}/{}",
                        repository.owner(),
                        repository.name()
                    )))
                    .header(AUTHORIZATION, format!("Bearer {access_token}")),
            ),
        )
        .await;
        match response {
            Ok(response) => Ok(Self::decode::<RepositoryAccess>(response)
                .await?
                .permissions
                .is_some_and(|permissions| permissions.push)),
            Err(GithubError::Status { status: 404, .. }) => Ok(false),
            Err(error) => Err(error),
        }
    }

    pub async fn revoke_user_grant(&self, access_token: &str) -> Result<(), GithubError> {
        let response = Self::send(
            self.request(
                self.http
                    .delete(self.url(&format!("/applications/{}/grant", self.app.client_id)))
                    .basic_auth(&self.app.client_id, Some(self.app.client_secret.expose()))
                    .json(&json!({"access_token": access_token})),
            ),
        )
        .await;
        match response {
            Ok(_) | Err(GithubError::Status { status: 404, .. }) => Ok(()),
            Err(error) => Err(error),
        }
    }

    pub async fn user_can_access_installation(
        &self,
        token: &UserToken,
        installation_id: i64,
    ) -> Result<bool, GithubError> {
        let api_base = self.app.api_base.trim_end_matches('/');
        let mut next = Some(self.url("/user/installations?per_page=100"));
        while let Some(url) = next {
            let response = Self::send(
                self.request(
                    self.http
                        .get(&url)
                        .header(AUTHORIZATION, format!("Bearer {}", token.token.expose())),
                ),
            )
            .await?;
            next = response
                .headers()
                .get(LINK)
                .and_then(|value| value.to_str().ok())
                .and_then(next_link)
                .filter(|link| same_origin(link, api_base))
                .map(str::to_owned);
            let page: UserInstallationsPage = Self::decode(response).await?;
            if page
                .installations
                .iter()
                .any(|entry| entry.id == installation_id)
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The open issues of `repository`, newest first, pull requests excluded.
    /// Stops after `ISSUE_PAGES` pages. Needs the `issues: read` permission,
    /// which an App created before it was declared must be re-approved for.
    pub async fn list_open_issues(
        &self,
        installation_id: i64,
        repository: &RepositoryRef,
    ) -> Result<Vec<RepositoryIssue>, GithubError> {
        let token = self.issues_token(installation_id, repository).await?;
        let api_base = self.app.api_base.trim_end_matches('/');
        let mut next = Some(self.url(&format!(
            "/repos/{}/{}/issues?state=open&sort=created&direction=desc&per_page=100",
            repository.owner(),
            repository.name()
        )));
        let mut issues = Vec::new();
        for _ in 0..ISSUE_PAGES {
            let Some(url) = next.take() else { break };
            let response = Self::send(
                self.request(
                    self.http
                        .get(&url)
                        .header(AUTHORIZATION, format!("Bearer {}", token.token.expose())),
                ),
            )
            .await
            .map_err(missing_issues_permission)?;
            next = response
                .headers()
                .get(LINK)
                .and_then(|value| value.to_str().ok())
                .and_then(next_link)
                .filter(|link| same_origin(link, api_base))
                .map(str::to_owned);
            let page: Vec<IssueEntry> = Self::decode(response).await?;
            issues.extend(
                page.into_iter()
                    .filter(|entry| entry.pull_request.is_none())
                    .map(|entry| RepositoryIssue {
                        number: entry.number,
                        title: entry.title,
                        body: entry.body.unwrap_or_default(),
                        html_url: entry.html_url,
                        labels: entry.labels.into_iter().map(|label| label.name).collect(),
                    }),
            );
        }
        Ok(issues)
    }

    async fn issues_token(
        &self,
        installation_id: i64,
        repository: &RepositoryRef,
    ) -> Result<InstallationToken, GithubError> {
        let jwt = jwt::sign(
            self.app.app_id,
            self.app.private_key_pem.expose(),
            Utc::now(),
        )?;
        let response = Self::send(
            self.request(
                self.http
                    .post(self.url(&format!(
                        "/app/installations/{installation_id}/access_tokens"
                    )))
                    .bearer_auth(jwt)
                    .json(&json!({
                        "repositories": [repository.name()],
                        "permissions": {"issues": "read"},
                    })),
            ),
        )
        .await
        .map_err(missing_issues_permission)?;
        let issued: TokenResponse = Self::decode(response).await?;
        Ok(InstallationToken {
            token: SecretToken::new(issued.token),
            expires_at: issued.expires_at,
        })
    }

    pub async fn list_repositories(
        &self,
        installation_id: i64,
    ) -> Result<Vec<RepositoryRef>, GithubError> {
        Ok(self
            .list_repository_listing(installation_id)
            .await?
            .into_iter()
            .map(|listing| listing.repository)
            .collect())
    }

    pub async fn list_repository_listing(
        &self,
        installation_id: i64,
    ) -> Result<Vec<RepositoryListing>, GithubError> {
        let token = self.token_for(installation_id, None).await?;
        let api_base = self.app.api_base.trim_end_matches('/');
        let mut next = Some(self.url("/installation/repositories?per_page=100"));
        let mut repositories = Vec::new();
        while let Some(url) = next {
            let response = Self::send(
                self.request(
                    self.http
                        .get(&url)
                        .header(AUTHORIZATION, format!("Bearer {}", token.token.expose())),
                ),
            )
            .await?;
            next = response
                .headers()
                .get(LINK)
                .and_then(|value| value.to_str().ok())
                .and_then(next_link)
                .filter(|link| same_origin(link, api_base))
                .map(str::to_owned);
            let page: RepositoriesPage = Self::decode(response).await?;
            for entry in page.repositories {
                match entry.full_name.parse::<RepositoryRef>() {
                    Ok(repository) => {
                        let html_url = entry.html_url.unwrap_or_else(|| {
                            format!(
                                "{}/{}",
                                self.app.web_base.trim_end_matches('/'),
                                entry.full_name
                            )
                        });
                        repositories.push(RepositoryListing {
                            repository,
                            private: entry.private,
                            html_url,
                        });
                    }
                    Err(_) => tracing::warn!(
                        full_name = %entry.full_name,
                        "skipping a repository with an unsupported name"
                    ),
                }
            }
        }
        Ok(repositories)
    }

    pub async fn create_draft_pr(
        &self,
        installation_id: i64,
        pull_request: &DraftPullRequest,
    ) -> Result<PullRequestUrl, GithubError> {
        let token = self
            .token_for(installation_id, Some(&pull_request.repository))
            .await?;
        self.open_draft_pr(token.token.expose(), pull_request).await
    }

    pub async fn create_draft_pr_as_user(
        &self,
        access_token: &str,
        pull_request: &DraftPullRequest,
    ) -> Result<PullRequestUrl, GithubError> {
        self.open_draft_pr(access_token, pull_request).await
    }

    pub async fn update_pull_request_body(
        &self,
        installation_id: i64,
        repository: &RepositoryRef,
        pull_request: &PullRequestUrl,
        body: &str,
    ) -> Result<(), GithubError> {
        let number = pull_request_number(&pull_request.0).ok_or_else(|| GithubError::Status {
            status: 422,
            message: "pull request URL without a number".to_owned(),
        })?;
        let token = self.token_for(installation_id, Some(repository)).await?;
        Self::send(
            self.request(
                self.http
                    .patch(self.url(&format!(
                        "/repos/{}/{}/pulls/{number}",
                        repository.owner(),
                        repository.name()
                    )))
                    .header(AUTHORIZATION, format!("Bearer {}", token.token.expose()))
                    .json(&json!({"body": body})),
            ),
        )
        .await?;
        Ok(())
    }

    async fn open_draft_pr(
        &self,
        bearer: &str,
        pull_request: &DraftPullRequest,
    ) -> Result<PullRequestUrl, GithubError> {
        let repository = &pull_request.repository;
        let pulls_url = self.url(&format!(
            "/repos/{}/{}/pulls",
            repository.owner(),
            repository.name()
        ));
        let created = Self::send(
            self.request(
                self.http
                    .post(&pulls_url)
                    .header(AUTHORIZATION, format!("Bearer {bearer}"))
                    .json(&json!({
                        "title": pull_request.title,
                        "head": pull_request.head,
                        "base": pull_request.base,
                        "body": pull_request.body,
                        "draft": true,
                    })),
            ),
        )
        .await;
        match created {
            Ok(response) => {
                let created: PullRequestResponse = Self::decode(response).await?;
                Ok(PullRequestUrl(created.html_url))
            }
            Err(GithubError::Status {
                status: 422,
                message,
            }) if message.contains(PULL_REQUEST_EXISTS) => {
                match self
                    .open_pull_request_of(&pulls_url, bearer, repository, &pull_request.head)
                    .await?
                {
                    Some(url) => Ok(url),
                    None => Err(GithubError::Status {
                        status: 422,
                        message,
                    }),
                }
            }
            Err(error) => Err(error),
        }
    }

    async fn open_pull_request_of(
        &self,
        pulls_url: &str,
        bearer: &str,
        repository: &RepositoryRef,
        branch: &str,
    ) -> Result<Option<PullRequestUrl>, GithubError> {
        let response = Self::send(
            self.request(
                self.http
                    .get(pulls_url)
                    .query(&[
                        ("head", format!("{}:{branch}", repository.owner())),
                        ("state", "open".to_owned()),
                    ])
                    .header(AUTHORIZATION, format!("Bearer {bearer}")),
            ),
        )
        .await?;
        let open: Vec<PullRequestListEntry> = Self::decode(response).await?;
        Ok(open
            .into_iter()
            .next()
            .map(|entry| PullRequestUrl(entry.html_url)))
    }
}

fn pull_request_number(url: &str) -> Option<u64> {
    let path = url.split(['?', '#']).next()?;
    let (_, number) = path.trim_end_matches('/').rsplit_once('/')?;
    number.parse().ok()
}

fn origin(url: &str) -> Option<(String, String, u16)> {
    let (scheme, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.contains('@') || authority.is_empty() {
        return None;
    }
    let default_port = match scheme {
        "https" => 443,
        "http" => 80,
        _ => return None,
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) if !host.ends_with(':') => (host, port.parse().ok()?),
        _ => (authority, default_port),
    };
    Some((scheme.to_ascii_lowercase(), host.to_ascii_lowercase(), port))
}

/// Same scheme, host and port: a `Link` header must not lead the client, and
/// the token it carries, to another server.
/// GitHub answers 403 or 422 when the App was not granted the permission the
/// token or the call asks for.
fn missing_issues_permission(error: GithubError) -> GithubError {
    match error {
        GithubError::Status {
            status: 403 | 422, ..
        } => GithubError::MissingPermission("issues"),
        other => other,
    }
}

fn same_origin(link: &str, base: &str) -> bool {
    match (origin(link), origin(base)) {
        (Some(link), Some(base)) => link == base,
        _ => false,
    }
}

pub(crate) fn next_link(header: &str) -> Option<&str> {
    header.split(',').find_map(|part| {
        let (target, params) = part.split_once(';')?;
        let is_next = params
            .split(';')
            .any(|param| param.trim().replace(' ', "") == "rel=\"next\"");
        let url = target.trim().strip_prefix('<')?.strip_suffix('>')?;
        is_next.then_some(url)
    })
}

fn encode_segment(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn encode_path(path: &str) -> String {
    path.split('/')
        .map(encode_segment)
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex as StdMutex};

    use axum::{
        Json, Router,
        extract::{Query, State},
        http::{HeaderMap, StatusCode as AxumStatus},
        response::IntoResponse,
        routing::{get, post},
    };
    use serde_json::Value;

    use super::*;
    use matos_common::CoreError;

    const PRIVATE: &str = include_str!("../tests/fixtures/test-only-app-key.pem");

    #[derive(Default)]
    struct Recorded {
        token_bodies: Vec<Value>,
        token_headers: Vec<HeaderMap>,
        pull_bodies: Vec<Value>,
        expires_at: Option<String>,
        fail_pulls_with: Option<u16>,
        pull_exists: bool,
        listed_pulls: Vec<Value>,
        account_kind: Option<String>,
        membership: Option<Value>,
        membership_forbidden: bool,
        user_login: Option<String>,
        branch_sha: Option<String>,
        file_text: Option<String>,
        file_requests: Vec<(String, String, String)>,
        list_queries: Vec<HashMap<String, String>>,
        base: String,
        oauth_bodies: Vec<Value>,
        oauth_reply: Option<Value>,
        user_auth: Vec<String>,
        user_id: i64,
        push_allowed: bool,
        repo_auth: Vec<String>,
        pull_auth: Vec<String>,
        grant_requests: Vec<(String, Value)>,
        grant_status: Option<u16>,
        patched: Vec<(String, String, Value)>,
        patch_status: Option<u16>,
    }

    type Shared = Arc<StdMutex<Recorded>>;

    async fn issue_token(
        State(shared): State<Shared>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        let mut recorded = shared.lock().unwrap();
        recorded.token_bodies.push(body);
        recorded.token_headers.push(headers);
        let n = recorded.token_bodies.len();
        let expires_at = recorded
            .expires_at
            .clone()
            .unwrap_or_else(|| (Utc::now() + Duration::hours(1)).to_rfc3339());
        Json(json!({"token": format!("ghs_secret{n}"), "expires_at": expires_at}))
    }

    async fn repositories(
        State(shared): State<Shared>,
        Query(query): Query<HashMap<String, String>>,
        headers: HeaderMap,
    ) -> impl IntoResponse {
        assert_eq!(headers["authorization"], "Bearer ghs_secret1");
        assert_eq!(query["per_page"], "100");
        let base = shared.lock().unwrap().base.clone();
        match query.get("page").map(String::as_str) {
            None => (
                [(
                    "link",
                    format!("<{base}/installation/repositories?per_page=100&page=2>; rel=\"next\""),
                )],
                Json(
                    json!({"repositories": [
                        {"full_name": "acme/api", "private": true, "html_url": "https://github.com/acme/api"},
                        {"full_name": "acme/web"}
                    ]}),
                ),
            )
                .into_response(),
            _ => Json(json!({"repositories": [{"full_name": "acme/docs"}]})).into_response(),
        }
    }

    async fn pulls(
        State(shared): State<Shared>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> impl IntoResponse {
        let mut recorded = shared.lock().unwrap();
        if recorded.pull_exists {
            return (
                AxumStatus::UNPROCESSABLE_ENTITY,
                Json(json!({
                    "message": "Validation Failed",
                    "errors": [{"resource": "PullRequest", "code": "custom", "message": "A pull request already exists for acme:matos/run-1."}],
                })),
            )
                .into_response();
        }
        if let Some(status) = recorded.fail_pulls_with {
            return (
                AxumStatus::from_u16(status).unwrap(),
                Json(json!({"message": "Validation Failed"})),
            )
                .into_response();
        }
        recorded
            .pull_auth
            .push(headers["authorization"].to_str().unwrap().to_owned());
        recorded.pull_bodies.push(body);
        Json(json!({"html_url": "https://github.com/acme/api/pull/9"})).into_response()
    }

    async fn patch_pull(
        State(shared): State<Shared>,
        axum::extract::Path((owner, repo, number)): axum::extract::Path<(String, String, String)>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> impl IntoResponse {
        let mut recorded = shared.lock().unwrap();
        if let Some(status) = recorded.patch_status {
            return (
                AxumStatus::from_u16(status).unwrap(),
                Json(json!({"message": "Forbidden"})),
            )
                .into_response();
        }
        recorded.patched.push((
            format!("{owner}/{repo}#{number}"),
            headers["authorization"].to_str().unwrap().to_owned(),
            body,
        ));
        Json(json!({"html_url": format!("https://github.com/{owner}/{repo}/pull/{number}")}))
            .into_response()
    }

    async fn list_pulls(
        State(shared): State<Shared>,
        Query(query): Query<HashMap<String, String>>,
    ) -> Json<Value> {
        let mut recorded = shared.lock().unwrap();
        recorded.list_queries.push(query);
        Json(Value::Array(recorded.listed_pulls.clone()))
    }

    async fn repo(State(shared): State<Shared>, headers: HeaderMap) -> impl IntoResponse {
        let mut recorded = shared.lock().unwrap();
        recorded
            .repo_auth
            .push(headers["authorization"].to_str().unwrap().to_owned());
        if recorded
            .repo_auth
            .last()
            .is_some_and(|auth| auth == "Bearer hidden")
        {
            return (AxumStatus::NOT_FOUND, Json(json!({"message": "Not Found"}))).into_response();
        }
        Json(json!({
            "default_branch": "trunk",
            "permissions": {"push": recorded.push_allowed},
        }))
        .into_response()
    }

    async fn revoke_grant(
        State(shared): State<Shared>,
        axum::extract::Path(client_id): axum::extract::Path<String>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> impl IntoResponse {
        let mut recorded = shared.lock().unwrap();
        recorded.grant_requests.push((
            format!("{client_id} {}", headers["authorization"].to_str().unwrap()),
            body,
        ));
        AxumStatus::from_u16(recorded.grant_status.unwrap_or(204)).unwrap()
    }

    async fn installation(
        State(shared): State<Shared>,
        axum::extract::Path(id): axum::extract::Path<i64>,
        headers: HeaderMap,
    ) -> impl IntoResponse {
        shared.lock().unwrap().token_headers.push(headers);
        if id == 404 {
            return (AxumStatus::NOT_FOUND, Json(json!({"message": "Not Found"}))).into_response();
        }
        let kind = shared
            .lock()
            .unwrap()
            .account_kind
            .clone()
            .unwrap_or_else(|| "Organization".to_owned());
        Json(json!({"id": id, "account": {"login": "acme", "type": kind}})).into_response()
    }

    async fn membership(State(shared): State<Shared>) -> impl IntoResponse {
        if shared.lock().unwrap().membership_forbidden {
            return (
                AxumStatus::FORBIDDEN,
                Json(json!({"message": "Resource not accessible by integration"})),
            )
                .into_response();
        }
        match shared.lock().unwrap().membership.clone() {
            Some(body) => Json(body).into_response(),
            None => (AxumStatus::NOT_FOUND, Json(json!({"message": "Not Found"}))).into_response(),
        }
    }

    async fn authenticated_user(State(shared): State<Shared>) -> Json<Value> {
        let login = shared
            .lock()
            .unwrap()
            .user_login
            .clone()
            .unwrap_or_default();
        let id = shared.lock().unwrap().user_id;
        Json(json!({"id": id, "login": login}))
    }

    async fn git_ref(
        State(shared): State<Shared>,
        axum::extract::Path((_owner, _repo, branch)): axum::extract::Path<(String, String, String)>,
    ) -> impl IntoResponse {
        assert_eq!(branch, "matos/run-1");
        match shared.lock().unwrap().branch_sha.clone() {
            Some(sha) => {
                Json(json!({"ref": format!("refs/heads/{branch}"), "object": {"sha": sha}}))
                    .into_response()
            }
            None => (AxumStatus::NOT_FOUND, Json(json!({"message": "Not Found"}))).into_response(),
        }
    }

    async fn contents(
        State(shared): State<Shared>,
        headers: HeaderMap,
        axum::extract::Path((_owner, _repo, path)): axum::extract::Path<(String, String, String)>,
        Query(query): Query<HashMap<String, String>>,
    ) -> impl IntoResponse {
        let mut recorded = shared.lock().unwrap();
        let accept = headers["accept"].to_str().unwrap().to_owned();
        recorded
            .file_requests
            .push((path, query["ref"].clone(), accept));
        match recorded.file_text.clone() {
            Some(text) => text.into_response(),
            None => (AxumStatus::NOT_FOUND, Json(json!({"message": "Not Found"}))).into_response(),
        }
    }

    async fn oauth_token(
        State(shared): State<Shared>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        assert_eq!(headers["accept"], "application/json");
        let mut recorded = shared.lock().unwrap();
        recorded.oauth_bodies.push(body);
        Json(
            recorded
                .oauth_reply
                .clone()
                .unwrap_or_else(|| json!({"access_token": "ghu_secret", "token_type": "bearer"})),
        )
    }

    async fn user_installations(
        State(shared): State<Shared>,
        Query(query): Query<HashMap<String, String>>,
        headers: HeaderMap,
    ) -> impl IntoResponse {
        let base = {
            let mut recorded = shared.lock().unwrap();
            recorded
                .user_auth
                .push(headers["authorization"].to_str().unwrap().to_owned());
            recorded.base.clone()
        };
        match query.get("page").map(String::as_str) {
            None => (
                [(
                    "link",
                    format!("<{base}/user/installations?per_page=100&page=2>; rel=\"next\""),
                )],
                Json(json!({"total_count": 3, "installations": [{"id": 1}, {"id": 2}]})),
            )
                .into_response(),
            _ => Json(json!({"total_count": 3, "installations": [{"id": 3}]})).into_response(),
        }
    }

    async fn serve(shared: Shared) -> GithubClient {
        let app = Router::new()
            .route("/login/oauth/access_token", post(oauth_token))
            .route("/user/installations", get(user_installations))
            .route("/app/installations/{id}", get(installation))
            .route("/user/memberships/orgs/{org}", get(membership))
            .route("/user", get(authenticated_user))
            .route(
                "/repos/{owner}/{repo}/git/ref/heads/{*branch}",
                get(git_ref),
            )
            .route("/repos/{owner}/{repo}/contents/{*path}", get(contents))
            .route("/app/installations/{id}/access_tokens", post(issue_token))
            .route("/installation/repositories", get(repositories))
            .route("/repos/{owner}/{repo}", get(repo))
            .route(
                "/applications/{client_id}/grant",
                axum::routing::delete(revoke_grant),
            )
            .route("/repos/{owner}/{repo}/pulls", post(pulls).get(list_pulls))
            .route(
                "/repos/{owner}/{repo}/pulls/{number}",
                axum::routing::patch(patch_pull),
            )
            .with_state(Arc::clone(&shared));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        shared.lock().unwrap().base = base.clone();
        tokio::spawn(async move { axum::serve(listener, app).await });
        GithubClient::new(app_for(&base)).unwrap()
    }

    fn credentials() -> GithubAppCredentials {
        GithubAppCredentials {
            organization_id: matos_common::OrganizationId::new(),
            app_id: 1,
            slug: "matos-acme".into(),
            client_id: "Iv1.abc".into(),
            client_secret: SecretToken::new("oauth-secret"),
            private_key_pem: SecretToken::new(PRIVATE),
            webhook_secret: SecretToken::new("hook"),
            html_url: "https://github.com/apps/matos-acme".into(),
            owner_login: "acme".into(),
            owner_type: "Organization".into(),
            created_by: "admin".into(),
            created_at: Utc::now(),
        }
    }

    fn app_for(base: &str) -> GithubApp {
        GithubApp::new(
            &credentials(),
            &GithubBases {
                api_base: base.to_owned(),
                web_base: base.to_owned(),
            },
        )
        .unwrap()
    }

    fn repo_ref(value: &str) -> RepositoryRef {
        value.parse().unwrap()
    }

    #[test]
    fn a_refused_permission_is_named_and_other_errors_pass_through() {
        for status in [403, 422] {
            let refused = missing_issues_permission(GithubError::Status {
                status,
                message: "Resource not accessible by integration".into(),
            });
            assert!(matches!(refused, GithubError::MissingPermission("issues")));
        }
        let other = missing_issues_permission(GithubError::Status {
            status: 500,
            message: "boom".into(),
        });
        assert!(matches!(other, GithubError::Status { status: 500, .. }));
    }

    #[test]
    fn next_link_is_parsed() {
        let header = "<https://x/a?page=1>; rel=\"prev\", <https://x/a?page=3>; rel=\"next\", <https://x/a?page=9>; rel=\"last\"";
        assert_eq!(next_link(header), Some("https://x/a?page=3"));
        assert_eq!(next_link("<https://x/a?page=9>; rel=\"last\""), None);
        assert_eq!(next_link(""), None);
    }

    #[test]
    fn debug_hides_secrets() {
        let token = InstallationToken {
            token: SecretToken::new("ghs_leak"),
            expires_at: Utc::now(),
        };
        assert!(!format!("{token:?}").contains("ghs_leak"));
        let mut leaky = credentials();
        leaky.private_key_pem = SecretToken::new("pem-leak");
        leaky.client_secret = SecretToken::new("oauth-leak");
        let app = GithubApp::new(&leaky, &GithubBases::default()).unwrap();
        assert!(!format!("{app:?}").contains("pem-leak"));
        assert!(!format!("{app:?}").contains("oauth-leak"));
        let user = UserToken {
            token: SecretToken::new("ghu_leak"),
            expires_at: None,
            refresh_token: Some(SecretToken::new("ghr_leak")),
            refresh_expires_at: None,
        };
        let shown = format!("{user:?}");
        assert!(
            !shown.contains("ghu_leak") && !shown.contains("ghr_leak"),
            "{shown}"
        );
    }

    #[tokio::test]
    async fn the_oauth_code_is_exchanged_for_a_user_token() {
        let shared = Shared::default();
        let client = serve(Arc::clone(&shared)).await;
        let token = client.exchange_user_code("the-code").await.unwrap();
        assert_eq!(token.access_token().expose(), "ghu_secret");
        assert_eq!(
            shared.lock().unwrap().oauth_bodies[0],
            json!({"client_id": "Iv1.abc", "client_secret": "oauth-secret", "code": "the-code"})
        );
    }

    #[tokio::test]
    async fn a_rejected_oauth_code_is_a_permanent_error() {
        let shared = Shared::default();
        shared.lock().unwrap().oauth_reply = Some(json!({"error": "bad_verification_code"}));
        let client = serve(Arc::clone(&shared)).await;
        let error = client.exchange_user_code("stale").await.unwrap_err();
        assert!(matches!(error, GithubError::Status { status: 400, .. }));
        assert!(matches!(
            CoreError::from(error),
            CoreError::Invalid(message) if message.contains("bad_verification_code")
        ));
    }

    #[test]
    fn an_app_id_must_be_positive() {
        let mut broken = credentials();
        broken.app_id = -3;
        assert!(matches!(
            GithubApp::new(&broken, &GithubBases::default()),
            Err(GithubError::InvalidApp(_))
        ));
    }

    #[tokio::test]
    async fn user_installations_are_searched_across_pages_with_the_user_token() {
        let shared = Shared::default();
        let client = serve(Arc::clone(&shared)).await;
        let token = client.exchange_user_code("c").await.unwrap();
        assert!(
            client
                .user_can_access_installation(&token, 3)
                .await
                .unwrap()
        );
        assert!(
            client
                .user_can_access_installation(&token, 1)
                .await
                .unwrap()
        );
        assert!(
            !client
                .user_can_access_installation(&token, 9)
                .await
                .unwrap()
        );
        let recorded = shared.lock().unwrap();
        assert!(
            recorded
                .user_auth
                .iter()
                .all(|auth| auth == "Bearer ghu_secret")
        );
    }

    #[test]
    fn errors_map_to_core_errors() {
        let status = |status| {
            CoreError::from(GithubError::Status {
                status,
                message: "m".into(),
            })
        };
        for transient in [429, 500, 502] {
            assert!(matches!(status(transient), CoreError::Internal(_)));
        }
        for permanent in [401, 403, 404, 422] {
            assert!(matches!(status(permanent), CoreError::Invalid(_)));
        }
        assert!(matches!(
            CoreError::from(GithubError::Transport("down".into())),
            CoreError::Internal(_)
        ));
    }

    #[tokio::test]
    async fn token_request_is_scoped_and_cached() {
        let shared = Shared::default();
        let client = serve(Arc::clone(&shared)).await;
        let api = repo_ref("acme/api");

        let first = client.installation_token(5, &api).await.unwrap();
        let second = client.installation_token(5, &api).await.unwrap();
        assert_eq!(first.token.expose(), "ghs_secret1");
        assert_eq!(second.token.expose(), "ghs_secret1");
        client
            .installation_token(5, &repo_ref("acme/web"))
            .await
            .unwrap();

        let recorded = shared.lock().unwrap();
        assert_eq!(recorded.token_bodies.len(), 2);
        assert_eq!(
            recorded.token_bodies[0],
            json!({
                "repositories": ["api"],
                "permissions": {"contents": "write", "pull_requests": "write"},
            })
        );
        let headers = &recorded.token_headers[0];
        assert!(
            headers["authorization"]
                .to_str()
                .unwrap()
                .starts_with("Bearer ey")
        );
        assert_eq!(headers["accept"], "application/vnd.github+json");
        assert_eq!(headers["x-github-api-version"], "2022-11-28");
        assert!(headers.contains_key("user-agent"));
    }

    #[tokio::test]
    async fn a_read_only_token_asks_only_to_read_and_is_cached_apart() {
        let shared = Shared::default();
        let client = serve(Arc::clone(&shared)).await;
        let api = repo_ref("acme/api");

        let read = client.read_only_installation_token(5, &api).await.unwrap();
        let again = client.read_only_installation_token(5, &api).await.unwrap();
        let write = client.installation_token(5, &api).await.unwrap();
        assert_eq!(read.token.expose(), again.token.expose());
        assert_ne!(read.token.expose(), write.token.expose());

        let recorded = shared.lock().unwrap();
        assert_eq!(recorded.token_bodies.len(), 2);
        assert_eq!(
            recorded.token_bodies[0],
            json!({
                "repositories": ["api"],
                "permissions": {"contents": "read", "metadata": "read"},
            })
        );
        assert_eq!(
            recorded.token_bodies[1]["permissions"],
            json!({"contents": "write", "pull_requests": "write"})
        );
    }

    #[tokio::test]
    async fn token_close_to_expiry_is_renewed() {
        let shared = Shared::default();
        shared.lock().unwrap().expires_at = Some((Utc::now() + Duration::minutes(4)).to_rfc3339());
        let client = serve(Arc::clone(&shared)).await;
        let api = repo_ref("acme/api");
        client.installation_token(5, &api).await.unwrap();
        client.installation_token(5, &api).await.unwrap();
        assert_eq!(shared.lock().unwrap().token_bodies.len(), 2);
    }

    #[tokio::test]
    async fn repositories_follow_pagination_with_an_unscoped_token() {
        let shared = Shared::default();
        let client = serve(Arc::clone(&shared)).await;
        let repositories = client.list_repositories(5).await.unwrap();
        let names: Vec<_> = repositories
            .iter()
            .map(|r| format!("{}/{}", r.owner(), r.name()))
            .collect();
        assert_eq!(names, ["acme/api", "acme/web", "acme/docs"]);
        let recorded = shared.lock().unwrap();
        assert!(recorded.token_bodies[0].get("repositories").is_none());
    }

    #[tokio::test]
    async fn repository_listing_carries_visibility_and_link() {
        let shared = Shared::default();
        let client = serve(Arc::clone(&shared)).await;
        let listing = client.list_repository_listing(5).await.unwrap();
        assert_eq!(listing.len(), 3);
        assert!(listing[0].private);
        assert_eq!(listing[0].html_url, "https://github.com/acme/api");
        assert!(!listing[1].private);
        assert!(listing[1].html_url.ends_with("/acme/web"));
    }

    #[tokio::test]
    async fn installation_account_is_read_with_the_app_jwt() {
        let shared = Shared::default();
        let client = serve(Arc::clone(&shared)).await;
        let account = client.installation_account(5).await.unwrap();
        assert_eq!(account.login, "acme");
        assert_eq!(account.kind, AccountKind::Organization);
        {
            let recorded = shared.lock().unwrap();
            assert!(
                recorded.token_headers[0]["authorization"]
                    .to_str()
                    .unwrap()
                    .starts_with("Bearer ey")
            );
        }
        let error = client.installation_account(404).await.unwrap_err();
        assert!(matches!(error, GithubError::Status { status: 404, .. }));
    }

    async fn owns(shared: &Shared, kind: &str) -> bool {
        shared.lock().unwrap().account_kind = Some(kind.to_owned());
        let client = serve(Arc::clone(shared)).await;
        let token = client.exchange_user_code("c").await.unwrap();
        let account = client.installation_account(5).await.unwrap();
        client
            .user_owns_installation(&token, &account)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn only_an_active_org_admin_owns_an_organization_installation() {
        let admin = Shared::default();
        admin.lock().unwrap().membership = Some(json!({"role": "admin", "state": "active"}));
        assert!(owns(&admin, "Organization").await);

        let member = Shared::default();
        member.lock().unwrap().membership = Some(json!({"role": "member", "state": "active"}));
        assert!(!owns(&member, "Organization").await);

        let pending = Shared::default();
        pending.lock().unwrap().membership = Some(json!({"role": "admin", "state": "pending"}));
        assert!(!owns(&pending, "Organization").await);

        let outsider = Shared::default();
        assert!(!owns(&outsider, "Organization").await);
    }

    #[tokio::test]
    async fn an_app_without_the_members_permission_is_told_so_not_refused() {
        let shared = Shared::default();
        shared.lock().unwrap().membership_forbidden = true;
        shared.lock().unwrap().account_kind = Some("Organization".into());
        let client = serve(Arc::clone(&shared)).await;
        let token = client.exchange_user_code("c").await.unwrap();
        let account = client.installation_account(5).await.unwrap();

        let error = client
            .user_owns_installation(&token, &account)
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            GithubError::MissingPermission("members: read")
        ));
    }

    #[tokio::test]
    async fn a_user_installation_is_owned_by_that_user_only() {
        let same = Shared::default();
        same.lock().unwrap().user_login = Some("ACME".into());
        assert!(owns(&same, "User").await);

        let other = Shared::default();
        other.lock().unwrap().user_login = Some("mallory".into());
        assert!(!owns(&other, "User").await);
    }

    #[tokio::test]
    async fn any_other_account_kind_is_not_owned() {
        let shared = Shared::default();
        shared.lock().unwrap().user_login = Some("acme".into());
        assert!(!owns(&shared, "Bot").await);
    }

    #[tokio::test]
    async fn the_head_of_a_branch_is_read_and_a_missing_branch_is_none() {
        let shared = Shared::default();
        shared.lock().unwrap().branch_sha = Some("abc123".into());
        let client = serve(Arc::clone(&shared)).await;
        let api = repo_ref("acme/api");

        assert_eq!(
            client.branch_sha(5, &api, "matos/run-1").await.unwrap(),
            Some("abc123".to_owned())
        );
        shared.lock().unwrap().branch_sha = None;
        assert_eq!(
            client.branch_sha(5, &api, "matos/run-1").await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn a_file_is_read_raw_at_a_ref_and_a_missing_file_is_none() {
        let shared = Shared::default();
        shared.lock().unwrap().file_text = Some("[guard]\nmax_files = 80\n".into());
        let client = serve(Arc::clone(&shared)).await;
        let api = repo_ref("acme/api");

        assert_eq!(
            client
                .file_text(5, &api, "release/1 x", ".matos.toml")
                .await
                .unwrap(),
            Some("[guard]\nmax_files = 80\n".to_owned())
        );
        assert_eq!(
            shared.lock().unwrap().file_requests,
            vec![(
                ".matos.toml".to_owned(),
                "release/1 x".to_owned(),
                "application/vnd.github.raw+json".to_owned()
            )]
        );
        shared.lock().unwrap().file_text = None;
        assert_eq!(
            client
                .file_text(5, &api, "main", ".matos.toml")
                .await
                .unwrap(),
            None
        );
    }

    #[test]
    fn a_path_is_encoded_segment_by_segment() {
        assert_eq!(encode_path("a b/c#d/é.toml"), "a%20b/c%23d/%C3%A9.toml");
        assert_eq!(encode_segment("feat/x"), "feat%2Fx");
    }

    #[test]
    fn a_link_must_share_scheme_host_and_port_with_the_api() {
        let base = "https://api.github.com";
        assert!(same_origin(
            "https://api.github.com/user/installations?page=2",
            base
        ));
        assert!(same_origin("https://API.github.com:443/x", base));
        assert!(!same_origin("https://api.github.com.evil.example/x", base));
        assert!(!same_origin("https://api.github.com@evil.example/x", base));
        assert!(!same_origin("http://api.github.com/x", base));
        assert!(!same_origin("https://api.github.com:8443/x", base));
        assert!(!same_origin("not a url", base));
        assert!(same_origin(
            "http://127.0.0.1:4000/a",
            "http://127.0.0.1:4000"
        ));
    }

    #[tokio::test]
    async fn default_branch_is_read() {
        let client = serve(Shared::default()).await;
        assert_eq!(
            client
                .default_branch(5, &repo_ref("acme/api"))
                .await
                .unwrap(),
            "trunk"
        );
    }

    fn draft() -> DraftPullRequest {
        DraftPullRequest {
            repository: repo_ref("acme/api"),
            base: "main".into(),
            head: "matos/run-1".into(),
            title: "Fix it".into(),
            body: "details".into(),
        }
    }

    #[tokio::test]
    async fn draft_pull_request_is_created() {
        let shared = Shared::default();
        let client = serve(Arc::clone(&shared)).await;
        let url = client.create_draft_pr(5, &draft()).await.unwrap();
        assert_eq!(
            url,
            PullRequestUrl("https://github.com/acme/api/pull/9".into())
        );
        assert_eq!(
            shared.lock().unwrap().pull_bodies[0],
            json!({
                "title": "Fix it",
                "head": "matos/run-1",
                "base": "main",
                "body": "details",
                "draft": true,
            })
        );
    }

    #[tokio::test]
    async fn an_existing_pull_request_for_the_branch_is_reused() {
        let shared = Shared::default();
        {
            let mut recorded = shared.lock().unwrap();
            recorded.pull_exists = true;
            recorded.listed_pulls = vec![json!({"html_url": "https://github.com/acme/api/pull/4"})];
        }
        let client = serve(Arc::clone(&shared)).await;

        let url = client.create_draft_pr(5, &draft()).await.unwrap();

        assert_eq!(
            url,
            PullRequestUrl("https://github.com/acme/api/pull/4".into())
        );
        let recorded = shared.lock().unwrap();
        assert_eq!(recorded.list_queries.len(), 1);
        assert_eq!(recorded.list_queries[0]["head"], "acme:matos/run-1");
        assert_eq!(recorded.list_queries[0]["state"], "open");
    }

    #[tokio::test]
    async fn a_duplicate_error_without_an_open_pull_request_stays_an_error() {
        let shared = Shared::default();
        shared.lock().unwrap().pull_exists = true;
        let client = serve(Arc::clone(&shared)).await;

        let error = CoreError::from(client.create_draft_pr(5, &draft()).await.unwrap_err());

        assert!(matches!(error, CoreError::Invalid(_)));
    }

    #[tokio::test]
    async fn github_failures_are_mapped_without_leaking_tokens() {
        let shared = Shared::default();
        let client = serve(Arc::clone(&shared)).await;
        for (status, transient) in [
            (422, false),
            (404, false),
            (403, false),
            (503, true),
            (429, true),
        ] {
            shared.lock().unwrap().fail_pulls_with = Some(status);
            let error = CoreError::from(client.create_draft_pr(5, &draft()).await.unwrap_err());
            let text = error.to_string();
            assert!(!text.contains("ghs_secret"));
            if transient {
                assert!(matches!(error, CoreError::Internal(_)), "{status}");
            } else {
                assert!(text.contains("Validation Failed"));
                assert!(matches!(error, CoreError::Invalid(_)), "{status}");
            }
        }
    }

    #[tokio::test]
    async fn unreachable_host_is_transient() {
        let client = GithubClient::new(app_for("http://127.0.0.1:1")).unwrap();
        let error = CoreError::from(client.list_repositories(5).await.unwrap_err());
        assert!(matches!(error, CoreError::Internal(_)));
    }

    #[tokio::test]
    async fn the_expiries_of_a_user_token_are_kept_as_absolute_times() {
        let shared = Shared::default();
        shared.lock().unwrap().oauth_reply = Some(json!({
            "access_token": "ghu_new",
            "expires_in": 28800,
            "refresh_token": "ghr_new",
            "refresh_token_expires_in": 15811200,
        }));
        let client = serve(Arc::clone(&shared)).await;
        let before = Utc::now();
        let tokens = client.exchange_user_code("c").await.unwrap().into_tokens();
        assert_eq!(tokens.access_token.expose(), "ghu_new");
        assert_eq!(tokens.refresh_token.unwrap().expose(), "ghr_new");
        let access_ttl = tokens.expires_at.unwrap() - before;
        assert!((Duration::seconds(28790)..=Duration::seconds(28810)).contains(&access_ttl));
        let refresh_ttl = tokens.refresh_expires_at.unwrap() - before;
        assert!(refresh_ttl > Duration::seconds(15811000));
    }

    #[tokio::test]
    async fn a_token_without_expiry_has_none() {
        let client = serve(Shared::default()).await;
        let tokens = client.exchange_user_code("c").await.unwrap().into_tokens();
        assert!(tokens.expires_at.is_none());
        assert!(tokens.refresh_token.is_none());
        assert!(tokens.refresh_expires_at.is_none());
    }

    #[tokio::test]
    async fn a_refresh_token_is_traded_for_a_new_pair() {
        let shared = Shared::default();
        shared.lock().unwrap().oauth_reply = Some(json!({
            "access_token": "ghu_next",
            "expires_in": 3600,
            "refresh_token": "ghr_next",
            "refresh_token_expires_in": 7200,
        }));
        let client = serve(Arc::clone(&shared)).await;
        let tokens = client
            .refresh_user_token("ghr_old")
            .await
            .unwrap()
            .into_tokens();
        assert_eq!(tokens.access_token.expose(), "ghu_next");
        assert_eq!(tokens.refresh_token.unwrap().expose(), "ghr_next");
        assert_eq!(
            shared.lock().unwrap().oauth_bodies[0],
            json!({
                "client_id": "Iv1.abc",
                "client_secret": "oauth-secret",
                "grant_type": "refresh_token",
                "refresh_token": "ghr_old",
            })
        );
    }

    #[tokio::test]
    async fn a_refused_refresh_is_a_permanent_error() {
        let shared = Shared::default();
        shared.lock().unwrap().oauth_reply = Some(json!({"error": "bad_refresh_token"}));
        let client = serve(Arc::clone(&shared)).await;
        let error = client.refresh_user_token("ghr_old").await.unwrap_err();
        assert!(matches!(
            CoreError::from(error),
            CoreError::Invalid(message) if message.contains("bad_refresh_token")
        ));
    }

    #[tokio::test]
    async fn the_authorize_url_carries_the_client_and_the_encoded_state() {
        let client = serve(Shared::default()).await;
        let base = client.app.web_base.clone();
        assert_eq!(
            client.authorize_url("a.b c"),
            format!("{base}/login/oauth/authorize?client_id=Iv1.abc&state=a.b%20c")
        );
    }

    #[tokio::test]
    async fn the_authenticated_user_is_read_with_their_token() {
        let shared = Shared::default();
        {
            let mut recorded = shared.lock().unwrap();
            recorded.user_id = 583231;
            recorded.user_login = Some("octocat".into());
        }
        let client = serve(Arc::clone(&shared)).await;
        let user = client.authenticated_user("ghu_secret").await.unwrap();
        assert_eq!(
            user,
            GithubUser {
                id: 583231,
                login: "octocat".into()
            }
        );
    }

    #[tokio::test]
    async fn push_access_follows_the_permissions_of_the_user_token() {
        let shared = Shared::default();
        let client = serve(Arc::clone(&shared)).await;
        let api = repo_ref("acme/api");

        shared.lock().unwrap().push_allowed = true;
        assert!(client.user_can_push("ghu_secret", &api).await.unwrap());
        shared.lock().unwrap().push_allowed = false;
        assert!(!client.user_can_push("ghu_secret", &api).await.unwrap());
        assert!(!client.user_can_push("hidden", &api).await.unwrap());
        assert_eq!(shared.lock().unwrap().repo_auth[0], "Bearer ghu_secret");
    }

    #[tokio::test]
    async fn a_draft_pull_request_can_be_opened_with_a_user_token() {
        let shared = Shared::default();
        let client = serve(Arc::clone(&shared)).await;
        let url = client
            .create_draft_pr_as_user("ghu_member", &draft())
            .await
            .unwrap();
        assert_eq!(
            url,
            PullRequestUrl("https://github.com/acme/api/pull/9".into())
        );
        let recorded = shared.lock().unwrap();
        assert_eq!(recorded.pull_auth, ["Bearer ghu_member"]);
        assert!(recorded.token_bodies.is_empty());
        assert_eq!(recorded.pull_bodies[0]["draft"], true);
    }

    #[tokio::test]
    async fn a_pull_request_body_is_replaced_by_its_number_with_an_installation_token() {
        let shared = Shared::default();
        let client = serve(Arc::clone(&shared)).await;
        client
            .update_pull_request_body(
                5,
                &repo_ref("acme/sdk"),
                &PullRequestUrl("https://github.com/acme/sdk/pull/12".into()),
                "new body",
            )
            .await
            .unwrap();
        let recorded = shared.lock().unwrap();
        assert_eq!(
            recorded.patched,
            [(
                "acme/sdk#12".to_owned(),
                "Bearer ghs_secret1".to_owned(),
                json!({"body": "new body"})
            )]
        );
        assert_eq!(recorded.token_bodies[0]["repositories"], json!(["sdk"]));
    }

    #[tokio::test]
    async fn a_pull_request_url_without_a_number_is_refused_before_any_call() {
        let shared = Shared::default();
        let client = serve(Arc::clone(&shared)).await;
        let error = client
            .update_pull_request_body(
                5,
                &repo_ref("acme/sdk"),
                &PullRequestUrl("https://github.com/acme/sdk/pulls".into()),
                "x",
            )
            .await
            .unwrap_err();
        assert!(matches!(error, GithubError::Status { status: 422, .. }));
        let recorded = shared.lock().unwrap();
        assert!(recorded.token_bodies.is_empty() && recorded.patched.is_empty());
    }

    #[tokio::test]
    async fn a_refused_body_update_is_an_error() {
        let shared = Shared::default();
        shared.lock().unwrap().patch_status = Some(403);
        let client = serve(Arc::clone(&shared)).await;
        let error = client
            .update_pull_request_body(
                5,
                &repo_ref("acme/sdk"),
                &PullRequestUrl("https://github.com/acme/sdk/pull/12".into()),
                "x",
            )
            .await
            .unwrap_err();
        assert!(matches!(error, GithubError::Status { status: 403, .. }));
    }

    #[tokio::test]
    async fn a_refused_user_pull_request_is_an_error_naming_github_not_the_token() {
        let shared = Shared::default();
        shared.lock().unwrap().fail_pulls_with = Some(403);
        let client = serve(Arc::clone(&shared)).await;
        let error = client
            .create_draft_pr_as_user("ghu_member", &draft())
            .await
            .unwrap_err();
        assert!(matches!(error, GithubError::Status { status: 403, .. }));
        assert!(!error.to_string().contains("ghu_member"));
    }

    #[tokio::test]
    async fn the_grant_is_revoked_with_the_app_credentials_and_a_missing_grant_is_fine() {
        let shared = Shared::default();
        let client = serve(Arc::clone(&shared)).await;
        client.revoke_user_grant("ghu_secret").await.unwrap();
        {
            let recorded = shared.lock().unwrap();
            let (who, body) = &recorded.grant_requests[0];
            assert_eq!(who, "Iv1.abc Basic SXYxLmFiYzpvYXV0aC1zZWNyZXQ=");
            assert_eq!(body, &json!({"access_token": "ghu_secret"}));
        }
        shared.lock().unwrap().grant_status = Some(404);
        client.revoke_user_grant("ghu_secret").await.unwrap();
        shared.lock().unwrap().grant_status = Some(500);
        assert!(client.revoke_user_grant("ghu_secret").await.is_err());
    }
}
