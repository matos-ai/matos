use matos_common::CoreError;
use matos_macros::transactional;

use super::MatosUseCase;
use crate::{Organization, domain::organization::service::OrganizationService};

impl MatosUseCase {
    #[transactional(organization)]
    pub async fn organization_by_issuer(
        &self,
        issuer_url: &str,
    ) -> Result<Organization, CoreError> {
        OrganizationService::new(organization_repository)
            .by_issuer(issuer_url)
            .await
    }
}
