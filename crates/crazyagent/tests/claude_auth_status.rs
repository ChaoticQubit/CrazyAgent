use crazyagent::adapters::claude::{
    AuthMethod, AuthStatus, AuthStatusField, ProtocolError, decode_auth_status,
};
use serde_json::{Value, json};

fn status() -> Value {
    json!({
        "loggedIn": true,
        "authMethod": "claude.ai",
        "apiProvider": "firstParty",
        "configDirectory": "/isolated/claude-profile"
    })
}

fn decode(value: Value) -> AuthStatus {
    decode_auth_status(&serde_json::to_vec(&value).unwrap()).unwrap()
}

fn invalid(value: Value) {
    assert_eq!(
        decode_auth_status(&serde_json::to_vec(&value).unwrap()).unwrap_err(),
        ProtocolError::InvalidMessage
    );
}

#[test]
fn subscription_stdout_preserves_supported_observed_fields() {
    let observed = decode(json!({
        "loggedIn": true,
        "authMethod": "claude.ai",
        "apiProvider": "firstParty",
        "configDirectory": "/isolated/subscription-profile",
        "analyticsDisabled": true,
        "email": "subscriber@example.invalid",
        "orgId": "observed-org",
        "orgName": "Observed Organization",
        "subscriptionType": "max"
    }));
    assert!(observed.logged_in);
    assert_eq!(observed.auth_method, AuthMethod::ClaudeAi);
    assert_eq!(observed.api_provider.as_str(), "firstParty");
    assert_eq!(
        observed.config_directory.as_str(),
        "/isolated/subscription-profile"
    );
    for (field, expected) in [
        (&observed.email, "subscriber@example.invalid"),
        (&observed.org_id, "observed-org"),
        (&observed.org_name, "Observed Organization"),
        (&observed.subscription_type, "max"),
    ] {
        let AuthStatusField::Value(text) = field else {
            panic!("expected an observed string");
        };
        assert_eq!(text.as_str(), expected);
    }
}

#[test]
fn logged_out_false_is_preserved() {
    let mut value = status();
    value["loggedIn"] = json!(false);
    value["authMethod"] = json!("none");
    let observed = decode(value);
    assert!(!observed.logged_in);
    assert_eq!(observed.auth_method, AuthMethod::None);
}

#[test]
fn supported_methods_are_facts_independent_of_logged_in() {
    for (wire, expected) in [
        ("none", AuthMethod::None),
        ("claude.ai", AuthMethod::ClaudeAi),
        ("api_key", AuthMethod::ApiKey),
        ("oauth_token", AuthMethod::OauthToken),
        ("api_key_helper", AuthMethod::ApiKeyHelper),
        ("third_party", AuthMethod::ThirdParty),
    ] {
        let mut value = status();
        value["loggedIn"] = json!(false);
        value["authMethod"] = json!(wire);
        let observed = decode(value);
        assert!(!observed.logged_in);
        assert_eq!(observed.auth_method, expected);
    }
}

#[test]
fn unknown_method_is_explicit_and_preserves_observed_text() {
    let mut value = status();
    value["authMethod"] = json!("future-auth-method");
    let observed = decode(value);
    let AuthMethod::Unknown(method) = observed.auth_method else {
        panic!("unknown method was reclassified");
    };
    assert_eq!(method.as_str(), "future-auth-method");
    assert!(!format!("{method:?}").contains("future-auth-method"));
}

#[test]
fn provider_is_observed_text_without_supported_backend_inference() {
    let mut value = status();
    value["apiProvider"] = json!("future-provider");
    let observed = decode(value);
    assert_eq!(observed.api_provider.as_str(), "future-provider");
    assert_eq!(observed.auth_method, AuthMethod::ClaudeAi);
}

#[test]
fn absent_subscription_fields_remain_missing() {
    let observed = decode(status());
    for field in [
        observed.email,
        observed.org_id,
        observed.org_name,
        observed.subscription_type,
    ] {
        assert_eq!(field, AuthStatusField::Missing);
    }
}

#[test]
fn nullable_subscription_fields_remain_null() {
    let mut value = status();
    for key in ["email", "orgId", "orgName", "subscriptionType"] {
        value[key] = Value::Null;
    }
    let observed = decode(value);
    for field in [
        observed.email,
        observed.org_id,
        observed.org_name,
        observed.subscription_type,
    ] {
        assert_eq!(field, AuthStatusField::Null);
    }
}

#[test]
fn empty_optional_strings_are_not_replaced_with_missing_or_null() {
    let mut value = status();
    value["email"] = json!("");
    let observed = decode(value);
    let AuthStatusField::Value(email) = observed.email else {
        panic!("empty observed value was discarded");
    };
    assert_eq!(email.as_str(), "");
    assert_eq!(observed.org_id, AuthStatusField::Missing);
}

#[test]
fn config_directory_is_preserved_without_authority_or_canonicalization() {
    let mut value = status();
    value["configDirectory"] = json!("../observed-only/../profile");
    let observed = decode(value);
    assert_eq!(
        observed.config_directory.as_str(),
        "../observed-only/../profile"
    );
    assert!(!format!("{observed:?}").contains("observed-only"));
}

#[test]
fn every_required_field_must_be_present() {
    for key in ["loggedIn", "authMethod", "apiProvider", "configDirectory"] {
        let mut value = status();
        value.as_object_mut().unwrap().remove(key);
        invalid(value);
    }
}

#[test]
fn required_fields_cannot_be_null() {
    for key in ["loggedIn", "authMethod", "apiProvider", "configDirectory"] {
        let mut value = status();
        value[key] = Value::Null;
        invalid(value);
    }
}

#[test]
fn required_fields_reject_wrong_json_types() {
    for (key, replacement) in [
        ("loggedIn", json!("true")),
        ("loggedIn", json!(1)),
        ("authMethod", json!(false)),
        ("authMethod", json!({})),
        ("apiProvider", json!(42)),
        ("configDirectory", json!([])),
    ] {
        let mut value = status();
        value[key] = replacement;
        invalid(value);
    }
}

#[test]
fn required_text_cannot_be_empty() {
    for key in ["authMethod", "apiProvider", "configDirectory"] {
        let mut value = status();
        value[key] = json!("");
        invalid(value);
    }
}

#[test]
fn subscription_fields_reject_non_string_non_null_values() {
    for key in ["email", "orgId", "orgName", "subscriptionType"] {
        for replacement in [json!(false), json!(7), json!([]), json!({})] {
            let mut value = status();
            value[key] = replacement;
            invalid(value);
        }
    }
}

#[test]
fn stdout_must_be_a_json_object() {
    for value in [
        Value::Null,
        json!(true),
        json!(1),
        json!("status"),
        json!([]),
    ] {
        invalid(value);
    }
}

#[test]
fn unknown_bounded_fields_are_discarded_without_debug_disclosure() {
    let mut value = status();
    value["futureField"] = json!({
        "accessToken": "discarded-secret-marker",
        "nested": [null, false, 12]
    });
    let observed = decode(value);
    assert!(observed.logged_in);
    assert!(!format!("{observed:?}").contains("discarded-secret-marker"));
    assert!(!format!("{observed:?}").contains("accessToken"));
}

#[test]
fn duplicate_required_fields_are_rejected_even_when_values_agree() {
    for duplicate in [
        r#""loggedIn":true"#,
        r#""authMethod":"claude.ai""#,
        r#""apiProvider":"firstParty""#,
        r#""configDirectory":"/isolated/claude-profile""#,
    ] {
        let mut bytes = serde_json::to_vec(&status()).unwrap();
        bytes.pop();
        bytes.extend_from_slice(format!(",{duplicate}}}").as_bytes());
        assert_eq!(
            decode_auth_status(&bytes).unwrap_err(),
            ProtocolError::InvalidMessage
        );
    }
}

#[test]
fn duplicate_optional_field_is_rejected() {
    let bytes = br#"{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty","configDirectory":"/profile","email":null,"email":"duplicate@example.invalid"}"#;
    assert_eq!(
        decode_auth_status(bytes).unwrap_err(),
        ProtocolError::InvalidMessage
    );
}

#[test]
fn escaped_duplicate_key_is_rejected() {
    let bytes = br#"{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty","configDirectory":"/profile","\u006coggedIn":false}"#;
    assert_eq!(
        decode_auth_status(bytes).unwrap_err(),
        ProtocolError::InvalidMessage
    );
}

#[test]
fn duplicate_unknown_fields_are_rejected() {
    let bytes = br#"{"loggedIn":true,"authMethod":"none","apiProvider":"firstParty","configDirectory":"/profile","future":1,"future":2}"#;
    assert_eq!(
        decode_auth_status(bytes).unwrap_err(),
        ProtocolError::InvalidMessage
    );
}

#[test]
fn duplicate_nested_unknown_keys_are_rejected() {
    let bytes = br#"{"loggedIn":true,"authMethod":"none","apiProvider":"firstParty","configDirectory":"/profile","future":{"token":"one","token":"two"}}"#;
    assert_eq!(
        decode_auth_status(bytes).unwrap_err(),
        ProtocolError::InvalidMessage
    );
}

#[test]
fn invalid_utf8_is_rejected() {
    let mut bytes = serde_json::to_vec(&status()).unwrap();
    let offset = bytes.iter().position(|byte| *byte == b'/').unwrap();
    bytes[offset] = 0xff;
    assert_eq!(
        decode_auth_status(&bytes).unwrap_err(),
        ProtocolError::InvalidMessage
    );
}

#[test]
fn unicode_observed_text_is_preserved() {
    let mut value = status();
    value["configDirectory"] = json!("/isolated/工作/é");
    value["orgName"] = json!("Équipe 工作");
    let observed = decode(value);
    assert_eq!(observed.config_directory.as_str(), "/isolated/工作/é");
    let AuthStatusField::Value(name) = observed.org_name else {
        panic!("expected observed organization name");
    };
    assert_eq!(name.as_str(), "Équipe 工作");
}

#[test]
fn malformed_partial_or_multiple_stdout_documents_are_rejected() {
    let valid = serde_json::to_string(&status()).unwrap();
    for bytes in [
        Vec::new(),
        b" \n\t".to_vec(),
        valid.as_bytes()[..valid.len() - 1].to_vec(),
        format!("{valid}\n{valid}").into_bytes(),
        format!("Login successful.\n{valid}").into_bytes(),
        format!("{valid}\ntrailing text").into_bytes(),
    ] {
        assert_eq!(
            decode_auth_status(&bytes).unwrap_err(),
            ProtocolError::InvalidMessage
        );
    }
}

#[test]
fn surrounding_json_whitespace_is_accepted() {
    let bytes = format!(" \r\n\t{}\r\n ", status());
    let observed = decode_auth_status(bytes.as_bytes()).unwrap();
    assert!(observed.logged_in);
    assert_eq!(observed.auth_method, AuthMethod::ClaudeAi);
}

#[test]
fn whole_stdout_bound_is_inclusive_and_counts_whitespace() {
    let mut bytes = serde_json::to_vec(&status()).unwrap();
    bytes.resize(65_536, b' ');
    assert_eq!(bytes.len(), 65_536);
    assert!(decode_auth_status(&bytes).unwrap().logged_in);
    bytes.push(b' ');
    assert_eq!(
        decode_auth_status(&bytes).unwrap_err(),
        ProtocolError::FrameTooLarge
    );
}

#[test]
fn recognized_strings_accept_exactly_4096_utf8_bytes() {
    for key in [
        "authMethod",
        "apiProvider",
        "configDirectory",
        "email",
        "orgId",
        "orgName",
        "subscriptionType",
    ] {
        let mut value = status();
        value[key] = json!("x".repeat(4096));
        assert!(decode(value).logged_in);
    }
}

#[test]
fn recognized_strings_reject_4097_utf8_bytes() {
    for key in [
        "authMethod",
        "apiProvider",
        "configDirectory",
        "email",
        "orgId",
        "orgName",
        "subscriptionType",
    ] {
        let mut value = status();
        value[key] = json!("x".repeat(4097));
        assert_eq!(
            decode_auth_status(&serde_json::to_vec(&value).unwrap()).unwrap_err(),
            ProtocolError::Capacity
        );
    }
}

#[test]
fn string_bound_counts_utf8_bytes_instead_of_characters() {
    let mut value = status();
    value["orgName"] = json!("é".repeat(2048));
    assert!(decode(value.clone()).logged_in);
    value["orgName"] = json!(format!("{}x", "é".repeat(2048)));
    assert_eq!(
        decode_auth_status(&serde_json::to_vec(&value).unwrap()).unwrap_err(),
        ProtocolError::Capacity
    );
}

#[test]
fn discarded_unknown_values_and_keys_have_the_same_string_bound() {
    for value in [
        {
            let mut value = status();
            value["future"] = json!({"nested": "x".repeat(4097)});
            value
        },
        {
            let mut value = status();
            value
                .as_object_mut()
                .unwrap()
                .insert("x".repeat(4097), Value::Null);
            value
        },
    ] {
        assert_eq!(
            decode_auth_status(&serde_json::to_vec(&value).unwrap()).unwrap_err(),
            ProtocolError::Capacity
        );
    }
}

#[test]
fn discarded_unknown_structure_remains_bounded() {
    let mut value = status();
    let mut nested = Value::Null;
    for _ in 0..33 {
        nested = json!([nested]);
    }
    value["future"] = nested;
    assert_eq!(
        decode_auth_status(&serde_json::to_vec(&value).unwrap()).unwrap_err(),
        ProtocolError::Capacity
    );
    value["future"] = json!(vec![Value::Null; 4091]);
    assert_eq!(
        decode_auth_status(&serde_json::to_vec(&value).unwrap()).unwrap_err(),
        ProtocolError::Capacity
    );
}

#[test]
fn every_sensitive_text_wrapper_redacts_debug() {
    let mut value = status();
    for (key, marker) in [
        ("authMethod", "unknown-secret-auth-marker"),
        ("apiProvider", "private-provider-marker"),
        ("configDirectory", "/private/profile-marker"),
        ("email", "private-email@example.invalid"),
        ("orgId", "private-org-id-marker"),
        ("orgName", "private-org-name-marker"),
        ("subscriptionType", "private-subscription-marker"),
    ] {
        value[key] = json!(marker);
    }
    let observed = decode(value);
    let debug = format!("{observed:?}");
    for marker in [
        "unknown-secret-auth-marker",
        "private-provider-marker",
        "/private/profile-marker",
        "private-email@example.invalid",
        "private-org-id-marker",
        "private-org-name-marker",
        "private-subscription-marker",
    ] {
        assert!(!debug.contains(marker));
    }
    assert!(!format!("{:?}", observed.auth_method).contains("unknown-secret-auth-marker"));
    assert!(!format!("{:?}", observed.api_provider).contains("private-provider-marker"));
    assert!(!format!("{:?}", observed.config_directory).contains("profile-marker"));
    for field in [
        observed.email,
        observed.org_id,
        observed.org_name,
        observed.subscription_type,
    ] {
        let AuthStatusField::Value(text) = &field else {
            panic!("expected observed sensitive text");
        };
        assert!(!format!("{field:?}").contains(text.as_str()));
        assert!(!format!("{text:?}").contains(text.as_str()));
    }
}

#[test]
fn parse_errors_do_not_echo_sensitive_input() {
    let bytes = br#"{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty","configDirectory":"/private/error-profile-marker","email":"error-email@example.invalid","future":"secret-error-token-marker","loggedIn":false}"#;
    let error = decode_auth_status(bytes).unwrap_err();
    assert_eq!(error, ProtocolError::InvalidMessage);
    let debug = format!("{error:?}");
    for marker in [
        "error-profile-marker",
        "error-email@example.invalid",
        "secret-error-token-marker",
    ] {
        assert!(!debug.contains(marker));
    }
}

#[test]
fn stream_auth_status_false_is_not_a_cli_login_result() {
    invalid(json!({
        "type": "auth_status",
        "isAuthenticating": false,
        "output": [],
        "uuid": "stream-auth-status",
        "session_id": "stream-session"
    }));
}
