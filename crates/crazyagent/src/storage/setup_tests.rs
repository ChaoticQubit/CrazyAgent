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
    };
    assert_eq!(owner(&mut connection, root, off), Outcome::Applied);
    assert!(!skills(&mut connection, root)[0].enabled);
    assert_eq!(servers(&mut connection, root)[0].status_label, "Off");
    let status = status_of(&mut connection, root);
    assert_eq!(pack_state(&status, "superpowers"), PackState::Off);

    let on = SetupAction::Set {
        id: "superpowers".into(),
        on: true,
    };
    assert_eq!(owner(&mut connection, root, on), Outcome::Applied);
    assert!(skills(&mut connection, root)[0].enabled);
    assert_eq!(servers(&mut connection, root)[0].status_label, "Approved");
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
            },
            "pack_not_installed",
        ),
        (
            SetupAction::Set {
                id: "graphify".into(),
                on: false,
            },
            "pack_not_installed",
        ),
        (
            SetupAction::Set {
                id: "superpowers".into(),
                on: false,
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
    entry_on(&tx, GRAPH_ENTRY).unwrap()
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
