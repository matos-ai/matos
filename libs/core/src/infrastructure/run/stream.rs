use matos_common::CoreError;
use sqlx::PgPool;

use crate::{
    domain::run::{
        ports::RunStreamPublisher,
        stream::{RUN_STREAM_CHANNEL, RunStreamNotice},
    },
    infrastructure::postgres::map_sqlx_error,
};

pub struct PgRunStreamPublisher<'pool> {
    pool: &'pool PgPool,
}

impl<'pool> PgRunStreamPublisher<'pool> {
    pub fn new(pool: &'pool PgPool) -> Self {
        Self { pool }
    }
}

impl RunStreamPublisher for PgRunStreamPublisher<'_> {
    async fn publish(&self, notice: &RunStreamNotice) -> Result<(), CoreError> {
        let payload = notice.encode()?;
        sqlx::query("SELECT pg_notify($1, $2)")
            .bind(RUN_STREAM_CHANNEL)
            .bind(payload)
            .execute(self.pool)
            .await
            .map_err(map_sqlx_error)?;
        Ok(())
    }
}
