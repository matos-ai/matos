use matos_common::CoreError;

use super::{AuthzenAuthorizer, LocalPolicyEngine, PgIssuers};
use crate::domain::authorization::{AccessRequest, Decision, ports::Authorizer};

#[derive(Clone)]
pub enum PolicyEngine {
    Local(LocalPolicyEngine),
    Authzen(AuthzenAuthorizer<PgIssuers>),
}

impl Authorizer for PolicyEngine {
    async fn evaluate(&self, request: &AccessRequest) -> Result<Decision, CoreError> {
        match self {
            Self::Local(engine) => engine.evaluate(request).await,
            Self::Authzen(engine) => engine.evaluate(request).await,
        }
    }

    async fn evaluate_all(&self, requests: &[AccessRequest]) -> Result<Vec<Decision>, CoreError> {
        match self {
            Self::Local(engine) => engine.evaluate_all(requests).await,
            Self::Authzen(engine) => engine.evaluate_all(requests).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use matos_common::OrganizationId;

    use super::*;
    use crate::domain::authorization::{Action, ResourceKind, Subject};

    fn request(is_admin: bool) -> AccessRequest {
        AccessRequest::new(
            Subject {
                id: "user-1".into(),
                organization_id: OrganizationId::new(),
                actor: None,
                is_admin,
            },
            Action::ProviderManage,
            ResourceKind::Provider,
            None,
        )
    }

    #[tokio::test]
    async fn the_local_variant_decides_with_the_local_rules() {
        let engine = PolicyEngine::Local(LocalPolicyEngine);
        assert_eq!(
            engine.evaluate(&request(true)).await.unwrap(),
            Decision::Allow
        );
        let decisions = engine
            .evaluate_all(&[request(true), request(false)])
            .await
            .unwrap();
        assert_eq!(decisions[0], Decision::Allow);
        assert!(matches!(decisions[1], Decision::Deny { .. }));
    }
}
