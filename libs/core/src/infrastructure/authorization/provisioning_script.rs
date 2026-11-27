use std::{
    path::PathBuf,
    process::Command,
    sync::{Arc, Mutex},
};

use serde_json::{Value, json};

use super::fake_server::{FakeServer, Recorded};

const BASE: &str = "/realms/acme/authz";

#[derive(Clone)]
struct StoredPolicy {
    id: String,
    name: String,
    source: String,
    enabled: bool,
    managed_by: Option<String>,
}

#[derive(Default)]
struct Realm {
    schema: Option<(String, String)>,
    policies: Vec<StoredPolicy>,
    enabled: bool,
    next: u32,
}

fn policy_json(policy: &StoredPolicy) -> Value {
    json!({
        "id": policy.id,
        "name": policy.name,
        "description": null,
        "origin": "custom",
        "enabled": policy.enabled,
        "managed_by": policy.managed_by,
        "body": {"type": "static", "source": policy.source},
    })
}

fn managed_by(request: &Recorded) -> Option<String> {
    request.query.strip_prefix("managed_by=").map(str::to_owned)
}

fn handle(realm: &Mutex<Realm>, request: &Recorded) -> (u16, String) {
    if request.header("authorization") != Some("Bearer admin") {
        return (401, "{}".to_owned());
    }
    let Some(path) = request.path.strip_prefix(BASE) else {
        return (404, "{}".to_owned());
    };
    let mut realm = realm.lock().unwrap();
    let body: Value = serde_json::from_str(&request.body).unwrap_or(Value::Null);
    match (request.method.as_str(), path) {
        ("GET", "/policies") => {
            let label = managed_by(request);
            let listed: Vec<Value> = realm
                .policies
                .iter()
                .filter(|p| label.is_none() || p.managed_by == label)
                .map(policy_json)
                .collect();
            (200, Value::Array(listed).to_string())
        }
        ("POST", "/policies") => {
            realm.next += 1;
            let policy = StoredPolicy {
                id: format!("id-{}", realm.next),
                name: body["name"].as_str().unwrap().to_owned(),
                source: body["body"]["source"].as_str().unwrap().to_owned(),
                enabled: body["enabled"].as_bool().unwrap(),
                managed_by: managed_by(request),
            };
            let created = policy_json(&policy);
            realm.policies.push(policy);
            (201, created.to_string())
        }
        ("GET", "/schema") => match &realm.schema {
            Some((namespace, source)) => (
                200,
                json!({"namespace": namespace, "source": source}).to_string(),
            ),
            None => (404, "{}".to_owned()),
        },
        ("PUT", "/schema") => {
            realm.schema = Some((
                body["namespace"].as_str().unwrap().to_owned(),
                body["source"].as_str().unwrap().to_owned(),
            ));
            (200, "{}".to_owned())
        }
        ("GET", "/state") => (
            200,
            json!({"enabled": realm.enabled, "version": 1}).to_string(),
        ),
        ("PUT", "/state") => {
            realm.enabled = body["enabled"].as_bool().unwrap();
            (200, "{}".to_owned())
        }
        (method, other) => {
            let Some(id) = other.strip_prefix("/policies/") else {
                return (404, "{}".to_owned());
            };
            let label = managed_by(request);
            let Some(at) = realm
                .policies
                .iter()
                .position(|p| p.id == id && p.managed_by == label)
            else {
                return (404, "{}".to_owned());
            };
            match method {
                "PUT" => {
                    realm.policies[at].source = body["body"]["source"].as_str().unwrap().to_owned();
                    realm.policies[at].enabled = body["enabled"].as_bool().unwrap();
                    (200, policy_json(&realm.policies[at]).to_string())
                }
                "DELETE" => {
                    realm.policies.remove(at);
                    (204, String::new())
                }
                _ => (405, "{}".to_owned()),
            }
        }
    }
}

fn script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../deploy/ferriskey/authz/push.sh")
}

fn bundle_dir() -> PathBuf {
    script().parent().unwrap().to_path_buf()
}

fn policy_files() -> Vec<(String, String)> {
    let mut found: Vec<(String, String)> = std::fs::read_dir(bundle_dir().join("policies"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|e| e == "cedar"))
        .map(|path| {
            (
                path.file_stem().unwrap().to_string_lossy().into_owned(),
                std::fs::read_to_string(&path)
                    .unwrap()
                    .trim_end()
                    .to_owned(),
            )
        })
        .collect();
    found.sort();
    found
}

fn run_push(server: &FakeServer, dir: &PathBuf) -> std::process::Output {
    Command::new("bash")
        .arg(script())
        .env("FERRISKEY_URL", server.url(""))
        .env("REALM", "acme")
        .env("ADMIN_TOKEN", "admin")
        .env("AUTHZ_DIR", dir)
        .output()
        .unwrap()
}

fn start(realm: Realm) -> (FakeServer, Arc<Mutex<Realm>>) {
    let realm = Arc::new(Mutex::new(realm));
    let shared = Arc::clone(&realm);
    (
        FakeServer::with_handler(move |request| handle(&shared, request)),
        realm,
    )
}

fn writes(server: &FakeServer) -> Vec<String> {
    server
        .requests()
        .iter()
        .filter(|r| r.method != "GET")
        .map(|r| format!("{} {}", r.method, r.path))
        .collect()
}

fn stored(name: &str, source: &str, managed_by: Option<&str>) -> StoredPolicy {
    StoredPolicy {
        id: format!("seed-{name}"),
        name: name.to_owned(),
        source: source.to_owned(),
        enabled: true,
        managed_by: managed_by.map(str::to_owned),
    }
}

#[test]
fn the_bundle_is_pushed_in_full_to_an_empty_realm() {
    let (server, realm) = start(Realm::default());
    let output = run_push(&server, &bundle_dir());
    assert!(output.status.success(), "{output:?}");

    let realm = realm.lock().unwrap();
    let files = policy_files();
    assert_eq!(files.len(), 21);
    assert_eq!(realm.policies.len(), files.len());
    for (name, source) in &files {
        let pushed = realm.policies.iter().find(|p| &p.name == name).unwrap();
        assert_eq!(&pushed.source, source);
        assert_eq!(pushed.managed_by.as_deref(), Some("matos"));
        assert!(pushed.enabled);
    }
    let (namespace, schema) = realm.schema.clone().unwrap();
    assert_eq!(namespace, "Matos");
    assert_eq!(
        schema,
        std::fs::read_to_string(bundle_dir().join("schema.cedarschema"))
            .unwrap()
            .trim_end()
    );
    assert!(realm.enabled);
}

#[test]
fn provisioning_twice_changes_nothing_the_second_time() {
    let (server, realm) = start(Realm::default());
    assert!(run_push(&server, &bundle_dir()).status.success());
    let after_first = realm.lock().unwrap().policies.len();
    let calls_after_first = server.requests().len();

    let second = run_push(&server, &bundle_dir());
    assert!(second.status.success(), "{second:?}");

    let second_calls = &server.requests()[calls_after_first..];
    assert!(!second_calls.is_empty());
    assert!(
        second_calls.iter().all(|r| r.method == "GET"),
        "{second_calls:?}"
    );
    assert_eq!(realm.lock().unwrap().policies.len(), after_first);
}

#[test]
fn a_changed_policy_is_updated_a_stale_one_deleted_and_foreign_ones_left_alone() {
    let (name, source) = policy_files().remove(0);
    let (server, realm) = start(Realm {
        policies: vec![
            stored(
                &name,
                "permit (principal, action, resource);",
                Some("matos"),
            ),
            stored(
                "matos-retired",
                "forbid (principal, action, resource);",
                Some("matos"),
            ),
            stored(
                "hand-written",
                "permit (principal, action, resource);",
                None,
            ),
            stored(
                "other-tool",
                "permit (principal, action, resource);",
                Some("other"),
            ),
        ],
        ..Realm::default()
    });

    let output = run_push(&server, &bundle_dir());
    assert!(output.status.success(), "{output:?}");

    let realm = realm.lock().unwrap();
    let names: Vec<&str> = realm.policies.iter().map(|p| p.name.as_str()).collect();
    assert!(!names.contains(&"matos-retired"));
    assert!(names.contains(&"hand-written"));
    assert!(names.contains(&"other-tool"));
    let updated = realm.policies.iter().find(|p| p.name == name).unwrap();
    assert_eq!(updated.source, source);
    assert_eq!(updated.id, format!("seed-{name}"));
    let touched = writes(&server);
    assert!(touched.contains(&format!("DELETE {BASE}/policies/seed-matos-retired")));
    assert!(touched.contains(&format!("PUT {BASE}/policies/seed-{name}")));
    assert!(
        touched
            .iter()
            .all(|w| !w.contains("seed-hand-written") && !w.contains("seed-other-tool")),
        "{touched:?}"
    );
}

#[test]
fn a_refusal_from_the_realm_fails_the_push() {
    let server = FakeServer::start(vec![]);
    let output = run_push(&server, &bundle_dir());
    assert!(!output.status.success());
}
