use super::*;
use crazyagent::checks::profile::PROFILE_INPUT_BYTES_MAX;
use crazyagent::engine::EngineError;
use crazyagent::engine::owner::{
    CHECKS_CODES, ChecksReply, ChecksRequest, OwnerClient, TRACKER_CODES,
};
use crazyagent::protocol::ProjectId;
use serde_json::{Map, Value, json};
use std::sync::Arc;
use trackwright_core::catalog::{self, CURSOR_BYTES_MAX, FieldType};
use trackwright_core::model::{OpKind, ProjectKey, ShortId};
use trackwright_core::tests_model::TestId;

/// The most bytes a test read's arguments take as JSON: ids, filters and a cursor.
pub const TESTS_ARGS_BYTES_MAX: usize = 2 * 1024;
/// The most bytes a checks request takes as JSON, room for the largest profile text.
pub const CHECKS_REQUEST_BYTES_MAX: usize = 48 * 1024;
/// The checks operations the window sends. Holdout files never pass through the window.
pub const CHECKS_OPS: [&str; 11] = [
    "profile_propose",
    "profiles",
    "profile_show",
    "profile_approve",
    "profile_revoke",
    "settings_show",
    "settings_save",
    "run",
    "cancel",
    "runs",
    "gate",
];
/// The checks operations that also read or write the app's defaults, sent with no project.
pub const APP_CHECKS_OPS: [&str; 2] = ["settings_show", "settings_save"];

const _: () = assert!(PROFILE_INPUT_BYTES_MAX + 4096 <= CHECKS_REQUEST_BYTES_MAX);
const _: () = assert!(CURSOR_BYTES_MAX + 512 <= TESTS_ARGS_BYTES_MAX);

const TESTS_INVALID: &str = "tracker_request_invalid";
const TESTS_UNKNOWN: &str = "tracker_outcome_unknown";
const NOT_TRACKED: &str = "project_not_attached";
const CHECKS_INVALID: &str = "checks_request_invalid";
const CHECKS_BOUNDS: &str = "checks_request_bounds";

/// One test read's answer for the window: a page of the core's data with the cursor of the
/// next page, or the refusal's code. The refusal's message stays in the engine.
#[derive(Debug, PartialEq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum TestsReply {
    Page { data: Value, cursor: Option<String> },
    Refused { code: &'static str },
}

fn engine_code(
    known: &'static [&'static str],
    fallback: &'static str,
    error: EngineError,
) -> BridgeError {
    if error.0 == "controller_disconnected" {
        return failure("not_connected");
    }
    let code = known.iter().copied().find(|code| *code == error.0);
    caused(code.unwrap_or(fallback), &error.0)
}

fn tracker_error(error: EngineError) -> BridgeError {
    engine_code(TRACKER_CODES, TESTS_UNKNOWN, error)
}

fn checks_error(error: EngineError) -> BridgeError {
    engine_code(CHECKS_CODES, "checks_outcome_unknown", error)
}

/// Names the operations the Tests tab reads: the eight test reads and `roadmap`, whose
/// versions the coverage picker lists. Input: the operation's name. Output: the operation,
/// or `tracker_request_invalid` for any other name, a write among them.
pub fn test_read(op: &str) -> Result<OpKind, BridgeError> {
    let kind = OpKind::parse(op).ok_or_else(|| failure(TESTS_INVALID))?;
    if !(kind.is_test_read() || kind == OpKind::Roadmap) {
        return Err(failure(TESTS_INVALID));
    }
    assert!(!kind.is_write(), "the Tests tab only reads");
    Ok(kind)
}

/// Checks a test read's arguments before the project's key is known: an object of at most
/// `TESTS_ARGS_BYTES_MAX` bytes naming only the operation's own catalog fields, never
/// `project`, which the bridge sets. Inputs: the operation and the arguments. Output: the
/// fields, or `tracker_request_invalid`.
pub fn test_args(kind: OpKind, args: Value) -> Result<Map<String, Value>, BridgeError> {
    assert!(
        kind.is_test_read() || kind == OpKind::Roadmap,
        "{kind} is a Tests tab read"
    );
    let Value::Object(args) = args else {
        return Err(failure(TESTS_INVALID));
    };
    let bytes = serde_json::to_vec(&args).map_err(|_| failure(TESTS_INVALID))?;
    if bytes.len() > TESTS_ARGS_BYTES_MAX {
        return Err(failure(TESTS_INVALID));
    }
    let fields = catalog::spec(kind).fields;
    let listed = |name: &String| {
        fields
            .iter()
            .any(|field| field.name == name && field.ty != FieldType::Project)
    };
    if !args.keys().all(listed) {
        return Err(failure(TESTS_INVALID));
    }
    assert!(
        !args.contains_key("project"),
        "the window never names the project"
    );
    Ok(args)
}

fn same_key(value: &Value, ty: FieldType, key: &ProjectKey) -> bool {
    let Some(text) = value.as_str() else {
        return false;
    };
    match ty {
        FieldType::Test(object) => TestId::parse_of(text, object).is_ok_and(|id| id.key() == key),
        FieldType::Ticket => ShortId::parse(text).is_ok_and(|id| id.key() == key),
        _ => true,
    }
}

/// Binds checked test read fields to one project: every test or ticket id they name must carry
/// the project's key, an operation with a project field gets the key, and one without must
/// name its object by an id of this project. Inputs: the operation, the fields from
/// `test_args` and the key. Output: the request's fields, or None for an id of another
/// project or a read nothing ties to this one.
pub fn scoped(
    kind: OpKind,
    mut args: Map<String, Value>,
    key: &ProjectKey,
) -> Option<Map<String, Value>> {
    assert!(
        !args.contains_key("project"),
        "test_args kept the project out"
    );
    let fields = catalog::spec(kind).fields;
    if !fields.iter().all(|field| {
        args.get(field.name)
            .is_none_or(|value| same_key(value, field.ty, key))
    }) {
        return None;
    }
    let project = fields.iter().any(|field| field.ty == FieldType::Project);
    let named = fields.iter().any(|field| {
        field.name == "id" && matches!(field.ty, FieldType::Test(_)) && args.contains_key("id")
    });
    if !(project || named) {
        return None;
    }
    if project {
        args.insert("project".to_owned(), Value::String(key.to_string()));
    }
    assert_eq!(
        args.contains_key("project"),
        project,
        "only a project read names the key"
    );
    Some(args)
}

/// Reads the core's reply envelope as the window's answer. Input: the envelope. Output: the
/// page with its cursor, the refusal with a known code, or `tracker_outcome_unknown` for an
/// envelope of another shape.
pub fn tests_reply(envelope: Value) -> Result<TestsReply, BridgeError> {
    let Value::Object(mut envelope) = envelope else {
        return Err(failure(TESTS_UNKNOWN));
    };
    match envelope.get("ok") {
        Some(Value::Bool(true)) => {
            let cursor = match envelope.remove("cursor") {
                None | Some(Value::Null) => None,
                Some(Value::String(cursor)) if cursor.len() <= CURSOR_BYTES_MAX => Some(cursor),
                Some(_) => return Err(failure(TESTS_UNKNOWN)),
            };
            let data = envelope
                .remove("data")
                .ok_or_else(|| failure(TESTS_UNKNOWN))?;
            Ok(TestsReply::Page { data, cursor })
        }
        Some(Value::Bool(false)) => {
            let code = envelope.get("code").and_then(Value::as_str);
            let known = TRACKER_CODES
                .iter()
                .copied()
                .find(|known| Some(*known) == code);
            Ok(TestsReply::Refused {
                code: known.unwrap_or(TESTS_UNKNOWN),
            })
        }
        _ => Err(failure(TESTS_UNKNOWN)),
    }
}

/// Builds the checks request the window asked for: one of `CHECKS_OPS`, its arguments without
/// `project`, which the bridge sets from the page's project; only the settings operations go
/// without one, for the app's defaults. Inputs: the project id, the operation and its
/// arguments. Output: the request, `checks_request_bounds` past `CHECKS_REQUEST_BYTES_MAX`, or
/// `checks_request_invalid` for anything the protocol does not name.
pub fn checks_request(
    project_id: Option<String>,
    op: &str,
    args: Value,
) -> Result<ChecksRequest, BridgeError> {
    if !CHECKS_OPS.contains(&op) {
        return Err(failure(CHECKS_INVALID));
    }
    let Value::Object(mut args) = args else {
        return Err(failure(CHECKS_INVALID));
    };
    if args.contains_key("project") {
        return Err(failure(CHECKS_INVALID));
    }
    match project_id {
        Some(id) => {
            let project = ProjectId::new(id).map_err(|_| failure(CHECKS_INVALID))?;
            args.insert("project".to_owned(), json!(project));
        }
        None if APP_CHECKS_OPS.contains(&op) => {}
        None => return Err(failure(CHECKS_INVALID)),
    }
    let body = json!({ "op": op, "args": args });
    let bytes = serde_json::to_vec(&body).map_err(|_| failure(CHECKS_INVALID))?;
    if bytes.len() > CHECKS_REQUEST_BYTES_MAX {
        return Err(failure(CHECKS_BOUNDS));
    }
    let request: ChecksRequest =
        serde_json::from_value(body).map_err(|_| failure(CHECKS_INVALID))?;
    assert_eq!(
        serde_json::to_value(&request)
            .ok()
            .and_then(|value| value["op"].as_str().map(str::to_owned)),
        Some(op.to_owned()),
        "the request is the operation the window named"
    );
    Ok(request)
}

impl NativeBridge {
    fn checks_client(&self) -> Result<Arc<OwnerClient>, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        self.controller
            .as_ref()
            .ok_or_else(|| failure("not_connected"))?
            .shared_client()
            .map_err(checks_error)
    }

    /// Runs one Tests tab read as the owner, run without the bridge lock: the arguments are
    /// checked against the catalog, tied to the project's ticket key and sent to the tracker.
    /// Inputs: the project id, the operation and its arguments. Output: the page or the
    /// refusal; `project_not_attached` while the project has no tickets.
    pub fn tests_read(
        &self,
        project_id: String,
        op: String,
        args: Value,
    ) -> Result<Unlocked<TestsReply>, BridgeError> {
        let project = ProjectId::new(project_id).map_err(|_| failure(TESTS_INVALID))?;
        let kind = test_read(&op)?;
        let args = test_args(kind, args)?;
        let client = self.checks_client()?;
        Ok(Box::pin(async move {
            let tracked = client.tracker_projects().await.map_err(tracker_error)?;
            let mut keys = tracked.into_iter().filter(|row| row.project == project);
            let Some(row) = keys.next() else {
                return Ok(TestsReply::Refused { code: NOT_TRACKED });
            };
            if keys.next().is_some() {
                return Err(failure(TESTS_UNKNOWN));
            }
            let args = scoped(kind, args, &row.key).ok_or_else(|| failure(TESTS_INVALID))?;
            let envelope = client
                .tracker_op(kind, Value::Object(args))
                .await
                .map_err(tracker_error)?;
            tests_reply(envelope)
        }))
    }

    /// Sends one checks request as the owner, run without the bridge lock. Inputs: the
    /// project id, None for the app's defaults, the operation and its arguments. Output: the
    /// engine's reply, or a refusal named in `CHECKS_CODES`.
    pub fn checks(
        &self,
        project_id: Option<String>,
        op: String,
        args: Value,
    ) -> Result<Unlocked<ChecksReply>, BridgeError> {
        let request = checks_request(project_id, &op, args)?;
        let client = self.checks_client()?;
        Ok(Box::pin(async move {
            client.checks(request).await.map_err(checks_error)
        }))
    }
}
