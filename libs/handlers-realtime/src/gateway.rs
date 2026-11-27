use std::{future::pending, pin::Pin, time::Duration};

use axum::{
    Router,
    extract::{
        State,
        ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade},
    },
    response::Response,
    routing::get,
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use tokio::{
    sync::broadcast::error::RecvError,
    time::{Instant, Sleep, interval_at, sleep, timeout},
};
use tracing::{Instrument, field};

use crate::{
    auth::{Authenticate, Session},
    hub::Hub,
    run_stream::{Relayed, StreamHub},
    wire::{ClientMessage, ServerMessage, Subscriptions},
};

pub const PATH: &str = "/api/v1/realtime";

pub const CLOSE_UNAUTHORIZED: u16 = 4001;
pub const CLOSE_SUBJECT_CHANGED: u16 = 4003;
const CLOSE_HEARTBEAT: u16 = 1011;
const AUTHORIZE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy)]
pub struct Timing {
    pub identify_timeout: Duration,
    pub heartbeat: Duration,
    pub send_timeout: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            identify_timeout: Duration::from_secs(10),
            heartbeat: Duration::from_secs(30),
            send_timeout: Duration::from_secs(10),
        }
    }
}

#[derive(Clone)]
pub struct Gateway<A> {
    auth: A,
    hub: Hub,
    stream: StreamHub,
    timing: Timing,
}

impl<A: Authenticate> Gateway<A> {
    pub fn new(auth: A, hub: Hub, stream: StreamHub) -> Self {
        Self {
            auth,
            hub,
            stream,
            timing: Timing::default(),
        }
    }

    pub fn with_timing(mut self, timing: Timing) -> Self {
        self.timing = timing;
        self
    }
}

/// Not behind the bearer middleware: browsers cannot set `Authorization` on a
/// WebSocket, so the first frame (`identify`) authenticates the connection.
pub fn router<A, S>(gateway: Gateway<A>) -> Router<S>
where
    A: Authenticate,
    S: Clone + Send + Sync + 'static,
{
    Router::new()
        .route(PATH, get(upgrade::<A>))
        .with_state(gateway)
}

async fn upgrade<A: Authenticate>(
    State(gateway): State<Gateway<A>>,
    ws: WebSocketUpgrade,
) -> Response {
    ws.on_upgrade(move |socket| serve(socket, gateway))
}

enum Exit {
    Gone,
    Close(u16, &'static str),
}

async fn serve<A: Authenticate>(mut socket: WebSocket, gateway: Gateway<A>) {
    let span = tracing::info_span!("realtime.connection", organization_id = field::Empty);
    let exit = run(&mut socket, &gateway, &span)
        .instrument(span.clone())
        .await;
    if let Exit::Close(code, reason) = exit {
        tracing::debug!(parent: &span, code, reason, "realtime connection closed by the server");
        let frame = CloseFrame {
            code,
            reason: reason.into(),
        };
        let _ = timeout(
            gateway.timing.send_timeout,
            socket.send(Message::Close(Some(frame))),
        )
        .await;
    }
}

enum Identified {
    Token(String),
    Invalid,
    Gone,
}

async fn read_identify(socket: &mut WebSocket) -> Identified {
    loop {
        match socket.recv().await {
            Some(Ok(Message::Text(text))) => {
                return match serde_json::from_str::<ClientMessage>(&text) {
                    Ok(ClientMessage::Identify { token }) => Identified::Token(token),
                    Ok(_) | Err(_) => Identified::Invalid,
                };
            }
            Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return Identified::Gone,
            Some(Ok(_)) => {}
        }
    }
}

async fn run<A: Authenticate>(
    socket: &mut WebSocket,
    gateway: &Gateway<A>,
    span: &tracing::Span,
) -> Exit {
    let timing = gateway.timing;
    let token = match timeout(timing.identify_timeout, read_identify(socket)).await {
        Ok(Identified::Token(token)) => token,
        Ok(Identified::Invalid) => return Exit::Close(CLOSE_UNAUTHORIZED, "identify expected"),
        Ok(Identified::Gone) => return Exit::Gone,
        Err(_) => return Exit::Close(CLOSE_UNAUTHORIZED, "identify timeout"),
    };
    let mut session = match gateway.auth.authenticate(&token).await {
        Ok(session) => session,
        Err(denied) => return Exit::Close(CLOSE_UNAUTHORIZED, denied.0),
    };
    span.record("organization_id", field::display(session.organization_id));

    let mut notices = gateway.hub.subscribe();
    let mut stream = gateway.stream.subscribe();
    let mut subscriptions = Subscriptions::default();
    let mut status = gateway.hub.watch_degraded();
    let mut degraded = *status.borrow_and_update();

    if !send_json(socket, &ServerMessage::Ready, timing.send_timeout).await {
        return Exit::Gone;
    }
    if degraded && !send_json(socket, &ServerMessage::Degraded, timing.send_timeout).await {
        return Exit::Gone;
    }

    let mut expiry = deadline(session.expires_at);
    let mut beat = interval_at(Instant::now() + timing.heartbeat, timing.heartbeat);
    let mut awaiting_pong = false;

    loop {
        tokio::select! {
            notice = notices.recv() => {
                let message = match notice {
                    Ok(notice) if notice.concerns(session.organization_id) => notice.message.clone(),
                    Ok(_) => continue,
                    Err(RecvError::Lagged(missed)) => {
                        tracing::warn!(missed, "realtime socket lagged, asking for a resync");
                        ServerMessage::Resync
                    }
                    Err(RecvError::Closed) => return Exit::Gone,
                };
                if !send_json(socket, &message, timing.send_timeout).await {
                    return Exit::Gone;
                }
            }
            relayed = stream.recv() => {
                let message = match relayed {
                    Ok(relayed) => match &*relayed {
                        Relayed::Notice(notice) => {
                            match subscriptions.relay(notice, session.organization_id) {
                                Some(message) => message,
                                None => continue,
                            }
                        }
                        Relayed::Gap => ServerMessage::Resync,
                    },
                    Err(RecvError::Lagged(missed)) => {
                        tracing::warn!(missed, "run stream socket lagged, asking for a resync");
                        ServerMessage::Resync
                    }
                    Err(RecvError::Closed) => return Exit::Gone,
                };
                if !send_json(socket, &message, timing.send_timeout).await {
                    return Exit::Gone;
                }
            }
            changed = status.changed() => {
                if changed.is_err() {
                    return Exit::Gone;
                }
                let now = *status.borrow_and_update();
                if now == degraded {
                    continue;
                }
                degraded = now;
                let sent = if now {
                    send_json(socket, &ServerMessage::Degraded, timing.send_timeout).await
                } else {
                    send_json(socket, &ServerMessage::Ready, timing.send_timeout).await
                        && send_json(socket, &ServerMessage::Resync, timing.send_timeout).await
                };
                if !sent {
                    return Exit::Gone;
                }
            }
            _ = beat.tick() => {
                if awaiting_pong {
                    return Exit::Close(CLOSE_HEARTBEAT, "heartbeat timeout");
                }
                awaiting_pong = true;
                let ping = Message::Ping(Vec::new().into());
                if !matches!(timeout(timing.send_timeout, socket.send(ping)).await, Ok(Ok(()))) {
                    return Exit::Gone;
                }
            }
            () = expired(&mut expiry) => {
                return Exit::Close(CLOSE_UNAUTHORIZED, "token expired");
            }
            frame = socket.recv() => {
                awaiting_pong = false;
                match frame {
                    Some(Ok(Message::Text(text))) => {
                        let token = match serde_json::from_str::<ClientMessage>(&text) {
                            Ok(ClientMessage::Identify { token }) => token,
                            Ok(ClientMessage::Subscribe { run_id }) => {
                                let reply = subscribe(gateway, &session, &mut subscriptions, run_id).await;
                                if !send_json(socket, &reply, timing.send_timeout).await {
                                    return Exit::Gone;
                                }
                                continue;
                            }
                            Ok(ClientMessage::Unsubscribe { run_id }) => {
                                subscriptions.remove(run_id);
                                continue;
                            }
                            Err(_) => continue,
                        };
                        match reidentify(gateway, &session, &token).await {
                            Ok(renewed) => {
                                expiry = deadline(renewed.expires_at);
                                session = renewed;
                                if !send_json(socket, &ServerMessage::Ready, timing.send_timeout).await {
                                    return Exit::Gone;
                                }
                            }
                            Err(exit) => return exit,
                        }
                    }
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return Exit::Gone,
                    Some(Ok(_)) => {}
                }
            }
        }
    }
}

async fn subscribe<A: Authenticate>(
    gateway: &Gateway<A>,
    session: &Session,
    subscriptions: &mut Subscriptions,
    run_id: uuid::Uuid,
) -> ServerMessage {
    if subscriptions.contains(run_id) {
        return ServerMessage::Subscribed { run_id };
    }
    if subscriptions.is_full() {
        return ServerMessage::Refused { run_id };
    }
    let allowed = matches!(
        timeout(
            AUTHORIZE_TIMEOUT,
            gateway.auth.authorize_run(session, run_id)
        )
        .await,
        Ok(true)
    );
    if allowed && subscriptions.insert(run_id) {
        ServerMessage::Subscribed { run_id }
    } else {
        ServerMessage::Refused { run_id }
    }
}

async fn reidentify<A: Authenticate>(
    gateway: &Gateway<A>,
    current: &Session,
    token: &str,
) -> Result<Session, Exit> {
    let renewed = gateway
        .auth
        .authenticate(token)
        .await
        .map_err(|denied| Exit::Close(CLOSE_UNAUTHORIZED, denied.0))?;
    if renewed.subject != current.subject || renewed.organization_id != current.organization_id {
        return Err(Exit::Close(CLOSE_SUBJECT_CHANGED, "subject changed"));
    }
    Ok(renewed)
}

fn deadline(expires_at: Option<DateTime<Utc>>) -> Option<Pin<Box<Sleep>>> {
    expires_at.map(|at| {
        let remaining = (at - Utc::now()).to_std().unwrap_or(Duration::ZERO);
        Box::pin(sleep(remaining))
    })
}

async fn expired(expiry: &mut Option<Pin<Box<Sleep>>>) {
    match expiry {
        Some(sleep) => sleep.as_mut().await,
        None => pending().await,
    }
}

async fn send_json(socket: &mut WebSocket, message: &impl Serialize, limit: Duration) -> bool {
    let Ok(text) = serde_json::to_string(message) else {
        return false;
    };
    matches!(
        timeout(limit, socket.send(Message::Text(text.into()))).await,
        Ok(Ok(()))
    )
}
