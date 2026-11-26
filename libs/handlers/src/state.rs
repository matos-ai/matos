use matos_auth::{FerrisKeyTokenExchange, FerrisKeyVerifier};
use matos_core::MatosUseCase;

pub type MatosTokenVerifier = FerrisKeyVerifier;
pub type MatosTokenExchanger = FerrisKeyTokenExchange;

#[derive(Clone)]
pub struct AppState {
    pub usecase: MatosUseCase,
    pub verifier: MatosTokenVerifier,
    /// `None` when the API has no exchange credentials: runs are then queued
    /// without a delegation and their write step fails, saying so.
    pub delegation: Option<Delegation>,
}

#[derive(Clone)]
pub struct Delegation {
    pub exchanger: MatosTokenExchanger,
    /// Client ids of the agents a run acts through.
    pub agents: Vec<String>,
}
