use super::{ACCOUNT_DEFAULT_MODEL, master_choice, master_command, master_connection};
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

/// A master turn reads the owner's master model for its harness in the project, never the
/// agent default beside it, on both master backends (DEC-150 F7).
#[test]
fn a_master_turn_reads_the_owner_s_master_model_not_the_agent_default() {
    use crate::harness_settings::Scope;
    use crate::model_choice::{Slot, Written, store};
    let tmp = tempfile::Builder::new()
        .prefix("ca-master-model-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let folder = base.join("project");
    std::fs::create_dir(&folder).unwrap();
    let db = rusqlite::Connection::open(base.join("engine.db")).unwrap();
    crate::storage::initialize_authority_schemas(&db).unwrap();
    crate::ide::workspace::tests::add_project(&db, "alpha", &folder);
    let project = crate::protocol::ProjectId::new("alpha").unwrap();
    for harness in ["codex", "claude-code"] {
        let catalog = crate::model_choice::catalog(harness);
        let (master, agent) = (catalog[0].clone(), catalog[1].clone());
        assert_ne!(master, agent);
        for (slot, model) in [(Slot::Master, &master), (Slot::Agent, &agent)] {
            let models = store::values(&db, harness, &catalog, &Scope::App, None).unwrap();
            let version = models
                .rows
                .iter()
                .find(|row| row.slot == slot)
                .unwrap()
                .version;
            let written = store::set(
                &db,
                harness,
                &catalog,
                &Scope::App,
                slot,
                &serde_json::json!(model),
                version,
            )
            .unwrap();
            assert!(matches!(written, Written::Saved { .. }), "{written:?}");
        }
        let (_, chosen) =
            master_choice(&db, harness, &base.join("no-such-executable"), &project).unwrap();
        assert_eq!(chosen, Some(master), "{harness}");
    }
}
