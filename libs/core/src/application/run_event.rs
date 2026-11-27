use matos_common::{CoreError, OrganizationId, RunId};
use matos_macros::transactional;

use super::MatosUseCase;
use crate::domain::{
    authorization::Subject,
    run::{
        event::{RunEvent, SequencedRunEvent},
        ports::RunEventRepository,
        service::RunService,
    },
};

const LIST_LIMIT_MAX: u32 = 1000;

impl MatosUseCase {
    #[transactional(run_event)]
    pub async fn append_run_events(
        &self,
        organization_id: OrganizationId,
        run_id: RunId,
        events: Vec<SequencedRunEvent>,
    ) -> Result<(), CoreError> {
        let mut run_event_repository = run_event_repository;
        run_event_repository
            .append(organization_id, run_id, &events)
            .await
    }

    #[transactional(transcript, run, run_event, emitter, authz)]
    pub async fn list_run_events(
        &self,
        subject: Subject,
        run_id: RunId,
        after_seq: i64,
        limit: u32,
    ) -> Result<Vec<RunEvent>, CoreError> {
        let organization_id = subject.organization_id;
        RunService::new(transcript_repository, run_repository, emitter, authz)
            .get(subject, run_id)
            .await?;
        let mut run_event_repository = run_event_repository;
        run_event_repository
            .list_after(
                organization_id,
                run_id,
                after_seq,
                limit.clamp(1, LIST_LIMIT_MAX),
            )
            .await
    }
}
