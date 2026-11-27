use matos_common::CoreError;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const RUN_STREAM_CHANNEL: &str = "run_stream";
const TEXT_BUDGET_BYTES: usize = 6_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t")]
pub enum RunStreamNotice {
    #[serde(rename = "d")]
    Delta {
        run: Uuid,
        org: Uuid,
        after: i64,
        text: String,
    },
    #[serde(rename = "e")]
    Events { run: Uuid, org: Uuid, last: i64 },
}

impl RunStreamNotice {
    pub fn run(&self) -> Uuid {
        match self {
            Self::Delta { run, .. } | Self::Events { run, .. } => *run,
        }
    }

    pub fn organization(&self) -> Uuid {
        match self {
            Self::Delta { org, .. } | Self::Events { org, .. } => *org,
        }
    }

    pub fn encode(&self) -> Result<String, CoreError> {
        serde_json::to_string(self)
            .map_err(|error| CoreError::Internal(format!("encoding a run stream notice: {error}")))
    }

    pub fn decode(raw: &str) -> Result<Self, CoreError> {
        serde_json::from_str(raw)
            .map_err(|error| CoreError::Internal(format!("decoding a run stream notice: {error}")))
    }

    pub fn deltas(run: Uuid, org: Uuid, after: i64, text: &str) -> Vec<Self> {
        let mut notices = Vec::new();
        let mut chunk = String::new();
        let mut escaped = 0;
        for character in text.chars() {
            let width = escaped_width(character);
            if escaped + width > TEXT_BUDGET_BYTES && !chunk.is_empty() {
                notices.push(Self::Delta {
                    run,
                    org,
                    after,
                    text: std::mem::take(&mut chunk),
                });
                escaped = 0;
            }
            chunk.push(character);
            escaped += width;
        }
        if !chunk.is_empty() {
            notices.push(Self::Delta {
                run,
                org,
                after,
                text: chunk,
            });
        }
        notices
    }
}

fn escaped_width(character: char) -> usize {
    serde_json::to_string(&character).map_or(12, |quoted| quoted.len().saturating_sub(2))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids() -> (Uuid, Uuid) {
        (Uuid::from_u128(1), Uuid::from_u128(2))
    }

    #[test]
    fn a_notice_round_trips_through_its_payload() {
        let (run, org) = ids();
        for notice in [
            RunStreamNotice::Delta {
                run,
                org,
                after: 7,
                text: "héllo \"x\"\n".into(),
            },
            RunStreamNotice::Events { run, org, last: 9 },
        ] {
            let raw = notice.encode().unwrap();
            assert_eq!(RunStreamNotice::decode(&raw).unwrap(), notice);
        }
    }

    #[test]
    fn a_long_text_is_split_under_the_payload_limit_and_rebuilt_in_order() {
        let (run, org) = ids();
        let text: String = "é\n\"€𝄞".repeat(2_000);
        let notices = RunStreamNotice::deltas(run, org, 3, &text);
        assert!(notices.len() > 1);
        let mut rebuilt = String::new();
        for notice in &notices {
            assert!(notice.encode().unwrap().len() < 8_000);
            let RunStreamNotice::Delta { text, after, .. } = notice else {
                panic!("not a delta");
            };
            assert_eq!(*after, 3);
            rebuilt.push_str(text);
        }
        assert_eq!(rebuilt, text);
    }

    #[test]
    fn an_empty_text_gives_no_notice() {
        let (run, org) = ids();
        assert!(RunStreamNotice::deltas(run, org, 0, "").is_empty());
    }
}
