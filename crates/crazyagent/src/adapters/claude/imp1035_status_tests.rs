use crate::adapters::claude::{AuthMethod, decode_auth_status};

#[test]
fn imp1035_actual_claude_decoder_keeps_each_config_directory_exact() {
    for home in ["/private/tmp/imp1035-account-a", "/private/tmp/imp1035-account-b"] {
        let bytes = serde_json::to_vec(&serde_json::json!({
            "loggedIn": true,
            "authMethod": "claude.ai",
            "apiProvider": "firstParty",
            "configDirectory": home,
            "email": "synthetic-owner@example.invalid",
            "orgId": "synthetic-org",
            "orgName": "synthetic-organization",
            "subscriptionType": "synthetic-subscription"
        })).unwrap();
        let status = decode_auth_status(&bytes).unwrap();
        assert!(status.logged_in);
        assert_eq!(status.auth_method, AuthMethod::ClaudeAi);
        assert_eq!(status.config_directory.as_str(), home);
        let debug = format!("{status:?}");
        for private in [home, "synthetic-owner@example.invalid", "synthetic-org",
            "synthetic-organization", "synthetic-subscription"]
        {
            assert!(!debug.contains(private));
        }
    }
}

#[test]
fn imp1035_actual_claude_decoder_rejects_missing_duplicate_or_invalid_profile_identity() {
    for bytes in [
        br#"{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty"}"#.as_slice(),
        br#"{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty","configDirectory":null}"#.as_slice(),
        br#"{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty","configDirectory":"/private/tmp/a","configDirectory":"/private/tmp/b"}"#.as_slice(),
    ] {
        assert!(decode_auth_status(bytes).is_err());
    }
}

#[test]
fn imp1035_actual_claude_decoder_does_not_promote_logged_out_or_api_key_method() {
    let logged_out = decode_auth_status(
        br#"{"loggedIn":false,"authMethod":"none","apiProvider":"firstParty","configDirectory":"/private/tmp/a"}"#,
    ).unwrap();
    assert!(!logged_out.logged_in);
    assert_eq!(logged_out.auth_method, AuthMethod::None);
    let api_key = decode_auth_status(
        br#"{"loggedIn":true,"authMethod":"api_key","apiProvider":"firstParty","configDirectory":"/private/tmp/b"}"#,
    ).unwrap();
    assert_eq!(api_key.auth_method, AuthMethod::ApiKey);
    assert_eq!(api_key.config_directory.as_str(), "/private/tmp/b");
}
