use super::*;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

#[test]
fn imp1035_claude_commands_use_only_official_auth_subcommands() {
    let home = Path::new("/private/tmp/imp1035-profile");
    let temp = tempfile::tempdir().unwrap();
    let claude = std::fs::canonicalize(temp.path()).unwrap().join("claude");
    std::fs::write(&claude, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o700)).unwrap();
    for (action, expected) in [
        (ClaudeAuthCommand::Login, "login"),
        (ClaudeAuthCommand::Status, "status"),
        (ClaudeAuthCommand::Logout, "logout"),
    ] {
        let command = auth_command(&[claude.clone().into_os_string()], home, action).unwrap();
        let args = command
            .get_args()
            .map(|value| value.to_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(args, ["auth", expected]);
        assert_eq!(command.get_program(), claude.as_os_str());
        assert_eq!(command.get_current_dir(), Some(home));
        let variables = command
            .get_envs()
            .map(|(name, value)| {
                (
                    name.to_str().unwrap(),
                    value.map(|value| value.to_str().unwrap()),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(
            variables.get("CLAUDE_CONFIG_DIR"),
            Some(&Some(home.to_str().unwrap()))
        );
        assert_eq!(variables.get("HOME"), Some(&Some(home.to_str().unwrap())));
        assert_eq!(variables.get("TMPDIR"), Some(&Some(home.to_str().unwrap())));
        assert!(!variables.contains_key("ANTHROPIC_API_KEY"));
        assert!(!variables.contains_key("CLAUDE_CODE_OAUTH_TOKEN"));
    }
}

/// Every Claude Code auth child starts the launch words its custodian was handed, an
/// interpreter and its first-line words before the script, never resolving a name itself.
#[test]
fn a_claude_auth_child_starts_the_handed_words_as_given() {
    let home = Path::new("/private/tmp/imp1035-profile");
    let words: Vec<std::ffi::OsString> = vec![
        "/bin/sh".into(),
        "--flag".into(),
        "/private/tmp/cli.js".into(),
    ];
    for action in [
        ClaudeAuthCommand::Login,
        ClaudeAuthCommand::Status,
        ClaudeAuthCommand::Logout,
        ClaudeAuthCommand::Renew,
    ] {
        let command = auth_command(&words, home, action).unwrap();
        assert_eq!(command.get_program(), "/bin/sh");
        let args: Vec<_> = command.get_args().take(2).collect();
        assert_eq!(args, ["--flag", "/private/tmp/cli.js"]);
    }
}

#[test]
fn imp1035_claude_command_rejects_relative_executable_and_profile() {
    let profile = Path::new("/private/tmp/profile");
    assert!(auth_command(&["claude".into()], profile, ClaudeAuthCommand::Login).is_err());
    assert!(
        auth_command(
            &["/bin/sh".into(), "cli.js".into()],
            profile,
            ClaudeAuthCommand::Login
        )
        .is_err()
    );
    assert!(
        auth_command(
            &["/private/tmp/claude".into()],
            Path::new("profile"),
            ClaudeAuthCommand::Login
        )
        .is_err()
    );
}

fn status(logged_in: bool, auth_method: &str, home: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "loggedIn": logged_in,
        "authMethod": auth_method,
        "apiProvider": "firstParty",
        "configDirectory": home,
        "email": null,
        "orgId": null,
        "orgName": null,
        "subscriptionType": null
    }))
    .unwrap()
}

#[test]
fn imp1035_claude_status_requires_matching_held_profile_and_subscription_method() {
    let home = Path::new("/private/tmp/held-claude-profile");
    let account = verified_claude_account(&status(true, "claude.ai", home.to_str().unwrap()), home)
        .unwrap()
        .unwrap();
    assert!(account.logged_in);
    assert_eq!(account.config_directory.as_str(), home.to_str().unwrap());
    assert!(
        verified_claude_account(
            &status(true, "claude.ai", "/private/tmp/other-profile"),
            home
        )
        .is_err()
    );
    for method in [
        "api_key",
        "oauth_token",
        "api_key_helper",
        "third_party",
        "unknown",
    ] {
        assert!(
            verified_claude_account(&status(true, method, home.to_str().unwrap()), home).is_err()
        );
    }
}

#[test]
fn imp1035_claude_logged_out_status_does_not_observe_authenticated_account() {
    let home = Path::new("/private/tmp/held-claude-profile");
    assert!(
        verified_claude_account(&status(false, "none", home.to_str().unwrap()), home)
            .unwrap()
            .is_none()
    );
}

#[test]
fn imp1035_claude_missing_status_identity_and_oversized_output_fail_closed() {
    let home = Path::new("/private/tmp/held-claude-profile");
    let mut value: serde_json::Value =
        serde_json::from_slice(&status(true, "claude.ai", home.to_str().unwrap())).unwrap();
    value.as_object_mut().unwrap().remove("configDirectory");
    assert!(verified_claude_account(&serde_json::to_vec(&value).unwrap(), home).is_err());
    assert!(verified_claude_account(b"", home).is_err());
    assert!(verified_claude_account(&vec![b' '; 65_537], home).is_err());
}

/// DEC-166: Claude Code before 2.1.268 answers `auth status` without `configDirectory`, so
/// that status reads as a harness too old to sign in, while a status missing anything else,
/// or not a status at all, stays an invalid answer.
#[test]
fn a_claude_status_without_its_config_directory_reads_as_too_old_to_sign_in() {
    let home = Path::new("/private/tmp/held-claude-profile");
    let without = |field: &str| {
        let mut value: serde_json::Value =
            serde_json::from_slice(&status(true, "claude.ai", home.to_str().unwrap())).unwrap();
        value.as_object_mut().unwrap().remove(field);
        serde_json::to_vec(&value).unwrap()
    };
    assert_eq!(
        verified_claude_account(&without("configDirectory"), home)
            .unwrap_err()
            .0,
        "native_auth_harness_too_old"
    );
    let mut logged_out: serde_json::Value =
        serde_json::from_slice(&status(false, "none", home.to_str().unwrap())).unwrap();
    logged_out.as_object_mut().unwrap().remove("configDirectory");
    assert_eq!(
        verified_claude_account(&serde_json::to_vec(&logged_out).unwrap(), home)
            .unwrap_err()
            .0,
        "native_auth_harness_too_old"
    );
    for field in ["loggedIn", "authMethod", "apiProvider"] {
        assert_eq!(
            verified_claude_account(&without(field), home).unwrap_err().0,
            "native_auth_claude_status_invalid",
            "{field}"
        );
    }
    for other in [&b""[..], b"[]", b"{}", b"not json"] {
        assert_eq!(
            verified_claude_account(other, home).unwrap_err().0,
            "native_auth_claude_status_invalid"
        );
    }
    assert_eq!(
        ClaudeAuthFailure::HarnessTooOld.code(),
        "native_auth_harness_too_old"
    );
    assert!(verified_claude_account(b"", home).is_err());
    assert!(verified_claude_account(&vec![b' '; 65_537], home).is_err());
}

/// v2.4.0 owner update: an account check's `claude auth status` ended
/// `native_auth_claude_deadline` while the machine was under build load, at a 2 s bound inside
/// the check's own 9 s. A status may now use the whole check bound; logout keeps its short one.
#[test]
fn a_status_command_may_use_the_whole_account_check_bound() {
    assert_eq!(
        process::command_bound(ClaudeAuthCommand::Status),
        CHECK_TIME
    );
    assert!(process::command_bound(ClaudeAuthCommand::Status) > Duration::from_secs(2));
    assert_eq!(
        process::command_bound(ClaudeAuthCommand::Logout),
        Duration::from_secs(2)
    );
    assert_eq!(
        process::command_bound(ClaudeAuthCommand::Renew),
        RENEWAL_TIME
    );
    assert_eq!(
        process::command_bound(ClaudeAuthCommand::Login),
        Duration::from_secs(575)
    );
}
