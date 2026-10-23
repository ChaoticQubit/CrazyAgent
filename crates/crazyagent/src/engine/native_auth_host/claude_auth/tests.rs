use super::*;
use std::path::Path;

#[test]
fn imp1035_claude_commands_use_only_official_auth_subcommands() {
    let home = Path::new("/private/tmp/imp1035-profile");
    for (action, expected) in [
        (ClaudeAuthCommand::Login, "login"),
        (ClaudeAuthCommand::Status, "status"),
        (ClaudeAuthCommand::Logout, "logout"),
    ] {
        let command =
            auth_command(Path::new("/private/tmp/official-claude"), home, action).unwrap();
        let args = command
            .get_args()
            .map(|value| value.to_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(args, ["auth", expected]);
        assert_eq!(command.get_program(), "/private/tmp/official-claude");
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

#[test]
fn imp1035_claude_command_rejects_relative_executable_and_profile() {
    assert!(
        auth_command(
            Path::new("claude"),
            Path::new("/private/tmp/profile"),
            ClaudeAuthCommand::Login
        )
        .is_err()
    );
    assert!(
        auth_command(
            Path::new("/private/tmp/claude"),
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
