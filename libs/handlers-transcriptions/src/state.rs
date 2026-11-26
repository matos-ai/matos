use std::sync::Arc;

use matos_core::MatosUseCase;
use tokio::sync::Semaphore;

use crate::MAX_CONCURRENT_UPLOADS;

pub struct TranscriptionsState<F> {
    pub usecase: MatosUseCase,
    pub factory: Arc<F>,
    pub permits: Arc<Semaphore>,
}

impl<F> TranscriptionsState<F> {
    pub fn new(usecase: MatosUseCase, factory: F) -> Self {
        Self {
            usecase,
            factory: Arc::new(factory),
            permits: Arc::new(Semaphore::new(MAX_CONCURRENT_UPLOADS)),
        }
    }
}

impl<F> Clone for TranscriptionsState<F> {
    fn clone(&self) -> Self {
        Self {
            usecase: self.usecase.clone(),
            factory: Arc::clone(&self.factory),
            permits: Arc::clone(&self.permits),
        }
    }
}
