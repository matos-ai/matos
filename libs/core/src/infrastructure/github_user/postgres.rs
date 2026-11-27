use matos_common::{CoreError, OrganizationId};

use crate::{
    domain::{
        agent::SecretToken,
        github_user::{
            GithubConnection, GithubConnectionStatus, UserTokens, ports::GithubConnectionRepository,
        },
    },
    infrastructure::{
        crypto::{Sealed, SecretCipher},
        postgres::{SharedTx, map_sqlx_error},
    },
};

const MISSING_KEY: &str = "SECRETS_KEY is not configured";

pub struct PgGithubConnectionRepository<'tx, 'c> {
    tx: SharedTx<'tx>,
    cipher: Option<&'c SecretCipher>,
}

impl<'tx, 'c> PgGithubConnectionRepository<'tx, 'c> {
    pub fn new(tx: &SharedTx<'tx>, cipher: Option<&'c SecretCipher>) -> Self {
        Self {
            tx: tx.clone(),
            cipher,
        }
    }

    fn cipher(&self) -> Result<&'c SecretCipher, CoreError> {
        self.cipher
            .ok_or_else(|| CoreError::Internal(MISSING_KEY.into()))
    }
}

fn binding(organization_id: OrganizationId, subject: &str, kind: &str) -> Vec<u8> {
    format!("github-user/{organization_id}/{subject}/{kind}").into_bytes()
}

fn open_text(cipher: &SecretCipher, sealed: &Sealed, binding: &[u8]) -> Result<String, CoreError> {
    String::from_utf8(cipher.open(sealed, binding)?)
        .map_err(|_| CoreError::Internal("a stored GitHub token is not text".into()))
}

impl GithubConnectionRepository for PgGithubConnectionRepository<'_, '_> {
    async fn find(
        &mut self,
        organization_id: OrganizationId,
        subject: &str,
    ) -> Result<Option<GithubConnection>, CoreError> {
        let cipher = self.cipher;
        let mut tx = self.tx.lock().await;
        let row = sqlx::query!(
            r#"
            SELECT github_user_id, github_login, access_nonce, access_ciphertext,
                   access_expires_at, refresh_nonce, refresh_ciphertext, refresh_expires_at,
                   created_at, updated_at
            FROM github_user_connections
            WHERE organization_id = $1 AND subject = $2
            "#,
            organization_id.0,
            subject,
        )
        .fetch_optional(&mut ***tx)
        .await
        .map_err(map_sqlx_error)?;
        row.map(|row| {
            let cipher = cipher.ok_or_else(|| CoreError::Internal(MISSING_KEY.into()))?;
            let access = open_text(
                cipher,
                &Sealed {
                    nonce: row.access_nonce,
                    ciphertext: row.access_ciphertext,
                },
                &binding(organization_id, subject, "access"),
            )?;
            let refresh = match (row.refresh_nonce, row.refresh_ciphertext) {
                (Some(nonce), Some(ciphertext)) => Some(SecretToken::new(open_text(
                    cipher,
                    &Sealed { nonce, ciphertext },
                    &binding(organization_id, subject, "refresh"),
                )?)),
                _ => None,
            };
            Ok(GithubConnection {
                organization_id,
                subject: subject.to_owned(),
                github_user_id: row.github_user_id,
                github_login: row.github_login,
                tokens: UserTokens {
                    access_token: SecretToken::new(access),
                    expires_at: row.access_expires_at,
                    refresh_token: refresh,
                    refresh_expires_at: row.refresh_expires_at,
                },
                created_at: row.created_at,
                updated_at: row.updated_at,
            })
        })
        .transpose()
    }

    async fn status(
        &mut self,
        organization_id: OrganizationId,
        subject: &str,
    ) -> Result<GithubConnectionStatus, CoreError> {
        let mut tx = self.tx.lock().await;
        let row = sqlx::query!(
            r#"
            SELECT github_login, created_at
            FROM github_user_connections
            WHERE organization_id = $1 AND subject = $2
            "#,
            organization_id.0,
            subject,
        )
        .fetch_optional(&mut ***tx)
        .await
        .map_err(map_sqlx_error)?;
        Ok(
            row.map_or_else(GithubConnectionStatus::disconnected, |row| {
                GithubConnectionStatus {
                    connected: true,
                    login: Some(row.github_login),
                    connected_at: Some(row.created_at),
                }
            }),
        )
    }

    async fn upsert(&mut self, connection: &GithubConnection) -> Result<(), CoreError> {
        let cipher = self.cipher()?;
        let organization_id = connection.organization_id;
        let subject = connection.subject.as_str();
        let access = cipher.seal(
            connection.tokens.access_token.expose().as_bytes(),
            &binding(organization_id, subject, "access"),
        )?;
        let refresh = connection
            .tokens
            .refresh_token
            .as_ref()
            .map(|token| {
                cipher.seal(
                    token.expose().as_bytes(),
                    &binding(organization_id, subject, "refresh"),
                )
            })
            .transpose()?;
        let (refresh_nonce, refresh_ciphertext) = match refresh {
            Some(sealed) => (Some(sealed.nonce), Some(sealed.ciphertext)),
            None => (None, None),
        };
        let mut tx = self.tx.lock().await;
        sqlx::query!(
            r#"
            INSERT INTO github_user_connections (
                organization_id, subject, github_user_id, github_login, access_nonce,
                access_ciphertext, access_expires_at, refresh_nonce, refresh_ciphertext,
                refresh_expires_at, created_at, updated_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
            ON CONFLICT (organization_id, subject) DO UPDATE
            SET github_user_id = EXCLUDED.github_user_id,
                github_login = EXCLUDED.github_login,
                access_nonce = EXCLUDED.access_nonce,
                access_ciphertext = EXCLUDED.access_ciphertext,
                access_expires_at = EXCLUDED.access_expires_at,
                refresh_nonce = EXCLUDED.refresh_nonce,
                refresh_ciphertext = EXCLUDED.refresh_ciphertext,
                refresh_expires_at = EXCLUDED.refresh_expires_at,
                updated_at = EXCLUDED.updated_at
            "#,
            organization_id.0,
            subject,
            connection.github_user_id,
            connection.github_login,
            access.nonce,
            access.ciphertext,
            connection.tokens.expires_at,
            refresh_nonce,
            refresh_ciphertext,
            connection.tokens.refresh_expires_at,
            connection.created_at,
            connection.updated_at,
        )
        .execute(&mut ***tx)
        .await
        .map(|_| ())
        .map_err(map_sqlx_error)
    }

    async fn delete(
        &mut self,
        organization_id: OrganizationId,
        subject: &str,
    ) -> Result<bool, CoreError> {
        let mut tx = self.tx.lock().await;
        sqlx::query!(
            "DELETE FROM github_user_connections WHERE organization_id = $1 AND subject = $2",
            organization_id.0,
            subject,
        )
        .execute(&mut ***tx)
        .await
        .map(|done| done.rows_affected() > 0)
        .map_err(map_sqlx_error)
    }
}
