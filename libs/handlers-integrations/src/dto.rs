use chrono::{DateTime, Utc};
use matos_core::domain::{
    execution::{GithubAppSummary, GithubInstallation, RepositoryListing},
    github_user::{GithubConnectionStatus, PersonalTokenStatus},
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Serialize, ToSchema)]
pub struct GithubAppResponse {
    pub slug: String,
    pub owner_login: String,
    /// `User` or `Organization`, as GitHub names the account the App belongs to.
    pub owner_type: String,
    /// The App's page on GitHub: where to delete it for good.
    pub html_url: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
}

impl From<GithubAppSummary> for GithubAppResponse {
    fn from(app: GithubAppSummary) -> Self {
        Self {
            slug: app.slug,
            owner_login: app.owner_login,
            owner_type: app.owner_type,
            html_url: app.html_url,
            created_by: app.created_by,
            created_at: app.created_at,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct GithubInstallationResponse {
    pub account_login: String,
    pub installation_id: i64,
    pub connected_by: String,
    pub connected_at: DateTime<Utc>,
}

impl From<GithubInstallation> for GithubInstallationResponse {
    fn from(installation: GithubInstallation) -> Self {
        Self {
            account_login: installation.account_login,
            installation_id: installation.installation_id,
            connected_by: installation.created_by,
            connected_at: installation.created_at,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct GithubIntegrationResponse {
    /// The GitHub App the organization created; `null` until it does.
    pub app: Option<GithubAppResponse>,
    /// Where that App is installed; `null` until an admin installs it.
    pub installation: Option<GithubInstallationResponse>,
}

impl GithubIntegrationResponse {
    pub fn new(app: Option<GithubAppSummary>, installation: Option<GithubInstallation>) -> Self {
        Self {
            app: app.map(Into::into),
            installation: installation.map(Into::into),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct InstallUrlResponse {
    pub url: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ConnectGithubRequest {
    pub installation_id: i64,
    /// The OAuth `code` GitHub returns with the redirect; proves the admin can access the installation.
    pub code: String,
    /// The `state` of the install URL, returned untouched by GitHub.
    pub state: String,
}

/// Who will own the App on GitHub.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ManifestOwner {
    User,
    Organization { login: String },
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateManifestRequest {
    pub owner: ManifestOwner,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ManifestResponse {
    /// Where the browser POSTs the `manifest` form field.
    pub action_url: String,
    /// The App manifest, a JSON document sent as the `manifest` form field.
    pub manifest: String,
    /// Signed, bound to the caller and the organization, valid 10 minutes.
    pub state: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateGithubAppRequest {
    /// The temporary `code` GitHub appends to the redirect URL.
    pub code: String,
    /// The `state` of the manifest, returned untouched by GitHub.
    pub state: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RepositoryResponse {
    pub owner: String,
    pub name: String,
    pub full_name: String,
    pub private: bool,
    pub html_url: String,
}

impl From<&RepositoryListing> for RepositoryResponse {
    fn from(listing: &RepositoryListing) -> Self {
        let repository = &listing.repository;
        Self {
            owner: repository.owner().to_owned(),
            name: repository.name().to_owned(),
            full_name: repository.to_string(),
            private: listing.private,
            html_url: listing.html_url.clone(),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RepositoryListResponse {
    pub repositories: Vec<RepositoryResponse>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct GithubConnectionResponse {
    pub connected: bool,
    pub login: Option<String>,
    pub connected_at: Option<DateTime<Utc>>,
    /// The caller's own personal access token; `null` when none is stored.
    pub personal_token: Option<GithubPersonalTokenResponse>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct GithubPersonalTokenResponse {
    /// The GitHub account the token belongs to.
    pub login: String,
    /// The last characters of the token, for recognition; the token itself is never returned.
    pub preview: String,
    /// When GitHub expires the token; `null` when it does not.
    pub expires_at: Option<DateTime<Utc>>,
    /// Whether runs use the token; an inactive token is kept but ignored.
    pub active: bool,
}

impl From<PersonalTokenStatus> for GithubPersonalTokenResponse {
    fn from(token: PersonalTokenStatus) -> Self {
        Self {
            login: token.login,
            preview: token.preview,
            expires_at: token.expires_at,
            active: token.active,
        }
    }
}

impl From<GithubConnectionStatus> for GithubConnectionResponse {
    fn from(status: GithubConnectionStatus) -> Self {
        Self {
            connected: status.connected,
            login: status.login,
            connected_at: status.connected_at,
            personal_token: status.personal_token.map(Into::into),
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ConnectGithubUserRequest {
    /// The OAuth `code` GitHub returns with the redirect.
    pub code: String,
    /// The `state` of the authorize URL, returned untouched by GitHub.
    pub state: String,
}

#[derive(Deserialize, ToSchema)]
pub struct PutGithubPersonalTokenRequest {
    /// A fine-grained personal access token; GitHub validates it before it is stored.
    pub token: String,
}

impl std::fmt::Debug for PutGithubPersonalTokenRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PutGithubPersonalTokenRequest")
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetGithubPersonalTokenActiveRequest {
    pub active: bool,
}
