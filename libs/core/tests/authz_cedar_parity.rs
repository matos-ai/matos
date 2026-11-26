#![allow(clippy::unwrap_used)]

use std::{collections::HashSet, fs, path::PathBuf};

use cedar_policy::{Effect, PolicyId, ValidationMode, Validator};
use matos_common::OrganizationId;
use matos_core::domain::{
    self,
    authorization::{AccessRequest, Action, ResourceKind, Subject},
    card::Column,
    conversation::CAPTAIN_AGENT,
    execution::EXECUTOR_AGENT,
    integration::{action::Access, journal::INTEGRATOR_AGENT},
};
use matos_core::infrastructure::authorization::{CedarPolicyEngine, actor_kind, evaluation_of};
use serde_json::{Value, json};

const CORPUS: &str = include_str!("fixtures/authz_parity.jsonl");
const ACTION_NAMES: usize = 44;
const POLICY_COUNT: usize = 20;

struct Case {
    evaluation: Value,
    allow: bool,
}

fn corpus() -> Vec<Case> {
    CORPUS
        .lines()
        .map(|line| {
            let case: Value = serde_json::from_str(line).unwrap();
            Case {
                allow: case["allow"].as_bool().unwrap(),
                evaluation: case["evaluation"].clone(),
            }
        })
        .collect()
}

fn actors() -> [Option<String>; 5] {
    [
        None,
        Some(CAPTAIN_AGENT.to_owned()),
        Some(INTEGRATOR_AGENT.to_owned()),
        Some(domain::agent_task::actor_of("darnold")),
        Some("agent-writer".to_owned()),
    ]
}

fn policy_files() -> Vec<(String, String)> {
    let directory =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../deploy/ferriskey/authz/policies");
    let mut files: Vec<PathBuf> = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "cedar"))
        .collect();
    files.sort();
    files
        .iter()
        .map(|path| {
            (
                path.file_stem().unwrap().to_str().unwrap().to_owned(),
                fs::read_to_string(path).unwrap(),
            )
        })
        .collect()
}

fn request_of(
    action: Action,
    kind: ResourceKind,
    actor: Option<&String>,
    is_admin: bool,
    same_organization: bool,
) -> AccessRequest {
    let mut request = AccessRequest::new(
        Subject {
            id: "0197c6a0-0000-7000-8000-000000000001".into(),
            organization_id: OrganizationId::new(),
            actor: actor.cloned(),
            is_admin,
        },
        action,
        kind,
        None,
    );
    if !same_organization {
        request.resource.organization_id = OrganizationId::new();
    }
    request
}

#[test]
fn the_policies_validate_against_the_schema_in_strict_mode() {
    let engine = CedarPolicyEngine::new();
    let result =
        Validator::new(engine.schema().clone()).validate(engine.policies(), ValidationMode::Strict);

    assert!(
        result.validation_passed(),
        "{:?}",
        result.validation_errors().collect::<Vec<_>>()
    );
    assert_eq!(result.validation_warnings().count(), 0);
}

#[test]
fn every_policy_is_its_own_file_named_after_its_id() {
    let files = policy_files();
    let engine = CedarPolicyEngine::new();
    let set = engine.policies();
    let mut seen = HashSet::new();

    assert_eq!(files.len(), POLICY_COUNT);
    assert_eq!(set.policies().count(), POLICY_COUNT);
    assert!(set.templates().next().is_none());
    for (stem, source) in &files {
        let policy = set.policy(&PolicyId::new(stem)).unwrap();
        assert_eq!(policy.annotation("id"), Some(stem.as_str()));
        assert_eq!(source.matches("@id(").count(), 1, "{stem}");
        assert!(seen.insert(stem.clone()), "duplicate {stem}");
    }
}

#[test]
fn the_corpus_covers_every_action_actor_kind_and_organization_case() {
    let cases = corpus();
    let text = |case: &Case, path: [&str; 2]| case.evaluation[path[0]][path[1]].clone();
    let distinct = |path: [&str; 2]| -> HashSet<String> {
        cases
            .iter()
            .map(|case| text(case, path).to_string())
            .collect()
    };

    assert_eq!(distinct(["action", "name"]).len(), ACTION_NAMES);
    assert_eq!(distinct(["context", "actor_kind"]).len(), 5);
    assert_eq!(distinct(["context", "is_admin"]).len(), 2);
    assert!(cases.iter().any(|case| case.allow));
    assert!(cases.iter().any(|case| !case.allow));
    assert!(cases.iter().any(|case| {
        case.evaluation["context"]["organization"]
            != case.evaluation["resource"]["properties"]["organization"]
    }));
    assert!(cases.len() >= 3000);
}

#[test]
fn the_actor_kind_follows_the_local_classification() {
    assert_eq!(actor_kind(None), "human");
    assert_eq!(actor_kind(Some(CAPTAIN_AGENT)), "captain");
    assert_eq!(actor_kind(Some(INTEGRATOR_AGENT)), "integrator");
    assert_eq!(actor_kind(Some("agent:darnold")), "registry");
    assert_eq!(actor_kind(Some(EXECUTOR_AGENT)), "agent");
    assert_eq!(actor_kind(Some("agent-writer")), "agent");
}

#[test]
fn the_evaluation_body_is_authzen_shaped() {
    let request = request_of(
        Action::CardMove {
            from: Column::Todo,
            to: Column::Done,
        },
        ResourceKind::Card,
        Some(&"agent-writer".to_owned()),
        false,
        true,
    );

    let body = evaluation_of(&request);

    assert_eq!(body["subject"]["type"], "user");
    assert_eq!(body["subject"]["id"], request.subject.id);
    assert_eq!(body["action"]["name"], "card.move");
    assert_eq!(body["resource"]["type"], "Matos::Card");
    assert_eq!(
        body["resource"]["properties"]["organization"],
        request.resource.organization_id.to_string()
    );
    assert_eq!(body["context"]["from"], "todo");
    assert_eq!(body["context"]["to"], "done");
    assert_eq!(body["context"]["actor"], "agent-writer");
    assert_eq!(body["context"]["actor_kind"], "agent");
    assert_eq!(body["context"]["is_admin"], false);
}

fn assert_no_failures(label: &str, cases: usize, failures: &[String]) {
    eprintln!(
        "{label}: replayed {cases} cases, {} mismatches",
        failures.len()
    );
    assert!(
        failures.is_empty(),
        "{} of {cases} cases differ, first:\n{}",
        failures.len(),
        failures
            .iter()
            .take(5)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n---\n")
    );
}

fn replay(engine: &CedarPolicyEngine, exact: bool) -> (usize, Vec<String>) {
    let cases = corpus();
    let mut failures = Vec::new();
    for case in &cases {
        match engine.allows(&case.evaluation) {
            Ok(cedar) if cedar == case.allow || (!exact && !cedar) => {}
            Ok(cedar) => failures.push(format!(
                "corpus={} cedar={cedar}\n{:#}",
                case.allow, case.evaluation
            )),
            Err(error) => failures.push(format!("{error}\n{:#}", case.evaluation)),
        }
    }
    (cases.len(), failures)
}

#[test]
fn cedar_decides_exactly_like_the_corpus() {
    let (cases, failures) = replay(&CedarPolicyEngine::new(), true);

    assert_no_failures("full policy set", cases, &failures);
}

#[test]
fn the_permits_alone_never_allow_more_than_the_corpus() {
    let permits = CedarPolicyEngine::new().permits_only();
    assert!(
        permits
            .policies()
            .policies()
            .all(|p| p.effect() == Effect::Permit)
    );

    let (cases, failures) = replay(&permits, false);

    assert_no_failures("permits only", cases, &failures);
}

fn representative_requests() -> Vec<AccessRequest> {
    let actions = [
        Action::CardRead,
        Action::CardCreate,
        Action::CardLink,
        Action::ProviderManage,
        Action::CardUpdate {
            column: Column::Todo,
        },
        Action::CardUpdate {
            column: Column::Done,
        },
        Action::CardMove {
            from: Column::Todo,
            to: Column::Done,
        },
        Action::CardMove {
            from: Column::Proposed,
            to: Column::Todo,
        },
        Action::IntegrationCall {
            access: Access::Write,
        },
        Action::McpCall {
            initiated: true,
            granted: true,
        },
    ];
    let mut requests = Vec::new();
    for action in actions {
        for actor in actors() {
            requests.push(request_of(
                action,
                ResourceKind::Card,
                actor.as_ref(),
                true,
                true,
            ));
        }
    }
    requests
}

fn flipped(value: &Value) -> Value {
    match value {
        Value::Bool(_) => json!("true"),
        Value::String(_) => json!(true),
        _ => json!("x"),
    }
}

#[test]
fn a_malformed_context_is_never_allowed() {
    let engine = CedarPolicyEngine::new();
    let mut mutations = 0usize;

    for request in representative_requests() {
        let body = evaluation_of(&request);
        let keys: Vec<String> = body["context"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        for key in keys {
            let mut deleted = body.clone();
            deleted["context"].as_object_mut().unwrap().remove(&key);
            let mut changed = body.clone();
            changed["context"][&key] = flipped(&body["context"][&key]);
            for mutated in [deleted, changed] {
                mutations += 1;
                let outcome = engine.allows(&mutated);
                assert_ne!(outcome, Ok(true), "key={key}\n{mutated:#}");
            }
        }
    }
    eprintln!("malformed context: {mutations} mutations, none allowed");
    assert!(mutations > 0);
}

fn with_context(request: &AccessRequest, key: &str, value: Value) -> Value {
    let mut body = evaluation_of(request);
    body["context"][key] = value;
    body
}

#[test]
fn spoofed_actor_kinds_and_columns_are_denied() {
    let engine = CedarPolicyEngine::new();
    let writer = "agent-writer".to_owned();
    let card = ResourceKind::Card;
    let denied = |body: &Value| {
        assert_ne!(engine.allows(body), Ok(true), "{body:#}");
    };

    let agent = request_of(Action::CardRead, card, Some(&writer), true, true);
    denied(&with_context(&agent, "actor_kind", json!("human")));

    let human = request_of(Action::CardRead, card, None, true, true);
    assert_eq!(engine.allows(&evaluation_of(&human)), Ok(true));
    denied(&with_context(&human, "actor_kind", json!("agent")));
    denied(&with_context(&human, "actor_kind", json!("captain")));

    for bogus in ["DONE", "bogus", "", "Todo"] {
        for actor in [None, Some(&writer)] {
            let update = request_of(
                Action::CardUpdate {
                    column: Column::Todo,
                },
                card,
                actor,
                true,
                true,
            );
            denied(&with_context(&update, "column", json!(bogus)));
            let movement = request_of(
                Action::CardMove {
                    from: Column::Todo,
                    to: Column::InProgress,
                },
                card,
                actor,
                true,
                true,
            );
            denied(&with_context(&movement, "from", json!(bogus)));
            denied(&with_context(&movement, "to", json!(bogus)));
        }
    }
}
