#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use matos_core::domain::run::stream::RunStreamNotice;
use matos_handlers_realtime::{
    Authenticate, Denied, Gateway, Hub, Notice, Relayed, ServerMessage, Session, StreamHub, Timing,
    router,
};
use serde_json::{Value, json};
use tokio::{net::TcpStream, time::timeout};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};
use uuid::Uuid;

#[derive(Clone)]
struct FakeAuth;

/// `<organization uuid>|<subject>[|<ttl in ms>]`, anything else is refused.
impl Authenticate for FakeAuth {
    async fn authenticate(&self, token: &str) -> Result<Session, Denied> {
        let mut parts = token.split('|');
        let organization_id = parts
            .next()
            .and_then(|part| Uuid::parse_str(part).ok())
            .ok_or(Denied("unauthorized"))?;
        let subject = parts.next().ok_or(Denied("unauthorized"))?.to_owned();
        let expires_at = parts
            .next()
            .and_then(|ms| ms.parse::<i64>().ok())
            .map(|ms| Utc::now() + chrono::Duration::milliseconds(ms));
        Ok(Session {
            actor: None,
            is_admin: false,
            subject,
            organization_id,
            expires_at,
        })
    }

    async fn authorize_run(&self, session: &Session, _run: Uuid) -> bool {
        session.subject != "mallory"
    }
}

type Client = WebSocketStream<MaybeTlsStream<TcpStream>>;

const WAIT: Duration = Duration::from_secs(5);

fn timing() -> Timing {
    Timing {
        identify_timeout: Duration::from_millis(300),
        heartbeat: Duration::from_secs(30),
        send_timeout: Duration::from_secs(5),
    }
}

async fn start(hub: &Hub) -> String {
    start_with(hub, &StreamHub::new(1024)).await
}

async fn start_with(hub: &Hub, stream: &StreamHub) -> String {
    let gateway = Gateway::new(FakeAuth, hub.clone(), stream.clone()).with_timing(timing());
    let app = router::<_, ()>(gateway);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("ws://{addr}/api/v1/realtime")
}

async fn connect(url: &str) -> Client {
    connect_async(url).await.unwrap().0
}

async fn identify(client: &mut Client, token: &str) {
    let frame = json!({"op": "identify", "token": token}).to_string();
    client.send(Message::Text(frame.into())).await.unwrap();
}

async fn next_json(client: &mut Client) -> Value {
    loop {
        match timeout(WAIT, client.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
        {
            Message::Text(text) => return serde_json::from_str(&text).unwrap(),
            Message::Close(frame) => panic!("closed: {frame:?}"),
            _ => {}
        }
    }
}

async fn next_close(client: &mut Client) -> (u16, String) {
    loop {
        match timeout(WAIT, client.next()).await.unwrap() {
            Some(Ok(Message::Close(Some(frame)))) => {
                return (u16::from(frame.code), frame.reason.to_string());
            }
            Some(Ok(_)) => {}
            other => panic!("expected a close frame, got {other:?}"),
        }
    }
}

fn event(organization_id: Uuid, kind: &str, run_id: Uuid) -> Notice {
    Notice {
        organization_id,
        message: ServerMessage::Event {
            kind: kind.into(),
            run_id: Some(run_id),
            card_id: None,
            occurred_at: Utc::now(),
        },
    }
}

fn healthy_hub(capacity: usize) -> Hub {
    let hub = Hub::new(capacity);
    hub.set_degraded(false);
    hub
}

#[tokio::test]
async fn no_identify_closes_with_4001() {
    let hub = healthy_hub(8);
    let mut client = connect(&start(&hub).await).await;
    let (code, reason) = next_close(&mut client).await;
    assert_eq!(code, 4001);
    assert_eq!(reason, "identify timeout");
}

#[tokio::test]
async fn a_first_frame_other_than_identify_closes_with_4001() {
    let hub = healthy_hub(8);
    let mut client = connect(&start(&hub).await).await;
    client
        .send(Message::Text(json!({"op": "hello"}).to_string().into()))
        .await
        .unwrap();
    assert_eq!(next_close(&mut client).await.0, 4001);
}

#[tokio::test]
async fn a_refused_token_closes_with_4001() {
    let hub = healthy_hub(8);
    let mut client = connect(&start(&hub).await).await;
    identify(&mut client, "garbage").await;
    let (code, reason) = next_close(&mut client).await;
    assert_eq!(code, 4001);
    assert_eq!(reason, "unauthorized");
}

#[tokio::test]
async fn identify_answers_ready() {
    let hub = healthy_hub(8);
    let mut client = connect(&start(&hub).await).await;
    identify(&mut client, &format!("{}|alice", Uuid::now_v7())).await;
    assert_eq!(next_json(&mut client).await, json!({"op": "ready"}));
}

#[tokio::test]
async fn events_of_another_organization_are_not_delivered() {
    let hub = healthy_hub(8);
    let mine = Uuid::now_v7();
    let run = Uuid::now_v7();
    let mut client = connect(&start(&hub).await).await;
    identify(&mut client, &format!("{mine}|alice")).await;
    assert_eq!(next_json(&mut client).await["op"], "ready");

    hub.publish(event(Uuid::now_v7(), "run.queued", Uuid::now_v7()));
    hub.publish(event(mine, "run.started", run));

    let received = next_json(&mut client).await;
    assert_eq!(received["op"], "event");
    assert_eq!(received["kind"], "run.started");
    assert_eq!(received["run_id"], run.to_string());
}

#[tokio::test]
async fn a_lagging_socket_gets_a_resync_and_stays_connected() {
    let hub = healthy_hub(2);
    let mine = Uuid::now_v7();
    let mut client = connect(&start(&hub).await).await;
    identify(&mut client, &format!("{mine}|alice")).await;
    assert_eq!(next_json(&mut client).await["op"], "ready");
    tokio::time::sleep(Duration::from_millis(100)).await;

    for _ in 0..50 {
        hub.publish(event(mine, "run.step_finished", Uuid::now_v7()));
    }

    let mut saw_resync = false;
    for _ in 0..60 {
        if next_json(&mut client).await["op"] == "resync" {
            saw_resync = true;
            break;
        }
    }
    assert!(saw_resync);
    hub.publish(event(mine, "run.finished", Uuid::now_v7()));
    let mut saw_after = false;
    for _ in 0..60 {
        if next_json(&mut client).await["kind"] == "run.finished" {
            saw_after = true;
            break;
        }
    }
    assert!(saw_after);
}

#[tokio::test]
async fn a_degraded_hub_is_announced_after_ready_and_recovery_resyncs() {
    let hub = Hub::new(8);
    let mut client = connect(&start(&hub).await).await;
    identify(&mut client, &format!("{}|alice", Uuid::now_v7())).await;
    assert_eq!(next_json(&mut client).await["op"], "ready");
    assert_eq!(next_json(&mut client).await["op"], "degraded");

    hub.set_degraded(false);
    assert_eq!(next_json(&mut client).await["op"], "ready");
    assert_eq!(next_json(&mut client).await["op"], "resync");

    hub.set_degraded(true);
    assert_eq!(next_json(&mut client).await["op"], "degraded");
}

#[tokio::test]
async fn reidentify_with_the_same_subject_renews_the_session() {
    let hub = healthy_hub(8);
    let mine = Uuid::now_v7();
    let mut client = connect(&start(&hub).await).await;
    identify(&mut client, &format!("{mine}|alice|300")).await;
    assert_eq!(next_json(&mut client).await["op"], "ready");
    identify(&mut client, &format!("{mine}|alice")).await;
    assert_eq!(next_json(&mut client).await["op"], "ready");

    tokio::time::sleep(Duration::from_millis(500)).await;
    hub.publish(event(mine, "run.finished", Uuid::now_v7()));
    assert_eq!(next_json(&mut client).await["kind"], "run.finished");
}

#[tokio::test]
async fn reidentify_with_another_subject_closes_with_4003() {
    let hub = healthy_hub(8);
    let mine = Uuid::now_v7();
    let mut client = connect(&start(&hub).await).await;
    identify(&mut client, &format!("{mine}|alice")).await;
    assert_eq!(next_json(&mut client).await["op"], "ready");
    identify(&mut client, &format!("{mine}|bob")).await;
    assert_eq!(next_close(&mut client).await.0, 4003);
}

#[tokio::test]
async fn an_expired_token_closes_with_4001() {
    let hub = healthy_hub(8);
    let mut client = connect(&start(&hub).await).await;
    identify(&mut client, &format!("{}|alice|200", Uuid::now_v7())).await;
    assert_eq!(next_json(&mut client).await["op"], "ready");
    let (code, reason) = next_close(&mut client).await;
    assert_eq!(code, 4001);
    assert_eq!(reason, "token expired");
}

async fn send(client: &mut Client, value: Value) {
    client
        .send(Message::Text(value.to_string().into()))
        .await
        .unwrap();
}

async fn ready_client(stream: &StreamHub, org: Uuid, subject: &str) -> Client {
    let hub = healthy_hub(8);
    let mut client = connect(&start_with(&hub, stream).await).await;
    identify(&mut client, &format!("{org}|{subject}")).await;
    assert_eq!(next_json(&mut client).await["op"], "ready");
    client
}

fn delta(run: Uuid, org: Uuid, after: i64, text: &str) -> Relayed {
    Relayed::Notice(RunStreamNotice::Delta {
        run,
        org,
        after,
        text: text.into(),
    })
}

fn events(run: Uuid, org: Uuid, last: i64) -> Relayed {
    Relayed::Notice(RunStreamNotice::Events { run, org, last })
}

async fn subscribe(client: &mut Client, run: Uuid) -> Value {
    send(client, json!({"op": "subscribe", "run_id": run})).await;
    next_json(client).await
}

#[tokio::test]
async fn subscribe_answers_subscribed_and_is_idempotent() {
    let stream = StreamHub::new(16);
    let (org, run) = (Uuid::now_v7(), Uuid::now_v7());
    let mut client = ready_client(&stream, org, "alice").await;
    let expected = json!({"op": "subscribed", "run_id": run.to_string()});
    assert_eq!(subscribe(&mut client, run).await, expected);
    assert_eq!(subscribe(&mut client, run).await, expected);
}

#[tokio::test]
async fn an_unauthorized_run_is_refused_and_never_relayed() {
    let stream = StreamHub::new(16);
    let (org, run) = (Uuid::now_v7(), Uuid::now_v7());
    let mut client = ready_client(&stream, org, "mallory").await;
    assert_eq!(
        subscribe(&mut client, run).await,
        json!({"op": "refused", "run_id": run.to_string()})
    );
    stream.publish(delta(run, org, 1, "secret"));
    assert!(
        timeout(Duration::from_millis(300), client.next())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn the_twenty_first_subscription_is_refused() {
    let stream = StreamHub::new(16);
    let org = Uuid::now_v7();
    let mut client = ready_client(&stream, org, "alice").await;
    for _ in 0..20 {
        assert_eq!(
            subscribe(&mut client, Uuid::now_v7()).await["op"],
            "subscribed"
        );
    }
    let extra = Uuid::now_v7();
    assert_eq!(
        subscribe(&mut client, extra).await,
        json!({"op": "refused", "run_id": extra.to_string()})
    );
}

#[tokio::test]
async fn only_subscribed_runs_of_my_organization_are_forwarded_in_order() {
    let stream = StreamHub::new(16);
    let (org, run, other_run) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    let mut client = ready_client(&stream, org, "alice").await;
    subscribe(&mut client, run).await;

    stream.publish(delta(other_run, org, 1, "not subscribed"));
    stream.publish(delta(run, Uuid::now_v7(), 1, "other organization"));
    stream.publish(delta(run, org, 4, "hel"));
    stream.publish(events(run, org, 5));

    assert_eq!(
        next_json(&mut client).await,
        json!({"op": "delta", "run_id": run.to_string(), "after": 4, "text": "hel"})
    );
    assert_eq!(
        next_json(&mut client).await,
        json!({"op": "run_events", "run_id": run.to_string(), "last": 5})
    );
}

#[tokio::test]
async fn nothing_is_forwarded_after_unsubscribe() {
    let stream = StreamHub::new(16);
    let (org, run) = (Uuid::now_v7(), Uuid::now_v7());
    let mut client = ready_client(&stream, org, "alice").await;
    subscribe(&mut client, run).await;
    send(&mut client, json!({"op": "unsubscribe", "run_id": run})).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    stream.publish(delta(run, org, 1, "late"));
    assert!(
        timeout(Duration::from_millis(300), client.next())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_stream_gap_and_a_stream_lag_both_resync() {
    let stream = StreamHub::new(2);
    let (org, run) = (Uuid::now_v7(), Uuid::now_v7());
    let mut client = ready_client(&stream, org, "alice").await;
    subscribe(&mut client, run).await;

    stream.publish(Relayed::Gap);
    assert_eq!(next_json(&mut client).await, json!({"op": "resync"}));

    for index in 0..50 {
        stream.publish(delta(run, org, index, "x"));
    }
    let mut saw_resync = false;
    for _ in 0..60 {
        if next_json(&mut client).await["op"] == "resync" {
            saw_resync = true;
            break;
        }
    }
    assert!(saw_resync);
}

#[tokio::test]
async fn subscribe_before_identify_closes_like_any_other_first_frame() {
    let hub = healthy_hub(8);
    let mut client = connect(&start(&hub).await).await;
    send(
        &mut client,
        json!({"op": "subscribe", "run_id": Uuid::now_v7()}),
    )
    .await;
    assert_eq!(next_close(&mut client).await.0, 4001);
}

async fn database() -> Option<sqlx::PgPool> {
    match std::env::var("DATABASE_URL") {
        Ok(url) => Some(sqlx::PgPool::connect(&url).await.unwrap()),
        Err(_) => {
            assert!(
                std::env::var("REQUIRE_DATABASE_URL").is_err(),
                "DATABASE_URL is required"
            );
            None
        }
    }
}

#[tokio::test]
async fn postgres_notifications_reach_a_subscribed_socket_in_order() {
    let Some(pool) = database().await else {
        return;
    };
    let stream = StreamHub::new(1024);
    let listener = matos_handlers_realtime::run_stream::spawn(pool.clone(), stream.clone());
    let (org, run) = (Uuid::now_v7(), Uuid::now_v7());
    let mut client = ready_client(&stream, org, "alice").await;
    subscribe(&mut client, run).await;

    let probe = RunStreamNotice::Events { run, org, last: -1 };
    let mut live = false;
    for _ in 0..50 {
        notify(&pool, &probe).await;
        if let Ok(Some(Ok(Message::Text(_)))) =
            timeout(Duration::from_millis(100), client.next()).await
        {
            live = true;
            break;
        }
    }
    assert!(live, "the listener never connected");
    tokio::time::sleep(Duration::from_millis(300)).await;
    while timeout(Duration::from_millis(100), client.next())
        .await
        .is_ok()
    {}

    notify(
        &pool,
        &RunStreamNotice::Delta {
            run,
            org,
            after: 7,
            text: "hello \"world\"".into(),
        },
    )
    .await;
    notify(&pool, &RunStreamNotice::Events { run, org, last: 8 }).await;

    let first = next_json(&mut client).await;
    let second = next_json(&mut client).await;
    println!("{first}\n{second}");
    assert_eq!(
        first,
        json!({"op": "delta", "run_id": run.to_string(), "after": 7, "text": "hello \"world\""})
    );
    assert_eq!(
        second,
        json!({"op": "run_events", "run_id": run.to_string(), "last": 8})
    );
    listener.abort();
}

async fn notify(pool: &sqlx::PgPool, notice: &RunStreamNotice) {
    sqlx::query("SELECT pg_notify($1, $2)")
        .bind(matos_core::domain::run::stream::RUN_STREAM_CHANNEL)
        .bind(notice.encode().unwrap())
        .execute(pool)
        .await
        .unwrap();
}
