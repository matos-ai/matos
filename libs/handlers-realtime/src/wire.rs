use std::collections::HashSet;

use chrono::{DateTime, Utc};
use matos_core::domain::run::stream::RunStreamNotice;
use matos_events::EventEnvelope;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ClientMessage {
    Identify { token: String },
    Subscribe { run_id: Uuid },
    Unsubscribe { run_id: Uuid },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ServerMessage {
    Ready,
    Degraded,
    Resync,
    Subscribed {
        run_id: Uuid,
    },
    Refused {
        run_id: Uuid,
    },
    Delta {
        run_id: Uuid,
        after: i64,
        text: String,
    },
    RunEvents {
        run_id: Uuid,
        last: i64,
    },
    Event {
        kind: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<Uuid>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        card_id: Option<Uuid>,
        occurred_at: DateTime<Utc>,
    },
}

pub const MAX_SUBSCRIPTIONS: usize = 20;

#[derive(Debug, Default)]
pub struct Subscriptions {
    runs: HashSet<Uuid>,
}

impl Subscriptions {
    pub fn is_full(&self) -> bool {
        self.runs.len() >= MAX_SUBSCRIPTIONS
    }

    pub fn contains(&self, run: Uuid) -> bool {
        self.runs.contains(&run)
    }

    pub fn insert(&mut self, run: Uuid) -> bool {
        if self.runs.contains(&run) {
            return true;
        }
        if self.is_full() {
            return false;
        }
        self.runs.insert(run)
    }

    pub fn remove(&mut self, run: Uuid) {
        self.runs.remove(&run);
    }

    pub fn relay(&self, notice: &RunStreamNotice, organization_id: Uuid) -> Option<ServerMessage> {
        if notice.organization() != organization_id || !self.contains(notice.run()) {
            return None;
        }
        Some(match notice {
            RunStreamNotice::Delta {
                run, after, text, ..
            } => ServerMessage::Delta {
                run_id: *run,
                after: *after,
                text: text.clone(),
            },
            RunStreamNotice::Events { run, last, .. } => ServerMessage::RunEvents {
                run_id: *run,
                last: *last,
            },
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub organization_id: Uuid,
    pub message: ServerMessage,
}

impl Notice {
    pub fn from_envelope(envelope: &EventEnvelope) -> Self {
        Self {
            organization_id: envelope.organization_id,
            message: ServerMessage::Event {
                kind: envelope.kind.clone(),
                run_id: id_in(&envelope.payload, "run_id"),
                card_id: id_in(&envelope.payload, "card_id"),
                occurred_at: envelope.occurred_at,
            },
        }
    }

    pub fn concerns(&self, organization_id: Uuid) -> bool {
        self.organization_id == organization_id
    }
}

fn id_in(payload: &serde_json::Value, field: &str) -> Option<Uuid> {
    payload
        .get(field)
        .and_then(serde_json::Value::as_str)
        .and_then(|value| Uuid::parse_str(value).ok())
}

#[cfg(test)]
mod tests {
    use matos_events::Topic;
    use serde_json::json;

    use super::*;

    fn envelope(kind: &str, topic: Topic, payload: serde_json::Value) -> EventEnvelope {
        EventEnvelope {
            id: Uuid::now_v7(),
            kind: kind.into(),
            topic,
            organization_id: Uuid::now_v7(),
            partition_key: Uuid::now_v7(),
            occurred_at: Utc::now(),
            traceparent: None,
            payload,
        }
    }

    #[test]
    fn identify_deserializes() {
        let message: ClientMessage =
            serde_json::from_str(r#"{"op":"identify","token":"abc"}"#).unwrap();
        assert_eq!(
            message,
            ClientMessage::Identify {
                token: "abc".into()
            }
        );
    }

    #[test]
    fn subscribe_and_unsubscribe_deserialize() {
        let run = Uuid::now_v7();
        let subscribe = format!(r#"{{"op":"subscribe","run_id":"{run}"}}"#);
        let unsubscribe = format!(r#"{{"op":"unsubscribe","run_id":"{run}"}}"#);
        assert_eq!(
            serde_json::from_str::<ClientMessage>(&subscribe).unwrap(),
            ClientMessage::Subscribe { run_id: run }
        );
        assert_eq!(
            serde_json::from_str::<ClientMessage>(&unsubscribe).unwrap(),
            ClientMessage::Unsubscribe { run_id: run }
        );
        assert!(
            serde_json::from_str::<ClientMessage>(r#"{"op":"subscribe","run_id":"x"}"#).is_err()
        );
    }

    #[test]
    fn run_stream_messages_serialize_exactly() {
        let run = Uuid::now_v7();
        let id = run.to_string();
        let cases = [
            (
                ServerMessage::Subscribed { run_id: run },
                json!({"op":"subscribed","run_id":id}),
            ),
            (
                ServerMessage::Refused { run_id: run },
                json!({"op":"refused","run_id":id}),
            ),
            (
                ServerMessage::Delta {
                    run_id: run,
                    after: 7,
                    text: "hi".into(),
                },
                json!({"op":"delta","run_id":id,"after":7,"text":"hi"}),
            ),
            (
                ServerMessage::RunEvents {
                    run_id: run,
                    last: 9,
                },
                json!({"op":"run_events","run_id":id,"last":9}),
            ),
        ];
        for (message, expected) in cases {
            assert_eq!(serde_json::to_value(&message).unwrap(), expected);
        }
    }

    #[test]
    fn the_twenty_first_subscription_is_refused_but_repeats_are_idempotent() {
        let mut subscriptions = Subscriptions::default();
        let runs: Vec<Uuid> = (0..MAX_SUBSCRIPTIONS).map(|_| Uuid::now_v7()).collect();
        for run in &runs {
            assert!(subscriptions.insert(*run));
        }
        assert!(!subscriptions.insert(Uuid::now_v7()));
        assert!(subscriptions.insert(runs[0]));
        subscriptions.remove(runs[0]);
        assert!(subscriptions.insert(Uuid::now_v7()));
    }

    #[test]
    fn relay_needs_the_run_and_the_organization() {
        let (run, org) = (Uuid::now_v7(), Uuid::now_v7());
        let delta = RunStreamNotice::Delta {
            run,
            org,
            after: 1,
            text: "x".into(),
        };
        let events = RunStreamNotice::Events { run, org, last: 3 };
        let mut subscriptions = Subscriptions::default();
        assert!(subscriptions.relay(&delta, org).is_none());
        subscriptions.insert(run);
        assert_eq!(
            subscriptions.relay(&delta, org),
            Some(ServerMessage::Delta {
                run_id: run,
                after: 1,
                text: "x".into()
            })
        );
        assert_eq!(
            subscriptions.relay(&events, org),
            Some(ServerMessage::RunEvents {
                run_id: run,
                last: 3
            })
        );
        assert!(subscriptions.relay(&delta, Uuid::now_v7()).is_none());
        subscriptions.remove(run);
        assert!(subscriptions.relay(&events, org).is_none());
    }

    #[test]
    fn unknown_client_op_is_rejected() {
        assert!(serde_json::from_str::<ClientMessage>(r#"{"op":"nope"}"#).is_err());
        assert!(serde_json::from_str::<ClientMessage>("not json").is_err());
    }

    #[test]
    fn control_messages_serialize_to_their_op() {
        for (message, op) in [
            (ServerMessage::Ready, "ready"),
            (ServerMessage::Degraded, "degraded"),
            (ServerMessage::Resync, "resync"),
        ] {
            assert_eq!(serde_json::to_value(&message).unwrap(), json!({ "op": op }));
        }
    }

    #[test]
    fn event_carries_ids_and_nothing_else_of_the_payload() {
        let run_id = Uuid::now_v7();
        let card_id = Uuid::now_v7();
        let envelope = envelope(
            "card.moved",
            Topic::Cards,
            json!({"run_id": run_id, "card_id": card_id, "title": "secret", "to": "done"}),
        );
        let notice = Notice::from_envelope(&envelope);
        let value = serde_json::to_value(&notice.message).unwrap();

        assert_eq!(value["op"], "event");
        assert_eq!(value["kind"], "card.moved");
        assert_eq!(value["run_id"], run_id.to_string());
        assert_eq!(value["card_id"], card_id.to_string());
        let mut keys: Vec<_> = value.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, ["card_id", "kind", "occurred_at", "op", "run_id"]);
    }

    #[test]
    fn missing_ids_are_omitted() {
        let run_id = Uuid::now_v7();
        let envelope = envelope("run.started", Topic::Runs, json!({"run_id": run_id}));
        let value = serde_json::to_value(Notice::from_envelope(&envelope).message).unwrap();
        assert!(value.get("card_id").is_none());
        assert_eq!(value["run_id"], run_id.to_string());
    }

    #[test]
    fn malformed_ids_are_dropped() {
        let envelope = envelope("run.started", Topic::Runs, json!({"run_id": "nope"}));
        let value = serde_json::to_value(Notice::from_envelope(&envelope).message).unwrap();
        assert!(value.get("run_id").is_none());
    }

    #[test]
    fn a_notice_concerns_only_its_organization() {
        let envelope = envelope("run.queued", Topic::Runs, json!({}));
        let notice = Notice::from_envelope(&envelope);
        assert!(notice.concerns(envelope.organization_id));
        assert!(!notice.concerns(Uuid::now_v7()));
    }
}
