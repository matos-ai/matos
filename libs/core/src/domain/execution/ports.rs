use chrono::{DateTime, Utc};
use matos_common::{CoreError, OrganizationId, RunId};
use tokio::sync::mpsc;

use super::{
    DraftPullRequest, GithubAppCredentials, GithubAppSummary, GithubInstallation, PullRequestUrl,
    RepositoryIssue, RepositoryListing, RepositoryRef, TaskGrant, TaskLine, TaskOutcome, TaskSpec,
};
use crate::domain::{agent::SecretToken, github_user::UserTokens};

/// Runs one execution in a disposable container. Implementations must not
/// leak their client types through this port.
#[cfg_attr(any(test, feature = "mock"), mockall::automock)]
pub trait TaskRunner: Send + Sync {
    /// Starts the task of a new run. Starting a task that already exists for
    /// this run is not an error.
    fn start(&self, spec: &TaskSpec) -> impl Future<Output = Result<(), CoreError>> + Send;

    /// Whether the task of this run still exists: a resumed run follows it
    /// instead of starting another.
    fn attach(&self, run_id: RunId) -> impl Future<Output = Result<bool, CoreError>> + Send;

    /// Sends every output line to `lines` until the task ends, then returns
    /// how it ended.
    fn follow(
        &self,
        run_id: RunId,
        lines: mpsc::Sender<TaskLine>,
    ) -> impl Future<Output = Result<TaskOutcome, CoreError>> + Send;

    fn delete(&self, run_id: RunId) -> impl Future<Output = Result<(), CoreError>> + Send;
}

const NO_DEFAULT_BRANCH: &str = "ce dépôt de code ne donne pas la branche par défaut";
const NO_PULL_REQUEST_UPDATE: &str = "ce dépôt de code ne met pas à jour les pull requests";
const NO_MEMBER_IDENTITY: &str = "ce dépôt de code ne gère pas l'identité des membres";

/// The code host the organization connected (GitHub).
#[cfg_attr(any(test, feature = "mock"), mockall::automock)]
pub trait CodeHost: Send + Sync {
    fn repositories(
        &self,
        organization_id: OrganizationId,
        installation_id: i64,
    ) -> impl Future<Output = Result<Vec<RepositoryRef>, CoreError>> + Send;

    fn repository_listing(
        &self,
        organization_id: OrganizationId,
        installation_id: i64,
    ) -> impl Future<Output = Result<Vec<RepositoryListing>, CoreError>> + Send;

    /// The open issues of a repository, pull requests excluded.
    fn open_issues(
        &self,
        organization_id: OrganizationId,
        installation_id: i64,
        repository: &RepositoryRef,
    ) -> impl Future<Output = Result<Vec<RepositoryIssue>, CoreError>> + Send;

    fn default_branch(
        &self,
        _organization_id: OrganizationId,
        _installation_id: i64,
        _repository: &RepositoryRef,
    ) -> impl Future<Output = Result<String, CoreError>> + Send {
        std::future::ready(Err(CoreError::Invalid(NO_DEFAULT_BRANCH.into())))
    }

    /// The commit `refs/heads/<branch>` points to, `None` if the branch does
    /// not exist.
    fn branch_head(
        &self,
        organization_id: OrganizationId,
        installation_id: i64,
        repository: &RepositoryRef,
        branch: &str,
    ) -> impl Future<Output = Result<Option<String>, CoreError>> + Send;

    /// The text of `path` at `git_ref`, `None` if the file does not exist.
    fn file_at(
        &self,
        organization_id: OrganizationId,
        installation_id: i64,
        repository: &RepositoryRef,
        git_ref: &str,
        path: &str,
    ) -> impl Future<Output = Result<Option<String>, CoreError>> + Send;

    fn open_draft_pr(
        &self,
        organization_id: OrganizationId,
        installation_id: i64,
        pull_request: DraftPullRequest,
    ) -> impl Future<Output = Result<PullRequestUrl, CoreError>> + Send;

    fn open_draft_pr_as_member(
        &self,
        _organization_id: OrganizationId,
        _token: &SecretToken,
        _pull_request: DraftPullRequest,
    ) -> impl Future<Output = Result<PullRequestUrl, CoreError>> + Send {
        std::future::ready(Err(CoreError::Invalid(NO_MEMBER_IDENTITY.into())))
    }

    /// Replaces the body of a pull request the run opened; `pull_request` is
    /// its URL.
    fn update_pull_request_body(
        &self,
        _organization_id: OrganizationId,
        _installation_id: i64,
        _repository: &RepositoryRef,
        _pull_request: &PullRequestUrl,
        _body: &str,
    ) -> impl Future<Output = Result<(), CoreError>> + Send {
        std::future::ready(Err(CoreError::Invalid(NO_PULL_REQUEST_UPDATE.into())))
    }

    fn member_can_push(
        &self,
        _organization_id: OrganizationId,
        _token: &SecretToken,
        _repository: &RepositoryRef,
    ) -> impl Future<Output = Result<bool, CoreError>> + Send {
        std::future::ready(Ok(false))
    }

    fn refresh_user_token(
        &self,
        _organization_id: OrganizationId,
        _refresh_token: &SecretToken,
    ) -> impl Future<Output = Result<UserTokens, CoreError>> + Send {
        std::future::ready(Err(CoreError::Invalid(NO_MEMBER_IDENTITY.into())))
    }
}

#[cfg_attr(any(test, feature = "mock"), mockall::automock)]
pub trait TaskGrantRepository: Send {
    /// Stores the grant under the SHA-256 of the token.
    fn insert(
        &mut self,
        token_sha256: [u8; 32],
        grant: &TaskGrant,
    ) -> impl Future<Output = Result<(), CoreError>> + Send;

    fn find(
        &mut self,
        token_sha256: [u8; 32],
    ) -> impl Future<Output = Result<Option<TaskGrant>, CoreError>> + Send;

    /// Revokes every grant of the run; revoking twice is not an error.
    fn revoke(
        &mut self,
        run_id: RunId,
        now: DateTime<Utc>,
    ) -> impl Future<Output = Result<(), CoreError>> + Send;
}

#[cfg_attr(any(test, feature = "mock"), mockall::automock)]
pub trait GithubInstallationRepository: Send {
    fn get(
        &mut self,
        organization_id: OrganizationId,
    ) -> impl Future<Output = Result<Option<GithubInstallation>, CoreError>> + Send;

    /// One installation per organization: saving replaces the previous one.
    fn save(
        &mut self,
        installation: &GithubInstallation,
    ) -> impl Future<Output = Result<(), CoreError>> + Send;

    fn delete(
        &mut self,
        organization_id: OrganizationId,
    ) -> impl Future<Output = Result<(), CoreError>> + Send;
}

#[cfg_attr(any(test, feature = "mock"), mockall::automock)]
pub trait GithubAppRepository: Send {
    fn get(
        &mut self,
        organization_id: OrganizationId,
    ) -> impl Future<Output = Result<Option<GithubAppCredentials>, CoreError>> + Send;

    fn summary(
        &mut self,
        organization_id: OrganizationId,
    ) -> impl Future<Output = Result<Option<GithubAppSummary>, CoreError>> + Send;

    fn insert(
        &mut self,
        app: &GithubAppCredentials,
    ) -> impl Future<Output = Result<(), CoreError>> + Send;

    fn delete(
        &mut self,
        organization_id: OrganizationId,
    ) -> impl Future<Output = Result<bool, CoreError>> + Send;
}
