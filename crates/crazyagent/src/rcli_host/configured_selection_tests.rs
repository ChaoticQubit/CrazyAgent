use super::*;
use crate::config::ModelReference;
use serde_json::json;
use std::os::unix::fs::PermissionsExt;

fn connection() -> ModelConnection {
    ModelConnection {
        id: CredentialRef::new("connection-a").unwrap(),
        provider: CredentialRef::new("openai").unwrap(),
        harness: CredentialRef::new("rcli").unwrap(),
        protocol: CredentialRef::new("openai-compatible").unwrap(),
        account_ref: CredentialRef::new("account-a").unwrap(),
        endpoint: ModelEndpoint::Http {
            url: "http://127.0.0.1:11434/v1".into(),
        },
        model: ModelReference {
            id: "selected-model".into(),
            author: CredentialRef::new("model-author-a").unwrap(),
        },
        auth: ModelAuthentication::None {},
    }
}

fn profile(home: &Path, selection: &ModelConnection) {
    let ModelEndpoint::Http { url } = &selection.endpoint else {
        panic!("HTTP profile fixture");
    };
    let auth = match selection.auth {
        ModelAuthentication::None {} => "none",
        ModelAuthentication::ApiKey { .. } => "apiKey",
        _ => panic!("configured profile fixture"),
    };
    let bytes = serde_json::to_vec(&json!({
        "model": {"provider": selection.provider.as_str(), "model": selection.model.id},
        "providers": [{
            "id": selection.provider.as_str(), "name": "Configured provider",
            "protocol": selection.protocol.as_str(), "baseUrl": url,
            "auth": {"type": auth}
        }]
    }))
    .unwrap();
    let path = home.join(".backboard/config.json");
    fs::write(&path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

fn home() -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    fs::set_permissions(home.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let sdk = home.path().join(".backboard");
    fs::create_dir(&sdk).unwrap();
    fs::set_permissions(sdk, fs::Permissions::from_mode(0o700)).unwrap();
    home
}

#[test]
fn logical_connection_validates_the_same_physical_profile_for_configuration_and_master() {
    let logical = connection();
    let original = serde_json::to_vec(&logical).unwrap();
    let selection = configured_sdk_connection(&logical).unwrap();
    assert_eq!(selection.protocol.as_str(), "openai-chat");
    assert_ne!(selection.provider, logical.provider);
    assert_eq!(selection.provider.as_str().len(), 64);
    assert_eq!(serde_json::to_vec(&logical).unwrap(), original);
    let home = home();
    let path = fs::canonicalize(home.path()).unwrap();
    profile(&path, &selection);
    let digest = checked_configured_profile(&path, &logical).unwrap();
    assert_eq!(
        checked_profile_config(&path, &logical, Some(&digest)).unwrap(),
        digest
    );
    assert_eq!(serde_json::to_vec(&logical).unwrap(), original);
}

#[test]
fn isolated_selection_is_stable_and_rejects_another_full_connection() {
    let logical = connection();
    let selection = configured_sdk_connection(&logical).unwrap();
    assert_eq!(configured_sdk_connection(&logical).unwrap(), selection);
    let home = home();
    let path = fs::canonicalize(home.path()).unwrap();
    profile(&path, &selection);
    let mut variants = vec![logical.clone(); 6];
    variants[0].id = CredentialRef::new("connection-b").unwrap();
    variants[1].account_ref = CredentialRef::new("account-b").unwrap();
    variants[2].provider = CredentialRef::new("other-provider").unwrap();
    variants[3].endpoint = ModelEndpoint::Http {
        url: "http://127.0.0.1:11435/v1".into(),
    };
    variants[4].model.id = "another-model".into();
    variants[5].auth = ModelAuthentication::ApiKey {
        credential_ref: CredentialRef::new("key-a").unwrap(),
    };
    for other in variants {
        assert_ne!(
            configured_sdk_connection(&other).unwrap().provider,
            selection.provider
        );
        assert!(checked_configured_profile(&path, &other).is_err());
    }
}

#[test]
fn native_protocols_and_existing_profile_rejections_remain_intact() {
    let home = home();
    let path = fs::canonicalize(home.path()).unwrap();
    for protocol in ["openai-chat", "openai-responses", "anthropic-messages"] {
        let mut logical = connection();
        logical.provider = CredentialRef::new("local-a").unwrap();
        logical.protocol = CredentialRef::new(protocol).unwrap();
        assert_eq!(configured_sdk_connection(&logical).unwrap(), logical);
        profile(&path, &logical);
        assert!(checked_configured_profile(&path, &logical).is_ok());
        logical.provider = CredentialRef::new("openai").unwrap();
        profile(&path, &logical);
        assert!(checked_configured_profile(&path, &logical).is_err());
    }
    let mut logical = connection();
    profile(&path, &logical);
    assert!(checked_configured_profile(&path, &logical).is_err());
    logical.protocol = CredentialRef::new("unsupported-protocol").unwrap();
    assert!(configured_sdk_connection(&logical).is_err());
    assert!(checked_configured_profile(&path, &logical).is_err());
}
