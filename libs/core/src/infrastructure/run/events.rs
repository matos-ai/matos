use chrono::{DateTime, Utc};
use matos_common::{CoreError, OrganizationId, RunId};
use matos_macros::repository;
use uuid::Uuid;

use crate::{
    domain::run::{
        event::{RunEvent, RunEventKind, SequencedRunEvent},
        ports::RunEventRepository,
    },
    infrastructure::postgres::{SharedTx, map_sqlx_error},
};

struct RunEventRow {
    run_id: Uuid,
    organization_id: Uuid,
    seq: i64,
    kind: String,
    payload: serde_json::Value,
    created_at: DateTime<Utc>,
}

impl TryFrom<RunEventRow> for RunEvent {
    type Error = CoreError;

    fn try_from(row: RunEventRow) -> Result<Self, Self::Error> {
        Ok(Self {
            run_id: RunId(row.run_id),
            organization_id: OrganizationId(row.organization_id),
            seq: row.seq,
            kind: RunEventKind::try_from(row.kind.as_str())?,
            payload: row.payload,
            created_at: row.created_at,
        })
    }
}

#[repository(domain = RunEvent, backend = Postgres)]
pub struct PgRunEventRepository<'tx> {
    tx: SharedTx<'tx>,
}

impl<'tx> PgRunEventRepository<'tx> {
    pub fn new(tx: &SharedTx<'tx>) -> Self {
        Self { tx: tx.clone() }
    }
}

impl RunEventRepository for PgRunEventRepository<'_> {
    async fn append(
        &mut self,
        organization_id: OrganizationId,
        run_id: RunId,
        events: &[SequencedRunEvent],
    ) -> Result<(), CoreError> {
        if events.is_empty() {
            return Ok(());
        }
        let seqs: Vec<i64> = events.iter().map(|event| event.seq).collect();
        let kinds: Vec<String> = events
            .iter()
            .map(|event| event.draft.kind().as_str().to_owned())
            .collect();
        let payloads: Vec<serde_json::Value> =
            events.iter().map(|event| event.draft.payload()).collect();
        let mut tx = self.tx.lock().await;
        sqlx::query!(
            r#"
            INSERT INTO run_events (run_id, organization_id, seq, kind, payload)
            SELECT $1, $2, seq, kind, payload
            FROM UNNEST($3::bigint[], $4::text[], $5::jsonb[]) AS e(seq, kind, payload)
            ON CONFLICT (run_id, seq) DO NOTHING
            "#,
            run_id.0,
            organization_id.0,
            &seqs,
            &kinds,
            &payloads,
        )
        .execute(&mut ***tx)
        .await
        .map_err(map_sqlx_error)?;
        Ok(())
    }

    async fn list_after(
        &mut self,
        organization_id: OrganizationId,
        run_id: RunId,
        after_seq: i64,
        limit: u32,
    ) -> Result<Vec<RunEvent>, CoreError> {
        let mut tx = self.tx.lock().await;
        sqlx::query_as!(
            RunEventRow,
            r#"
            SELECT run_id, organization_id, seq, kind, payload, created_at
            FROM run_events
            WHERE organization_id = $1 AND run_id = $2 AND seq > $3
            ORDER BY seq
            LIMIT $4
            "#,
            organization_id.0,
            run_id.0,
            after_seq,
            i64::from(limit),
        )
        .fetch_all(&mut ***tx)
        .await
        .map_err(map_sqlx_error)?
        .into_iter()
        .map(RunEvent::try_from)
        .collect()
    }
}
