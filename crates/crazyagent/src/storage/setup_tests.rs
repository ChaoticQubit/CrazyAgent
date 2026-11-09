use super::*;
use crate::plugins::{ServerView, SkillView};
use crate::setup::PackState;
use std::os::unix::fs::PermissionsExt;

fn database() -> (tempfile::TempDir, Connection) {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let connection = Connection::open(root.path().join("engine.db")).unwrap();
    super::super::initialize_schema(&connection).unwrap();
    plugins::initialize(&connection).unwrap();
    super::super::trash::initialize(&connection).unwrap();
    initialize(&connection).unwrap();
    (root, connection)
}

fn skill(parent: &Path, name: &str) -> PathBuf {
    let folder = parent.join(name);
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(
        folder.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: The {name} skill.\n---\n"),
    )
    .unwrap();
    folder
}

fn spec() -> ServerSpec {
    ServerSpec::Stdio {
        command: "/usr/bin/true".into(),
        args: vec!["--stdio".into()],
    }
}

fn skills(connection: &mut Connection, root: &Path) -> Vec<SkillView> {
    let plugins::ResultValue::Skills(views, next) = plugins::execute(
        connection,
        root,
        plugins::Command::Skills {
            project: None,
            offset: 0,
        },
    )
    .unwrap() else {
        panic!("skill list required");
    };
    assert!(next.is_none());
    views
}

fn servers(connection: &mut Connection, root: &Path) -> Vec<ServerView> {
    let plugins::ResultValue::Servers(views, next) = plugins::execute(
        connection,
        root,
        plugins::Command::Servers {
            project: None,
            offset: 0,
        },
    )
    .unwrap() else {
        panic!("server list required");
    };
    assert!(next.is_none());
    views
}

fn install(
    connection: &mut Connection,
    root: &Path,
    id: &str,
    source: &str,
    folders: Vec<PathBuf>,
    server: Option<(String, ServerSpec)>,
) -> Result<(), EngineError> {
    execute(
        connection,
        root,
        Command::Install {
            id: id.into(),
            source: source.into(),
            skills: folders,
            server,
        },
    )
}

fn owner(connection: &mut Connection, root: &Path, action: SetupAction) -> Outcome {
    let tx = connection.transaction().unwrap();
    let outcome = apply_owner(&tx, &root.join("engine.db"), &action).unwrap();
    tx.commit().unwrap();
    outcome
}

fn status_of(connection: &mut Connection, root: &Path) -> SetupStatus {
    let tx = connection.transaction().unwrap();
    status(&tx, &root.join("engine.db")).unwrap()
}

fn pack_state(status: &SetupStatus, id: &str) -> PackState {
    status
        .pack
        .iter()
        .find(|entry| entry.id == id)
        .unwrap()
        .state
        .clone()
}

fn rejected_with(outcome: Outcome) -> String {
    match outcome {
        Outcome::Rejected { code } => code,
        _ => panic!("refusal required"),
    }
}

#[test]
fn installing_an_entry_adds_its_skills_and_an_approved_server_at_once() {
    let (dir, mut connection) = database();
    let root = dir.path();
    let work = tempfile::tempdir().unwrap();
    let folders = vec![skill(work.path(), "alpha"), skill(work.path(), "beta")];
    install(
        &mut connection,
        root,
        "superpowers",
        "https://example.invalid/pack@1",
        folders,
        Some(("context-mode".into(), spec())),
    )
    .unwrap();

    let listed = skills(&mut connection, root);
    let names: Vec<&str> = listed.iter().map(|skill| skill.name.as_str()).collect();
    assert_eq!(names, vec!["alpha", "beta"]);
    assert!(listed.iter().all(|skill| skill.enabled));
    let listed = servers(&mut connection, root);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "context-mode");
    assert_eq!(listed[0].status_label, "Approved");
    let status = status_of(&mut connection, root);
    assert_eq!(pack_state(&status, "superpowers"), PackState::On);
    assert_eq!(pack_state(&status, "caveman"), PackState::Available);
    assert!(!status.running);
}

#[test]
fn switching_an_entry_off_and_on_switches_every_item_it_added() {
    let (dir, mut connection) = database();
    let root = dir.path();
    let work = tempfile::tempdir().unwrap();
    install(
        &mut connection,
        root,
        "superpowers",
        "https://example.invalid/pack@1",
        vec![skill(work.path(), "alpha")],
        Some(("context-mode".into(), spec())),
    )
    .unwrap();

    let off = SetupAction::Set {
        id: "superpowers".into(),
        on: false,
        version: 0,
    };
    assert_eq!(owner(&mut connection, root, off), Outcome::Applied);
    assert!(!skills(&mut connection, root)[0].enabled);
    assert_eq!(servers(&mut connection, root)[0].status_label, "Off");
    let status = status_of(&mut connection, root);
    assert_eq!(pack_state(&status, "superpowers"), PackState::Off);

    let on = SetupAction::Set {
        id: "superpowers".into(),
        on: true,
        version: 1,
    };
    assert_eq!(owner(&mut connection, root, on), Outcome::Applied);
    assert!(skills(&mut connection, root)[0].enabled);
    assert_eq!(servers(&mut connection, root)[0].status_label, "Approved");
}

#[test]
fn stale_pack_write_is_refused() {
    let (dir, mut connection) = database();
    let root = dir.path();
    let work = tempfile::tempdir().unwrap();
    install(
        &mut connection,
        root,
        "superpowers",
        "https://example.invalid/pack@1",
        vec![skill(work.path(), "alpha")],
        None,
    )
    .unwrap();
    assert_eq!(status_of(&mut connection, root).version, 0);

    let future = SetupAction::Set {
        id: "superpowers".into(),
        on: false,
        version: 1,
    };
    assert_eq!(
        rejected_with(owner(&mut connection, root, future)),
        "stale_revision"
    );
    assert!(skills(&mut connection, root)[0].enabled);
    assert_eq!(status_of(&mut connection, root).version, 0);
    let missing = SetupAction::Set {
        id: "graphify".into(),
        on: false,
        version: 9,
    };
    assert_eq!(
        rejected_with(owner(&mut connection, root, missing)),
        "pack_not_installed"
    );

    let off = SetupAction::Set {
        id: "superpowers".into(),
        on: false,
        version: 0,
    };
    assert_eq!(owner(&mut connection, root, off), Outcome::Applied);
    let status = status_of(&mut connection, root);
    assert_eq!(status.version, 1);
    assert_eq!(pack_state(&status, "superpowers"), PackState::Off);

    let stale_on = SetupAction::Set {
        id: "superpowers".into(),
        on: true,
        version: 0,
    };
    assert_eq!(
        rejected_with(owner(&mut connection, root, stale_on)),
        "stale_revision"
    );
    let stale_remove = SetupAction::Remove {
        id: "superpowers".into(),
        version: 0,
    };
    assert_eq!(
        rejected_with(owner(&mut connection, root, stale_remove)),
        "stale_revision"
    );
    let listed = skills(&mut connection, root);
    assert_eq!(listed.len(), 1);
    assert!(!listed[0].enabled);
    let status = status_of(&mut connection, root);
    assert_eq!(status.version, 1);
    assert_eq!(pack_state(&status, "superpowers"), PackState::Off);

    let remove = SetupAction::Remove {
        id: "superpowers".into(),
        version: 1,
    };
    assert_eq!(owner(&mut connection, root, remove), Outcome::Applied);
    assert!(skills(&mut connection, root).is_empty());
    assert_eq!(status_of(&mut connection, root).version, 2);
}

#[test]
fn a_pack_change_and_a_plugin_screen_change_refuse_each_others_stale_saves() {
    let (dir, mut connection) = database();
    let root = dir.path();
    let work = tempfile::tempdir().unwrap();
    install(
        &mut connection,
        root,
        "superpowers",
        "https://example.invalid/pack@1",
        vec![skill(work.path(), "alpha")],
        None,
    )
    .unwrap();
    let shown = skills(&mut connection, root).remove(0);
    let pack = status_of(&mut connection, root).version;

    let off = SetupAction::Set {
        id: "superpowers".into(),
        on: false,
        version: pack,
    };
    assert_eq!(owner(&mut connection, root, off), Outcome::Applied);
    let Err(stale_skill) = plugins::execute(
        &mut connection,
        root,
        plugins::Command::SetSkill {
            id: shown.id.clone(),
            enabled: true,
            version: shown.version,
        },
    ) else {
        panic!("a refusal required");
    };
    assert_eq!(stale_skill.0, "stale_revision");
    let listed = skills(&mut connection, root);
    assert!(
        !listed[0].enabled,
        "the stale plugin screen changed nothing"
    );
    assert_eq!(listed[0].version, shown.version + 1);
    assert_eq!(status_of(&mut connection, root).version, pack + 1);

    let on = plugins::execute(
        &mut connection,
        root,
        plugins::Command::SetSkill {
            id: shown.id.clone(),
            enabled: true,
            version: listed[0].version,
        },
    );
    assert!(on.is_ok());
    assert_eq!(
        status_of(&mut connection, root).version,
        pack + 2,
        "a plugin-screen change of a pack skill moves the pack version"
    );
    let stale_pack = SetupAction::Set {
        id: "superpowers".into(),
        on: false,
        version: pack + 1,
    };
    assert_eq!(
        rejected_with(owner(&mut connection, root, stale_pack)),
        "stale_revision"
    );
    assert!(skills(&mut connection, root)[0].enabled);

    let owned_elsewhere = owner_skill(&mut connection, root, work.path(), "beta");
    let version = skills(&mut connection, root)
        .into_iter()
        .find(|view| view.id == owned_elsewhere)
        .unwrap()
        .version;
    let unrelated = plugins::execute(
        &mut connection,
        root,
        plugins::Command::SetSkill {
            id: owned_elsewhere,
            enabled: false,
            version,
        },
    );
    assert!(unrelated.is_ok());
    assert_eq!(
        status_of(&mut connection, root).version,
        pack + 2,
        "a skill no pack entry holds leaves the pack version alone"
    );
}

#[test]
fn removing_an_entry_removes_its_items_and_the_sweep_deletes_their_folders() {
    let (dir, mut connection) = database();
    let root = dir.path();
    let work = tempfile::tempdir().unwrap();
    install(
        &mut connection,
        root,
        "superpowers",
        "https://example.invalid/pack@1",
        vec![skill(work.path(), "alpha")],
        Some(("context-mode".into(), spec())),
    )
    .unwrap();
    let stored = PathBuf::from(&skills(&mut connection, root)[0].folder);
    assert!(stored.join("SKILL.md").is_file());

    let remove = SetupAction::Remove {
        id: "superpowers".into(),
        version: 0,
    };
    assert_eq!(owner(&mut connection, root, remove), Outcome::Applied);
    assert!(skills(&mut connection, root).is_empty());
    assert!(servers(&mut connection, root).is_empty());
    assert!(!stored.exists());
    let store = stored.parent().unwrap().to_path_buf();
    assert_eq!(std::fs::read_dir(&store).unwrap().count(), 1);

    execute(&mut connection, root, Command::Sweep).unwrap();
    assert_eq!(std::fs::read_dir(&store).unwrap().count(), 0);
    let status = status_of(&mut connection, root);
    assert_eq!(pack_state(&status, "superpowers"), PackState::Available);
    let again = SetupAction::Remove {
        id: "superpowers".into(),
        version: 1,
    };
    assert_eq!(
        rejected_with(owner(&mut connection, root, again)),
        "pack_not_installed"
    );
}

#[test]
fn a_removal_that_did_not_commit_is_put_back_by_the_sweep() {
    let (dir, mut connection) = database();
    let root = dir.path();
    let work = tempfile::tempdir().unwrap();
    install(
        &mut connection,
        root,
        "superpowers",
        "https://example.invalid/pack@1",
        vec![skill(work.path(), "alpha")],
        None,
    )
    .unwrap();
    let stored = PathBuf::from(&skills(&mut connection, root)[0].folder);

    {
        let tx = connection.transaction().unwrap();
        let remove = SetupAction::Remove {
            id: "superpowers".into(),
            version: 0,
        };
        assert_eq!(
            apply_owner(&tx, &root.join("engine.db"), &remove).unwrap(),
            Outcome::Applied
        );
        assert!(!stored.exists());
    }

    assert_eq!(skills(&mut connection, root).len(), 1);
    execute(&mut connection, root, Command::Sweep).unwrap();
    assert!(stored.join("SKILL.md").is_file());
}

#[test]
fn unknown_and_uninstalled_entries_are_refused_without_changes_and_none_is_forced() {
    let (dir, mut connection) = database();
    let root = dir.path();
    let cases = [
        (
            SetupAction::Remove {
                id: "graphify".into(),
                version: 0,
            },
            "pack_not_installed",
        ),
        (
            SetupAction::Set {
                id: "graphify".into(),
                on: false,
                version: 0,
            },
            "pack_not_installed",
        ),
        (
            SetupAction::Set {
                id: "superpowers".into(),
                on: false,
                version: 0,
            },
            "pack_not_installed",
        ),
        (
            SetupAction::Run {
                pack: vec!["no-such-entry".into()],
            },
            "pack_unknown",
        ),
        (
            SetupAction::Run {
                pack: vec!["caveman".into(), "caveman".into()],
            },
            "pack_unknown",
        ),
        (
            SetupAction::Remove {
                id: "bad id!".into(),
                version: 0,
            },
            "pack_unknown",
        ),
    ];
    for (action, code) in cases {
        assert_eq!(rejected_with(owner(&mut connection, root, action)), code);
    }
    let run = SetupAction::Run {
        pack: vec!["caveman".into()],
    };
    assert_eq!(owner(&mut connection, root, run), Outcome::Applied);
}

#[test]
fn a_setup_run_already_going_refuses_another() {
    let (dir, mut connection) = database();
    let root = dir.path();
    progress::with(root, |state| state.running = true);
    let run = SetupAction::Run { pack: vec![] };
    assert_eq!(
        rejected_with(owner(&mut connection, root, run)),
        "setup_busy"
    );
    assert!(status_of(&mut connection, root).running);
    progress::with(root, |state| state.running = false);
}

#[test]
fn an_entry_whose_skill_name_is_taken_installs_nothing() {
    let (dir, mut connection) = database();
    let root = dir.path();
    let mine = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    plugins::execute(
        &mut connection,
        root,
        plugins::Command::AddSkill {
            scope: Scope::Global,
            source: skill(mine.path(), "beta").display().to_string(),
        },
    )
    .unwrap();

    let refused = install(
        &mut connection,
        root,
        "superpowers",
        "https://example.invalid/pack@1",
        vec![skill(work.path(), "alpha"), skill(work.path(), "beta")],
        Some(("context-mode".into(), spec())),
    );
    assert_eq!(refused.unwrap_err().0, "pack_name_taken");
    let listed = skills(&mut connection, root);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "beta");
    assert!(servers(&mut connection, root).is_empty());
    let status = status_of(&mut connection, root);
    assert_eq!(pack_state(&status, "superpowers"), PackState::Available);
    let store: Vec<String> = std::fs::read_dir(root.join("plugins/skills/global"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(store, vec!["beta"]);
}

fn graph_on(connection: &mut Connection) -> bool {
    let tx = connection.transaction().unwrap();
    entry_switch(&tx, GRAPH_ENTRY).unwrap() == Some(true)
}

#[test]
fn a_graphify_skill_already_in_the_store_reads_installed_and_is_adopted_not_installed_again() {
    let (dir, mut connection) = database();
    let root = dir.path();
    let mine = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    assert!(!graph_on(&mut connection));
    plugins::execute(
        &mut connection,
        root,
        plugins::Command::AddSkill {
            scope: Scope::Global,
            source: skill(mine.path(), "graphify").display().to_string(),
        },
    )
    .unwrap();
    assert_eq!(
        pack_state(&status_of(&mut connection, root), "graphify"),
        PackState::On
    );
    assert!(graph_on(&mut connection));

    install(
        &mut connection,
        root,
        "graphify",
        "https://example.invalid/graphify@1",
        vec![skill(work.path(), "graphify")],
        None,
    )
    .unwrap();
    let listed = skills(&mut connection, root);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "graphify");
    assert_eq!(
        pack_state(&status_of(&mut connection, root), "graphify"),
        PackState::On
    );

    let off = SetupAction::Set {
        id: "graphify".into(),
        on: false,
        version: 0,
    };
    assert_eq!(owner(&mut connection, root, off), Outcome::Applied);
    assert_eq!(
        pack_state(&status_of(&mut connection, root), "graphify"),
        PackState::Off
    );
    assert!(!skills(&mut connection, root)[0].enabled);
    assert!(!graph_on(&mut connection));
}

#[test]
fn a_setup_run_installs_the_graphify_command_only_while_graphify_is_ticked_or_on() {
    let (dir, mut connection) = database();
    let root = dir.path();
    let mine = tempfile::tempdir().unwrap();
    let run = |pack: Vec<&str>| SetupAction::Run {
        pack: pack.into_iter().map(Into::into).collect(),
    };
    assert_eq!(
        owner(&mut connection, root, run(vec!["caveman"])),
        Outcome::Applied
    );
    assert!(!progress::snapshot(root).graph_tool);
    assert_eq!(
        owner(&mut connection, root, run(vec!["graphify"])),
        Outcome::Applied
    );
    assert!(progress::snapshot(root).graph_tool);
    assert_eq!(owner(&mut connection, root, run(vec![])), Outcome::Applied);
    assert!(!progress::snapshot(root).graph_tool);
    plugins::execute(
        &mut connection,
        root,
        plugins::Command::AddSkill {
            scope: Scope::Global,
            source: skill(mine.path(), "graphify").display().to_string(),
        },
    )
    .unwrap();
    assert_eq!(owner(&mut connection, root, run(vec![])), Outcome::Applied);
    assert!(progress::snapshot(root).graph_tool);
}

#[test]
fn a_new_pin_with_the_same_skill_names_replaces_the_old_copy() {
    let (dir, mut connection) = database();
    let root = dir.path();
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    install(
        &mut connection,
        root,
        "superpowers",
        "https://example.invalid/pack@1",
        vec![skill(first.path(), "alpha")],
        None,
    )
    .unwrap();
    let before = skills(&mut connection, root);
    install(
        &mut connection,
        root,
        "superpowers",
        "https://example.invalid/pack@1",
        vec![skill(first.path(), "alpha")],
        None,
    )
    .unwrap();
    assert_eq!(skills(&mut connection, root), before);

    let newer = skill(second.path(), "alpha");
    std::fs::write(newer.join("NOTES.md"), "second pin").unwrap();
    install(
        &mut connection,
        root,
        "superpowers",
        "https://example.invalid/pack@2",
        vec![newer],
        None,
    )
    .unwrap();
    let after = skills(&mut connection, root);
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].name, "alpha");
    assert_ne!(after[0].id, before[0].id);
    assert!(PathBuf::from(&after[0].folder).join("NOTES.md").is_file());
    let store: Vec<String> = std::fs::read_dir(root.join("plugins/skills/global"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(store, vec!["alpha"]);
}

fn owner_skill(connection: &mut Connection, root: &Path, parent: &Path, name: &str) -> String {
    let folder = skill(parent, name);
    std::fs::write(folder.join("NOTES.md"), "the owner's own copy").unwrap();
    let plugins::ResultValue::Skill(change) = plugins::execute(
        connection,
        root,
        plugins::Command::AddSkill {
            scope: Scope::Global,
            source: folder.display().to_string(),
        },
    )
    .unwrap() else {
        panic!("skill change required");
    };
    change.view.id
}

#[test]
fn an_owner_skill_named_like_an_entry_survives_every_new_pin_and_switch() {
    let (dir, mut connection) = database();
    let root = dir.path();
    let mine = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let id = owner_skill(&mut connection, root, mine.path(), "tiger-style");
    let kept = |connection: &mut Connection| {
        let listed = skills(connection, root);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, id);
        assert!(PathBuf::from(&listed[0].folder).join("NOTES.md").is_file());
        listed[0].enabled
    };

    let refused = install(
        &mut connection,
        root,
        "tiger-style",
        PRESENT,
        vec![skill(&work.path().join("0"), "tiger-style")],
        None,
    );
    assert_eq!(refused.unwrap_err().0, "pack_unknown");
    assert!(kept(&mut connection));

    for pin in ["1", "2"] {
        install(
            &mut connection,
            root,
            "tiger-style",
            &format!("https://example.invalid/tiger-style@{pin}"),
            vec![skill(&work.path().join(pin), "tiger-style")],
            None,
        )
        .unwrap();
        execute(&mut connection, root, Command::Sweep).unwrap();
        assert!(kept(&mut connection));
    }

    let off = SetupAction::Set {
        id: "tiger-style".into(),
        on: false,
        version: 0,
    };
    assert_eq!(owner(&mut connection, root, off), Outcome::Applied);
    install(
        &mut connection,
        root,
        "tiger-style",
        "https://example.invalid/tiger-style@3",
        vec![skill(&work.path().join("3"), "tiger-style")],
        None,
    )
    .unwrap();
    execute(&mut connection, root, Command::Sweep).unwrap();
    assert!(!kept(&mut connection));
    assert_eq!(
        pack_state(&status_of(&mut connection, root), "tiger-style"),
        PackState::Off
    );
    let store: Vec<String> = std::fs::read_dir(root.join("plugins/skills/global"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(store, vec!["tiger-style"]);
}

#[test]
fn an_owner_server_named_like_an_entry_keeps_its_secret_across_new_pins() {
    let (dir, mut connection) = database();
    let root = dir.path();
    let plugins::ResultValue::Server(view) = plugins::execute(
        &mut connection,
        root,
        plugins::Command::AddServer {
            scope: Scope::Global,
            name: "context-mode".into(),
            spec: spec(),
            secrets: Vec::new(),
        },
    )
    .unwrap() else {
        panic!("server view required");
    };
    let reference = plugins::new_secret_reference().unwrap();
    plugins::store_secret(root, &reference, b"dummy-owner-value".to_vec()).unwrap();
    plugins::execute(
        &mut connection,
        root,
        plugins::Command::AttachSecret {
            id: view.id.clone(),
            name: "API_TOKEN".into(),
            reference: reference.clone(),
        },
    )
    .unwrap();

    for pin in ["1", "2"] {
        install(
            &mut connection,
            root,
            "context-mode",
            &format!("https://example.invalid/context-mode@{pin}"),
            Vec::new(),
            Some(("context-mode".into(), spec())),
        )
        .unwrap();
        execute(&mut connection, root, Command::Sweep).unwrap();
        let listed = servers(&mut connection, root);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, view.id);
        assert!(
            root.join(plugins::STORE)
                .join(plugins::SECRETS)
                .join(&reference)
                .is_file()
        );
    }
}

#[test]
fn a_pack_skill_named_like_its_entry_is_still_replaced_by_a_new_pin() {
    let (dir, mut connection) = database();
    let root = dir.path();
    let work = tempfile::tempdir().unwrap();
    install(
        &mut connection,
        root,
        "graphify",
        "https://example.invalid/graphify@1",
        vec![skill(&work.path().join("1"), "graphify")],
        None,
    )
    .unwrap();
    let before = skills(&mut connection, root);
    let newer = skill(&work.path().join("2"), "graphify");
    std::fs::write(newer.join("NOTES.md"), "second pin").unwrap();
    install(
        &mut connection,
        root,
        "graphify",
        "https://example.invalid/graphify@2",
        vec![newer],
        None,
    )
    .unwrap();
    let after = skills(&mut connection, root);
    assert_eq!(after.len(), 1);
    assert_ne!(after[0].id, before[0].id);
    assert!(PathBuf::from(&after[0].folder).join("NOTES.md").is_file());
}

#[test]
fn a_new_pin_keeps_the_owner_switches_secrets_and_an_entry_left_off() {
    let (dir, mut connection) = database();
    let root = dir.path();
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let third = tempfile::tempdir().unwrap();
    connection
        .execute("INSERT INTO projects(id, body) VALUES('app', '{}')", [])
        .unwrap();
    let app = crate::protocol::ProjectId::new("app").unwrap();
    install(
        &mut connection,
        root,
        "superpowers",
        "https://example.invalid/pack@1",
        vec![skill(first.path(), "alpha")],
        Some(("context-mode".into(), spec())),
    )
    .unwrap();
    let server = servers(&mut connection, root)[0].id.clone();
    let reference = plugins::new_secret_reference().unwrap();
    plugins::store_secret(root, &reference, b"dummy-owner-value".to_vec()).unwrap();
    plugins::execute(
        &mut connection,
        root,
        plugins::Command::AttachSecret {
            id: server.clone(),
            name: "API_TOKEN".into(),
            reference: reference.clone(),
        },
    )
    .unwrap();
    let alpha = skills(&mut connection, root).remove(0);
    plugins::execute(
        &mut connection,
        root,
        plugins::Command::Switch {
            project: app.clone(),
            id: alpha.id,
            on: false,
            version: alpha.version,
        },
    )
    .unwrap();
    let off = SetupAction::Set {
        id: "superpowers".into(),
        on: false,
        version: status_of(&mut connection, root).version,
    };
    assert_eq!(owner(&mut connection, root, off), Outcome::Applied);

    let newer = ServerSpec::Stdio {
        command: "/usr/bin/true".into(),
        args: vec!["--stdio".into(), "--newer".into()],
    };
    install(
        &mut connection,
        root,
        "superpowers",
        "https://example.invalid/pack@2",
        vec![skill(second.path(), "alpha")],
        Some(("context-mode".into(), newer.clone())),
    )
    .unwrap();
    execute(&mut connection, root, Command::Sweep).unwrap();

    let status = status_of(&mut connection, root);
    assert_eq!(pack_state(&status, "superpowers"), PackState::Off);
    let after = skills(&mut connection, root);
    assert_eq!(after.len(), 1);
    assert!(!after[0].enabled);
    let plugins::ResultValue::Skills(here, _) = plugins::execute(
        &mut connection,
        root,
        plugins::Command::Skills {
            project: Some(app),
            offset: 0,
        },
    )
    .unwrap() else {
        panic!("skill list required");
    };
    assert_eq!(here[0].on_here, Some(false));
    let listed = servers(&mut connection, root);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, server);
    assert_eq!(listed[0].spec, newer);
    assert!(!listed[0].enabled);
    assert_eq!(
        listed[0].secrets,
        vec![crate::plugins::SecretView {
            name: "API_TOKEN".into(),
            saved: true,
        }]
    );
    assert!(
        root.join(plugins::STORE)
            .join(plugins::SECRETS)
            .join(&reference)
            .is_file()
    );

    let on = SetupAction::Set {
        id: "superpowers".into(),
        on: true,
        version: status_of(&mut connection, root).version,
    };
    assert_eq!(owner(&mut connection, root, on), Outcome::Applied);
    assert_ne!(
        servers(&mut connection, root)[0].status_label,
        "Approved",
        "a changed server the owner had switched off waits for approval"
    );
    install(
        &mut connection,
        root,
        "superpowers",
        "https://example.invalid/pack@3",
        vec![skill(third.path(), "alpha")],
        Some(("context-mode".into(), spec())),
    )
    .unwrap();
    let listed = servers(&mut connection, root);
    assert_eq!(listed[0].id, server);
    assert_eq!(listed[0].status_label, "Approved");
    assert!(skills(&mut connection, root)[0].enabled);
}

fn hook_plugin(parent: &Path, name: &str) -> PathBuf {
    let folder = parent.join(name);
    std::fs::create_dir_all(folder.join("hooks")).unwrap();
    std::fs::write(
        folder.join("hooks/hooks.json"),
        r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"sh \"${CLAUDE_PLUGIN_ROOT}/hooks/start.sh\""}]}],"UserPromptSubmit":[{"matcher":"","hooks":[{"type":"command","command":"sh \"${CLAUDE_PLUGIN_ROOT}/hooks/prompt.sh\"","timeout":5}]}]}}"#,
    )
    .unwrap();
    for script in ["start.sh", "prompt.sh"] {
        std::fs::write(folder.join("hooks").join(script), "exit 0\n").unwrap();
    }
    std::fs::create_dir_all(folder.join(".claude-plugin")).unwrap();
    std::fs::write(
        folder.join(".claude-plugin/plugin.json"),
        format!(r#"{{"name":"{name}"}}"#),
    )
    .unwrap();
    folder
}

fn hook_views(
    connection: &mut Connection,
    root: &Path,
    project: Option<crate::protocol::ProjectId>,
) -> Vec<crate::plugins::HookView> {
    let plugins::ResultValue::Hooks(views, next) = plugins::execute(
        connection,
        root,
        plugins::Command::Hooks { project, offset: 0 },
    )
    .unwrap() else {
        panic!("hook list required");
    };
    assert!(next.is_none());
    views
}

fn seed(
    connection: &mut Connection,
    root: &Path,
    id: &str,
    plugin: &Path,
) -> Result<(), EngineError> {
    execute(
        connection,
        root,
        Command::SeedHooks {
            id: id.into(),
            plugin: plugin.to_path_buf(),
        },
    )
}

fn effective_events(connection: &mut Connection, root: &Path, project: &str) -> Vec<String> {
    let project = crate::protocol::ProjectId::new(project).unwrap();
    let tx = connection.transaction().unwrap();
    let hooks = plugins::effective_hooks(&tx, root, &project).unwrap();
    for hook in &hooks {
        assert!(hook.folder.join("hooks/start.sh").is_file(), "{hook:?}");
    }
    hooks.into_iter().map(|hook| hook.event).collect()
}

#[test]
fn seeded_hooks_follow_their_entry_and_turn_off_per_hook_per_project_or_with_the_entry() {
    let (dir, mut connection) = database();
    let root = dir.path();
    connection
        .execute(
            "INSERT INTO projects(id, body) VALUES('project-1', '{}')",
            [],
        )
        .unwrap();
    let work = tempfile::tempdir().unwrap();
    install(
        &mut connection,
        root,
        "caveman",
        "https://example.invalid/caveman@1",
        vec![skill(work.path(), "caveman")],
        None,
    )
    .unwrap();
    let plugin = hook_plugin(work.path(), "caveman");
    seed(&mut connection, root, "caveman", &plugin).unwrap();
    let views = hook_views(&mut connection, root, None);
    let mut events: Vec<&str> = views.iter().map(|view| view.event.as_str()).collect();
    events.sort();
    assert_eq!(events, ["SessionStart", "UserPromptSubmit"]);
    assert!(views.iter().all(|view| view.enabled
        && view.plugin == "caveman"
        && view.scope == Scope::Global
        && view.follows.as_deref() == Some("caveman")));
    seed(&mut connection, root, "caveman", &plugin).unwrap();
    assert_eq!(hook_views(&mut connection, root, None).len(), 2);
    assert_eq!(
        effective_events(&mut connection, root, "project-1"),
        ["SessionStart", "UserPromptSubmit"]
    );

    let start = views
        .iter()
        .find(|view| view.event == "SessionStart")
        .unwrap()
        .id
        .clone();
    let prompt = views
        .iter()
        .find(|view| view.event == "UserPromptSubmit")
        .unwrap()
        .id
        .clone();
    plugins::execute(
        &mut connection,
        root,
        plugins::Command::SetHook {
            id: start.clone(),
            enabled: false,
        },
    )
    .unwrap();
    assert_eq!(
        effective_events(&mut connection, root, "project-1"),
        ["UserPromptSubmit"]
    );
    let version = plugins::plugin_version(&connection, &prompt).unwrap();
    plugins::execute(
        &mut connection,
        root,
        plugins::Command::Switch {
            project: crate::protocol::ProjectId::new("project-1").unwrap(),
            id: prompt.clone(),
            on: false,
            version,
        },
    )
    .unwrap();
    assert!(effective_events(&mut connection, root, "project-1").is_empty());
    assert_eq!(
        effective_events(&mut connection, root, "project-2"),
        ["UserPromptSubmit"]
    );
    let here = hook_views(
        &mut connection,
        root,
        Some(crate::protocol::ProjectId::new("project-1").unwrap()),
    );
    assert_eq!(
        here.iter().find(|view| view.id == prompt).unwrap().on_here,
        Some(false)
    );

    let off = SetupAction::Set {
        id: "caveman".into(),
        on: false,
        version: status_of(&mut connection, root).version,
    };
    assert_eq!(owner(&mut connection, root, off), Outcome::Applied);
    assert!(effective_events(&mut connection, root, "project-2").is_empty());
    let on = SetupAction::Set {
        id: "caveman".into(),
        on: true,
        version: status_of(&mut connection, root).version,
    };
    assert_eq!(owner(&mut connection, root, on), Outcome::Applied);
    assert_eq!(
        effective_events(&mut connection, root, "project-2"),
        ["UserPromptSubmit"]
    );

    let folders: Vec<PathBuf> = {
        let tx = connection.transaction().unwrap();
        let project = crate::protocol::ProjectId::new("project-2").unwrap();
        plugins::effective_hooks(&tx, root, &project)
            .unwrap()
            .into_iter()
            .map(|hook| hook.folder)
            .collect()
    };
    assert_eq!(folders.len(), 1);
    let remove = SetupAction::Remove {
        id: "caveman".into(),
        version: status_of(&mut connection, root).version,
    };
    assert_eq!(owner(&mut connection, root, remove), Outcome::Applied);
    assert!(hook_views(&mut connection, root, None).is_empty());
    let id = folders[0].file_name().unwrap().to_str().unwrap();
    let noted: i64 = connection
        .query_row(
            "SELECT count(*) FROM file_trash WHERE kind='hook_folder' AND name=?1",
            [id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(noted, 1, "the removal notes the folder in the file trash");
    crate::storage::trash::sweep(&connection, root, None).unwrap();
    assert!(!folders[0].exists(), "{}", folders[0].display());
}

#[test]
fn no_hooks_are_seeded_for_an_entry_not_installed_switched_off_or_without_hooks() {
    let (dir, mut connection) = database();
    let root = dir.path();
    let work = tempfile::tempdir().unwrap();
    let plugin = hook_plugin(work.path(), "superpowers");
    seed(&mut connection, root, "superpowers", &plugin).unwrap();
    assert!(hook_views(&mut connection, root, None).is_empty());

    install(
        &mut connection,
        root,
        "superpowers",
        "https://example.invalid/pack@1",
        vec![skill(work.path(), "alpha")],
        None,
    )
    .unwrap();
    let off = SetupAction::Set {
        id: "superpowers".into(),
        on: false,
        version: status_of(&mut connection, root).version,
    };
    assert_eq!(owner(&mut connection, root, off), Outcome::Applied);
    seed(&mut connection, root, "superpowers", &plugin).unwrap();
    assert!(hook_views(&mut connection, root, None).is_empty());

    install(
        &mut connection,
        root,
        "caveman",
        "https://example.invalid/caveman@1",
        vec![skill(work.path(), "caveman")],
        None,
    )
    .unwrap();
    let hookless = work.path().join("hookless");
    std::fs::create_dir(&hookless).unwrap();
    seed(&mut connection, root, "caveman", &hookless).unwrap();
    assert!(hook_views(&mut connection, root, None).is_empty());
}

fn hooks_refused(status: &SetupStatus, id: &str) -> Option<String> {
    status
        .pack
        .iter()
        .find(|entry| entry.id == id)
        .unwrap()
        .hooks_refused
        .clone()
}

#[test]
fn a_hooks_pack_without_plugin_json_is_refused_and_its_setup_row_says_why_until_seeded() {
    let (dir, mut connection) = database();
    let root = dir.path();
    let work = tempfile::tempdir().unwrap();
    install(
        &mut connection,
        root,
        "caveman",
        "https://example.invalid/caveman@1",
        vec![skill(work.path(), "caveman")],
        None,
    )
    .unwrap();
    assert_eq!(
        hooks_refused(&status_of(&mut connection, root), "caveman"),
        None
    );
    let plugin = hook_plugin(work.path(), "caveman-hooks");
    std::fs::remove_dir_all(plugin.join(".claude-plugin")).unwrap();
    let refused = seed(&mut connection, root, "caveman", &plugin).map_err(|problem| problem.0);
    assert_eq!(refused, Err("hooks_plugin_manifest_missing".to_owned()));
    assert!(hook_views(&mut connection, root, None).is_empty());
    let sentence = "This folder has hooks but no .claude-plugin/plugin.json naming the plugin.";
    assert_eq!(
        crate::plugins::plain_problem("hooks_plugin_manifest_missing"),
        Some(sentence)
    );
    let status = status_of(&mut connection, root);
    assert_eq!(pack_state(&status, "caveman"), PackState::On);
    assert_eq!(hooks_refused(&status, "caveman").as_deref(), Some(sentence));
    assert_eq!(hooks_refused(&status, "superpowers"), None);

    std::fs::create_dir_all(plugin.join(".claude-plugin")).unwrap();
    std::fs::write(
        plugin.join(".claude-plugin/plugin.json"),
        r#"{"name":"caveman-hooks"}"#,
    )
    .unwrap();
    seed(&mut connection, root, "caveman", &plugin).unwrap();
    assert_eq!(hook_views(&mut connection, root, None).len(), 2);
    assert_eq!(
        hooks_refused(&status_of(&mut connection, root), "caveman"),
        None
    );
}

#[test]
fn hooks_seeded_for_an_adopted_owner_skill_follow_that_skill() {
    let (dir, mut connection) = database();
    let root = dir.path();
    let mine = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    plugins::execute(
        &mut connection,
        root,
        plugins::Command::AddSkill {
            scope: Scope::Global,
            source: skill(mine.path(), "graphify").display().to_string(),
        },
    )
    .unwrap();
    install(
        &mut connection,
        root,
        "graphify",
        "https://example.invalid/graphify@1",
        vec![skill(work.path(), "graphify")],
        None,
    )
    .unwrap();
    let owned = skills(&mut connection, root);
    assert_eq!(owned.len(), 1);
    seed(
        &mut connection,
        root,
        "graphify",
        &hook_plugin(work.path(), "graphify-hooks"),
    )
    .unwrap();
    let views = hook_views(&mut connection, root, None);
    assert_eq!(views.len(), 2);
    assert!(
        views
            .iter()
            .all(|view| view.follows.as_deref() == Some("graphify"))
    );
    assert_eq!(
        effective_events(&mut connection, root, "project-1").len(),
        2
    );
    let version = skills(&mut connection, root)
        .into_iter()
        .find(|skill| skill.id == owned[0].id)
        .unwrap()
        .version;
    plugins::execute(
        &mut connection,
        root,
        plugins::Command::SetSkill {
            id: owned[0].id.clone(),
            enabled: false,
            version,
        },
    )
    .unwrap();
    assert!(effective_events(&mut connection, root, "project-1").is_empty());
}
