use matos_common::{CoreError, OrganizationId};

use crate::{
    domain::{
        agent::SecretToken,
        github_user::{PersonalToken, PersonalTokenStatus, ports::PersonalTokenRepository},
    },
    infrastructure::{
        crypto::{Sealed, SecretCipher},
        postgres::{SharedTx, map_sqlx_error},
    },
};

const MISSING_KEY: &str = "SECRETS_KEY is not configured";

pub struct PgPersonalTokenRepository<'tx, 'c> {
    tx: SharedTx<'tx>,
    cipher: Option<&'c SecretCipher>,
}

impl<'tx, 'c> PgPersonalTokenRepository<'tx, 'c> {
    pub fn new(tx: &SharedTx<'tx>, cipher: Option<&'c SecretCipher>) -> Self {
        Self {
            tx: tx.clone(),
            cipher,
        }
    }
}

fn binding(organization_id: OrganizationId, subject: &str) -> Vec<u8> {
    format!("github-pat/{organization_id}/{subject}").into_bytes()
}

impl PersonalTokenRepository for PgPersonalTokenRepository<'_, '_> {
    async fn find(
        &mut self,
        organization_id: OrganizationId,
        subject: &str,
    ) -> Result<Option<PersonalToken>, CoreError> {
        let cipher = self.cipher;
        let mut tx = self.tx.lock().await;
        let row = sqlx::query!(
            r#"
            SELECT github_user_id, github_login, token_nonce, token_ciphertext, token_preview,
                   expires_at, active, created_at, updated_at
            FROM github_personal_tokens
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
            let opened = cipher.open(
                &Sealed {
                    nonce: row.token_nonce,
                    ciphertext: row.token_ciphertext,
                },
                &binding(organization_id, subject),
            )?;
            let token = String::from_utf8(opened)
                .map_err(|_| CoreError::Internal("a stored GitHub token is not text".into()))?;
            Ok(PersonalToken {
                organization_id,
                subject: subject.to_owned(),
                github_user_id: row.github_user_id,
                github_login: row.github_login,
                token: SecretToken::new(token),
                preview: row.token_preview,
                expires_at: row.expires_at,
                active: row.active,
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
    ) -> Result<Option<PersonalTokenStatus>, CoreError> {
        let mut tx = self.tx.lock().await;
        let row = sqlx::query!(
            r#"
            SELECT github_login, token_preview, expires_at, active
            FROM github_personal_tokens
            WHERE organization_id = $1 AND subject = $2
            "#,
            organization_id.0,
            subject,
        )
        .fetch_optional(&mut ***tx)
        .await
        .map_err(map_sqlx_error)?;
        Ok(row.map(|row| PersonalTokenStatus {
            login: row.github_login,
            preview: row.token_preview,
            expires_at: row.expires_at,
            active: row.active,
        }))
    }

    async fn upsert(&mut self, token: &PersonalToken) -> Result<(), CoreError> {
        let cipher = self
            .cipher
            .ok_or_else(|| CoreError::Internal(MISSING_KEY.into()))?;
        let sealed = cipher.seal(
            token.token.expose().as_bytes(),
            &binding(token.organization_id, &token.subject),
        )?;
        let mut tx = self.tx.lock().await;
        sqlx::query!(
            r#"
            INSERT INTO github_personal_tokens (
                organization_id, subject, github_user_id, github_login, token_nonce,
                token_ciphertext, token_preview, expires_at, active, created_at, updated_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
            ON CONFLICT (organization_id, subject) DO UPDATE
            SET github_user_id = EXCLUDED.github_user_id,
                github_login = EXCLUDED.github_login,
                token_nonce = EXCLUDED.token_nonce,
                token_ciphertext = EXCLUDED.token_ciphertext,
                token_preview = EXCLUDED.token_preview,
                expires_at = EXCLUDED.expires_at,
                active = EXCLUDED.active,
                updated_at = EXCLUDED.updated_at
            "#,
            token.organization_id.0,
            token.subject,
            token.github_user_id,
            token.github_login,
            sealed.nonce,
            sealed.ciphertext,
            token.preview,
            token.expires_at,
            token.active,
            token.created_at,
            token.updated_at,
        )
        .execute(&mut ***tx)
        .await
        .map(|_| ())
        .map_err(map_sqlx_error)
    }

    async fn set_active(
        &mut self,
        organization_id: OrganizationId,
        subject: &str,
        active: bool,
    ) -> Result<bool, CoreError> {
        let mut tx = self.tx.lock().await;
        sqlx::query!(
            r#"
            UPDATE github_personal_tokens SET active = $3, updated_at = now()
            WHERE organization_id = $1 AND subject = $2
            "#,
            organization_id.0,
            subject,
            active,
        )
        .execute(&mut ***tx)
        .await
        .map(|done| done.rows_affected() > 0)
        .map_err(map_sqlx_error)
    }

    async fn delete(
        &mut self,
        organization_id: OrganizationId,
        subject: &str,
    ) -> Result<bool, CoreError> {
        let mut tx = self.tx.lock().await;
        sqlx::query!(
            "DELETE FROM github_personal_tokens WHERE organization_id = $1 AND subject = $2",
            organization_id.0,
            subject,
        )
        .execute(&mut ***tx)
        .await
        .map(|done| done.rows_affected() > 0)
        .map_err(map_sqlx_error)
    }
}
