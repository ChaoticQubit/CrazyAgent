use super::grants::{self, GRANTS_PER_PROJECT_MAX};
use super::*;
use crate::app_plugins::{Binding, FOLDER};
use crate::protocol::{CredentialRef, ProjectId};
use crate::storage::accounts::{self, PLUGIN_KEYS};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

const PROJECT: &str = "alpha";
const OTHER: &str = "beta";

const BOARD: &str = "schema: 1
kind: connector
name: board
title: Board
version: 1
mcp:
  url: https://mcp.board.example/mcp
  hosts: [mcp.board.example]
key:
  secret: api_key
  header: Authorization
  scheme: Bearer
";

const BOARD_ELSEWHERE: &str = "schema: 1
kind: connector
name: board
title: Board
version: 1
mcp:
  url: https://mcp.board-two.example/mcp
  hosts: [mcp.board-two.example]
key:
  secret: api_key
  header: Authorization
  scheme: Bearer
";

const OLD_LINEAR: &str = "schema: 1
kind: embedding
name: linear
title: Linear vectors
version: 1
http:
  url: https://embed.old.example/v1/embed
  hosts: [embed.old.example]
  secrets: [api_key]
  headers:
    Authorization: \"Bearer {{secret.api_key}}\"
  body:
    input: \"{{inputs}}\"
  vectors: embeddings.*
  dimensions: 128
";

const ROLES: [Role; 3] = [
    Role::Master,
    Role::Agent { tools_on: true },
    Role::Agent { tools_on: false },
];

struct Case {
    _tmp: tempfile::TempDir,
    base: PathBuf,
    root: PathBuf,
    db: Connection,
}

/// A real engine database with the authority tables and two set-up projects, in an engine
/// root of its own. Output: the case, which keeps the temporary folder alive.
fn case() -> Case {
    let tmp = tempfile::Builder::new()
        .prefix("ca-connectors-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let root = base.join("engine");
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let db = crate::questions::open(&root).unwrap();
    crate::storage::initialize_authority_schemas(&db).unwrap();
    let case = Case {
        _tmp: tmp,
        base,
        root,
        db,
    };
    for project in [PROJECT, OTHER] {
        add_project(&case, project);
    }
    case
}

fn add_project(case: &Case, project: &str) {
    let folder = case.base.join(project);
    std::fs::create_dir_all(&folder).unwrap();
    crate::ide::workspace::tests::add_project(&case.db, project, &folder);
}

fn sha(text: &str) -> String {
    crate::semantic::sha256_hex(text.as_bytes())
}

fn add(case: &Case, text: &str, replace: bool) {
    store::add(&case.db, &case.root, text, &sha(text), replace, 1).unwrap();
}

fn key(case: &Case, connector: &str) {
    store::set_key(
        &case.db,
        &case.root,
        connector,
        "api_key",
        b"sk-test-key-0001".to_vec(),
    )
    .unwrap();
}

fn turn_on(case: &Case, project: &str, connector: &str, who: Who) -> Result<(), EngineError> {
    grants::set(&case.db, &case.root, project, connector, true, who, 2)
}

fn reach(case: &Case, project: &str, role: Role) -> Vec<String> {
    grants::for_launch(&case.db, &case.root, project, role)
        .unwrap()
        .into_iter()
        .map(|granted| granted.name)
        .collect()
}

fn count(case: &Case, sql: &str) -> i64 {
    case.db.query_row(sql, [], |row| row.get(0)).unwrap()
}

fn board(enabled: bool, who: Who) -> Vec<Grant> {
    vec![Grant {
        connector: "board".into(),
        enabled,
        who,
    }]
}

#[test]
fn a_new_connector_reaches_nobody_until_the_owner_turns_it_on_for_a_project() {
    let case = case();
    add(&case, BOARD, false);
    key(&case, "board");
    key(&case, "linear");
    for role in ROLES {
        assert!(reach(&case, PROJECT, role).is_empty(), "{role:?}");
    }
    let page = view(&case.db, &case.root, Some(PROJECT)).unwrap();
    assert!(page.grants.is_empty());
    assert_eq!(page.project.as_deref(), Some(PROJECT));
    let names: Vec<&str> = page
        .connectors
        .iter()
        .map(|connector| connector.name.as_str())
        .collect();
    assert_eq!(names, ["linear", "github", "atlassian", "board"]);

    turn_on(&case, PROJECT, "board", Who::Both).unwrap();
    assert_eq!(reach(&case, PROJECT, Role::Master), ["board"]);
    assert_eq!(
        reach(&case, PROJECT, Role::Agent { tools_on: true }),
        ["board"]
    );
    assert_eq!(
        view(&case.db, &case.root, Some(PROJECT)).unwrap().grants,
        board(true, Who::Both)
    );
    assert!(
        view(&case.db, &case.root, None).unwrap().grants.is_empty(),
        "no project asked, no switches listed"
    );

    grants::set(&case.db, &case.root, PROJECT, "board", false, Who::Both, 3).unwrap();
    for role in ROLES {
        assert!(reach(&case, PROJECT, role).is_empty(), "{role:?}");
    }
    assert_eq!(
        grants::of_project(&case.db, PROJECT).unwrap(),
        board(false, Who::Both)
    );
    grants::set(&case.db, &case.root, PROJECT, "linear", false, Who::Both, 3).unwrap();
    assert_eq!(
        grants::of_project(&case.db, PROJECT).unwrap().len(),
        1,
        "turning off a connector that was never on writes no row"
    );
}

#[test]
fn a_connector_without_its_key_or_its_trust_reaches_nobody() {
    let case = case();
    add(&case, BOARD, false);
    assert_eq!(
        turn_on(&case, PROJECT, "board", Who::Both).unwrap_err().0,
        "app_plugin_needs_key"
    );
    assert!(grants::of_project(&case.db, PROJECT).unwrap().is_empty());
    key(&case, "board");
    turn_on(&case, PROJECT, "board", Who::Both).unwrap();
    assert_eq!(reach(&case, PROJECT, Role::Master), ["board"]);

    store::set_enabled(&case.db, &case.root, "board", false).unwrap();
    assert_eq!(
        reach(&case, PROJECT, Role::Master),
        ["board"],
        "the app-wide plugin switch is not a connector's switch; the project's is"
    );

    store::unbind(&case.db, &case.root, "board", "api_key").unwrap();
    assert!(reach(&case, PROJECT, Role::Master).is_empty());
    assert_eq!(
        grants::of_project(&case.db, PROJECT).unwrap(),
        board(true, Who::Both)
    );
    key(&case, "board");
    assert_eq!(reach(&case, PROJECT, Role::Master), ["board"]);

    let edited = BOARD.replacen("version: 1", "version: 2", 1);
    std::fs::write(case.root.join(FOLDER).join("board.yaml"), &edited).unwrap();
    assert!(
        reach(&case, PROJECT, Role::Master).is_empty(),
        "a file changed since the owner trusted it reaches nobody"
    );
    assert_eq!(
        turn_on(&case, OTHER, "board", Who::Both).unwrap_err().0,
        "app_plugin_needs_review"
    );
    store::trust_file(&case.db, &case.root, "board", &sha(&edited), 4).unwrap();
    assert_eq!(
        reach(&case, PROJECT, Role::Master),
        ["board"],
        "trusted again on the same hosts, the key and the switch stay"
    );

    assert_eq!(
        turn_on(&case, PROJECT, "openai", Who::Both).unwrap_err().0,
        "app_plugin_not_found",
        "an embedding plugin is not a connector"
    );
    assert_eq!(
        turn_on(&case, PROJECT, "nothing", Who::Both).unwrap_err().0,
        "app_plugin_not_found"
    );
}

#[test]
fn master_only_reaches_the_master_and_not_agents() {
    let case = case();
    add(&case, BOARD, false);
    key(&case, "board");
    let agent = Role::Agent { tools_on: true };

    turn_on(&case, PROJECT, "board", Who::Master).unwrap();
    assert_eq!(reach(&case, PROJECT, Role::Master), ["board"]);
    assert!(reach(&case, PROJECT, agent).is_empty());
    assert_eq!(
        grants::of_project(&case.db, PROJECT).unwrap(),
        board(true, Who::Master)
    );

    turn_on(&case, PROJECT, "board", Who::Agents).unwrap();
    assert!(reach(&case, PROJECT, Role::Master).is_empty());
    assert_eq!(reach(&case, PROJECT, agent), ["board"]);

    turn_on(&case, PROJECT, "board", Who::Both).unwrap();
    assert_eq!(reach(&case, PROJECT, Role::Master), ["board"]);
    assert_eq!(reach(&case, PROJECT, agent), ["board"]);
}

#[test]
fn an_agent_with_tools_off_gets_no_connector() {
    let case = case();
    add(&case, BOARD, false);
    key(&case, "board");
    key(&case, "linear");
    for who in Who::ALL {
        turn_on(&case, PROJECT, "board", who).unwrap();
        turn_on(&case, PROJECT, "linear", who).unwrap();
        assert!(
            reach(&case, PROJECT, Role::Agent { tools_on: false }).is_empty(),
            "{who:?}"
        );
    }
    assert_eq!(
        reach(&case, PROJECT, Role::Agent { tools_on: true }),
        ["board", "linear"]
    );
}

#[test]
fn a_grant_in_one_project_does_not_reach_another_project() {
    let case = case();
    add(&case, BOARD, false);
    key(&case, "board");
    turn_on(&case, PROJECT, "board", Who::Both).unwrap();
    for role in ROLES {
        assert!(reach(&case, OTHER, role).is_empty(), "{role:?}");
        assert!(reach(&case, "ghost", role).is_empty(), "{role:?}");
    }
    assert!(
        view(&case.db, &case.root, Some(OTHER))
            .unwrap()
            .grants
            .is_empty()
    );
    assert_eq!(
        turn_on(&case, "ghost", "board", Who::Both).unwrap_err().0,
        PROJECT_UNKNOWN
    );
    assert_eq!(
        view(&case.db, &case.root, Some("ghost")).unwrap_err().0,
        PROJECT_UNKNOWN
    );
    assert_eq!(
        count(&case, "SELECT COUNT(*) FROM connector_grants"),
        1,
        "a refused switch writes nothing"
    );
}

#[test]
fn deleting_a_project_deletes_its_grants() {
    let case = case();
    add(&case, BOARD, false);
    key(&case, "board");
    turn_on(&case, PROJECT, "board", Who::Both).unwrap();
    turn_on(&case, OTHER, "board", Who::Master).unwrap();

    let config = crate::config::resolve_config_bytes("", "").unwrap();
    let tx = case.db.unchecked_transaction().unwrap();
    let outcome = crate::project_setup::execute(
        &tx,
        &config,
        &crate::project_setup::SetupAction::Remove {
            project_id: ProjectId::new(PROJECT).unwrap(),
        },
        2,
    )
    .unwrap();
    assert!(
        matches!(outcome, crate::protocol::Outcome::Applied),
        "{outcome:?}"
    );
    tx.commit().unwrap();

    assert_eq!(
        count(
            &case,
            "SELECT COUNT(*) FROM connector_grants WHERE project='alpha'"
        ),
        0
    );
    assert_eq!(
        grants::of_project(&case.db, OTHER).unwrap(),
        board(true, Who::Master)
    );
    add_project(&case, PROJECT);
    for role in ROLES {
        assert!(
            reach(&case, PROJECT, role).is_empty(),
            "a project added again under the same id starts with every connector off"
        );
    }
}

#[test]
fn a_removed_connector_added_again_under_its_name_starts_off_everywhere() {
    let case = case();
    add(&case, BOARD, false);
    key(&case, "board");
    turn_on(&case, PROJECT, "board", Who::Both).unwrap();
    turn_on(&case, OTHER, "board", Who::Both).unwrap();
    store::remove(&case.db, &case.root, "board").unwrap();
    assert_eq!(count(&case, "SELECT COUNT(*) FROM connector_grants"), 0);

    add(&case, BOARD_ELSEWHERE, false);
    key(&case, "board");
    for project in [PROJECT, OTHER] {
        for role in ROLES {
            assert!(reach(&case, project, role).is_empty(), "{project} {role:?}");
        }
    }
}

#[test]
fn a_connector_trusted_again_on_other_hosts_is_off_in_every_project() {
    let case = case();
    add(&case, BOARD, false);
    key(&case, "board");
    turn_on(&case, PROJECT, "board", Who::Both).unwrap();
    turn_on(&case, OTHER, "board", Who::Master).unwrap();

    add(&case, &BOARD.replacen("version: 1", "version: 2", 1), true);
    assert_eq!(
        reach(&case, PROJECT, Role::Master),
        ["board"],
        "a new version on the same hosts keeps its key and its switches"
    );

    add(&case, BOARD_ELSEWHERE, true);
    assert_eq!(count(&case, "SELECT COUNT(*) FROM connector_grants"), 0);
    key(&case, "board");
    for project in [PROJECT, OTHER] {
        assert!(
            reach(&case, project, Role::Master).is_empty(),
            "{project}: other hosts need the owner to turn the connector on again"
        );
    }
}

#[test]
fn a_bundled_connector_never_inherits_an_older_plugin_of_its_name() {
    let case = case();
    add(&case, BOARD, false);
    grants::of_project(&case.db, PROJECT).unwrap();
    crate::semantic::settings::view(&case.db, &case.root).unwrap();
    let file = case.root.join(FOLDER).join("linear.yaml");
    std::fs::write(&file, OLD_LINEAR).unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    let old_key = CredentialRef::new("plugin-key-0123456789abcdef").unwrap();
    accounts::replace_key_in(
        &case.root,
        PLUGIN_KEYS,
        &old_key,
        b"sk-old-embedding-key".to_vec(),
    )
    .unwrap();
    case.db
        .execute(
            "INSERT INTO app_plugin_trust (name, sha256, hosts, trusted_ms)
             VALUES ('linear', ?1, 'embed.old.example', 1)",
            [sha(OLD_LINEAR)],
        )
        .unwrap();
    case.db
        .execute_batch(
            "INSERT INTO app_plugin_state (name, enabled) VALUES ('linear', 1);
             INSERT INTO app_plugin_secrets (plugin, secret, source, reference)
             VALUES ('linear', 'api_key', 'key', 'plugin-key-0123456789abcdef');
             INSERT INTO connector_grants (project, connector, enabled, who, changed_ms)
             VALUES ('alpha', 'linear', 1, 'both', 1);
             INSERT INTO search_settings (id, version, plugin, host, sha256, note)
             VALUES (1, 3, 'linear', 'embed.old.example', NULL, NULL);",
        )
        .unwrap();

    for role in ROLES {
        assert!(
            reach(&case, PROJECT, role).is_empty(),
            "{role:?}: the old plugin's key never goes to the bundled connector's host"
        );
    }
    let listing = store::list(&case.db, &case.root).unwrap();
    let bundled = listing
        .plugins
        .iter()
        .find(|plugin| plugin.name == "linear" && plugin.origin == Origin::Bundled)
        .unwrap();
    assert_eq!(bundled.status, PluginStatus::NeedsKey);
    assert!(
        bundled
            .secrets
            .iter()
            .all(|secret| secret.binding == Binding::None)
    );
    let leftover = listing
        .plugins
        .iter()
        .find(|plugin| plugin.name == "linear" && plugin.origin == Origin::User)
        .unwrap();
    assert!(
        matches!(&leftover.status, PluginStatus::Refused { problem }
            if problem.code == "app_plugin_name_taken"),
        "{:?}",
        leftover.status
    );
    assert!(!accounts::key_saved_in(&case.root, PLUGIN_KEYS, &old_key).unwrap());
    for sql in [
        "SELECT COUNT(*) FROM app_plugin_trust WHERE name='linear'",
        "SELECT COUNT(*) FROM app_plugin_state WHERE name='linear'",
        "SELECT COUNT(*) FROM app_plugin_secrets WHERE plugin='linear'",
        "SELECT COUNT(*) FROM connector_grants WHERE connector='linear'",
        "SELECT COUNT(*) FROM search_settings WHERE plugin='linear'",
    ] {
        assert_eq!(count(&case, sql), 0, "{sql}");
    }
    assert_eq!(
        count(
            &case,
            "SELECT COUNT(*) FROM search_settings WHERE note='search_choice_removed'"
        ),
        1,
        "Search goes back to full text and says why"
    );

    store::remove(&case.db, &case.root, "linear").unwrap();
    assert!(!file.exists(), "the owner can remove their old file");
    assert_eq!(
        store::remove(&case.db, &case.root, "linear").unwrap_err().0,
        "app_plugin_bundled"
    );

    key(&case, "linear");
    turn_on(&case, PROJECT, "linear", Who::Both).unwrap();
    store::list(&case.db, &case.root).unwrap();
    assert_eq!(
        reach(&case, PROJECT, Role::Master),
        ["linear"],
        "a key the owner enters for the bundled connector stays"
    );
}

#[test]
fn grant_rows_of_connectors_that_are_gone_never_fill_a_project() {
    let case = case();
    add(&case, BOARD, false);
    key(&case, "board");
    grants::of_project(&case.db, PROJECT).unwrap();
    for index in 0..GRANTS_PER_PROJECT_MAX {
        case.db
            .execute(
                "INSERT INTO connector_grants (project, connector, enabled, who, changed_ms)
                 VALUES ('alpha', ?1, 1, 'both', 1)",
                [format!("gone-{index:03}")],
            )
            .unwrap();
    }
    assert!(reach(&case, PROJECT, Role::Master).is_empty());
    turn_on(&case, PROJECT, "board", Who::Both).unwrap();
    assert_eq!(
        grants::of_project(&case.db, PROJECT).unwrap(),
        board(true, Who::Both)
    );
    assert_eq!(reach(&case, PROJECT, Role::Master), ["board"]);
}

#[test]
fn the_default_settings_are_the_labelled_ones_and_a_stale_save_is_refused() {
    let case = case();
    assert_eq!(
        settings::load(&case.db).unwrap(),
        (ConnectorSettings::DEFAULT, 0)
    );
    assert_eq!(
        ConnectorSettings::DEFAULT,
        ConnectorSettings {
            write: WritePolicy::AskEveryTime,
            wait: Wait::Minutes10,
            native: NativeConnectors::Off,
            size_cap: SizeCap::Kib256,
            keep_log: KeepLog::Days30,
        }
    );
    assert_eq!(Wait::ALL.map(Wait::minutes), [2, 10, 30]);
    assert_eq!(
        SizeCap::ALL.map(SizeCap::bytes),
        [65_536, 262_144, 1_048_576]
    );
    assert_eq!(KeepLog::ALL.map(KeepLog::days), [7, 30, 90]);

    let quiet = ConnectorSettings {
        write: WritePolicy::ReadOnly,
        ..ConnectorSettings::DEFAULT
    };
    assert_eq!(settings::save(&case.db, &quiet, 3).unwrap(), Err(0));
    assert_eq!(settings::save(&case.db, &quiet, 0).unwrap(), Ok(1));
    assert_eq!(
        settings::save(&case.db, &ConnectorSettings::DEFAULT, 0).unwrap(),
        Err(1)
    );
    assert_eq!(settings::load(&case.db).unwrap(), (quiet, 1));
    assert_eq!(view(&case.db, &case.root, None).unwrap().settings, quiet);
    assert_eq!(view(&case.db, &case.root, None).unwrap().version, 1);
}

#[test]
fn every_connector_code_of_this_module_has_a_plain_sentence() {
    for code in [
        PROJECT_UNKNOWN,
        "connector_request_invalid",
        "connector_request_bounds",
        "connector_outcome_unknown",
        "connectors_unavailable",
        "app_plugin_needs_key",
    ] {
        let plain = plain_problem(code).unwrap_or_else(|| panic!("{code}"));
        assert!(!plain.contains('_'), "{code}");
    }
    assert_eq!(plain_problem("connector_nothing"), None);
}
