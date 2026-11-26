#![allow(clippy::expect_used)]

use std::{fs, path::PathBuf, str::FromStr, sync::Arc, sync::OnceLock};

use cedar_policy::{
    Authorizer as CedarAuthorizer, Context, Decision as CedarDecision, Effect, Entities, Entity,
    EntityId, EntityTypeName, EntityUid, Policy, PolicyId, PolicySet, Request, Schema,
    SchemaFragment,
};
use matos_common::CoreError;
use serde_json::{Value, json};

use super::evaluation_of;
use crate::domain::authorization::{AccessRequest, Decision, ports::Authorizer};

const BUILTIN_SCHEMA: &str =
    include_str!("../../../tests/fixtures/ferriskey-builtin-1078670.cedarschema");
const REALM_SCHEMA: &str = include_str!("../../../../../deploy/ferriskey/authz/schema.cedarschema");
const REALM_NAMESPACE: &str = "Matos";

struct Rules {
    schema: Schema,
    policies: PolicySet,
}

#[derive(Clone)]
pub struct CedarPolicyEngine {
    rules: Arc<Rules>,
}

impl CedarPolicyEngine {
    pub fn new() -> Self {
        static RULES: OnceLock<Arc<Rules>> = OnceLock::new();
        let rules = RULES.get_or_init(|| Arc::new(load()));
        Self {
            rules: Arc::clone(rules),
        }
    }

    pub fn permits_only(&self) -> Self {
        let mut permits = PolicySet::new();
        for policy in self
            .rules
            .policies
            .policies()
            .filter(|policy| policy.effect() == Effect::Permit)
        {
            permits.add(policy.clone()).expect("a policy of the set");
        }
        Self {
            rules: Arc::new(Rules {
                schema: self.rules.schema.clone(),
                policies: permits,
            }),
        }
    }

    pub fn schema(&self) -> &Schema {
        &self.rules.schema
    }

    pub fn policies(&self) -> &PolicySet {
        &self.rules.policies
    }

    pub fn allows(&self, evaluation: &Value) -> Result<bool, String> {
        let schema = &self.rules.schema;
        let subject = &evaluation["subject"];
        let resource_ref = &evaluation["resource"];
        let subject_id = text(subject, "id")?;
        let resource_type = text(resource_ref, "type")?;
        let resource_id = text(resource_ref, "id")?;

        let user = Entity::from_json_value(
            json!({
                "uid": {"type": "FerrisKey::User", "id": subject_id},
                "attrs": {
                    "username": subject_id,
                    "email_verified": false,
                    "enabled": true,
                    "roles": [],
                },
                "parents": [],
            }),
            Some(schema),
        )
        .map_err(|e| e.to_string())?;
        let resource_entity = Entity::from_json_value(
            json!({
                "uid": {"type": resource_type, "id": resource_id},
                "attrs": resource_ref["properties"],
                "parents": [],
            }),
            Some(schema),
        )
        .map_err(|e| e.to_string())?;
        let entities = Entities::from_entities([user, resource_entity], Some(schema))
            .map_err(|e| e.to_string())?;

        let principal = uid("FerrisKey::User", subject_id)?;
        let resource = uid(resource_type, resource_id)?;
        let action = uid(
            &format!("{REALM_NAMESPACE}::Action"),
            text(&evaluation["action"], "name")?,
        )?;
        let context =
            Context::from_json_value(evaluation["context"].clone(), Some((schema, &action)))
                .map_err(|e| e.to_string())?;
        let request = Request::new(principal, action, resource, context, Some(schema))
            .map_err(|e| e.to_string())?;

        let response =
            CedarAuthorizer::new().is_authorized(&request, &self.rules.policies, &entities);
        if response.diagnostics().errors().next().is_some() {
            return Err(format!(
                "policy evaluation errors: {:?}",
                response.diagnostics()
            ));
        }
        Ok(response.decision() == CedarDecision::Allow)
    }
}

impl Default for CedarPolicyEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl Authorizer for CedarPolicyEngine {
    async fn evaluate(&self, request: &AccessRequest) -> Result<Decision, CoreError> {
        match self.allows(&evaluation_of(request)) {
            Ok(true) => Ok(Decision::Allow),
            Ok(false) => Ok(Decision::Deny {
                reason: format!(
                    "{} denied by the policy decision point",
                    request.action.as_str()
                ),
            }),
            Err(error) => Err(CoreError::Internal(error)),
        }
    }
}

fn load() -> Rules {
    let fragment = |source: &str| {
        SchemaFragment::from_cedarschema_str(source)
            .expect("a valid Cedar schema")
            .0
    };
    let schema = Schema::from_schema_fragments([fragment(BUILTIN_SCHEMA), fragment(REALM_SCHEMA)])
        .expect("schemas that compose");

    let directory =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../deploy/ferriskey/authz/policies");
    let mut files: Vec<PathBuf> = fs::read_dir(directory)
        .expect("the policies directory")
        .map(|entry| entry.expect("a directory entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "cedar"))
        .collect();
    files.sort();
    let mut policies = PolicySet::new();
    for path in files {
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .expect("a utf-8 file name");
        let source = fs::read_to_string(&path).expect("a readable policy");
        let policy = Policy::parse(Some(PolicyId::new(stem)), source).expect("a valid policy");
        policies.add(policy).expect("a unique policy id");
    }
    Rules { schema, policies }
}

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value[key]
        .as_str()
        .ok_or_else(|| format!("{key} is not a string"))
}

fn uid(ty: &str, id: &str) -> Result<EntityUid, String> {
    let ty = EntityTypeName::from_str(ty).map_err(|e| e.to_string())?;
    Ok(EntityUid::from_type_name_and_id(ty, EntityId::new(id)))
}
