use super::*;
use crate::plugins::{ServerView, SkillView};
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

fn installed_packs(connection: &mut Connection, root: &Path) -> Vec<InstalledPack> {
    let ResultValue::Installed(packs) = execute(connection, root, Command::Installed).unwrap()
    else {
        panic!("installed list required");
    };
    packs
}

fn install(
    connection: &mut Connection,
    root: &Path,
    source: &str,
    folders: Vec<PathBuf>,
    server: Option<(String, ServerSpec)>,
) -> Result<ResultValue, EngineError> {
    execute(
        connection,
        root,
        Command::Install {
            id: "superpowers".into(),
            source: source.into(),
            skills: folders,
            server,
        },
    )
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
    assert_eq!(
        installed_packs(&mut connection, root),
        vec![InstalledPack {
            id: "superpowers".into(),
            enabled: true
        }]
    );
}

#[test]
fn switching_an_entry_off_and_on_switches_every_item_it_added() {
    let (dir, mut connection) = database();
    let root = dir.path();
    let work = tempfile::tempdir().unwrap();
    install(
        &mut connection,
        root,
        "https://example.invalid/pack@1",
        vec![skill(work.path(), "alpha")],
        Some(("context-mode".into(), spec())),
    )
    .unwrap();

    let off = Command::Set {
        id: "superpowers".into(),
        enabled: false,
    };
    execute(&mut connection, root, off).unwrap();
    assert!(!skills(&mut connection, root)[0].enabled);
    assert_eq!(servers(&mut connection, root)[0].status_label, "Off");
    assert!(!installed_packs(&mut connection, root)[0].enabled);

    let on = Command::Set {
        id: "superpowers".into(),
        enabled: true,
    };
    execute(&mut connection, root, on).unwrap();
    assert!(skills(&mut connection, root)[0].enabled);
    assert_eq!(servers(&mut connection, root)[0].status_label, "Approved");
}

#[test]
fn removing_an_entry_removes_its_items_and_their_stored_folders() {
    let (dir, mut connection) = database();
    let root = dir.path();
    let work = tempfile::tempdir().unwrap();
    install(
        &mut connection,
        root,
        "https://example.invalid/pack@1",
        vec![skill(work.path(), "alpha")],
        Some(("context-mode".into(), spec())),
    )
    .unwrap();
    let stored = PathBuf::from(&skills(&mut connection, root)[0].folder);
    assert!(stored.join("SKILL.md").is_file());

    let remove = Command::Remove {
        id: "superpowers".into(),
    };
    execute(&mut connection, root, remove).unwrap();
    assert!(skills(&mut connection, root).is_empty());
    assert!(servers(&mut connection, root).is_empty());
    assert!(installed_packs(&mut connection, root).is_empty());
    assert!(!stored.exists());
    let again = Command::Remove {
        id: "superpowers".into(),
    };
    assert_eq!(
        execute(&mut connection, root, again).err().unwrap().0,
        "pack_not_installed"
    );
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
        "https://example.invalid/pack@1",
        vec![skill(work.path(), "alpha"), skill(work.path(), "beta")],
        Some(("context-mode".into(), spec())),
    );
    assert_eq!(refused.err().unwrap().0, "pack_name_taken");
    let listed = skills(&mut connection, root);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "beta");
    assert!(servers(&mut connection, root).is_empty());
    assert!(installed_packs(&mut connection, root).is_empty());
    let store: Vec<String> = std::fs::read_dir(root.join("plugins/skills/global"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(store, vec!["beta"]);
}

#[test]
fn a_new_pin_replaces_the_items_and_the_same_pin_changes_nothing() {
    let (dir, mut connection) = database();
    let root = dir.path();
    let work = tempfile::tempdir().unwrap();
    install(
        &mut connection,
        root,
        "https://example.invalid/pack@1",
        vec![skill(work.path(), "alpha")],
        None,
    )
    .unwrap();
    let first = skills(&mut connection, root);
    install(
        &mut connection,
        root,
        "https://example.invalid/pack@1",
        vec![skill(work.path(), "alpha")],
        None,
    )
    .unwrap();
    assert_eq!(skills(&mut connection, root), first);

    install(
        &mut connection,
        root,
        "https://example.invalid/pack@2",
        vec![skill(work.path(), "beta")],
        None,
    )
    .unwrap();
    let listed = skills(&mut connection, root);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "beta");
    assert!(!PathBuf::from(&first[0].folder).exists());
}
