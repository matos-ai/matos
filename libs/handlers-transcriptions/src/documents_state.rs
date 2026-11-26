use std::sync::Arc;

use matos_core::MatosUseCase;
use tokio::sync::Semaphore;

use crate::MAX_CONCURRENT_UPLOADS;

pub struct DocumentsState<R> {
    pub usecase: MatosUseCase,
    pub readers: Arc<R>,
    pub permits: Arc<Semaphore>,
}

impl<R> DocumentsState<R> {
    pub fn new(usecase: MatosUseCase, readers: R) -> Self {
        Self {
            usecase,
            readers: Arc::new(readers),
            permits: Arc::new(Semaphore::new(MAX_CONCURRENT_UPLOADS)),
        }
    }
}

impl<R> Clone for DocumentsState<R> {
    fn clone(&self) -> Self {
        Self {
            usecase: self.usecase.clone(),
            readers: Arc::clone(&self.readers),
            permits: Arc::clone(&self.permits),
        }
    }
}
