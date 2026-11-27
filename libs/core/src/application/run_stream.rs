use matos_common::{CoreError, RunId};
use matos_macros::transactional;

use super::MatosUseCase;
use crate::{
    domain::{
        authorization::Subject,
        run::{ports::RunStreamPublisher, service::RunService, stream::RunStreamNotice},
    },
    infrastructure::run::stream::PgRunStreamPublisher,
};

impl MatosUseCase {
    #[transactional(transcript, run, emitter, authz)]
    pub async fn authorize_run_stream(
        &self,
        subject: Subject,
        run_id: RunId,
    ) -> Result<(), CoreError> {
        RunService::new(transcript_repository, run_repository, emitter, authz)
            .get(subject, run_id)
            .await?;
        Ok(())
    }

    pub async fn publish_run_stream(&self, notices: &[RunStreamNotice]) -> Result<(), CoreError> {
        let publisher = PgRunStreamPublisher::new(&self.pool);
        for notice in notices {
            publisher.publish(notice).await?;
        }
        Ok(())
    }
}
