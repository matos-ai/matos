mod agent_identity;
mod agent_run;
mod agent_task_pipeline;
mod approval;
mod artifact;
mod audit;
mod card;
mod client;
mod conversation;
mod crew;
mod document;
mod execute_pipeline;
mod execute_stream;
mod execution;
mod inbox;
mod integration;
mod integration_action;
mod integration_revert;
mod intervene_pipeline;
mod intervention;
mod mcp;
mod organization;
mod pipeline;
mod provider;
mod routine;
mod run;
mod skill;
mod source;
mod transcription;
mod user_provider;
mod worksite;

use std::sync::Arc;

use matos_common::CoreError;
use sqlx::PgPool;

use crate::domain::agent::SecretToken;
#[cfg(feature = "test-policies")]
use crate::infrastructure::authorization::CedarPolicyEngine;
use crate::infrastructure::{
    authorization::{
        AuditedAuthorizer, AuthzenAuthorizer, PdpCredentials, PgIssuers, PolicyEngine,
    },
    crypto::SecretCipher,
    postgres,
};

pub use agent_identity::IdentitySweep;
pub use agent_run::{
    AgentTaskSource, ExecutionAgentSource, RunDelegation, StartedAgentRun, TaskStage,
};
pub use agent_task_pipeline::{AgentTaskConfig, AgentTaskPipeline};
pub use approval::DecidedApproval;
pub use client::RunCardLinks;
pub use document::PreparedDocumentReading;
pub use execute_pipeline::{ExecuteConfig, ExecutePipeline, ExecutionContext, Executor};
pub use execution::StartedExecution;
pub use intervene_pipeline::{InterventionConfig, InterventionPipeline};
pub use intervention::StartedIntervention;
pub use pipeline::{MeetingPipeline, PipelineConfig, RunContext};
pub use provider::{ModelListing, ProviderFailure};
pub use run::{AgentDelegation, RunTimeline, SubmittedTranscript};
pub use transcription::PreparedTranscription;

pub type MatosAuthorizer = AuditedAuthorizer<PolicyEngine>;

/// Long-lived and cloned into every request: never hold request-scoped state here.
#[derive(Clone)]
pub struct MatosUseCase {
    pub(crate) pool: PgPool,
    pub(crate) authz: MatosAuthorizer,
    /// Seals the delegated tokens kept between submission and execution.
    /// `None` without `DELEGATION_KEY`: submissions that need a delegation
    /// are then refused rather than stored in clear.
    pub(crate) cipher: Option<Arc<SecretCipher>>,
    /// Seals the organizations' provider API keys, with a key distinct from
    /// the delegation one. `None` without `SECRETS_KEY`: no key can then be
    /// stored or read, and the rest of the API is unaffected.
    pub(crate) secrets: Option<Arc<SecretCipher>>,
}

impl MatosUseCase {
    pub fn new(pool: PgPool, authz: MatosAuthorizer) -> Self {
        Self {
            pool,
            authz,
            cipher: None,
            secrets: None,
        }
    }

    pub fn with_cipher(mut self, cipher: SecretCipher) -> Self {
        self.cipher = Some(Arc::new(cipher));
        self
    }

    pub fn with_secrets_cipher(mut self, cipher: SecretCipher) -> Self {
        self.secrets = Some(Arc::new(cipher));
        self
    }

    pub(crate) fn cipher(&self) -> Result<&SecretCipher, CoreError> {
        self.cipher
            .as_deref()
            .ok_or_else(|| CoreError::Internal("DELEGATION_KEY is not configured".into()))
    }

    pub fn with_authzen_policies(pool: PgPool, credentials: PdpCredentials) -> Self {
        let engine = AuthzenAuthorizer::new(PgIssuers::new(pool.clone()), credentials);
        Self::with_engine(pool, PolicyEngine::Authzen(engine))
    }

    pub fn with_policies(pool: PgPool, client_id: &str, client_secret: &str) -> Self {
        Self::with_authzen_policies(
            pool,
            PdpCredentials {
                client_id: client_id.to_owned(),
                client_secret: SecretToken::new(client_secret),
            },
        )
    }

    pub fn without_policies(pool: PgPool) -> Self {
        Self::with_engine(pool, PolicyEngine::DenyAll)
    }

    #[cfg(feature = "test-policies")]
    pub fn with_test_policies(pool: PgPool) -> Self {
        Self::with_engine(pool, PolicyEngine::Cedar(CedarPolicyEngine::new()))
    }

    fn with_engine(pool: PgPool, engine: PolicyEngine) -> Self {
        let authz = AuditedAuthorizer::new(engine, pool.clone());
        Self::new(pool, authz)
    }

    pub async fn readiness(&self) -> Result<(), CoreError> {
        postgres::ping(&self.pool).await
    }
}
