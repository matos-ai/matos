use chrono::Utc;
use matos_common::{CoreError, OrganizationId, RunId};
use matos_events::run::RunFinished;
use matos_macros::transactional;

use super::MatosUseCase;
use crate::{
    domain::{
        authorization::{AccessRequest, Action, ResourceKind, Subject, require},
        event::ports::EventEmitter,
        execution::ports::TaskGrantRepository,
        member::ports::MemberRepository,
        run::{RunStatus, ports::RunRepository},
    },
    infrastructure::delegation::purge_delegations,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunCancellation {
    Cancelled,
    AlreadyCancelled,
}

impl MatosUseCase {
    #[transactional(run)]
    pub async fn run_cancellation(
        &self,
        organization_id: OrganizationId,
        run_id: RunId,
    ) -> Result<Option<String>, CoreError> {
        let mut run_repository = run_repository;
        Ok(run_repository
            .find(organization_id, run_id)
            .await?
            .filter(|overview| overview.run.status == RunStatus::Cancelled)
            .map(|overview| overview.run.failure.unwrap_or_default()))
    }

    #[transactional(run, member, task_grant, emitter, authz)]
    pub async fn cancel_run(
        &self,
        subject: Subject,
        run_id: RunId,
    ) -> Result<RunCancellation, CoreError> {
        let mut run_repository = run_repository;
        let mut member_repository = member_repository;
        let mut task_grant_repository = task_grant_repository;
        let organization_id = subject.organization_id;
        let mut run = run_repository
            .find(organization_id, run_id)
            .await?
            .ok_or(CoreError::NotFound)?
            .run;
        require(
            authz,
            AccessRequest::new(
                subject.clone(),
                Action::RunCancel {
                    initiated: run.initiated_by == subject.id,
                },
                ResourceKind::Run,
                Some(run_id.0),
            )
            .in_run(Some(run_id)),
        )
        .await?;
        match run.status {
            RunStatus::Cancelled => return Ok(RunCancellation::AlreadyCancelled),
            RunStatus::Succeeded | RunStatus::Failed => {
                return Err(CoreError::Conflict("run_already_finished".into()));
            }
            RunStatus::Queued | RunStatus::Running => {}
        }
        let name = member_repository
            .find(organization_id, &subject.id)
            .await?
            .map(|member| member.name)
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| subject.id.clone());
        let now = Utc::now();
        run.cancel(now, format!("Arrêté par {name}"))?;
        run_repository.save(&run, Some(now)).await?;
        task_grant_repository.revoke(run.id, now).await?;
        purge_delegations(&tx, run.id).await?;
        emitter.emit(&RunFinished {
            run_id: run.id.0,
            organization_id: run.organization_id.0,
            status: run.status.as_str().to_owned(),
            failure: run.failure.clone(),
        })?;
        Ok(RunCancellation::Cancelled)
    }
}
