use super::{APP_PLUGINS_CODES, Action, parse};
use kernel_plugin_host::usage::TurnOff;
use std::path::PathBuf;

fn code<T: std::fmt::Debug>(result: Result<T, crate::engine::EngineError>) -> String {
    result.unwrap_err().0
}

fn folder() -> String {
    if cfg!(windows) {
        r"C:\\Users\\owner\\acme".into()
    } else {
        "/Users/owner/acme".into()
    }
}

/// Each action reads from its JSON shape; a turn off may carry a one-time choice of what
/// happens to runs holding the plugin, and without one it follows the owner's setting.
#[test]
fn every_action_reads_from_its_json_shape() {
    let folder = folder();
    let cases = [
        (r#"{"kind":"list"}"#.to_string(), Action::List {}),
        (
            format!(r#"{{"kind":"preview","folder":"{folder}"}}"#),
            Action::Preview {
                folder: PathBuf::from(folder.replace(r"\\", r"\")),
            },
        ),
        (
            format!(r#"{{"kind":"add","folder":"{folder}","sha256":"ab","replace":false}}"#),
            Action::Add {
                folder: PathBuf::from(folder.replace(r"\\", r"\")),
                sha256: "ab".into(),
                replace: false,
            },
        ),
        (
            r#"{"kind":"set_enabled","name":"acme","enabled":false,"when_off":"stop_runs"}"#
                .to_string(),
            Action::SetEnabled {
                name: "acme".into(),
                enabled: false,
                when_off: Some(TurnOff::StopRuns),
            },
        ),
        (
            r#"{"kind":"set_enabled","name":"acme","enabled":false}"#.to_string(),
            Action::SetEnabled {
                name: "acme".into(),
                enabled: false,
                when_off: None,
            },
        ),
        (
            r#"{"kind":"users","name":"acme"}"#.to_string(),
            Action::Users {
                name: "acme".into(),
            },
        ),
        (
            r#"{"kind":"set_when_off","when_off":"stop_runs"}"#.to_string(),
            Action::SetWhenOff {
                when_off: TurnOff::StopRuns,
            },
        ),
    ];
    for (action, expected) in cases {
        let body = format!(r#"{{"app_plugins":{action},"secret_present":false}}"#);
        assert_eq!(
            parse(body.as_bytes()).unwrap(),
            (expected, false),
            "{action}"
        );
    }
}

/// A key frame is announced by `set_key` and by nothing else, and `set_key` must
/// announce it.
#[test]
fn only_set_key_announces_a_key_frame() {
    let set_key = r#"{"app_plugins":{"kind":"set_key","name":"acme","secret":"api_key"},"secret_present":true}"#;
    assert!(parse(set_key.as_bytes()).unwrap().1);
    for body in [
        r#"{"app_plugins":{"kind":"set_key","name":"acme","secret":"api_key"},"secret_present":false}"#,
        r#"{"app_plugins":{"kind":"list"},"secret_present":true}"#,
    ] {
        assert_eq!(code(parse(body.as_bytes())), "app_plugin_request_invalid");
    }
}

/// Empty or long names, an empty or long account, a relative folder, an unknown field and
/// an unknown action are refused before the engine runs anything; an oversized body is
/// refused for its size.
#[test]
fn bad_requests_are_refused_before_anything_runs() {
    let long = "a".repeat(65);
    let account = "a".repeat(129);
    let bodies = [
        r#"{"kind":"review","name":""}"#.to_string(),
        format!(r#"{{"kind":"remove","name":"{long}"}}"#),
        format!(r#"{{"kind":"bind_account","name":"acme","secret":"k","account":"{account}"}}"#),
        r#"{"kind":"bind_account","name":"acme","secret":"k","account":""}"#.to_string(),
        r#"{"kind":"preview","folder":"relative/acme"}"#.to_string(),
        r#"{"kind":"add","folder":"acme","sha256":"ab","replace":true}"#.to_string(),
        r#"{"kind":"list","extra":1}"#.to_string(),
        r#"{"kind":"search"}"#.to_string(),
        r#"{"kind":"set_when_off"}"#.to_string(),
        r#"{"kind":"set_when_off","when_off":"later"}"#.to_string(),
    ];
    for action in bodies {
        let body = format!(r#"{{"app_plugins":{action},"secret_present":false}}"#);
        assert_eq!(
            code(parse(body.as_bytes())),
            "app_plugin_request_invalid",
            "{action}"
        );
    }
    let huge = vec![b' '; crate::protocol::FRAME_BYTES_MAX + 1];
    assert_eq!(code(parse(&huge)), "app_plugin_request_bounds");
}

/// Every refusal this protocol's own code can answer is listed for the clients.
#[test]
fn the_protocol_refusals_are_listed_for_the_clients() {
    for listed in [
        "app_plugin_request_invalid",
        "app_plugin_request_bounds",
        "app_plugin_key_invalid",
        "app_plugin_outcome_unknown",
        "app_plugins_readonly",
        "app_plugin_essential",
        "app_plugin_past_cap",
        "app_plugin_port_handed_out",
        "app_plugin_setting_unavailable",
        "app_plugin_setting_unreadable",
        "app_plugin_setting_unsafe",
    ] {
        assert!(APP_PLUGINS_CODES.contains(&listed), "{listed}");
    }
    let mut sorted = APP_PLUGINS_CODES.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted, APP_PLUGINS_CODES);
}

/// An App plugins change holds the owner barrier until storage answers it, so a revocation
/// sent while the change waits on storage waits for it.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_revocation_waits_for_an_app_plugin_change_in_flight() {
    use super::super::barrier_tests::{revocation, storage_settled};
    use super::super::usage_tests::{IN_FLIGHT, Usage, busy_storage, hold_storage};
    use crate::engine::owner::OwnerClientKind;
    use std::time::Duration;
    let settled = Duration::from_secs(60);
    let _disk = busy_storage().await;
    let owner = Usage::start_waiting(30_000).await;
    let runtime = owner.runtime();
    let release = hold_storage(&runtime.engine.storage.sender);
    let mut changed = Box::pin(owner.client.app_plugin_set_enabled(
        "held-plugin".into(),
        false,
        None,
    ));
    assert!(
        tokio::time::timeout(IN_FLIGHT, &mut changed).await.is_err(),
        "the change waits behind the busy storage"
    );
    assert!(
        runtime.engine.owner_egress.try_write().is_err(),
        "the change holds the owner barrier while it waits"
    );
    let control = revocation(runtime, OwnerClientKind::Tui, "revoke-during-app-plugin");
    let mut revoke = Box::pin(runtime.control(control.clone()));
    assert!(
        tokio::time::timeout(IN_FLIGHT, &mut revoke).await.is_err(),
        "a revocation waits for the change already in flight"
    );
    release.send(()).unwrap();
    let _ = tokio::time::timeout(settled, changed).await.unwrap();
    let revoked = match tokio::time::timeout(settled, revoke).await.unwrap() {
        Ok(reply) => reply,
        Err(error) => {
            assert_eq!(error.0, "access_change_pending");
            storage_settled(&owner).await;
            runtime.reconcile_control(control).await.unwrap().unwrap()
        }
    };
    assert!(
        !revoked.active,
        "the revocation committed once the change was answered"
    );
    assert!(
        !runtime
            .active_kinds()
            .unwrap()
            .contains(&OwnerClientKind::Tui),
        "the owner state holds the TUI client revoked"
    );
    assert!(
        owner.client.app_plugins().await.is_err(),
        "the revoked client is refused"
    );
    owner.stop().await;
}
