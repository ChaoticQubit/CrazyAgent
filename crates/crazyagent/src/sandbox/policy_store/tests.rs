use super::*;
use kernel_sandbox::policy::*;
use kernel_sandbox::toolchains::{ToolHomes, Toolchain};
use std::path::Path;

/// DEC-173 add.1: a policy saved before the agent home caches setting reads Clear when
/// access is raised, each choice survives a save, a load and a pin, and the view lists
/// every choice with the words the owner reads.
#[test]
fn a_policy_saved_before_the_home_caches_setting_clears_when_access_is_raised() {
    let old: AgentPolicy =
        serde_json::from_str(r#"{"access":"sandboxed","toolchains":[]}"#).unwrap();
    assert_eq!(old.home_caches, HomeCaches::ClearWhenRaised);
    let connection = Connection::open_in_memory().unwrap();
    let folder = tempfile::tempdir().unwrap();
    for choice in HomeCaches::ALL {
        let policy = AgentPolicy {
            home_caches: choice,
            ..AgentPolicy::default()
        };
        let body = serde_json::to_string(&policy).unwrap();
        assert!(
            body.contains(&format!(r#""home_caches":"{}""#, choice.id())),
            "{body}"
        );
        save(&connection, "p", &policy).unwrap();
        let read = load(&connection, "p").unwrap();
        assert_eq!(read.home_caches, choice);
        assert_eq!(read.pinned(folder.path()).home_caches, choice);
        let view = read.view(
            &crate::sandbox::POLICY_HOOKS,
            kernel_sandbox::host::linux_host_kept(),
            folder.path(),
            &ToolHomes::at(folder.path()),
            1,
            None,
            true,
        );
        assert_eq!(view.home_caches, choice);
        let listed: Vec<(&str, &str)> = view
            .home_cache_choices
            .iter()
            .map(|row| (row.choice.id(), row.label.as_str()))
            .collect();
        assert_eq!(
            listed,
            [
                ("keep", "Keep"),
                ("clear_when_raised", "Clear when access is raised"),
                ("clear_every_launch", "Clear every launch")
            ]
        );
    }
    assert!(
        serde_json::from_str::<AgentPolicy>(
            r#"{"access":"sandboxed","toolchains":[],"home_caches":"clear_sometimes"}"#
        )
        .is_err()
    );
}

#[test]
fn full_access_on_windows_names_no_level_this_version_refuses_there() {
    let plain = crate::config::plain_problem(SANDBOX_FULL_UNAVAILABLE_WINDOWS)
        .expect("the code has a plain sentence");
    for level in [AgentAccess::Sandboxed, AgentAccess::OwnBranch] {
        assert!(!plain.contains(level.label()), "{plain}");
    }
    assert!(!plain.contains("always run"), "{plain}");
    #[cfg(target_os = "windows")]
    {
        assert!(
            AgentAccess::ALL
                .iter()
                .all(|level| level.refusal().is_some())
        );
        for level in [AgentAccess::Sandboxed, AgentAccess::OwnBranch] {
            assert!(!FULL_WINDOWS_UNAVAILABLE.contains(level.label()));
        }
        assert!(!FULL_WINDOWS_UNAVAILABLE.contains("always run"));
    }
}

#[test]
fn a_saved_policy_round_trips_and_a_missing_or_tampered_one_is_never_wider() {
    let connection = Connection::open_in_memory().unwrap();
    assert_eq!(load(&connection, "p").unwrap(), AgentPolicy::default());
    let policy = AgentPolicy {
        access: AgentAccess::Full,
        network: AgentNetwork::default(),
        toolchains: vec![ToolchainChoice {
            toolchain: Toolchain::Python,
            allowed: false,
        }],
        ide_git_hooks: true,
        tool_cache: Default::default(),
        home_caches: Default::default(),
        ide_agent_files: Default::default(),
        shared_folders: Default::default(),
        ..Default::default()
    };
    save(&connection, "p", &policy).unwrap();
    assert_eq!(load(&connection, "p").unwrap(), policy);
    let view = policy.view(
        &crate::sandbox::POLICY_HOOKS,
        kernel_sandbox::host::linux_host_kept(),
        Path::new("/nonexistent"),
        &ToolHomes::at(Path::new("/nonexistent")),
        0,
        Default::default(),
        true,
    );
    assert!(view.ide_git_hooks);
    connection
        .execute(
            "UPDATE project_agent_policy SET body=?1 WHERE project='p'",
            [r#"{"access":"full","toolchains":[]}"#],
        )
        .unwrap();
    let old = load(&connection, "p").unwrap();
    assert_eq!(old.access, AgentAccess::Full);
    assert!(!old.ide_git_hooks);
    assert!(!AgentPolicy::default().ide_git_hooks);
    assert_eq!(load(&connection, "other").unwrap(), AgentPolicy::default());
    for body in [
            r#"{"access":"root","toolchains":[]}"#.to_owned(),
            r#"{"access":"full","toolchains":[],"extra":1}"#.to_owned(),
            r#"{"access":"full","toolchains":[],"ide_git_hooks":"yes"}"#.to_owned(),
            r#"{"access":"full","toolchains":[{"toolchain":"node","allowed":true},{"toolchain":"node","allowed":false}]}"#.to_owned(),
            format!(r#"{{"access":"full","toolchains":[],"pad":"{}"}}"#, "x".repeat(BODY_BYTES_MAX)),
        ] {
            connection
                .execute(
                    "UPDATE project_agent_policy SET body=?1 WHERE project='p'",
                    [&body],
                )
                .unwrap();
            assert!(load(&connection, "p").is_err(), "{body}");
        }
    remove(&connection, "p").unwrap();
    assert_eq!(load(&connection, "p").unwrap(), AgentPolicy::default());
}

#[test]
fn a_saved_network_choice_reads_back_and_defaults_to_open() {
    let connection = Connection::open_in_memory().unwrap();
    assert_eq!(AgentNetwork::default(), AgentNetwork::Open);
    assert_eq!(load(&connection, "p").unwrap().network, AgentNetwork::Open);
    let mut policy = AgentPolicy::default();
    for choice in AgentNetwork::ALL {
        policy.network = choice;
        save(&connection, "p", &policy).unwrap();
        assert_eq!(load(&connection, "p").unwrap().network, choice);
    }
    connection.execute(TABLE, []).unwrap();
    connection
        .execute(
            "INSERT INTO project_agent_policy (project, body) VALUES ('old', ?1)
                 ON CONFLICT(project) DO UPDATE SET body=excluded.body",
            [r#"{"access":"own_branch","toolchains":[]}"#],
        )
        .unwrap();
    let old = load(&connection, "old").unwrap();
    assert_eq!(old.access, AgentAccess::OwnBranch);
    assert_eq!(old.network, AgentNetwork::Open);
}

/// DEC-124: the agent folders choice reads back as saved, through the view and a pinned
/// policy; a policy saved before the setting existed reads Allow all changes, and an
/// unknown choice makes the policy unreadable, so it never reads as a wider one.
#[test]
fn a_saved_agent_folders_choice_reads_back_and_an_older_policy_reads_allow_all() {
    let connection = Connection::open_in_memory().unwrap();
    let nowhere = Path::new("/nonexistent");
    assert_eq!(
        load(&connection, "p").unwrap().ide_agent_files,
        IdeAgentFiles::AllowAll
    );
    assert_eq!(
        IdeAgentFiles::ALL.map(IdeAgentFiles::label),
        ["Allow all changes", "No delete or rename", "Read only"]
    );
    let mut policy = AgentPolicy::default();
    for choice in IdeAgentFiles::ALL {
        policy.ide_agent_files = choice;
        save(&connection, "p", &policy).unwrap();
        let read = load(&connection, "p").unwrap();
        assert_eq!(read.ide_agent_files, choice);
        assert_eq!(read.pinned(nowhere).ide_agent_files, choice);
        assert_eq!(
            read.view(
                &crate::sandbox::POLICY_HOOKS,
                kernel_sandbox::host::linux_host_kept(),
                nowhere,
                &ToolHomes::at(nowhere),
                0,
                None,
                true
            )
            .ide_agent_files,
            choice
        );
    }
    for (body, choice) in [
        (
            r#"{"access":"own_branch","toolchains":[]}"#,
            IdeAgentFiles::AllowAll,
        ),
        (
            r#"{"access":"sandboxed","toolchains":[],"ide_agent_files":"no_delete_or_rename"}"#,
            IdeAgentFiles::NoDeleteOrRename,
        ),
        (
            r#"{"access":"sandboxed","toolchains":[],"ide_agent_files":"read_only"}"#,
            IdeAgentFiles::ReadOnly,
        ),
    ] {
        connection
            .execute(
                "UPDATE project_agent_policy SET body=?1 WHERE project='p'",
                [body],
            )
            .unwrap();
        assert_eq!(load(&connection, "p").unwrap().ide_agent_files, choice);
    }
    for body in [
        r#"{"access":"sandboxed","toolchains":[],"ide_agent_files":"delete_all"}"#,
        r#"{"access":"sandboxed","toolchains":[],"ide_agent_files":true}"#,
    ] {
        connection
            .execute(
                "UPDATE project_agent_policy SET body=?1 WHERE project='p'",
                [body],
            )
            .unwrap();
        assert!(load(&connection, "p").is_err(), "{body}");
    }
}

#[test]
fn a_supervised_linux_says_its_narrower_lines_and_root_says_why_none_of_them_hold() {
    use kernel_sandbox::host::LinuxHost;
    use kernel_sandbox::lines::{
        LINUX_KEPT_GIT_LINE, LINUX_NESTED_GIT_LINE, LINUX_PROCESS_LIST_LINE, LINUX_ROOT_LINE,
        LINUX_SIGNALS_LINE, LINUX_SUPERVISED_LINE,
    };
    use kernel_sandbox::settings::LinuxSupervisor;
    let held = |nested, scoped| LinuxHost {
        nested,
        scoped,
        supervised: true,
        root: false,
        armored: false,
        routed: false,
    };
    let root = LinuxHost {
        nested: false,
        scoped: false,
        supervised: false,
        root: true,
        armored: false,
        routed: false,
    };
    let sandboxed = |host, found| linux_gaps(AgentAccess::Sandboxed, Some(host), found);
    let (kept, said) = (LINUX_KEPT_GIT_LINE, LINUX_SUPERVISED_LINE);
    assert_eq!(sandboxed(held(true, false), false), [said]);
    assert_eq!(sandboxed(held(true, true), true), [kept, said]);
    for scoped in [false, true] {
        let listed = [kept, said, LINUX_PROCESS_LIST_LINE];
        assert_eq!(
            sandboxed(held(false, scoped), true),
            listed,
            "scoped {scoped}"
        );
    }
    let open = [LINUX_ROOT_LINE, LINUX_NESTED_GIT_LINE, LINUX_SIGNALS_LINE];
    assert_eq!(sandboxed(root, true), open);
    assert_eq!(sandboxed(root, false), open);
    for host in [held(false, false), root] {
        assert!(linux_gaps(AgentAccess::Full, Some(host), true).is_empty());
    }
    let (on, off) = (Some(LinuxSupervisor::On), Some(LinuxSupervisor::Off));
    assert_eq!(held(true, false).under(on), held(true, false));
    assert!(!held(true, false).under(off).supervised);
    assert!(
        !held(true, false).under(None).supervised,
        "unreadable shows every line"
    );
    assert_eq!(root.under(on), root);
    assert_eq!(root.under(None), root);
    assert!(
        !root.under(off).root,
        "with the supervisor Off, root changes nothing"
    );
    let catalog = LevelView::on(Some(held(true, false)), true);
    assert_eq!(catalog[0].access, AgentAccess::Sandboxed);
    let told: Vec<&str> = catalog[0].gaps.iter().map(String::as_str).collect();
    assert_eq!(
        told,
        [kept, said],
        "the catalog has no project to rule one out"
    );
    let unread = LevelView::other_git(
        Some(held(true, false)),
        Path::new("/nonexistent"),
        kernel_sandbox::host::linux_other_git,
    );
    assert!(unread, "a folder that cannot be read rules out none");
    let quiet = LevelView::on(Some(held(true, false)), false);
    let told: Vec<&str> = quiet[0].gaps.iter().map(String::as_str).collect();
    assert_eq!(told, [said], "a project with no other repository");
}

/// The Agent access page reads the project folder for another git repository before its
/// database transaction, and only where the call supervisor runs; the view takes that
/// answer (rv-secreview linux-t4c note 1, rv-code4 linux-t4c N2, DEC-179 T4d).
#[test]
fn only_a_supervised_host_reads_the_project_for_another_repository_and_the_view_takes_it() {
    use kernel_sandbox::host::LinuxHost;
    use kernel_sandbox::lines::LINUX_KEPT_GIT_LINE;
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    std::fs::create_dir_all(project.join("vendor/.git")).unwrap();
    let missing = temp.path().join("missing");
    let host = |supervised| LinuxHost {
        nested: true,
        scoped: true,
        supervised,
        root: false,
        armored: false,
        routed: false,
    };
    assert!(LevelView::other_git(
        Some(host(true)),
        &project,
        kernel_sandbox::host::linux_other_git
    ));
    assert!(LevelView::other_git(
        Some(host(true)),
        &missing,
        kernel_sandbox::host::linux_other_git
    ));
    for unread in [Some(host(false)), None] {
        assert!(
            !LevelView::other_git(unread, &project, kernel_sandbox::host::linux_other_git),
            "{unread:?}"
        );
        assert!(
            !LevelView::other_git(unread, &missing, kernel_sandbox::host::linux_other_git),
            "{unread:?}"
        );
    }
    let lines = |other_git| LevelView::on(Some(host(true)), other_git).remove(0).gaps;
    assert!(lines(true).contains(&LINUX_KEPT_GIT_LINE.to_owned()));
    assert!(!lines(false).contains(&LINUX_KEPT_GIT_LINE.to_owned()));
}

/// A body an older engine saved reads through the kernel policy type, saves again and reads
/// back to the same policy, a second save writes the same bytes as the first, and the saved
/// bytes match the stored format field for field, every option value spelled as it is kept.
#[test]
fn a_saved_policy_reads_back_byte_equal() {
    let connection = Connection::open_in_memory().unwrap();
    connection.execute(TABLE, []).unwrap();
    connection
        .execute(
            "INSERT INTO project_agent_policy (project, body) VALUES ('older', ?1)",
            [r#"{"access":"sandboxed","toolchains":[]}"#],
        )
        .unwrap();
    let older = load(&connection, "older").unwrap();
    assert_eq!(older.access, AgentAccess::Sandboxed);
    assert_eq!(older.apply_changes, AccessApply::WhenIdle);
    let body = |project: &str| -> String {
        connection
            .query_row(
                "SELECT body FROM project_agent_policy WHERE project=?1",
                [project],
                |row| row.get(0),
            )
            .unwrap()
    };
    save(&connection, "first", &older).unwrap();
    let first = load(&connection, "first").unwrap();
    assert_eq!(first, older);
    save(&connection, "second", &first).unwrap();
    assert_eq!(body("second"), body("first"));
    assert_eq!(body("first"), serde_json::to_string(&older).unwrap());
    assert_eq!(
        body("first"),
        concat!(
            r#"{"access":"sandboxed","toolchains":[],"ide_git_hooks":false,"network":"open","#,
            r#""tool_cache":"shared","home_caches":"clear_when_raised","#,
            r#""ide_agent_files":"allow_all","apply_changes":"when_idle","grants":[],"#,
            r#""shared_folders":{"scope":"all","access":"read_only","chosen":[],"secrets":[]}}"#
        )
    );
    if cfg!(unix) {
        let every = AgentPolicy {
            access: AgentAccess::OwnBranch,
            toolchains: vec![ToolchainChoice {
                toolchain: Toolchain::Python,
                allowed: false,
            }],
            ide_git_hooks: true,
            network: AgentNetwork::Https,
            tool_cache: ToolCacheScope::PerAgent,
            home_caches: HomeCaches::Keep,
            ide_agent_files: IdeAgentFiles::ReadOnly,
            apply_changes: AccessApply::NextStart,
            grants: vec![
                AccessGrant::Socket {
                    path: "/tmp/agent.sock".into(),
                },
                AccessGrant::Port { port: 47_821 },
                AccessGrant::TempFolder,
            ],
            shared_folders: Default::default(),
        };
        save(&connection, "every", &every).unwrap();
        assert_eq!(
            body("every"),
            concat!(
                r#"{"access":"own_branch","toolchains":[{"toolchain":"python","allowed":false}],"#,
                r#""ide_git_hooks":true,"network":"https","tool_cache":"per_agent","#,
                r#""home_caches":"keep","ide_agent_files":"read_only","apply_changes":"next_start","#,
                r#""grants":[{"kind":"socket","path":"/tmp/agent.sock"},{"kind":"port","port":47821},"#,
                r#"{"kind":"temp_folder"}],"#,
                r#""shared_folders":{"scope":"all","access":"read_only","chosen":[],"secrets":[]}}"#
            )
        );
        assert_eq!(load(&connection, "every").unwrap(), every);
    }
}
