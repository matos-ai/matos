use chrono::{Duration, Utc};
use matos_common::{CoreError, OrganizationId};
use matos_macros::transactional;

use super::MatosUseCase;
use crate::{
    domain::{
        agent::SecretToken,
        authorization::Subject,
        execution::ports::CodeHost,
        github_user::{
            GithubConnection, GithubConnectionStatus, ports::GithubConnectionRepository,
            ports::GithubUserOAuth,
        },
    },
    infrastructure::github_user::postgres::PgGithubConnectionRepository,
};

impl MatosUseCase {
    #[transactional()]
    pub async fn github_connection_status(
        &self,
        subject: Subject,
    ) -> Result<GithubConnectionStatus, CoreError> {
        PgGithubConnectionRepository::new(&tx, self.secrets.as_deref())
            .status(subject.organization_id, &subject.id)
            .await
    }

    pub async fn github_authorize_url<O: GithubUserOAuth>(
        &self,
        subject: Subject,
        state: &str,
        oauth: &O,
    ) -> Result<String, CoreError> {
        oauth.authorize_url(subject.organization_id, state).await
    }

    pub async fn connect_github_user<O: GithubUserOAuth>(
        &self,
        subject: Subject,
        code: &str,
        oauth: &O,
    ) -> Result<GithubConnectionStatus, CoreError> {
        let organization_id = subject.organization_id;
        let tokens = oauth.exchange_code(organization_id, code).await?;
        let user = oauth.user(organization_id, &tokens.access_token).await?;
        let now = Utc::now();
        let connection = GithubConnection {
            organization_id,
            subject: subject.id,
            github_user_id: user.id,
            github_login: user.login,
            tokens,
            created_at: now,
            updated_at: now,
        };
        self.store_github_connection(&connection).await?;
        Ok(GithubConnectionStatus::from(&connection))
    }

    pub async fn disconnect_github_user<O: GithubUserOAuth>(
        &self,
        subject: Subject,
        oauth: &O,
    ) -> Result<(), CoreError> {
        let organization_id = subject.organization_id;
        let Some(connection) = self
            .take_github_connection(organization_id, &subject.id)
            .await?
        else {
            return Ok(());
        };
        if let Err(error) = oauth
            .revoke(organization_id, &connection.tokens.access_token)
            .await
        {
            tracing::warn!(%organization_id, %error, "the GitHub grant was not revoked");
        }
        Ok(())
    }

    pub async fn member_git_token<H: CodeHost>(
        &self,
        organization_id: OrganizationId,
        subject: &str,
        min_validity: Duration,
        host: &H,
    ) -> Result<Option<SecretToken>, CoreError> {
        let Some(connection) = self
            .stored_github_connection(organization_id, subject)
            .await?
        else {
            return Ok(None);
        };
        let now = Utc::now();
        if !connection.tokens.needs_refresh(now, min_validity) {
            return Ok(Some(connection.tokens.access_token));
        }
        let Some(refresh_token) = connection.tokens.refresh_token.as_ref().filter(|_| {
            connection
                .tokens
                .refresh_expires_at
                .is_none_or(|expires_at| expires_at > now)
        }) else {
            tracing::warn!(%organization_id, "the member's GitHub token expires soon and cannot be renewed");
            return Ok(None);
        };
        match host
            .refresh_user_token(organization_id, refresh_token)
            .await
        {
            Ok(fresh) => {
                let renewed = GithubConnection {
                    tokens: connection.tokens.renewed_by(fresh),
                    updated_at: Utc::now(),
                    ..connection
                };
                self.store_github_connection(&renewed).await?;
                Ok(Some(renewed.tokens.access_token))
            }
            Err(error) => {
                let current = self
                    .stored_github_connection(organization_id, subject)
                    .await?;
                if let Some(current) = current
                    && current.tokens.access_token != connection.tokens.access_token
                    && !current.tokens.needs_refresh(Utc::now(), min_validity)
                {
                    return Ok(Some(current.tokens.access_token));
                }
                tracing::warn!(%organization_id, %error, "GitHub refused to renew the member's token");
                Ok(None)
            }
        }
    }

    pub async fn member_git_access_token(
        &self,
        organization_id: OrganizationId,
        subject: &str,
    ) -> Result<Option<SecretToken>, CoreError> {
        Ok(self
            .stored_github_connection(organization_id, subject)
            .await?
            .map(|connection| connection.tokens)
            .filter(|tokens| !tokens.is_expired(Utc::now()))
            .map(|tokens| tokens.access_token))
    }

    #[transactional()]
    async fn stored_github_connection(
        &self,
        organization_id: OrganizationId,
        subject: &str,
    ) -> Result<Option<GithubConnection>, CoreError> {
        PgGithubConnectionRepository::new(&tx, self.secrets.as_deref())
            .find(organization_id, subject)
            .await
    }

    #[transactional()]
    async fn store_github_connection(
        &self,
        connection: &GithubConnection,
    ) -> Result<(), CoreError> {
        PgGithubConnectionRepository::new(&tx, self.secrets.as_deref())
            .upsert(connection)
            .await
    }

    #[transactional()]
    async fn take_github_connection(
        &self,
        organization_id: OrganizationId,
        subject: &str,
    ) -> Result<Option<GithubConnection>, CoreError> {
        let mut repository = PgGithubConnectionRepository::new(&tx, self.secrets.as_deref());
        let found = match repository.find(organization_id, subject).await {
            Ok(found) => found,
            Err(error @ CoreError::Database(_)) => return Err(error),
            Err(error) => {
                tracing::warn!(%organization_id, %error, "a stored GitHub connection is unreadable, it is deleted without revoking");
                None
            }
        };
        repository.delete(organization_id, subject).await?;
        Ok(found)
    }
}
