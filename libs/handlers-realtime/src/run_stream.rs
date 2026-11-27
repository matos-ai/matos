use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

use matos_core::domain::run::stream::{RUN_STREAM_CHANNEL, RunStreamNotice};
use sqlx::{PgPool, postgres::PgListener};
use tokio::{
    sync::broadcast::{self, error::SendError},
    task::JoinHandle,
};

const FIRST_RETRY: Duration = Duration::from_secs(1);
const LAST_RETRY: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Relayed {
    Notice(RunStreamNotice),
    Gap,
}

#[derive(Clone)]
pub struct StreamHub {
    sender: broadcast::Sender<Arc<Relayed>>,
}

impl StreamHub {
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity.max(1));
        Self { sender }
    }

    pub fn publish(&self, relayed: Relayed) {
        let _: Result<usize, SendError<Arc<Relayed>>> = self.sender.send(Arc::new(relayed));
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<Relayed>> {
        self.sender.subscribe()
    }
}

pub fn spawn(pool: PgPool, hub: StreamHub) -> JoinHandle<()> {
    spawn_until(pool, hub, std::future::pending())
}

pub fn spawn_until<S>(pool: PgPool, hub: StreamHub, shutdown: S) -> JoinHandle<()>
where
    S: Future<Output = ()> + Send + 'static,
{
    tokio::spawn(async move { run(pool, hub, shutdown).await })
}

#[derive(PartialEq, Eq)]
enum Outcome {
    Lost,
    Stopped,
}

async fn run<S: Future<Output = ()>>(pool: PgPool, hub: StreamHub, shutdown: S) {
    tokio::pin!(shutdown);
    let mut retry = FIRST_RETRY;
    let mut first = true;
    loop {
        if session(&pool, &hub, &mut retry, &mut first, &mut shutdown).await == Outcome::Stopped {
            return;
        }
        tokio::select! {
            () = &mut shutdown => return,
            () = tokio::time::sleep(retry) => {}
        }
        retry = (retry * 2).min(LAST_RETRY);
    }
}

async fn session<S: Future<Output = ()>>(
    pool: &PgPool,
    hub: &StreamHub,
    retry: &mut Duration,
    first: &mut bool,
    shutdown: &mut Pin<&mut S>,
) -> Outcome {
    let connected = tokio::select! {
        () = &mut *shutdown => return Outcome::Stopped,
        connected = connect(pool) => connected,
    };
    let mut listener = match connected {
        Ok(listener) => listener,
        Err(error) => {
            tracing::warn!(%error, "run stream listener could not connect, retrying");
            return Outcome::Lost;
        }
    };
    *retry = FIRST_RETRY;
    tracing::info!("run stream listener connected");
    if !*first {
        hub.publish(Relayed::Gap);
    }
    *first = false;
    loop {
        let received = tokio::select! {
            () = &mut *shutdown => return Outcome::Stopped,
            received = listener.try_recv() => received,
        };
        match received {
            Ok(Some(notification)) => match RunStreamNotice::decode(notification.payload()) {
                Ok(notice) => hub.publish(Relayed::Notice(notice)),
                Err(error) => tracing::warn!(%error, "dropping an unreadable run stream notice"),
            },
            Ok(None) => {
                tracing::warn!("run stream listener lost its connection, reconnecting");
                hub.publish(Relayed::Gap);
            }
            Err(error) => {
                tracing::warn!(%error, "run stream listener lost its connection");
                return Outcome::Lost;
            }
        }
    }
}

async fn connect(pool: &PgPool) -> Result<PgListener, sqlx::Error> {
    let mut listener = PgListener::connect_with(pool).await?;
    listener.listen(RUN_STREAM_CHANNEL).await?;
    Ok(listener)
}
