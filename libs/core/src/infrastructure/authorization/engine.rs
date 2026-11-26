use matos_common::CoreError;

#[cfg(feature = "test-policies")]
use super::CedarPolicyEngine;
use super::{AuthzenAuthorizer, PgIssuers};
use crate::domain::authorization::{AccessRequest, Decision, ports::Authorizer};

pub const NO_DECISION_POINT: &str = "no policy decision point in this service";

#[derive(Clone)]
pub enum PolicyEngine {
    Authzen(AuthzenAuthorizer<PgIssuers>),
    DenyAll,
    #[cfg(feature = "test-policies")]
    Cedar(CedarPolicyEngine),
}

impl Authorizer for PolicyEngine {
    async fn evaluate(&self, request: &AccessRequest) -> Result<Decision, CoreError> {
        match self {
            Self::Authzen(engine) => engine.evaluate(request).await,
            Self::DenyAll => Ok(denied()),
            #[cfg(feature = "test-policies")]
            Self::Cedar(engine) => engine.evaluate(request).await,
        }
    }

    async fn evaluate_all(&self, requests: &[AccessRequest]) -> Result<Vec<Decision>, CoreError> {
        match self {
            Self::Authzen(engine) => engine.evaluate_all(requests).await,
            Self::DenyAll => Ok(requests.iter().map(|_| denied()).collect()),
            #[cfg(feature = "test-policies")]
            Self::Cedar(engine) => engine.evaluate_all(requests).await,
        }
    }
}

fn denied() -> Decision {
    Decision::Deny {
        reason: NO_DECISION_POINT.to_owned(),
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
            Action::CardRead,
            ResourceKind::Card,
            None,
        )
    }

    #[tokio::test]
    async fn deny_all_denies_every_request_with_the_same_reason() {
        let denied = Decision::Deny {
            reason: NO_DECISION_POINT.to_owned(),
        };

        assert_eq!(
            PolicyEngine::DenyAll
                .evaluate(&request(true))
                .await
                .unwrap(),
            denied
        );
        assert_eq!(
            PolicyEngine::DenyAll
                .evaluate_all(&[request(true), request(false)])
                .await
                .unwrap(),
            vec![denied.clone(), denied]
        );
    }

    #[cfg(feature = "test-policies")]
    #[tokio::test]
    async fn the_cedar_variant_decides_with_the_realm_policies() {
        let engine = PolicyEngine::Cedar(CedarPolicyEngine::new());
        let mut provider = request(false);
        provider.action = Action::ProviderManage;

        let decisions = engine
            .evaluate_all(&[request(false), provider])
            .await
            .unwrap();

        assert_eq!(decisions[0], Decision::Allow);
        assert!(matches!(decisions[1], Decision::Deny { .. }));
    }
}
