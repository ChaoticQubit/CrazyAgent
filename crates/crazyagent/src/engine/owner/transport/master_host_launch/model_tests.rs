use super::{
    ACCOUNT_DEFAULT_MODEL, MASTER_MODEL_REFUSED, master_choice, master_command, master_connection,
};
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

/// An engine database with one project, alpha, in a private folder. Output: the folder
/// guard, its real path, the database and the project.
fn engine() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    rusqlite::Connection,
    crate::protocol::ProjectId,
) {
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
    (tmp, base, db, project)
}

/// Saves one app-wide model slot for a harness at its current version. Inputs: the
/// database, the harness, the slot and the model.
fn set_slot(
    db: &rusqlite::Connection,
    harness: &str,
    slot: crate::model_choice::Slot,
    model: &str,
) {
    use crate::harness_settings::Scope;
    use crate::model_choice::{Written, store};
    let catalog = crate::model_choice::catalog(harness);
    let models = store::values(db, harness, &catalog, &Scope::App, None).unwrap();
    let version = models
        .rows
        .iter()
        .find(|row| row.slot == slot)
        .unwrap()
        .version;
    let written = store::set(
        db,
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

/// A master turn reads the owner's master model for its harness in the project, never the
/// agent default beside it, on both master backends (DEC-150 F7).
#[test]
fn a_master_turn_reads_the_owner_s_master_model_not_the_agent_default() {
    use crate::model_choice::Slot;
    let (_tmp, base, db, project) = engine();
    for harness in ["codex", "claude-code"] {
        let catalog = crate::model_choice::catalog(harness);
        let (master, agent) = (catalog[0].clone(), catalog[1].clone());
        assert_ne!(master, agent);
        set_slot(&db, harness, Slot::Master, &master);
        set_slot(&db, harness, Slot::Agent, &agent);
        let (_, chosen) = master_choice(
            &db,
            harness,
            &base.join("no-such-executable"),
            &project,
            &reference("codex-managed"),
            1_000,
        )
        .unwrap();
        assert_eq!(chosen, Some(master), "{harness}");
    }
}

/// A master model the master's account refused falls back by the owner's rule on that
/// account only: Notice runs the account's own model and tells the owner once, Silent
/// tells no one, Stop fails the turn with `master_model_refused`, and once the refusal ends
/// and the model runs a new refusal is told again (DEC-150 M-10).
#[test]
fn a_refused_master_model_falls_back_by_the_owner_s_rule_on_that_account_only() {
    use crate::model_choice::Slot;
    use crate::model_choice::rules::{FallbackRule, Rule, RuleWritten, rules, set_rule};
    use crate::team::model_pick::{MODEL_REFUSAL_MS, refuse};
    let (_tmp, base, db, project) = engine();
    let harness = "codex";
    let model = crate::model_choice::catalog(harness)[0].clone();
    set_slot(&db, harness, Slot::Master, &model);
    let (refusing, other) = (reference("codex-a"), reference("codex-b"));
    let executable = base.join("no-such-executable");
    let choose = |connection: &CredentialRef, now: i64| {
        master_choice(&db, harness, &executable, &project, connection, now)
            .map(|(_, chosen)| chosen)
    };
    let told = || -> i64 {
        db.query_row(
            "SELECT COUNT(*) FROM team_messages WHERE project='alpha' AND text LIKE '%(model_fallback)'",
            [],
            |row| row.get(0),
        )
        .unwrap()
    };
    let fallback = |rule: FallbackRule| {
        let version = rules(&db).unwrap().fallback_version;
        let written = set_rule(&db, Rule::Fallback(rule), version).unwrap();
        assert!(matches!(written, RuleWritten::Saved { .. }), "{written:?}");
    };
    assert_eq!(choose(&refusing, 1_000).unwrap(), Some(model.clone()));
    refuse(&db, &project, &refusing, &model, 1_000).unwrap();
    assert_eq!(choose(&other, 1_001).unwrap(), Some(model.clone()));
    assert_eq!(choose(&refusing, 1_002).unwrap(), None);
    assert_eq!(told(), 1, "Notice tells the owner once");
    let line: String = db
        .query_row(
            "SELECT text FROM team_messages WHERE project='alpha' AND text LIKE '%(model_fallback)'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        line.contains(&model) && line.contains("master runs on"),
        "{line}"
    );
    assert!(
        !line.contains("No account in this project's pool"),
        "{line}"
    );
    assert_eq!(choose(&refusing, 1_003).unwrap(), None);
    assert_eq!(told(), 1, "a held fallback is told once");
    fallback(FallbackRule::Silent);
    assert_eq!(choose(&refusing, 1_004).unwrap(), None);
    assert_eq!(told(), 1, "Silent tells no one");
    fallback(FallbackRule::Stop);
    assert_eq!(
        choose(&refusing, 1_005).unwrap_err().0,
        MASTER_MODEL_REFUSED,
        "Stop fails the turn"
    );
    assert_eq!(choose(&other, 1_006).unwrap(), Some(model.clone()));
    fallback(FallbackRule::Notice);
    let ended = 1_000 + MODEL_REFUSAL_MS;
    assert_eq!(choose(&refusing, ended).unwrap(), Some(model.clone()));
    refuse(&db, &project, &refusing, &model, ended).unwrap();
    assert_eq!(choose(&refusing, ended + 1).unwrap(), None);
    assert_eq!(
        told(),
        2,
        "a refusal after the model ran again is told again"
    );
}
