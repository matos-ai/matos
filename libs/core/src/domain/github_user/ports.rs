use matos_common::{CoreError, OrganizationId};

use super::{
    GithubConnection, GithubConnectionStatus, GithubUser, PersonalToken, PersonalTokenCheck,
    PersonalTokenStatus, UserTokens,
};
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

pub trait PersonalTokenRepository: Send {
    fn find(
        &mut self,
        organization_id: OrganizationId,
        subject: &str,
    ) -> impl Future<Output = Result<Option<PersonalToken>, CoreError>> + Send;

    fn status(
        &mut self,
        organization_id: OrganizationId,
        subject: &str,
    ) -> impl Future<Output = Result<Option<PersonalTokenStatus>, CoreError>> + Send;

    fn upsert(
        &mut self,
        token: &PersonalToken,
    ) -> impl Future<Output = Result<(), CoreError>> + Send;

    fn set_active(
        &mut self,
        organization_id: OrganizationId,
        subject: &str,
        active: bool,
    ) -> impl Future<Output = Result<bool, CoreError>> + Send;

    fn delete(
        &mut self,
        organization_id: OrganizationId,
        subject: &str,
    ) -> impl Future<Output = Result<bool, CoreError>> + Send;
}

#[cfg_attr(any(test, feature = "mock"), mockall::automock)]
pub trait PersonalTokenValidator: Send + Sync {
    fn validate(
        &self,
        token: &SecretToken,
    ) -> impl Future<Output = Result<PersonalTokenCheck, CoreError>> + Send;
}
