use matos_common::{CoreError, OrganizationId};

use super::{GithubConnection, GithubConnectionStatus, GithubUser, UserTokens};
use crate::domain::agent::SecretToken;

pub trait GithubConnectionRepository: Send {
    fn find(
        &mut self,
        organization_id: OrganizationId,
        subject: &str,
    ) -> impl Future<Output = Result<Option<GithubConnection>, CoreError>> + Send;

    fn status(
        &mut self,
        organization_id: OrganizationId,
        subject: &str,
    ) -> impl Future<Output = Result<GithubConnectionStatus, CoreError>> + Send;

    fn upsert(
        &mut self,
        connection: &GithubConnection,
    ) -> impl Future<Output = Result<(), CoreError>> + Send;

    fn delete(
        &mut self,
        organization_id: OrganizationId,
        subject: &str,
    ) -> impl Future<Output = Result<bool, CoreError>> + Send;
}

#[cfg_attr(any(test, feature = "mock"), mockall::automock)]
pub trait GithubUserOAuth: Send + Sync {
    fn authorize_url(
        &self,
        organization_id: OrganizationId,
        state: &str,
    ) -> impl Future<Output = Result<String, CoreError>> + Send;

    fn exchange_code(
        &self,
        organization_id: OrganizationId,
        code: &str,
    ) -> impl Future<Output = Result<UserTokens, CoreError>> + Send;

    fn user(
        &self,
        organization_id: OrganizationId,
        access_token: &SecretToken,
    ) -> impl Future<Output = Result<GithubUser, CoreError>> + Send;

    fn revoke(
        &self,
        organization_id: OrganizationId,
        access_token: &SecretToken,
    ) -> impl Future<Output = Result<(), CoreError>> + Send;
}
