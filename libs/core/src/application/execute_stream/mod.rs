mod antigravity;
mod claude_code;
mod codex;
mod vibe;

use std::{collections::HashMap, time::Instant};

use serde_json::Value;

use crate::domain::{
    agent::ToolCall,
    execution::{
        QualityStatus, TaskLine, TaskMarker,
        harness::{AgentEvent, AgentFinish, Harness},
        report::{ReportAssembler, ReportOutcome},
    },
    run::{
        event::{
            EVENTS_MAX_PER_RUN, RunEventDraft, SequencedRunEvent, TEXT_MAX_CHARS,
            TOOL_FIELD_MAX_CHARS,
        },
        step::CachedUsage,
    },
};

pub(super) const REDACTED: &str = "***";
const INPUT_MAX_CHARS: usize = 200;
const OUTPUT_MAX_CHARS: usize = 300;
const SUMMARY_MAX_CHARS: usize = 4_000;
pub(super) const TOOL_CALLS_MAX: usize = 500;
const CONTRIBUTIONS_MAX: usize = 16;

#[derive(Debug, Clone)]
pub(super) struct Redactor {
    secret: String,
}

impl Redactor {
    pub(super) fn new(secret: &str) -> Self {
        Self {
            secret: secret.to_owned(),
        }
    }

    pub(super) fn apply(&self, text: &str) -> String {
        if self.secret.is_empty() {
            return text.to_owned();
        }
        text.replace(&self.secret, REDACTED)
    }

    fn partial_tail_len(&self, text: &str) -> usize {
        let mut starts = text
            .char_indices()
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        starts.reverse();
        starts
            .into_iter()
            .map(|start| text.len() - start)
            .filter(|length| *length < self.secret.len())
            .find(|length| self.secret.starts_with(&text[text.len() - length..]))
            .unwrap_or(0)
    }
}

pub(super) fn truncate_chars(text: &str, max_chars: usize) -> String {
    match text.char_indices().nth(max_chars) {
        Some((index, _)) => text[..index].to_owned(),
        None => text.to_owned(),
    }
}

struct PendingCall {
    tool: String,
    input: String,
    since: Instant,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct Digest {
    pub tool_calls: Vec<ToolCall>,
    pub dropped_tool_calls: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub summary: Option<String>,
    pub stopped: Option<String>,
    pub marker: Option<TaskMarker>,
    pub reported_cost_usd: Option<f64>,
    pub turns: Option<u64>,
}

impl Digest {
    pub(super) fn cached_usage(&self) -> CachedUsage {
        CachedUsage {
            fresh_input: self
                .input_tokens
                .saturating_sub(self.cache_read_tokens)
                .saturating_sub(self.cache_creation_tokens),
            cache_read: self.cache_read_tokens,
            cache_creation: self.cache_creation_tokens,
            output: self.output_tokens,
        }
    }

    /// The cost the harness reports, for a model without a known price.
    pub(super) fn reported_cost_micros(&self) -> i64 {
        self.reported_cost_usd
            .filter(|usd| usd.is_finite() && *usd > 0.0)
            .map_or(0, |usd| (usd * 1_000_000.0).round() as i64)
    }

    fn keep(&mut self, call: ToolCall) {
        if self.tool_calls.len() < TOOL_CALLS_MAX {
            self.tool_calls.push(call);
        } else {
            self.dropped_tool_calls += 1;
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StreamDelta {
    pub after: i64,
    pub text: String,
}

pub(super) struct StreamReader {
    harness: Harness,
    parser: AgentParser,
    redactor: Redactor,
    digest: Digest,
    report: ReportAssembler,
    plan: ReportAssembler,
    contributions: HashMap<Option<String>, ReportAssembler>,
    blocked: Option<Vec<String>>,
    qualities: HashMap<Option<String>, Quality>,
    pushed: Vec<PushedBranch>,
    companions: Option<CompanionOutcome>,
    extra_outcome: Option<CompanionOutcome>,
    pending: HashMap<String, PendingCall>,
    order: Vec<String>,
    events: Vec<SequencedRunEvent>,
    emitted: i64,
    deltas: Vec<StreamDelta>,
    held: Option<StreamDelta>,
    dirty: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Quality {
    pub(super) lint: QualityStatus,
    pub(super) excerpt: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PushedBranch {
    pub(super) repository: Option<String>,
    pub(super) sha: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct CompanionOutcome {
    pub(super) cloned: Vec<String>,
    pub(super) failed: Vec<String>,
}

impl StreamReader {
    pub(super) fn new(harness: Harness, redactor: Redactor) -> Self {
        Self {
            harness,
            parser: AgentParser::new(harness),
            redactor,
            digest: Digest::default(),
            report: ReportAssembler::default(),
            plan: ReportAssembler::default(),
            contributions: HashMap::new(),
            blocked: None,
            qualities: HashMap::new(),
            pushed: Vec::new(),
            companions: None,
            extra_outcome: None,
            pending: HashMap::new(),
            order: Vec::new(),
            events: Vec::new(),
            emitted: 0,
            deltas: Vec::new(),
            held: None,
            dirty: false,
        }
    }

    pub(super) fn reset(&mut self) {
        self.parser = AgentParser::new(self.harness);
        self.digest = Digest::default();
        self.report = ReportAssembler::default();
        self.plan = ReportAssembler::default();
        self.contributions.clear();
        self.blocked = None;
        self.qualities.clear();
        self.pushed.clear();
        self.companions = None;
        self.extra_outcome = None;
        self.pending.clear();
        self.order.clear();
        self.events.clear();
        self.emitted = 0;
        self.deltas.clear();
        self.held = None;
        self.dirty = true;
    }

    pub(super) fn take_deltas(&mut self) -> Vec<StreamDelta> {
        let mut taken: Vec<StreamDelta> = Vec::new();
        let pending = self
            .held
            .take()
            .into_iter()
            .chain(std::mem::take(&mut self.deltas));
        for delta in pending {
            match taken.last_mut() {
                Some(last) if last.after == delta.after => last.text.push_str(&delta.text),
                _ => taken.push(delta),
            }
        }
        for delta in &mut taken {
            delta.text = self.redactor.apply(&delta.text);
        }
        if let Some(last) = taken.last_mut() {
            let keep = self.redactor.partial_tail_len(&last.text);
            if keep > 0 {
                let cut = last.text.len() - keep;
                self.held = Some(StreamDelta {
                    after: last.after,
                    text: last.text.split_off(cut),
                });
            }
        }
        taken.retain(|delta| !delta.text.is_empty());
        taken
    }

    pub(super) fn take_events(&mut self) -> Vec<SequencedRunEvent> {
        std::mem::take(&mut self.events)
    }

    fn emit(&mut self, draft: RunEventDraft) {
        if self.emitted >= EVENTS_MAX_PER_RUN {
            return;
        }
        self.emitted += 1;
        self.events.push(SequencedRunEvent {
            seq: self.emitted,
            draft,
        });
    }

    fn tool_field(&self, text: &str) -> String {
        truncate_chars(&self.redactor.apply(text), TOOL_FIELD_MAX_CHARS)
    }

    pub(super) fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    pub(super) fn snapshot(&self) -> &Digest {
        &self.digest
    }

    pub(super) fn marker(&self) -> Option<&TaskMarker> {
        self.digest.marker.as_ref()
    }

    /// Why the checks refused the diff, redacted; `None` when they did not.
    pub(super) fn blocked(&self) -> Option<Vec<String>> {
        self.blocked.as_ref().map(|reasons| {
            reasons
                .iter()
                .map(|reason| self.redactor.apply(reason))
                .collect()
        })
    }

    /// Whether the harness said it stopped on its turn limit. Only the
    /// harnesses the executor gives a turn limit can say so.
    pub(super) fn hit_turn_limit(&self) -> bool {
        self.digest
            .stopped
            .as_deref()
            .is_some_and(|stopped| match self.harness {
                Harness::ClaudeCode => claude_code::hit_turn_limit(stopped),
                Harness::MistralVibe => vibe::hit_turn_limit(stopped),
                Harness::Codex | Harness::Antigravity => false,
            })
    }

    /// The branches the task pushed, in the order it reported them.
    pub(super) fn pushed(&self) -> &[PushedBranch] {
        &self.pushed
    }

    /// The linter's result the task reported for a repository, redacted. The
    /// primary repository is the one a result without a name belongs to.
    pub(super) fn quality(&self, repository: &str, primary: bool) -> Option<Quality> {
        let key = Some(repository.to_owned());
        self.qualities
            .get(&key)
            .or_else(|| primary.then(|| self.qualities.get(&None)).flatten())
            .map(|quality| Quality {
                lint: quality.lint,
                excerpt: quality
                    .excerpt
                    .as_deref()
                    .map(|excerpt| self.redactor.apply(excerpt)),
            })
    }

    /// The extra repositories the task cloned and could not clone, redacted.
    pub(super) fn extra_repositories(&self) -> Option<CompanionOutcome> {
        self.extra_outcome.as_ref().map(|outcome| CompanionOutcome {
            cloned: outcome
                .cloned
                .iter()
                .map(|name| self.redactor.apply(name))
                .collect(),
            failed: outcome
                .failed
                .iter()
                .map(|name| self.redactor.apply(name))
                .collect(),
        })
    }

    pub(super) fn companions(&self) -> Option<CompanionOutcome> {
        self.companions.as_ref().map(|outcome| CompanionOutcome {
            cloned: outcome
                .cloned
                .iter()
                .map(|name| self.redactor.apply(name))
                .collect(),
            failed: outcome
                .failed
                .iter()
                .map(|name| self.redactor.apply(name))
                .collect(),
        })
    }

    /// The audit report the output carried, redacted.
    pub(super) fn report(&self) -> ReportOutcome {
        match self.report.outcome() {
            ReportOutcome::Ready { text, truncated } => ReportOutcome::Ready {
                text: self.redactor.apply(&text),
                truncated,
            },
            other => other,
        }
    }

    /// The plan the output carried, redacted; `None` without a complete and
    /// intact one, which proposes nothing.
    pub(super) fn plan(&self) -> Option<String> {
        self.document(&self.plan, "plan")
    }

    /// The title and body the change run wrote, redacted; `None` without a
    /// complete and intact text.
    pub(super) fn contribution(&self) -> Option<String> {
        self.contributions
            .get(&None)
            .and_then(|assembler| self.document(assembler, "contribution text"))
    }

    /// The title and body the change run wrote for one repository alone.
    pub(super) fn contribution_of(&self, repository: &str) -> Option<String> {
        self.contributions
            .get(&Some(repository.to_owned()))
            .and_then(|assembler| self.document(assembler, "contribution text"))
    }

    fn contribution_slot(&mut self, repository: &Option<String>) -> Option<&mut ReportAssembler> {
        if !self.contributions.contains_key(repository)
            && self.contributions.len() >= CONTRIBUTIONS_MAX
        {
            return None;
        }
        Some(self.contributions.entry(repository.clone()).or_default())
    }

    fn document(&self, assembler: &ReportAssembler, what: &str) -> Option<String> {
        match assembler.outcome() {
            ReportOutcome::Ready { text, .. } if !text.trim().is_empty() => {
                Some(self.redactor.apply(&text))
            }
            ReportOutcome::Ready { .. } | ReportOutcome::Missing => None,
            ReportOutcome::Invalid(reason) => {
                tracing::warn!(%reason, what, "a document the task sent is unusable");
                None
            }
        }
    }

    pub(super) fn accept(&mut self, line: &TaskLine) {
        match line {
            TaskLine::Matos(TaskMarker::ContributionChunk { data, repository }) => {
                if let Some(slot) = self.contribution_slot(repository) {
                    slot.chunk(data);
                }
            }
            TaskLine::Matos(TaskMarker::ContributionEnd {
                sha256,
                bytes,
                truncated,
                repository,
            }) => {
                if let Some(slot) = self.contribution_slot(repository) {
                    slot.end(sha256, *bytes, *truncated);
                }
                self.dirty = true;
            }
            TaskLine::Matos(TaskMarker::PlanChunk { data }) => self.plan.chunk(data),
            TaskLine::Matos(TaskMarker::PlanEnd {
                sha256,
                bytes,
                truncated,
            }) => {
                self.plan.end(sha256, *bytes, *truncated);
                self.dirty = true;
            }
            TaskLine::Matos(TaskMarker::ReportChunk { data }) => self.report.chunk(data),
            TaskLine::Matos(TaskMarker::ReportEnd {
                sha256,
                bytes,
                truncated,
            }) => {
                self.report.end(sha256, *bytes, *truncated);
                self.dirty = true;
            }
            TaskLine::Matos(TaskMarker::GuardBlocked { reasons }) => {
                self.blocked = Some(reasons.clone());
                self.dirty = true;
            }
            TaskLine::Matos(TaskMarker::Quality {
                lint,
                excerpt,
                repository,
            }) => {
                self.qualities.insert(
                    repository.clone(),
                    Quality {
                        lint: *lint,
                        excerpt: excerpt.clone(),
                    },
                );
                self.dirty = true;
            }
            TaskLine::Matos(TaskMarker::ExtraRepositories { cloned, failed }) => {
                self.extra_outcome = Some(CompanionOutcome {
                    cloned: cloned.clone(),
                    failed: failed.clone(),
                });
                self.dirty = true;
            }
            TaskLine::Matos(marker @ TaskMarker::Pushed { sha, repository }) => {
                self.pushed.push(PushedBranch {
                    repository: repository.clone(),
                    sha: sha.clone(),
                });
                self.digest.marker = Some(marker.clone());
                self.dirty = true;
            }
            TaskLine::Matos(TaskMarker::Companions { cloned, failed }) => {
                self.companions = Some(CompanionOutcome {
                    cloned: cloned.clone(),
                    failed: failed.clone(),
                });
                self.dirty = true;
            }
            TaskLine::Matos(marker) => {
                self.digest.marker = Some(marker.clone());
                self.dirty = true;
            }
            TaskLine::Agent(line) => {
                for event in self.parser.events(line) {
                    self.apply(event);
                }
            }
        }
    }

    fn apply(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::Text { text } => {
                if text.trim().is_empty() {
                    return;
                }
                let redacted = self.redactor.apply(&text);
                let truncated = redacted.chars().count() > TEXT_MAX_CHARS;
                self.emit(RunEventDraft::AssistantText {
                    text: truncate_chars(&redacted, TEXT_MAX_CHARS),
                    truncated,
                });
            }
            AgentEvent::TextDelta { text } => {
                if self.emitted < EVENTS_MAX_PER_RUN && !text.is_empty() {
                    self.deltas.push(StreamDelta {
                        after: self.emitted,
                        text,
                    });
                }
            }
            AgentEvent::ToolCall { id, tool, input } => {
                let input = self.redactor.apply(&input);
                self.emit(RunEventDraft::ToolUse {
                    id: id.clone(),
                    tool: tool.clone(),
                    input: truncate_chars(&input, TOOL_FIELD_MAX_CHARS),
                });
                self.order.push(id.clone());
                self.pending.insert(
                    id,
                    PendingCall {
                        tool,
                        input,
                        since: Instant::now(),
                    },
                );
            }
            AgentEvent::ToolResult {
                id,
                output,
                succeeded,
            } => {
                let shown = self.tool_field(&output);
                self.emit(RunEventDraft::ToolResult {
                    id: id.clone(),
                    succeeded,
                    output: shown,
                });
                let Some(call) = self.pending.remove(&id) else {
                    return;
                };
                let output = self.redactor.apply(&output);
                self.digest.keep(ToolCall {
                    tool: call.tool,
                    succeeded,
                    latency_ms: u64::try_from(call.since.elapsed().as_millis()).unwrap_or(u64::MAX),
                    summary: call_summary(&call.input, &output),
                });
                self.dirty = true;
            }
            AgentEvent::Finished(finish) => self.finished(finish),
        }
    }

    fn finished(&mut self, finish: AgentFinish) {
        self.digest.input_tokens = finish.usage.input_tokens;
        self.digest.output_tokens = finish.usage.output_tokens;
        self.digest.cache_read_tokens = finish.usage.cache_read_tokens;
        self.digest.cache_creation_tokens = finish.usage.cache_creation_tokens;
        self.digest.summary = finish
            .summary
            .map(|text| truncate_chars(&self.redactor.apply(&text), SUMMARY_MAX_CHARS))
            .filter(|text| !text.trim().is_empty());
        self.digest.stopped = finish.stopped;
        self.digest.reported_cost_usd = finish.reported_cost_usd;
        self.digest.turns = finish.turns;
        self.dirty = true;
    }

    pub(super) fn finish(mut self) -> Digest {
        for id in std::mem::take(&mut self.order) {
            if let Some(call) = self.pending.remove(&id) {
                self.digest.keep(ToolCall {
                    tool: call.tool,
                    succeeded: false,
                    latency_ms: u64::try_from(call.since.elapsed().as_millis()).unwrap_or(u64::MAX),
                    summary: call_summary(&call.input, "(no result)"),
                });
            }
        }
        self.digest
    }
}

fn call_summary(input: &str, output: &str) -> String {
    format!(
        "{}\n→ {}",
        truncate_chars(input, INPUT_MAX_CHARS),
        truncate_chars(output, OUTPUT_MAX_CHARS)
    )
}

enum AgentParser {
    ClaudeCode,
    Antigravity,
    Codex(codex::CodexParser),
    MistralVibe(vibe::VibeParser),
}

impl AgentParser {
    fn new(harness: Harness) -> Self {
        match harness {
            Harness::ClaudeCode => Self::ClaudeCode,
            Harness::Codex => Self::Codex(codex::CodexParser::default()),
            Harness::MistralVibe => Self::MistralVibe(vibe::VibeParser::default()),
            Harness::Antigravity => Self::Antigravity,
        }
    }

    fn events(&mut self, line: &Value) -> Vec<AgentEvent> {
        match self {
            Self::ClaudeCode => claude_code::events(line),
            Self::Antigravity => antigravity::events(line),
            Self::Codex(parser) => parser.events(line),
            Self::MistralVibe(parser) => parser.events(line),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use serde_json::json;

    use super::*;

    const SAMPLE: &str = include_str!("../../../tests/fixtures/claude-stream.jsonl");

    fn lines(sample: &str) -> Vec<TaskLine> {
        sample
            .lines()
            .map(|line| {
                let value: Value = serde_json::from_str(line).unwrap();
                if value["type"] == "matos" {
                    TaskLine::Matos(serde_json::from_value(value).unwrap())
                } else {
                    TaskLine::Agent(value)
                }
            })
            .collect()
    }

    fn read(sample: &str, secret: &str) -> Digest {
        let mut reader = StreamReader::new(Harness::ClaudeCode, Redactor::new(secret));
        for line in lines(sample) {
            reader.accept(&line);
        }
        reader.finish()
    }

    #[test]
    fn a_recorded_stream_gives_tool_calls_usage_summary_and_marker() {
        let digest = read(SAMPLE, "");

        let tools: Vec<_> = digest
            .tool_calls
            .iter()
            .map(|call| (call.tool.as_str(), call.succeeded))
            .collect();
        assert_eq!(
            tools,
            [("Bash", true), ("Write", true), ("Bash", false)],
            "{digest:?}"
        );
        assert!(digest.tool_calls[0].summary.contains("ls -la"));
        assert!(digest.tool_calls[0].summary.contains("README.md"));
        assert!(digest.tool_calls[1].summary.contains("CHANGELOG.md"));
        assert!(digest.tool_calls[2].summary.contains("jest: not found"));
        assert_eq!(digest.input_tokens, 9 + 16_360 + 5_805);
        assert_eq!(digest.output_tokens, 283);
        assert_eq!(digest.cache_read_tokens, 16_360);
        assert_eq!(digest.cache_creation_tokens, 5_805);
        assert!(
            digest
                .summary
                .as_deref()
                .unwrap()
                .starts_with("I added CHANGELOG.md")
        );
        assert_eq!(digest.stopped, None);
        assert_eq!(digest.reported_cost_usd, Some(0.0291));
        assert_eq!(digest.turns, Some(4));
        assert_eq!(
            digest.marker,
            Some(TaskMarker::Pushed {
                sha: "3f9c2a7d1e5b48a6c0d7e9f1a2b3c4d5e6f70812".into(),
                repository: None,
            })
        );
    }

    const CODEX_SAMPLE: &str = include_str!("../../../tests/fixtures/codex-stream.jsonl");

    #[test]
    fn a_reattached_follower_does_not_count_a_codex_turn_twice() {
        let mut reader = StreamReader::new(Harness::Codex, Redactor::new(""));
        for line in lines(CODEX_SAMPLE) {
            reader.accept(&line);
        }
        reader.reset();
        for line in lines(CODEX_SAMPLE) {
            reader.accept(&line);
        }
        let digest = reader.finish();
        assert_eq!(digest.input_tokens, 27_500);
        assert_eq!(digest.output_tokens, 420);
        assert_eq!(digest.tool_calls.len(), 3, "{digest:?}");
    }

    #[test]
    fn a_recorded_codex_stream_gives_tool_calls_usage_summary_and_marker() {
        let mut reader = StreamReader::new(Harness::Codex, Redactor::new(""));
        for line in lines(CODEX_SAMPLE) {
            reader.accept(&line);
        }
        let digest = reader.finish();

        let tools: Vec<_> = digest
            .tool_calls
            .iter()
            .map(|call| (call.tool.as_str(), call.succeeded))
            .collect();
        assert_eq!(
            tools,
            [("shell", true), ("file_change", true), ("shell", false)],
            "{digest:?}"
        );
        assert!(digest.tool_calls[0].summary.contains("ls -la"));
        assert!(digest.tool_calls[1].summary.contains("add CHANGELOG.md"));
        assert!(digest.tool_calls[2].summary.contains("jest: not found"));
        assert_eq!(digest.input_tokens, 27_500);
        assert_eq!(digest.cache_read_tokens, 21_000);
        assert_eq!(digest.cache_creation_tokens, 0);
        assert_eq!(digest.output_tokens, 420);
        assert!(
            digest
                .summary
                .as_deref()
                .unwrap()
                .starts_with("I added CHANGELOG.md")
        );
        assert_eq!(digest.stopped, None);
        assert_eq!(digest.reported_cost_usd, None);
        assert_eq!(digest.turns, Some(1));
        assert_eq!(
            digest.marker,
            Some(TaskMarker::Pushed {
                sha: "3f9c2a7d1e5b48a6c0d7e9f1a2b3c4d5e6f70812".into(),
                repository: None,
            })
        );
        assert_eq!(digest.cached_usage().fresh_input, 6_500);
    }

    const ANTIGRAVITY_SAMPLE: &str =
        include_str!("../../../tests/fixtures/antigravity-stream.jsonl");

    #[test]
    fn a_recorded_antigravity_stream_gives_tool_calls_usage_summary_and_marker() {
        let mut reader = StreamReader::new(Harness::Antigravity, Redactor::new(""));
        for line in lines(ANTIGRAVITY_SAMPLE) {
            reader.accept(&line);
        }
        let digest = reader.finish();

        let tools: Vec<_> = digest
            .tool_calls
            .iter()
            .map(|call| (call.tool.as_str(), call.succeeded))
            .collect();
        assert_eq!(tools, [("run_command", true)], "{digest:?}");
        assert!(digest.tool_calls[0].summary.contains("hello_lab"));
        assert_eq!(digest.input_tokens, 2_400);
        assert_eq!(digest.cache_read_tokens, 1_600);
        assert_eq!(digest.cache_creation_tokens, 0);
        assert_eq!(digest.output_tokens, 70);
        assert_eq!(digest.summary.as_deref(), Some("Done: I ran echo.\n"));
        assert_eq!(digest.stopped, None);
        assert_eq!(digest.reported_cost_usd, None);
        assert_eq!(digest.turns, Some(1));
        assert_eq!(
            digest.marker,
            Some(TaskMarker::Pushed {
                sha: "3f9c2a7d1e5b48a6c0d7e9f1a2b3c4d5e6f70812".into(),
                repository: None,
            })
        );
        assert_eq!(digest.cached_usage().fresh_input, 800);
    }

    const VIBE_SAMPLE: &str = include_str!("../../../tests/fixtures/vibe-stream.jsonl");

    #[test]
    fn a_recorded_vibe_stream_gives_tool_calls_usage_summary_and_marker() {
        let mut reader = StreamReader::new(Harness::MistralVibe, Redactor::new(""));
        for line in lines(VIBE_SAMPLE) {
            reader.accept(&line);
        }
        let digest = reader.finish();

        let tools: Vec<_> = digest
            .tool_calls
            .iter()
            .map(|call| (call.tool.as_str(), call.succeeded))
            .collect();
        assert_eq!(tools, [("file_system.bash", true)], "{digest:?}");
        assert!(digest.tool_calls[0].summary.contains("VIBE_LAB_OK"));
        assert_eq!(digest.input_tokens, 2_000);
        assert_eq!(digest.cache_read_tokens, 800);
        assert_eq!(digest.cache_creation_tokens, 0);
        assert_eq!(digest.output_tokens, 100);
        assert_eq!(
            digest.summary.as_deref(),
            Some("Done: the command printed VIBE_LAB_OK.")
        );
        assert_eq!(digest.stopped, None);
        assert_eq!(digest.reported_cost_usd, None);
        assert_eq!(digest.turns, Some(2));
        assert_eq!(
            digest.marker,
            Some(TaskMarker::Pushed {
                sha: "3f9c2a7d1e5b48a6c0d7e9f1a2b3c4d5e6f70812".into(),
                repository: None,
            })
        );
        assert_eq!(digest.cached_usage().fresh_input, 1_200);
    }

    #[test]
    fn a_reattached_follower_does_not_keep_a_vibe_summary_from_the_first_attempt() {
        let mut reader = StreamReader::new(Harness::MistralVibe, Redactor::new(""));
        let sample = lines(VIBE_SAMPLE);
        for line in &sample[..sample.len() - 2] {
            reader.accept(line);
        }
        reader.reset();
        for line in sample
            .iter()
            .filter(|line| matches!(line, TaskLine::Agent(value) if value["type"] == "vibe_export"))
        {
            reader.accept(line);
        }
        assert_eq!(reader.finish().summary, None);
    }

    #[test]
    fn the_task_token_never_survives_in_a_stored_field() {
        let secret = "dG9rZW4tZGUtdGFjaGUtMDEyMzQ1Njc4OWFiY2RlZg";
        let sample = format!(
            "{}\n{}\n{}\n{}",
            json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":format!("echo ${{MATOS_TASK_TOKEN}} {secret}")}}]}}),
            json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":format!("token={secret}"),"is_error":false}]}}),
            json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"t2","name":"Read","input":{"file_path":secret}}]}}),
            json!({"type":"result","subtype":"success","result":format!("I saw {secret} in the env"),"usage":{"input_tokens":1,"output_tokens":1}}),
        );

        let digest = read(&sample, secret);

        let stored = format!("{digest:?}");
        assert!(!stored.contains(secret), "{stored}");
        assert!(stored.contains(REDACTED));
        assert_eq!(digest.tool_calls.len(), 2);
        assert!(digest.summary.unwrap().contains("I saw *** in the env"));
    }

    fn events_of(harness: Harness, sample: &str, secret: &str) -> Vec<SequencedRunEvent> {
        let mut reader = StreamReader::new(harness, Redactor::new(secret));
        for line in lines(sample) {
            reader.accept(&line);
        }
        reader.take_events()
    }

    #[test]
    fn every_harness_yields_text_and_tool_events_with_contiguous_seqs() {
        for (harness, sample) in [
            (Harness::ClaudeCode, SAMPLE),
            (Harness::Codex, CODEX_SAMPLE),
            (Harness::Antigravity, ANTIGRAVITY_SAMPLE),
            (Harness::MistralVibe, VIBE_SAMPLE),
        ] {
            let events = events_of(harness, sample, "");
            let seqs: Vec<i64> = events.iter().map(|event| event.seq).collect();
            let expected: Vec<i64> = (1..=events.len() as i64).collect();
            assert_eq!(seqs, expected, "{harness}");
            assert!(
                events
                    .iter()
                    .any(|event| matches!(event.draft, RunEventDraft::AssistantText { .. })),
                "{harness}: {events:?}"
            );
            assert!(
                events
                    .iter()
                    .any(|event| matches!(event.draft, RunEventDraft::ToolUse { .. })),
                "{harness}"
            );
            assert!(
                events
                    .iter()
                    .any(|event| matches!(event.draft, RunEventDraft::ToolResult { .. })),
                "{harness}"
            );
        }
    }

    #[test]
    fn events_keep_the_order_of_the_blocks_in_a_line() {
        let sample = format!(
            "{}\n{}",
            json!({"type":"assistant","message":{"content":[{"type":"text","text":"look"},{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls"}},{"type":"text","text":"then"}]}}),
            json!({"type":"result","subtype":"success","result":"x"}),
        );
        let kinds: Vec<_> = events_of(Harness::ClaudeCode, &sample, "")
            .iter()
            .map(|event| (event.seq, event.draft.kind().as_str()))
            .collect();
        assert_eq!(
            kinds,
            [
                (1, "assistant_text"),
                (2, "tool_use"),
                (3, "assistant_text")
            ]
        );
    }

    #[test]
    fn a_secret_never_reaches_an_event() {
        let secret = "dG9rZW4tZGUtdGFjaGUtMDEyMzQ1Njc4OWFiY2RlZg";
        let sample = format!(
            "{}\n{}",
            json!({"type":"assistant","message":{"content":[{"type":"text","text":format!("saw {secret}")},{"type":"tool_use","id":"t1","name":"Bash","input":{"command":format!("echo {secret}")}}]}}),
            json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":format!("token={secret}")}]}}),
        );
        let events = events_of(Harness::ClaudeCode, &sample, secret);
        let stored = format!("{events:?}");
        assert!(!stored.contains(secret), "{stored}");
        assert_eq!(events.len(), 3);
        assert!(
            matches!(&events[0].draft, RunEventDraft::AssistantText { text, .. } if text == "saw ***")
        );
    }

    #[test]
    fn long_text_is_cut_and_flagged_and_tool_fields_are_cut() {
        let sample = format!(
            "{}\n{}",
            json!({"type":"assistant","message":{"content":[
                {"type":"text","text":"a".repeat(TEXT_MAX_CHARS + 10)},
                {"type":"text","text":"b".repeat(TEXT_MAX_CHARS)},
                {"type":"tool_use","id":"t1","name":"Bash","input":{"command":"c".repeat(TOOL_FIELD_MAX_CHARS * 2)}}]}}),
            json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"d".repeat(TOOL_FIELD_MAX_CHARS + 1)}]}}),
        );
        let events = events_of(Harness::ClaudeCode, &sample, "");
        assert!(
            matches!(&events[0].draft, RunEventDraft::AssistantText { text, truncated: true } if text.chars().count() == TEXT_MAX_CHARS)
        );
        assert!(
            matches!(&events[1].draft, RunEventDraft::AssistantText { text, truncated: false } if text.chars().count() == TEXT_MAX_CHARS)
        );
        assert!(
            matches!(&events[2].draft, RunEventDraft::ToolUse { input, .. } if input.chars().count() == TOOL_FIELD_MAX_CHARS)
        );
        assert!(
            matches!(&events[3].draft, RunEventDraft::ToolResult { output, .. } if output.chars().count() == TOOL_FIELD_MAX_CHARS)
        );
    }

    #[test]
    fn a_second_reader_fed_the_same_lines_yields_the_same_events() {
        for (harness, sample) in [
            (Harness::ClaudeCode, SAMPLE),
            (Harness::Codex, CODEX_SAMPLE),
            (Harness::Antigravity, ANTIGRAVITY_SAMPLE),
            (Harness::MistralVibe, VIBE_SAMPLE),
        ] {
            assert_eq!(
                events_of(harness, sample, ""),
                events_of(harness, sample, ""),
                "{harness}"
            );
        }
    }

    #[test]
    fn take_events_returns_only_what_is_new() {
        let mut reader = StreamReader::new(Harness::ClaudeCode, Redactor::new(""));
        let all = lines(SAMPLE);
        let (first, rest) = all.split_at(3);
        for line in first {
            reader.accept(line);
        }
        let early = reader.take_events();
        assert!(!early.is_empty());
        assert!(reader.take_events().is_empty());
        for line in rest {
            reader.accept(line);
        }
        let late = reader.take_events();
        assert!(!late.is_empty());
        assert_eq!(late[0].seq, early.len() as i64 + 1);
        let total = events_of(Harness::ClaudeCode, SAMPLE, "");
        assert_eq!(total.len(), early.len() + late.len());
    }

    fn delta_line(text: &str) -> TaskLine {
        TaskLine::Agent(
            json!({"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":text}}}),
        )
    }

    fn text_line(text: &str) -> TaskLine {
        TaskLine::Agent(
            json!({"type":"assistant","message":{"content":[{"type":"text","text":text}]}}),
        )
    }

    #[test]
    fn deltas_with_the_same_position_are_coalesced_and_tagged_across_an_event() {
        let mut reader = StreamReader::new(Harness::ClaudeCode, Redactor::new(""));
        assert!(reader.take_deltas().is_empty());
        reader.accept(&delta_line("hel"));
        reader.accept(&delta_line("lo"));
        reader.accept(&text_line("hello"));
        reader.accept(&delta_line("wor"));
        reader.accept(&delta_line("ld"));
        assert_eq!(
            reader.take_deltas(),
            vec![
                StreamDelta {
                    after: 0,
                    text: "hello".into()
                },
                StreamDelta {
                    after: 1,
                    text: "world".into()
                },
            ]
        );
        assert!(reader.take_deltas().is_empty());
        reader.accept(&delta_line("!"));
        assert_eq!(
            reader.take_deltas(),
            vec![StreamDelta {
                after: 1,
                text: "!".into()
            }]
        );
    }

    #[test]
    fn deltas_are_redacted_and_reset_clears_them() {
        let mut reader = StreamReader::new(Harness::ClaudeCode, Redactor::new("tok-secret"));
        reader.accept(&delta_line("the tok-"));
        reader.accept(&delta_line("secret is"));
        let taken = reader.take_deltas();
        assert_eq!(taken.len(), 1);
        assert!(!taken[0].text.contains("tok-secret"));
        reader.accept(&delta_line("pending"));
        reader.reset();
        assert!(reader.take_deltas().is_empty());
    }

    #[test]
    fn a_secret_split_across_two_takes_never_leaves_the_reader() {
        let mut reader = StreamReader::new(Harness::ClaudeCode, Redactor::new("tok-secret"));
        reader.accept(&delta_line("the tok-"));
        let first = reader.take_deltas();
        reader.accept(&delta_line("secret is"));
        let second = reader.take_deltas();
        let shown: String = first
            .iter()
            .chain(&second)
            .map(|d| d.text.as_str())
            .collect();
        assert!(!shown.contains("tok-"), "{shown}");
        assert!(!shown.contains("secret"), "{shown}");
        assert_eq!(shown, "the *** is");
    }

    #[test]
    fn a_held_tail_that_is_not_the_secret_is_released_on_the_next_take() {
        let mut reader = StreamReader::new(Harness::ClaudeCode, Redactor::new("tok-secret"));
        reader.accept(&delta_line("see tok"));
        let first = reader.take_deltas();
        reader.accept(&delta_line("ens"));
        let second = reader.take_deltas();
        let shown: String = first
            .iter()
            .chain(&second)
            .map(|d| d.text.as_str())
            .collect();
        assert_eq!(shown, "see tokens");
    }

    #[test]
    fn deltas_stop_after_the_per_run_cap() {
        let mut reader = StreamReader::new(Harness::ClaudeCode, Redactor::new(""));
        for _ in 0..EVENTS_MAX_PER_RUN {
            reader.accept(&text_line("x"));
        }
        reader.take_deltas();
        reader.accept(&delta_line("late"));
        assert!(reader.take_deltas().is_empty());
    }

    #[test]
    fn empty_text_yields_no_event() {
        let sample = json!({"type":"assistant","message":{"content":[{"type":"text","text":""},{"type":"text","text":"  \n"}]}}).to_string();
        assert!(events_of(Harness::ClaudeCode, &sample, "").is_empty());
    }

    #[test]
    fn events_stop_after_the_per_run_cap() {
        let mut reader = StreamReader::new(Harness::ClaudeCode, Redactor::new(""));
        let line = TaskLine::Agent(
            json!({"type":"assistant","message":{"content":[{"type":"text","text":"x"}]}}),
        );
        for _ in 0..(EVENTS_MAX_PER_RUN + 5) {
            reader.accept(&line);
        }
        let events = reader.take_events();
        assert_eq!(events.len() as i64, EVENTS_MAX_PER_RUN);
        assert_eq!(events.last().unwrap().seq, EVENTS_MAX_PER_RUN);
    }

    #[test]
    fn tool_calls_beyond_the_cap_are_counted_not_stored() {
        let mut sample = Vec::new();
        for n in 0..(TOOL_CALLS_MAX + 7) {
            sample.push(json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":format!("t{n}"),"name":"Bash","input":{"command":"ls"}}]}}).to_string());
            sample.push(json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":format!("t{n}"),"content":"ok"}]}}).to_string());
        }
        let digest = read(&sample.join("\n"), "");
        assert_eq!(digest.tool_calls.len(), TOOL_CALLS_MAX);
        assert_eq!(digest.dropped_tool_calls, 7);
    }

    #[test]
    fn the_reported_cost_stands_in_for_an_unpriced_model() {
        let digest = Digest {
            reported_cost_usd: Some(0.0291),
            ..Digest::default()
        };
        assert_eq!(digest.reported_cost_micros(), 29_100);
        assert_eq!(Digest::default().reported_cost_micros(), 0);
        let negative = Digest {
            reported_cost_usd: Some(-1.0),
            ..Digest::default()
        };
        assert_eq!(negative.reported_cost_micros(), 0);
    }

    #[test]
    fn a_call_without_result_is_kept_as_failed() {
        let sample = json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"sleep 999"}}]}}).to_string();
        let digest = read(&sample, "");
        assert_eq!(digest.tool_calls.len(), 1);
        assert!(!digest.tool_calls[0].succeeded);
        assert!(digest.tool_calls[0].summary.contains("(no result)"));
    }

    #[test]
    fn outputs_are_cut() {
        let long = "x".repeat(5_000);
        let sample = format!(
            "{}\n{}",
            json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":long}}]}}),
            json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":long}]}}),
        );
        let digest = read(&sample, "");
        let summary = &digest.tool_calls[0].summary;
        assert!(summary.chars().count() <= INPUT_MAX_CHARS + OUTPUT_MAX_CHARS + 3);
    }

    fn stopped_on(harness: Harness, stopped: Option<&str>) -> StreamReader {
        let mut reader = StreamReader::new(harness, Redactor::new(""));
        reader.digest.stopped = stopped.map(str::to_owned);
        reader
    }

    #[test]
    fn claude_code_stops_on_its_turn_limit_with_error_max_turns_only() {
        for (stopped, limit) in [
            (Some("error_max_turns"), true),
            (Some("error_during_execution"), false),
            (Some("success"), false),
            (Some("limit"), false),
            (None, false),
        ] {
            assert_eq!(
                stopped_on(Harness::ClaudeCode, stopped).hit_turn_limit(),
                limit,
                "{stopped:?}"
            );
        }
    }

    #[test]
    fn mistral_vibe_stops_on_its_turn_limit_when_its_export_says_limit() {
        for (stopped, limit) in [
            (Some("limit"), true),
            (Some("Limit"), true),
            (Some("iteration_limit"), true),
            (Some("max_turns"), true),
            (Some("max turns reached"), true),
            (Some("rate limited"), false),
            (Some("quota"), false),
            (Some("aborted"), false),
            (None, false),
        ] {
            assert_eq!(
                stopped_on(Harness::MistralVibe, stopped).hit_turn_limit(),
                limit,
                "{stopped:?}"
            );
        }
    }

    #[test]
    fn harnesses_without_a_turn_limit_never_report_one() {
        for harness in [Harness::Codex, Harness::Antigravity] {
            assert!(!stopped_on(harness, Some("limit")).hit_turn_limit());
            assert!(!stopped_on(harness, Some("error_max_turns")).hit_turn_limit());
        }
    }

    #[test]
    fn a_result_that_is_not_a_success_says_why() {
        let sample = json!({"type":"result","subtype":"error_max_turns","is_error":false,"usage":{"input_tokens":4,"output_tokens":2}}).to_string();
        let digest = read(&sample, "");
        assert_eq!(digest.stopped.as_deref(), Some("error_max_turns"));
        assert_eq!(digest.summary, None);
    }

    #[test]
    fn a_reattached_follower_starts_from_a_clean_slate() {
        let mut reader = StreamReader::new(Harness::ClaudeCode, Redactor::new(""));
        for line in lines(SAMPLE) {
            reader.accept(&line);
        }
        reader.reset();
        assert!(reader.snapshot().tool_calls.is_empty());
        assert_eq!(reader.marker(), None);
        for line in lines(SAMPLE) {
            reader.accept(&line);
        }
        assert_eq!(reader.finish().tool_calls.len(), 3);
    }

    fn report_lines(text: &str, chunk: usize, sha: Option<String>) -> Vec<TaskLine> {
        use base64::{Engine, engine::general_purpose::STANDARD};
        let encoded = STANDARD.encode(text);
        let mut lines: Vec<TaskLine> = encoded
            .as_bytes()
            .chunks(chunk)
            .map(|piece| {
                TaskLine::Matos(TaskMarker::ReportChunk {
                    data: String::from_utf8(piece.to_vec()).unwrap(),
                })
            })
            .collect();
        let digest = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, text.as_bytes());
        let hex: String = digest.as_ref().iter().map(|b| format!("{b:02x}")).collect();
        lines.push(TaskLine::Matos(TaskMarker::ReportEnd {
            sha256: sha.unwrap_or(hex),
            bytes: text.len() as u64,
            truncated: false,
        }));
        lines.push(TaskLine::Matos(TaskMarker::AuditDone));
        lines
    }

    #[test]
    fn report_chunks_rebuild_the_report_and_leave_the_marker_alone() {
        let mut reader = StreamReader::new(Harness::ClaudeCode, Redactor::new("tok-secret"));
        for line in lines(SAMPLE) {
            reader.accept(&line);
        }
        let text = "# Rapport\n\nLe jeton tok-secret traînait dans .env.";
        for line in report_lines(text, 5, None) {
            reader.accept(&line);
        }
        assert_eq!(reader.marker(), Some(&TaskMarker::AuditDone));
        assert_eq!(
            reader.report(),
            ReportOutcome::Ready {
                text: "# Rapport\n\nLe jeton *** traînait dans .env.".into(),
                truncated: false
            }
        );
        assert_eq!(reader.finish().tool_calls.len(), 3);
    }

    fn plan_lines(text: &str) -> Vec<TaskLine> {
        report_lines(text, 6, None)
            .into_iter()
            .filter_map(|line| match line {
                TaskLine::Matos(TaskMarker::ReportChunk { data }) => {
                    Some(TaskLine::Matos(TaskMarker::PlanChunk { data }))
                }
                TaskLine::Matos(TaskMarker::ReportEnd {
                    sha256,
                    bytes,
                    truncated,
                }) => Some(TaskLine::Matos(TaskMarker::PlanEnd {
                    sha256,
                    bytes,
                    truncated,
                })),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_plan_arrives_beside_the_report_redacted_and_apart_from_it() {
        let mut reader = StreamReader::new(Harness::MistralVibe, Redactor::new("tok-secret"));
        for line in report_lines("# Rapport", 4, None) {
            reader.accept(&line);
        }
        assert_eq!(reader.plan(), None);
        for line in plan_lines("- retirer Pitchout (tok-secret)") {
            reader.accept(&line);
        }
        assert_eq!(reader.plan().as_deref(), Some("- retirer Pitchout (***)"));
        assert!(
            matches!(reader.report(), ReportOutcome::Ready { text, .. } if text == "# Rapport")
        );
        reader.reset();
        assert_eq!(reader.plan(), None);
    }

    #[test]
    fn a_blank_or_broken_plan_proposes_nothing() {
        let mut reader = StreamReader::new(Harness::MistralVibe, Redactor::new(""));
        for line in plan_lines("  \n") {
            reader.accept(&line);
        }
        assert_eq!(reader.plan(), None);
        let mut reader = StreamReader::new(Harness::MistralVibe, Redactor::new(""));
        reader.accept(&TaskLine::Matos(TaskMarker::PlanChunk {
            data: "aGk=".into(),
        }));
        assert_eq!(reader.plan(), None);
    }

    #[test]
    fn a_contribution_text_arrives_redacted_and_leaves_the_pushed_marker_alone() {
        let mut reader = StreamReader::new(Harness::ClaudeCode, Redactor::new("tok-secret"));
        for line in report_lines("feat: x (tok-secret)", 4, None)
            .into_iter()
            .filter_map(|line| match line {
                TaskLine::Matos(TaskMarker::ReportChunk { data }) => {
                    Some(TaskLine::Matos(TaskMarker::ContributionChunk {
                        data,
                        repository: None,
                    }))
                }
                TaskLine::Matos(TaskMarker::ReportEnd {
                    sha256,
                    bytes,
                    truncated,
                }) => Some(TaskLine::Matos(TaskMarker::ContributionEnd {
                    sha256,
                    bytes,
                    truncated,
                    repository: None,
                })),
                _ => None,
            })
        {
            reader.accept(&line);
        }
        reader.accept(&TaskLine::Matos(TaskMarker::Pushed {
            sha: "abc".into(),
            repository: None,
        }));
        assert_eq!(reader.contribution().as_deref(), Some("feat: x (***)"));
        assert_eq!(reader.plan(), None);
        assert_eq!(
            reader.marker(),
            Some(&TaskMarker::Pushed {
                sha: "abc".into(),
                repository: None,
            })
        );
    }

    #[test]
    fn the_companions_marker_is_kept_apart_from_the_final_marker() {
        let mut reader = StreamReader::new(Harness::ClaudeCode, Redactor::new("tok-secret"));
        reader.accept(&TaskLine::Matos(TaskMarker::Companions {
            cloned: vec!["acme/sdk".into()],
            failed: vec!["acme/tok-secret".into()],
        }));
        reader.accept(&TaskLine::Matos(TaskMarker::NoChanges));
        assert_eq!(
            reader.companions(),
            Some(CompanionOutcome {
                cloned: vec!["acme/sdk".into()],
                failed: vec!["acme/***".into()]
            })
        );
        assert_eq!(reader.marker(), Some(&TaskMarker::NoChanges));
        reader.reset();
        assert_eq!(reader.companions(), None);
    }

    #[test]
    fn the_guard_and_quality_markers_are_kept_apart_from_the_final_marker() {
        let mut reader = StreamReader::new(Harness::ClaudeCode, Redactor::new("tok-secret"));
        reader.accept(&TaskLine::Matos(TaskMarker::Quality {
            lint: QualityStatus::Failed,
            excerpt: Some("error tok-secret".into()),
            repository: None,
        }));
        reader.accept(&TaskLine::Matos(TaskMarker::Pushed {
            sha: "abc".into(),
            repository: None,
        }));
        assert_eq!(
            reader.quality("acme/api", true),
            Some(Quality {
                lint: QualityStatus::Failed,
                excerpt: Some("error ***".into())
            })
        );
        assert_eq!(reader.blocked(), None);
        assert_eq!(
            reader.marker(),
            Some(&TaskMarker::Pushed {
                sha: "abc".into(),
                repository: None,
            })
        );
        reader.accept(&TaskLine::Matos(TaskMarker::GuardBlocked {
            reasons: vec!["chemin sensible : tok-secret".into()],
        }));
        assert_eq!(reader.blocked(), Some(vec!["chemin sensible : ***".into()]));
        reader.reset();
        assert_eq!(
            (reader.blocked(), reader.quality("acme/api", true)),
            (None, None)
        );
    }

    fn contribution_lines(text: &str, repository: Option<&str>) -> Vec<TaskLine> {
        report_lines(text, 4, None)
            .into_iter()
            .filter_map(|line| match line {
                TaskLine::Matos(TaskMarker::ReportChunk { data }) => {
                    Some(TaskLine::Matos(TaskMarker::ContributionChunk {
                        data,
                        repository: repository.map(str::to_owned),
                    }))
                }
                TaskLine::Matos(TaskMarker::ReportEnd {
                    sha256,
                    bytes,
                    truncated,
                }) => Some(TaskLine::Matos(TaskMarker::ContributionEnd {
                    sha256,
                    bytes,
                    truncated,
                    repository: repository.map(str::to_owned),
                })),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn every_pushed_marker_is_kept_in_order_and_the_last_one_stays_the_marker() {
        let mut reader = StreamReader::new(Harness::ClaudeCode, Redactor::new(""));
        reader.accept(&TaskLine::Matos(TaskMarker::Pushed {
            sha: "aaa".into(),
            repository: None,
        }));
        reader.accept(&TaskLine::Matos(TaskMarker::Pushed {
            sha: "bbb".into(),
            repository: Some("acme/sdk".into()),
        }));
        assert_eq!(
            reader.pushed(),
            [
                PushedBranch {
                    repository: None,
                    sha: "aaa".into()
                },
                PushedBranch {
                    repository: Some("acme/sdk".into()),
                    sha: "bbb".into()
                }
            ]
        );
        assert!(matches!(reader.marker(), Some(TaskMarker::Pushed { .. })));
        reader.reset();
        assert!(reader.pushed().is_empty());
    }

    #[test]
    fn contributions_are_kept_per_repository_beside_the_shared_one() {
        let mut reader = StreamReader::new(Harness::ClaudeCode, Redactor::new("tok-secret"));
        for line in contribution_lines("shared", None)
            .into_iter()
            .chain(contribution_lines("sdk only tok-secret", Some("acme/sdk")))
        {
            reader.accept(&line);
        }
        assert_eq!(reader.contribution().as_deref(), Some("shared"));
        assert_eq!(
            reader.contribution_of("acme/sdk").as_deref(),
            Some("sdk only ***")
        );
        assert_eq!(reader.contribution_of("acme/web"), None);
        reader.reset();
        assert_eq!(reader.contribution_of("acme/sdk"), None);
    }

    #[test]
    fn a_quality_without_a_name_is_the_primary_s_and_a_named_one_is_its_own() {
        let mut reader = StreamReader::new(Harness::ClaudeCode, Redactor::new(""));
        reader.accept(&TaskLine::Matos(TaskMarker::Quality {
            lint: QualityStatus::Passed,
            excerpt: None,
            repository: None,
        }));
        reader.accept(&TaskLine::Matos(TaskMarker::Quality {
            lint: QualityStatus::Failed,
            excerpt: Some("boom".into()),
            repository: Some("acme/sdk".into()),
        }));
        assert_eq!(
            reader.quality("acme/api", true).map(|quality| quality.lint),
            Some(QualityStatus::Passed)
        );
        assert_eq!(
            reader
                .quality("acme/sdk", false)
                .map(|quality| quality.lint),
            Some(QualityStatus::Failed)
        );
        assert_eq!(reader.quality("acme/web", false), None);
    }

    #[test]
    fn the_extra_repositories_marker_is_kept_apart_from_the_final_marker() {
        let mut reader = StreamReader::new(Harness::ClaudeCode, Redactor::new("tok-secret"));
        reader.accept(&TaskLine::Matos(TaskMarker::ExtraRepositories {
            cloned: vec!["acme/sdk".into()],
            failed: vec!["acme/tok-secret".into()],
        }));
        reader.accept(&TaskLine::Matos(TaskMarker::NoChanges));
        assert_eq!(
            reader.extra_repositories(),
            Some(CompanionOutcome {
                cloned: vec!["acme/sdk".into()],
                failed: vec!["acme/***".into()]
            })
        );
        assert_eq!(reader.marker(), Some(&TaskMarker::NoChanges));
    }

    #[test]
    fn a_report_whose_digest_does_not_match_is_invalid() {
        let mut reader = StreamReader::new(Harness::ClaudeCode, Redactor::new(""));
        for line in report_lines("rapport", 4, Some("00".repeat(32))) {
            reader.accept(&line);
        }
        assert!(matches!(reader.report(), ReportOutcome::Invalid(_)));
    }

    #[test]
    fn a_reattached_follower_rebuilds_the_report_from_scratch() {
        let mut reader = StreamReader::new(Harness::ClaudeCode, Redactor::new(""));
        let sent = report_lines("rapport", 4, None);
        for line in &sent {
            reader.accept(line);
        }
        reader.reset();
        assert_eq!(reader.report(), ReportOutcome::Missing);
        for line in &sent {
            reader.accept(line);
        }
        assert!(matches!(reader.report(), ReportOutcome::Ready { text, .. } if text == "rapport"));
    }

    #[test]
    fn an_empty_secret_redacts_nothing() {
        assert_eq!(Redactor::new("").apply("abc"), "abc");
    }
}
