use super::{ACCOUNT_DEFAULT_MODEL, master_command, master_connection};
use crate::config::{ModelAuthentication, ModelConnection, ModelEndpoint, ModelReference};
use crate::protocol::CredentialRef;
use std::path::Path;

fn reference(value: &str) -> CredentialRef {
    CredentialRef::new(value).unwrap()
}

fn account(model: &str) -> ModelConnection {
    ModelConnection {
        id: reference("codex-managed"),
        provider: reference("openai"),
        harness: reference("codex"),
        protocol: reference("openai-responses"),
        account_ref: reference("owner-account"),
        endpoint: ModelEndpoint::Http {
            url: "http://127.0.0.1:11434/v1".into(),
        },
        model: ModelReference {
            id: model.into(),
            author: reference("openai"),
        },
        auth: ModelAuthentication::None {},
    }
}

#[test]
fn a_master_turn_runs_the_owner_s_master_model_over_the_account_s() {
    for own in [ACCOUNT_DEFAULT_MODEL, "gpt-account"] {
        let account = account(own);
        let mut expected = account.clone();
        expected.model.id = "gpt-5.6-sol".into();
        assert_eq!(master_connection(&account, Some("gpt-5.6-sol")), expected);
        assert_eq!(master_connection(&account, None), account);
    }
    let chosen = master_connection(&account(ACCOUNT_DEFAULT_MODEL), Some("gpt-5.6-sol"));
    let command = master_command(
        Path::new("/registered/codex"),
        Path::new("/private/profile"),
        Path::new("/leased/worktree"),
        &chosen.model.id,
        None,
    )
    .unwrap();
    let args = command
        .get_args()
        .map(|arg| arg.to_str().unwrap())
        .collect::<Vec<_>>();
    assert!(
        args.windows(2)
            .any(|pair| pair == ["-c", "model=\"gpt-5.6-sol\""]),
        "{args:?}"
    );
}
