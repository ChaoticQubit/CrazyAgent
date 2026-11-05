use crazyagent::engine::owner::{CHECKS_CODES, ChecksRequest};
use crazyagent::protocol::ProjectId;
use crazyagent_desktop::bridge::checks::{
    APP_CHECKS_OPS, CHECKS_OPS, CHECKS_REQUEST_BYTES_MAX, TESTS_ARGS_BYTES_MAX, TestsReply,
    checks_request, scoped, test_args, test_read, tests_reply,
};
use serde_json::{Map, Value, json};
use trackwright_core::catalog::{self, FieldType};
use trackwright_core::model::{OpKind, ProjectKey};

const COMMANDS: [&str; 2] = ["desktop_tests_read", "desktop_checks"];
const PROJECT: &str = "project-tests";
const PROFILE: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const CHANGE_SET: &str = "0123456789abcdef0123456789abcdef";

#[test]
fn checks_commands_are_registered_in_all_four_places() {
    let build = include_str!("../build.rs");
    let bindings = include_str!("../src/lib.rs");
    let capability = include_str!("../capabilities/main-window.json");
    let window = include_str!("../../src/api-checks.ts");
    for command in COMMANDS {
        assert_eq!(
            build.matches(&format!("\"{command}\"")).count(),
            1,
            "{command} build"
        );
        assert!(
            bindings.contains(&format!("async fn {command}(")),
            "{command} binding"
        );
        assert_eq!(
            bindings
                .lines()
                .filter(|line| line.trim() == format!("{command},"))
                .count(),
            1,
            "{command} handler"
        );
        let permission = format!("\"allow-{}\"", command.replace('_', "-"));
        assert_eq!(
            capability.matches(&permission).count(),
            1,
            "{command} capability"
        );
        assert_eq!(
            window.matches(&format!("\"{command}\"")).count(),
            1,
            "{command} window"
        );
    }
}

/// The parameters of one command binding in `lib.rs`, one per line as rustfmt writes them.
fn parameters(command: &str) -> Vec<String> {
    let bindings = include_str!("../src/lib.rs");
    let start = bindings
        .find(&format!("async fn {command}("))
        .unwrap_or_else(|| panic!("{command} binding"));
    let rest = &bindings[start..];
    let signature = &rest[..rest.find(") -> ").expect("a return type")];
    signature
        .lines()
        .skip(1)
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

#[test]
fn the_window_passes_the_project_the_operation_and_its_arguments_only() {
    let window = ["window: WebviewWindow,", "state: State<'_, NativeState>,"];
    let read = [
        "project_id: String,",
        "op: String,",
        "args: serde_json::Value,",
    ];
    assert_eq!(
        parameters("desktop_tests_read"),
        [&window[..], &read[..]].concat()
    );
    let checks = [
        "project_id: Option<String>,",
        "op: String,",
        "args: serde_json::Value,",
    ];
    assert_eq!(
        parameters("desktop_checks"),
        [&window[..], &checks[..]].concat()
    );
}

fn key() -> ProjectKey {
    ProjectKey::new("CA").expect("a key")
}

fn code<T: std::fmt::Debug>(
    result: Result<T, crazyagent_desktop::bridge::BridgeError>,
) -> &'static str {
    result.expect_err("refused").code
}

fn object(value: Value) -> Map<String, Value> {
    let Value::Object(map) = value else {
        panic!("an object")
    };
    map
}

#[test]
fn the_tests_tab_reads_only_test_reads_and_the_roadmap() {
    for kind in OpKind::ALL {
        let read = kind.is_test_read() || *kind == OpKind::Roadmap;
        assert_eq!(test_read(kind.as_str()).is_ok(), read, "{kind}");
        if !read {
            assert_eq!(
                code(test_read(kind.as_str())),
                "tracker_request_invalid",
                "{kind}"
            );
        }
    }
    assert_eq!(code(test_read("run_record")), "tracker_request_invalid");
    assert_eq!(code(test_read("")), "tracker_request_invalid");
    assert_eq!(test_read("run_show").expect("a read"), OpKind::RunShow);
}

#[test]
fn only_catalog_fields_pass_and_the_project_is_the_bridges() {
    for kind in OpKind::ALL
        .iter()
        .copied()
        .filter(|kind| test_read(kind.as_str()).is_ok())
    {
        for field in catalog::spec(kind).fields {
            let args = json!({ field.name: "x" });
            let passed = test_args(kind, args);
            assert_eq!(
                passed.is_ok(),
                field.ty != FieldType::Project,
                "{kind} {}",
                field.name
            );
        }
        assert_eq!(
            code(test_args(kind, json!({ "owner": true }))),
            "tracker_request_invalid"
        );
        assert_eq!(
            code(test_args(kind, json!(["id"]))),
            "tracker_request_invalid"
        );
        assert_eq!(
            code(test_args(kind, Value::Null)),
            "tracker_request_invalid"
        );
    }
    let long = "a".repeat(TESTS_ARGS_BYTES_MAX);
    assert_eq!(
        code(test_args(OpKind::Coverage, json!({ "release": long }))),
        "tracker_request_invalid"
    );
    let filters = json!({ "verdict": "failed", "batch": "b1", "commit": "c", "cursor": "run:2" });
    assert_eq!(
        test_args(OpKind::RunList, filters.clone()).expect("filters"),
        object(filters)
    );
}

#[test]
fn every_id_must_carry_the_projects_key() {
    let key = key();
    let run = |id: &str| scoped(OpKind::RunShow, object(json!({ "id": id })), &key);
    assert_eq!(
        run("CA-R7"),
        Some(object(json!({ "id": "CA-R7" }))),
        "a show names no project"
    );
    assert_eq!(run("XY-R7"), None, "a run of another project");
    assert_eq!(run("CA-T7"), None, "a case id is not a run id");
    assert_eq!(run("CA-7"), None, "a ticket id is not a run id");
    assert_eq!(
        scoped(OpKind::RunShow, Map::new(), &key),
        None,
        "a show must name its run"
    );
    let cases = scoped(OpKind::CaseList, object(json!({ "set": "CA-S1" })), &key);
    assert_eq!(
        cases,
        Some(object(json!({ "set": "CA-S1", "project": "CA" })))
    );
    assert_eq!(
        scoped(OpKind::CaseList, object(json!({ "set": "XY-S1" })), &key),
        None
    );
    assert_eq!(
        scoped(OpKind::RunList, object(json!({ "plan": "XY-P1" })), &key),
        None
    );
    assert_eq!(
        scoped(OpKind::Coverage, object(json!({ "ticket": "XY-3" })), &key),
        None
    );
    let coverage = scoped(OpKind::Coverage, object(json!({ "ticket": "CA-3" })), &key);
    assert_eq!(
        coverage,
        Some(object(json!({ "ticket": "CA-3", "project": "CA" })))
    );
    let roadmap = scoped(OpKind::Roadmap, Map::new(), &key);
    assert_eq!(roadmap, Some(object(json!({ "project": "CA" }))));
    assert_eq!(
        scoped(OpKind::RunShow, object(json!({ "id": 7 })), &key),
        None
    );
}

#[test]
fn the_core_envelope_becomes_a_page_or_a_known_code() {
    let page =
        json!({ "api": 1, "ok": true, "data": { "runs": [] }, "next": [], "cursor": "run:3" });
    assert_eq!(
        tests_reply(page).expect("a page"),
        TestsReply::Page {
            data: json!({ "runs": [] }),
            cursor: Some("run:3".to_owned())
        }
    );
    let last = json!({ "api": 1, "ok": true, "data": {}, "next": [], "cursor": null });
    assert_eq!(
        tests_reply(last).expect("a page"),
        TestsReply::Page {
            data: json!({}),
            cursor: None
        }
    );
    let long = json!({ "api": 1, "ok": true, "data": {}, "cursor": "c".repeat(65) });
    assert_eq!(code(tests_reply(long)), "tracker_outcome_unknown");
    assert_eq!(
        code(tests_reply(json!({ "ok": true }))),
        "tracker_outcome_unknown"
    );
    assert_eq!(
        code(tests_reply(json!({ "ok": "yes" }))),
        "tracker_outcome_unknown"
    );
    assert_eq!(code(tests_reply(json!([]))), "tracker_outcome_unknown");
    let refused =
        json!({ "api": 1, "ok": false, "code": "cursor_invalid", "message": "secret\u{202e}" });
    let reply = tests_reply(refused).expect("a refusal");
    assert_eq!(
        reply,
        TestsReply::Refused {
            code: "cursor_invalid"
        }
    );
    let shown = serde_json::to_string(&reply).expect("json");
    assert_eq!(
        shown, r#"{"outcome":"refused","code":"cursor_invalid"}"#,
        "the message stays out"
    );
    let odd = json!({ "api": 1, "ok": false, "code": "made_up_code", "message": "" });
    assert_eq!(
        tests_reply(odd).expect("a refusal"),
        TestsReply::Refused {
            code: "tracker_outcome_unknown"
        }
    );
}

fn window_args(op: &str) -> Value {
    match op {
        "profile_propose" => json!({ "profile": "{}" }),
        "profiles" => json!({ "after": 3 }),
        "profile_show" | "profile_approve" | "profile_revoke" => json!({ "id": PROFILE }),
        "settings_show" => json!({}),
        "settings_save" => json!({ "values": { "gate": "warn", "parallel": 2 }, "version": 4 }),
        "run" => json!({ "change_set": CHANGE_SET, "checks": [] }),
        "cancel" | "runs" | "gate" => json!({ "change_set": CHANGE_SET }),
        other => panic!("{other} has window arguments"),
    }
}

#[test]
fn every_checks_op_the_window_sends_builds_its_request() {
    for op in CHECKS_OPS {
        let request = checks_request(Some(PROJECT.to_owned()), op, window_args(op)).expect(op);
        let sent = serde_json::to_value(&request).expect("json");
        assert_eq!(sent["op"], op);
        assert_eq!(
            sent["args"]["project"], PROJECT,
            "{op} names the page's project"
        );
        let app = checks_request(None, op, window_args(op));
        assert_eq!(
            app.is_ok(),
            APP_CHECKS_OPS.contains(&op),
            "{op} without a project"
        );
    }
    let app = checks_request(None, "settings_show", json!({})).expect("app settings");
    assert_eq!(app, ChecksRequest::SettingsShow { project: None });
    let project = ProjectId::new(PROJECT).expect("an id");
    let shown = checks_request(Some(PROJECT.to_owned()), "settings_show", json!({}));
    assert_eq!(
        shown.expect("project settings"),
        ChecksRequest::SettingsShow {
            project: Some(project)
        }
    );
}

#[test]
fn the_window_sends_exactly_the_bridges_checks_ops() {
    let window = include_str!("../../src/api-checks.ts");
    let start = window
        .find("export type ChecksOp =")
        .expect("the window's op list");
    let line = window[start..].lines().next().expect("one line");
    let sent: Vec<&str> = line.split('"').skip(1).step_by(2).collect();
    assert_eq!(
        sent, CHECKS_OPS,
        "the window and the bridge list the same ops"
    );
}

#[test]
fn anything_the_protocol_does_not_name_is_refused() {
    let project = || Some(PROJECT.to_owned());
    let refused = |result| code(result);
    assert_eq!(
        refused(checks_request(
            project(),
            "holdout_store",
            json!({ "base64": "AA==" })
        )),
        "checks_request_invalid",
        "holdout bytes never pass through the window"
    );
    assert_eq!(
        refused(checks_request(
            project(),
            "runs",
            json!({ "change_set": CHANGE_SET, "project": "other" })
        )),
        "checks_request_invalid"
    );
    assert_eq!(
        refused(checks_request(
            project(),
            "runs",
            json!({ "change_set": CHANGE_SET, "owner": true })
        )),
        "checks_request_invalid"
    );
    assert_eq!(
        refused(checks_request(project(), "runs", json!({}))),
        "checks_request_invalid"
    );
    assert_eq!(
        refused(checks_request(project(), "runs", json!([CHANGE_SET]))),
        "checks_request_invalid"
    );
    assert_eq!(
        refused(checks_request(
            project(),
            "profile_show",
            json!({ "id": "short" })
        )),
        "checks_request_invalid"
    );
    assert_eq!(
        refused(checks_request(
            Some("../x".to_owned()),
            "runs",
            window_args("runs")
        )),
        "checks_request_invalid"
    );
    assert_eq!(
        refused(checks_request(
            project(),
            "settings_save",
            json!({ "values": { "gate": "maybe" }, "version": 1 })
        )),
        "checks_request_invalid"
    );
    let large = "x".repeat(CHECKS_REQUEST_BYTES_MAX);
    assert_eq!(
        refused(checks_request(
            project(),
            "profile_propose",
            json!({ "profile": large })
        )),
        "checks_request_bounds"
    );
}

/// The keys of the `Record<string, string>` copy tables in a window source: lines of two
/// spaces, a lowercase code and a quoted or template sentence.
fn copy_keys(source: &str) -> Vec<&str> {
    source
        .lines()
        .filter_map(|line| {
            let entry = line.strip_prefix("  ")?;
            let (key, _) = entry
                .split_once(": \"")
                .or_else(|| entry.split_once(": `"))?;
            Some(key)
        })
        .filter(|key| {
            key.bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
        })
        .collect()
}

#[test]
fn every_code_the_bridge_can_return_has_a_sentence() {
    let sources = [
        include_str!("../../src/api-checks.ts"),
        include_str!("../../src/api-questions.ts"),
        include_str!("../../src/api-changes.ts"),
        include_str!("../../src/api.ts"),
        include_str!("../../src/api-projects.ts"),
    ];
    let known: Vec<&str> = sources
        .iter()
        .flat_map(|source| copy_keys(source))
        .collect();
    assert!(
        known.len() > CHECKS_CODES.len(),
        "the copy tables were read"
    );
    let bridge = [
        "not_connected",
        "tracker_request_invalid",
        "tracker_outcome_unknown",
        "project_not_attached",
        "checks_request_invalid",
        "checks_request_bounds",
        "checks_outcome_unknown",
        "cursor_invalid",
    ];
    for code in CHECKS_CODES.iter().chain(bridge.iter()) {
        assert!(known.contains(code), "{code} has its own sentence");
    }
}
