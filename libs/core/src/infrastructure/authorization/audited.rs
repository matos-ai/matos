use matos_common::CoreError;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    domain::authorization::{AccessRequest, Decision, ports::Authorizer},
    infrastructure::postgres::map_sqlx_error,
};

/// Records every decision of the wrapped PDP. Written on its own connection,
/// outside the business transaction, so a denial survives the rollback it
/// causes. A decision that cannot be recorded is not returned: no unaudited
/// write goes through.
#[derive(Clone)]
pub struct AuditedAuthorizer<A> {
    inner: A,
    pool: PgPool,
}

impl<A> AuditedAuthorizer<A> {
    pub fn new(inner: A, pool: PgPool) -> Self {
        Self { inner, pool }
    }
}

impl<A> AuditedAuthorizer<A> {
    async fn record(&self, request: &AccessRequest, decision: &Decision) -> Result<(), CoreError> {
        let (allowed, reason) = match decision {
            Decision::Allow => (true, None),
            Decision::Deny { reason } => (false, Some(reason.as_str())),
        };
        sqlx::query!(
            r#"
            INSERT INTO authorization_decisions
                (id, organization_id, subject, actor, action, resource_kind, resource_id, allowed, reason, run_id)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
            "#,
            Uuid::now_v7(),
            request.subject.organization_id.0,
            request.subject.id,
            request.subject.actor,
            request.action.as_str(),
            request.resource.kind.as_str(),
            request.resource.id,
            allowed,
            reason,
            request.run_id.map(|id| id.0),
        )
        .execute(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        tracing::info!(
            action = request.action.as_str(),
            actor = request.subject.actor.as_deref(),
            allowed,
            reason,
            "authorization decision"
        );
        Ok(())
    }
}

impl<A: Authorizer> Authorizer for AuditedAuthorizer<A> {
    async fn evaluate(&self, request: &AccessRequest) -> Result<Decision, CoreError> {
        let decision = self.inner.evaluate(request).await?;
        self.record(request, &decision).await?;
        Ok(decision)
    }

    async fn evaluate_all(&self, requests: &[AccessRequest]) -> Result<Vec<Decision>, CoreError> {
        let decisions = self.inner.evaluate_all(requests).await?;
        for (request, decision) in requests.iter().zip(&decisions) {
            self.record(request, decision).await?;
        }
        Ok(decisions)
    }
}

#[cfg(test)]
mod tests {
    use matos_common::OrganizationId;

    use super::*;
    use crate::{
        domain::{
            agent::SecretToken,
            authorization::{Action, ResourceKind, Subject},
        },
        infrastructure::{
            authorization::{
                AuthzenAuthorizer, LocalPolicyEngine, PDP_UNAVAILABLE, PdpCredentials, PgIssuers,
                PolicyEngine,
            },
            postgres,
        },
    };

    async fn organization(pool: &PgPool, issuer: &str) -> OrganizationId {
        let id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO organizations (id, slug, name, issuer_url) VALUES ($1, $2, 'Test', $3)",
        )
        .bind(id)
        .bind(format!("audit-{}", id.simple()))
        .bind(format!("{issuer}/{}", id.simple()))
        .execute(pool)
        .await
        .unwrap();
        OrganizationId(id)
    }

    fn request(organization_id: OrganizationId, action: Action) -> AccessRequest {
        AccessRequest::new(
            Subject {
                id: "user-1".into(),
                organization_id,
                actor: None,
                is_admin: false,
            },
            action,
            ResourceKind::Card,
            None,
        )
    }

    async fn rows(
        pool: &PgPool,
        organization_id: OrganizationId,
    ) -> Vec<(String, bool, Option<String>)> {
        sqlx::query_as(
            "SELECT action, allowed, reason FROM authorization_decisions WHERE organization_id = $1 ORDER BY id",
        )
        .bind(organization_id.0)
        .fetch_all(pool)
        .await
        .unwrap()
    }

    async fn pool() -> Option<PgPool> {
        let Ok(url) = std::env::var("DATABASE_URL") else {
            eprintln!("skipped: DATABASE_URL is unset");
            return None;
        };
        Some(postgres::connect(&url, 2).await.unwrap())
    }

    #[tokio::test]
    async fn a_pdp_that_is_down_is_a_recorded_denial() {
        let Some(pool) = pool().await else { return };
        let organization_id = organization(&pool, "http://127.0.0.1:1/realms/down").await;
        let pdp = AuthzenAuthorizer::new(
            PgIssuers::new(pool.clone()),
            PdpCredentials {
                client_id: "matos-api".into(),
                client_secret: SecretToken::new("s"),
            },
        );
        let audited = AuditedAuthorizer::new(PolicyEngine::Authzen(pdp), pool.clone());

        let decision = audited
            .evaluate(&request(organization_id, Action::CardCreate))
            .await
            .unwrap();

        assert_eq!(
            decision,
            Decision::Deny {
                reason: PDP_UNAVAILABLE.into()
            }
        );
        assert_eq!(
            rows(&pool, organization_id).await,
            vec![("card.create".into(), false, Some(PDP_UNAVAILABLE.into()))]
        );
    }

    #[tokio::test]
    async fn every_decision_of_a_batch_is_recorded_denials_included() {
        let Some(pool) = pool().await else { return };
        let organization_id = organization(&pool, "http://idp.test/realms/batch").await;
        let audited = AuditedAuthorizer::new(PolicyEngine::Local(LocalPolicyEngine), pool.clone());

        let decisions = audited
            .evaluate_all(&[
                request(organization_id, Action::CardRead),
                request(organization_id, Action::ProviderManage),
            ])
            .await
            .unwrap();

        assert_eq!(decisions.len(), 2);
        let recorded = rows(&pool, organization_id).await;
        assert_eq!(recorded.len(), 2);
        assert_eq!(recorded[0], ("card.read".into(), true, None));
        assert_eq!(recorded[1].0, "provider.manage");
        assert!(!recorded[1].1);
        assert!(recorded[1].2.is_some());
    }
}
