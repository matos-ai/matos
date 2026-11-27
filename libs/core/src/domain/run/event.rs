use chrono::{DateTime, Utc};
use matos_common::{CoreError, OrganizationId, RunId};

pub const TEXT_MAX_CHARS: usize = 65_536;
pub const TOOL_FIELD_MAX_CHARS: usize = 2_000;
pub const EVENTS_MAX_PER_RUN: i64 = 20_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunEventKind {
    AssistantText,
    ToolUse,
    ToolResult,
    System,
}

impl RunEventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AssistantText => "assistant_text",
            Self::ToolUse => "tool_use",
            Self::ToolResult => "tool_result",
            Self::System => "system",
        }
    }
}

impl TryFrom<&str> for RunEventKind {
    type Error = CoreError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "assistant_text" => Ok(Self::AssistantText),
            "tool_use" => Ok(Self::ToolUse),
            "tool_result" => Ok(Self::ToolResult),
            "system" => Ok(Self::System),
            other => Err(CoreError::Internal(format!(
                "unknown run event kind {other}"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum RunEventDraft {
    AssistantText {
        text: String,
        truncated: bool,
    },
    ToolUse {
        id: String,
        tool: String,
        input: String,
    },
    ToolResult {
        id: String,
        succeeded: bool,
        output: String,
    },
    System {
        text: String,
    },
}

impl RunEventDraft {
    pub fn kind(&self) -> RunEventKind {
        match self {
            Self::AssistantText { .. } => RunEventKind::AssistantText,
            Self::ToolUse { .. } => RunEventKind::ToolUse,
            Self::ToolResult { .. } => RunEventKind::ToolResult,
            Self::System { .. } => RunEventKind::System,
        }
    }

    pub fn payload(&self) -> serde_json::Value {
        match self {
            Self::AssistantText { text, truncated } => {
                serde_json::json!({ "text": text, "truncated": truncated })
            }
            Self::ToolUse { id, tool, input } => {
                serde_json::json!({ "id": id, "tool": tool, "input": input })
            }
            Self::ToolResult {
                id,
                succeeded,
                output,
            } => serde_json::json!({ "id": id, "succeeded": succeeded, "output": output }),
            Self::System { text } => serde_json::json!({ "text": text }),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SequencedRunEvent {
    pub seq: i64,
    pub draft: RunEventDraft,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RunEvent {
    pub run_id: RunId,
    pub organization_id: OrganizationId,
    pub seq: i64,
    pub kind: RunEventKind,
    pub payload: serde_json::Value,
    pub created_at: DateTime<Utc>,
}
