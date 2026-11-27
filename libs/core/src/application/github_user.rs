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
            GithubConnection, GithubConnectionStatus, PersonalToken,
            ports::{
                GithubConnectionRepository, GithubUserOAuth, PersonalTokenRepository,
                PersonalTokenValidator,
            },
        },
        provider::preview,
    },
    infrastructure::github_user::{
        personal::PgPersonalTokenRepository, postgres::PgGithubConnectionRepository,
    },
};

impl MatosUseCase {
    #[transactional()]
    pub async fn github_connection_status(
        &self,
        subject: Subject,
    ) -> Result<GithubConnectionStatus, CoreError> {
        let mut status = PgGithubConnectionRepository::new(&tx, self.secrets.as_deref())
            .status(subject.organization_id, &subject.id)
            .await?;
        status.personal_token = PgPersonalTokenRepository::new(&tx, self.secrets.as_deref())
            .status(subject.organization_id, &subject.id)
            .await?;
        Ok(status)
    }

    pub async fn save_github_personal_token<V: PersonalTokenValidator>(
        &self,
        subject: Subject,
        token: SecretToken,
        validator: &V,
    ) -> Result<GithubConnectionStatus, CoreError> {
        let trimmed = token.expose().trim();
        if trimmed.is_empty() {
            return Err(CoreError::Invalid("the GitHub token is empty".into()));
        }
        let token = SecretToken::new(trimmed);
        let check = validator.validate(&token).await?;
        let now = Utc::now();
        let stored = PersonalToken {
            organization_id: subject.organization_id,
            subject: subject.id.clone(),
            github_user_id: check.user.id,
            github_login: check.user.login,
            preview: preview(token.expose()),
            token,
            expires_at: check.expires_at,
            active: true,
            created_at: now,
            updated_at: now,
        };
        self.store_personal_token(&stored).await?;
        self.github_connection_status(subject).await
    }

    pub async fn set_github_personal_token_active(
        &self,
        subject: Subject,
        active: bool,
    ) -> Result<GithubConnectionStatus, CoreError> {
        if !self
            .update_personal_token(subject.organization_id, &subject.id, active)
            .await?
        {
            return Err(CoreError::NotFound);
        }
        self.github_connection_status(subject).await
    }

    #[transactional()]
    pub async fn delete_github_personal_token(&self, subject: Subject) -> Result<(), CoreError> {
        PgPersonalTokenRepository::new(&tx, self.secrets.as_deref())
            .delete(subject.organization_id, &subject.id)
            .await
            .map(|_| ())
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
            subject: subject.id.clone(),
            github_user_id: user.id,
            github_login: user.login,
            tokens,
            created_at: now,
            updated_at: now,
        };
        self.store_github_connection(&connection).await?;
        self.github_connection_status(subject).await
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
        if let Some(token) = self.usable_personal_token(organization_id, subject).await? {
            return Ok(Some(token));
        }
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
        if let Some(token) = self.usable_personal_token(organization_id, subject).await? {
            return Ok(Some(token));
        }
        Ok(self
            .stored_github_connection(organization_id, subject)
            .await?
            .map(|connection| connection.tokens)
            .filter(|tokens| !tokens.is_expired(Utc::now()))
            .map(|tokens| tokens.access_token))
    }

    async fn usable_personal_token(
        &self,
        organization_id: OrganizationId,
        subject: &str,
    ) -> Result<Option<SecretToken>, CoreError> {
        match self.stored_personal_token(organization_id, subject).await {
            Ok(found) => Ok(found
                .filter(|token| token.is_usable(Utc::now()))
                .map(|token| token.token)),
            Err(error @ CoreError::Database(_)) => Err(error),
            Err(error) => {
                tracing::warn!(%organization_id, %error, "the member's personal GitHub token is unreadable");
                Ok(None)
            }
        }
    }

    #[transactional()]
    async fn stored_personal_token(
        &self,
        organization_id: OrganizationId,
        subject: &str,
    ) -> Result<Option<PersonalToken>, CoreError> {
        PgPersonalTokenRepository::new(&tx, self.secrets.as_deref())
            .find(organization_id, subject)
            .await
    }

    #[transactional()]
    async fn store_personal_token(&self, token: &PersonalToken) -> Result<(), CoreError> {
        PgPersonalTokenRepository::new(&tx, self.secrets.as_deref())
            .upsert(token)
            .await
    }

    #[transactional()]
    async fn update_personal_token(
        &self,
        organization_id: OrganizationId,
        subject: &str,
        active: bool,
    ) -> Result<bool, CoreError> {
        PgPersonalTokenRepository::new(&tx, self.secrets.as_deref())
            .set_active(organization_id, subject, active)
            .await
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
