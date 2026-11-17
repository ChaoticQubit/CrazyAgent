use super::*;

#[test]
fn no_network_grants_a_model_only_on_loopback_and_never_a_host_port() {
    use crate::sandbox::{SandboxNetwork, SandboxRemote};
    assert_eq!(
        AgentNetwork::None.sandbox(Some(SandboxRemote::AnyHost(443))),
        SandboxNetwork::Only(vec![])
    );
    assert_eq!(
        AgentNetwork::None.sandbox(Some(SandboxRemote::Loopback(11_434))),
        SandboxNetwork::Only(vec![SandboxRemote::Loopback(11_434)])
    );
    assert_eq!(
        AgentNetwork::None.sandbox(None),
        SandboxNetwork::Only(vec![])
    );
    assert_eq!(
        AgentNetwork::Https.sandbox(Some(SandboxRemote::AnyHost(8443))),
        SandboxNetwork::Only(vec![
            SandboxRemote::AnyHost(443),
            SandboxRemote::AnyHost(8443)
        ])
    );
    assert_eq!(AgentNetwork::Open.sandbox(None), SandboxNetwork::Open);
}

/// T8: a policy saved before the tool cache setting reads Shared, a pinned policy keeps
/// the owner's choice, a save names it, and the view lists every choice, the default
/// first.
#[test]
fn a_policy_saved_before_the_tool_cache_setting_reads_shared_and_the_view_lists_every_choice() {
    let old: AgentPolicy =
        serde_json::from_str(r#"{"access":"sandboxed","toolchains":[]}"#).unwrap();
    assert_eq!(old.tool_cache, ToolCacheScope::Shared);
    let own = AgentPolicy {
        tool_cache: ToolCacheScope::PerAgent,
        ..AgentPolicy::default()
    };
    let folder = tempfile::tempdir().unwrap();
    assert_eq!(
        own.pinned(folder.path()).tool_cache,
        ToolCacheScope::PerAgent
    );
    let saved = serde_json::to_string(&own).unwrap();
    assert!(saved.contains(r#""tool_cache":"per_agent""#), "{saved}");
    let view = own.view(folder.path(), &ToolHomes::at(folder.path()), 1, None, true);
    assert_eq!(view.tool_cache, ToolCacheScope::PerAgent);
    let listed: Vec<(&str, &str)> = view
        .tool_caches
        .iter()
        .map(|row| (row.scope.id(), row.label.as_str()))
        .collect();
    assert_eq!(
        listed,
        [
            ("shared", "Shared by agents with the same access"),
            ("per_agent", "One per agent")
        ]
    );
}

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
        let view = read.view(folder.path(), &ToolHomes::at(folder.path()), 1, None, true);
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

/// F08: a policy saved before the access change settings reads When the agent is idle
/// and no grants, a pinned policy keeps both, the view lists every Apply choice, the
/// default first, and the saved grants with their words.
#[test]
fn a_policy_saved_before_the_access_change_settings_reads_when_idle_and_no_grants() {
    let old: AgentPolicy =
        serde_json::from_str(r#"{"access":"own_branch","toolchains":[]}"#).unwrap();
    assert_eq!(old.apply_changes, AccessApply::WhenIdle);
    assert!(old.grants.is_empty());
    let own = AgentPolicy {
        apply_changes: AccessApply::NextStart,
        grants: vec![AccessGrant::Port { port: 47_821 }, AccessGrant::TempFolder],
        ..AgentPolicy::default()
    };
    let folder = tempfile::tempdir().unwrap();
    let pinned = own.pinned(folder.path());
    assert_eq!(
        (pinned.apply_changes, pinned.grants.clone()),
        (own.apply_changes, own.grants.clone())
    );
    let saved = serde_json::to_string(&own).unwrap();
    assert!(saved.contains(r#""apply_changes":"next_start""#), "{saved}");
    assert!(saved.contains(r#"{"kind":"port","port":47821}"#), "{saved}");
    assert!(saved.contains(r#"{"kind":"temp_folder"}"#), "{saved}");
    let view = own.view(folder.path(), &ToolHomes::at(folder.path()), 1, None, true);
    let listed: Vec<(&str, &str)> = view
        .apply_choices
        .iter()
        .map(|row| (row.apply.id(), row.label.as_str()))
        .collect();
    assert_eq!(
        listed,
        [
            ("when_idle", "When the agent is idle"),
            ("next_start", "At its next start")
        ]
    );
    let words: Vec<&str> = view.grants.iter().map(|row| row.label.as_str()).collect();
    assert_eq!(words, ["Port 47821 on this computer", "Your temp folder"]);
    assert!(view.temp_folder.path.is_some() != view.temp_folder.unavailable.is_some());
}

/// F08: at most two sockets, four ports and one temp folder, each plain and named once.
/// A socket grant is a Unix socket path, so off Unix every socket is refused.
#[test]
fn grants_are_bounded_per_kind_plain_and_named_once() {
    let with = |grants: Vec<AccessGrant>| AgentPolicy {
        grants,
        ..AgentPolicy::default()
    };
    let socket = |name: &str| AccessGrant::Socket {
        path: std::path::PathBuf::from(format!("/private/tmp/{name}")),
    };
    let port = |port| AccessGrant::Port { port };
    let held = |mut grants: Vec<AccessGrant>| {
        grants.extend([port(1), port(2), port(3), port(4), AccessGrant::TempFolder]);
        with(grants).validate()
    };
    #[cfg(unix)]
    assert!(held(vec![socket("a"), socket("b")]).is_ok());
    #[cfg(not(unix))]
    {
        assert!(held(Vec::new()).is_ok());
        assert_eq!(
            held(vec![AccessGrant::Socket {
                path: r"C:\Temp\a".into(),
            }]),
            Err("agent_grants_invalid")
        );
    }
    for refused in [
        vec![socket("a"), socket("b"), socket("c")],
        vec![port(1), port(2), port(3), port(4), port(5)],
        vec![AccessGrant::TempFolder, AccessGrant::TempFolder],
        vec![port(9), port(9)],
        vec![port(0)],
        vec![socket("a b")],
        vec![socket("../etc")],
        vec![AccessGrant::Socket {
            path: "relative".into(),
        }],
        vec![socket(&"x".repeat(GRANT_SOCKET_BYTES_MAX))],
    ] {
        assert_eq!(
            with(refused.clone()).validate(),
            Err("agent_grants_invalid"),
            "{refused:?}"
        );
    }
}

/// F08: a running agent is widened by a wider level or network, a toolchain its profile
/// lacks or a grant it does not hold, and revoked by a grant the saved policy dropped; a
/// record that kept no level, network or toolchains never asks for a restart.
#[test]
fn a_saved_policy_widens_or_revokes_a_running_agent_only_by_what_it_holds() {
    let folder = tempfile::tempdir().unwrap();
    let root = Some(folder.path());
    let port = AccessGrant::Port { port: 47_821 };
    let saved = AgentPolicy {
        access: AgentAccess::Full,
        network: AgentNetwork::Open,
        grants: vec![port.clone()],
        ..AgentPolicy::default()
    };
    let sandboxed = Some(AgentAccess::Sandboxed);
    let none = Some(AgentNetwork::None);
    assert!(saved.widens(
        root,
        sandboxed,
        None,
        Some(&[]),
        std::slice::from_ref(&port)
    ));
    assert!(saved.widens(
        root,
        Some(AgentAccess::Full),
        none,
        Some(&[]),
        std::slice::from_ref(&port)
    ));
    assert!(saved.widens(root, Some(AgentAccess::Full), None, Some(&[]), &[]));
    assert!(!saved.widens(
        root,
        Some(AgentAccess::Full),
        Some(AgentNetwork::Open),
        Some(&[]),
        std::slice::from_ref(&port)
    ));
    assert!(!saved.widens(None, None, None, None, std::slice::from_ref(&port)));
    assert!(!saved.revokes(std::slice::from_ref(&port)));
    assert!(saved.revokes(&[port.clone(), AccessGrant::TempFolder]));
    assert!(!AgentPolicy::default().revokes(&[]));
}

/// F26: a new project records every family it uses or the owner has installed, so an
/// empty project still gets the owner's toolchains later; a family neither used nor
/// installed keeps following the project.
#[test]
fn a_new_project_records_the_toolchains_it_uses_or_the_owner_has_installed() {
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let homes = ToolHomes::at(home.path());
    assert!(recorded_toolchains(project.path(), Some(&homes)).is_empty());
    assert!(recorded_toolchains(project.path(), None).is_empty());
    std::fs::create_dir_all(home.path().join(".nvm")).unwrap();
    std::fs::write(project.path().join("Cargo.toml"), "[package]\n").unwrap();
    let recorded = recorded_toolchains(project.path(), Some(&homes));
    assert_eq!(
        recorded,
        vec![
            ToolchainChoice {
                toolchain: Toolchain::Rust,
                allowed: true
            },
            ToolchainChoice {
                toolchain: Toolchain::Node,
                allowed: true
            },
        ]
    );
    let policy = AgentPolicy {
        toolchains: recorded,
        ..AgentPolicy::default()
    };
    assert!(policy.validate().is_ok());
    std::fs::remove_file(project.path().join("Cargo.toml")).unwrap();
    assert_eq!(
        policy.allowed(project.path()),
        vec![Toolchain::Rust, Toolchain::Node],
        "a recorded family stays granted after its marker goes"
    );
}

#[test]
fn a_network_choice_is_wider_only_than_the_narrower_ones() {
    let (open, https, none) = (AgentNetwork::Open, AgentNetwork::Https, AgentNetwork::None);
    assert!(open.wider_than(https) && open.wider_than(none) && https.wider_than(none));
    for (one, other) in [
        (https, open),
        (none, open),
        (none, https),
        (open, open),
        (https, https),
        (none, none),
    ] {
        assert!(
            !one.wider_than(other),
            "{one:?} is not wider than {other:?}"
        );
    }
}

#[test]
fn each_platform_offers_only_the_levels_it_can_enforce() {
    let offered: Vec<AgentAccess> = AgentAccess::ALL
        .into_iter()
        .filter(|access| access.unavailable().is_none())
        .collect();
    #[cfg(target_os = "windows")]
    {
        assert!(offered.is_empty());
        assert!(
            AgentAccess::ALL
                .iter()
                .all(|access| access.unavailable() == Some(WINDOWS_UNAVAILABLE))
        );
    }
    #[cfg(target_os = "linux")]
    {
        assert_eq!(offered, [AgentAccess::Sandboxed]);
        assert!(
            [AgentAccess::OwnBranch, AgentAccess::Full]
                .iter()
                .all(|access| access.unavailable() == Some(LINUX_UNAVAILABLE))
        );
    }
    #[cfg(target_os = "macos")]
    assert_eq!(offered, AgentAccess::ALL);
    assert_eq!(AgentAccess::default(), AgentAccess::Sandboxed);
    assert!(AgentAccess::Full.wider_than(AgentAccess::OwnBranch));
    assert!(AgentAccess::OwnBranch.wider_than(AgentAccess::Sandboxed));
    assert!(!AgentAccess::Sandboxed.wider_than(AgentAccess::Full));
    assert!(!AgentAccess::Full.wider_than(AgentAccess::Full));
    assert_eq!(AgentAccess::NEW_PROJECT, AgentAccess::OwnBranch);
}

#[test]
fn a_running_process_is_narrowed_only_by_what_the_saved_policy_no_longer_allows() {
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("Cargo.toml"), "[package]\n").unwrap();
    let root = Some(project.path());
    let rust = [Toolchain::Rust];
    let chose = |access, choices: &[(Toolchain, bool)]| AgentPolicy {
        access,
        toolchains: choices
            .iter()
            .map(|(toolchain, allowed)| ToolchainChoice {
                toolchain: *toolchain,
                allowed: *allowed,
            })
            .collect(),
        ide_git_hooks: false,
        tool_cache: Default::default(),
        home_caches: Default::default(),
        ide_agent_files: Default::default(),
        shared_folders: Default::default(),
        network: AgentNetwork::default(),
        ..Default::default()
    };
    let sandboxed = chose(AgentAccess::Sandboxed, &[]);
    let full = Some(AgentAccess::Full);
    assert_eq!(narrowing(Some(&sandboxed), root, None, None, &rust), None);
    assert_eq!(
        narrowing(Some(&sandboxed), root, full, None, &rust),
        Some(Narrowing::Access)
    );
    assert_eq!(
        narrowing(
            Some(&chose(AgentAccess::Full, &[])),
            root,
            full,
            None,
            &rust
        ),
        None
    );
    let off = chose(AgentAccess::Sandboxed, &[(Toolchain::Rust, false)]);
    assert_eq!(
        narrowing(Some(&off), None, None, None, &rust),
        Some(Narrowing::TurnedOff)
    );
    let node = [Toolchain::Node];
    assert_eq!(
        narrowing(Some(&sandboxed), root, None, None, &node),
        Some(Narrowing::NotAllowed),
        "an unused family turned off leaves no choice behind"
    );
    let on = chose(AgentAccess::Sandboxed, &[(Toolchain::Node, true)]);
    assert_eq!(narrowing(Some(&on), None, None, None, &node), None);
    assert_eq!(
        narrowing(Some(&sandboxed), None, None, None, &rust),
        Some(Narrowing::FolderUnreadable)
    );
    assert_eq!(narrowing(Some(&sandboxed), None, None, None, &[]), None);
    assert_eq!(
        narrowing(None, root, None, None, &rust),
        Some(Narrowing::PolicyUnreadable)
    );
    assert_eq!(
        narrowing(None, root, Some(AgentAccess::OwnBranch), None, &[]),
        Some(Narrowing::PolicyUnreadable)
    );
    assert_eq!(
        narrowing(None, root, Some(AgentAccess::Sandboxed), None, &[]),
        None
    );
    assert_eq!(Narrowing::NotAllowed.code(), "agent_toolchain_lowered");
    let closed = AgentPolicy {
        network: AgentNetwork::None,
        ..sandboxed.clone()
    };
    let open = Some(AgentNetwork::Open);
    assert_eq!(
        narrowing(Some(&closed), root, None, open, &rust),
        Some(Narrowing::Network)
    );
    assert_eq!(
        narrowing(Some(&closed), root, full, open, &rust),
        Some(Narrowing::Access),
        "a wider level is named before a wider network"
    );
    assert_eq!(
        narrowing(Some(&closed), root, None, Some(AgentNetwork::None), &rust),
        None
    );
    assert_eq!(
        narrowing(Some(&closed), root, None, None, &rust),
        None,
        "a process without tools has no network rule to narrow"
    );
    assert_eq!(narrowing(Some(&sandboxed), root, None, open, &rust), None);
    assert_eq!(
        narrowing(Some(&closed), None, None, open, &rust),
        Some(Narrowing::Network),
        "a wider network is named before an unreadable folder"
    );
    assert_eq!(
        narrowing(None, root, None, open, &[]),
        Some(Narrowing::PolicyUnreadable)
    );
    assert_eq!(
        narrowing(None, root, None, Some(AgentNetwork::None), &[]),
        None
    );
    assert_eq!(Narrowing::Network.code(), "agent_network_narrowed");

    let pinned = sandboxed.pinned(project.path());
    assert_eq!(pinned.allowed(project.path()), rust);
    std::fs::remove_file(project.path().join("Cargo.toml")).unwrap();
    assert_eq!(pinned.allowed(project.path()), rust);
    assert!(sandboxed.allowed(project.path()).is_empty());
    assert_eq!(narrowing(Some(&pinned), None, None, None, &rust), None);
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
            read.view(nowhere, &ToolHomes::at(nowhere), 0, None, true)
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

/// DEC-124: Allow all changes adds no rule. No delete or rename denies every unlink and
/// Read only every write at and below each agent folder, in any letter case, and both
/// keep the agents root from being removed or moved aside. The older root is named only
/// while agents stay there.
#[cfg(unix)]
#[test]
fn an_agent_folders_rule_holds_the_agents_root_in_any_letter_case() {
    let root = Path::new("/tmp/ca-proj");
    let rule = |choice, older_agents| {
        AgentFilesRule {
            choice,
            older_agents,
        }
        .seatbelt(root)
    };
    assert_eq!(rule(IdeAgentFiles::AllowAll, false), "");
    assert_eq!(rule(IdeAgentFiles::AllowAll, true), "");
    let root_text = super::super::any_case("/tmp/ca-proj");
    let new = format!(
        "^{root_text}{}/{}",
        super::super::any_case("/.crazyagent"),
        super::super::any_case("worktrees")
    );
    let both = format!(
        "^{root_text}({})?/{}",
        super::super::any_case("/.crazyagent"),
        super::super::any_case("worktrees")
    );
    let state = format!("^{root_text}{}$", super::super::any_case("/.crazyagent"));
    for (older_agents, agents) in [(false, &new), (true, &both)] {
        let keep_root = format!("(deny file-write-unlink\n  (regex #\"{agents}$\"))\n");
        assert_eq!(
            rule(IdeAgentFiles::NoDeleteOrRename, older_agents),
            format!(
                "(deny file-write-unlink\n  (regex #\"{agents}/[^/]+$\") (regex #\"{agents}/[^/]+/\"))\n{keep_root}(deny file-write-unlink\n  (regex #\"{state}\"))\n"
            )
        );
        assert_eq!(
            rule(IdeAgentFiles::ReadOnly, older_agents),
            format!(
                "(deny file-write*\n  (regex #\"{agents}/[^/]+$\") (regex #\"{agents}/[^/]+/\"))\n{keep_root}(deny file-write*\n  (regex #\"{state}\"))\n"
            )
        );
    }
    assert!(state.contains("[cC][rR][aA][zZ][yY][aA][gG][eE][nN][tT]$"));
    assert!(new.contains("[wW][oO][rR](k|K|\u{212a})[tT][rR][eE][eE](s|S|\u{17f})"));
}

#[test]
fn only_a_none_network_this_linux_bridges_drops_the_ports_line() {
    use crate::sandbox::{LINUX_PORTS_ANY_HOST_LINE, LinuxHost, LinuxSockets};
    let host = |nested| {
        Some(LinuxHost {
            nested,
            scoped: false,
            supervised: false,
            root: false,
            armored: false,
            routed: false,
        })
    };
    let port = |port| AccessGrant::Port { port };
    let line = Some(LINUX_PORTS_ANY_HOST_LINE.to_owned());
    let tcp = Some(LinuxSockets::TcpOnly);
    let none = |nested, sockets, grants: &[AccessGrant]| {
        linux_network_gap(AgentNetwork::None, host(nested), sockets, grants)
    };
    for network in AgentNetwork::ALL {
        assert_eq!(
            linux_network_gap(network, None, tcp, &[]),
            None,
            "not Linux"
        );
        assert_eq!(linux_network_gap(network, host(false), tcp, &[]), line);
    }
    for network in [AgentNetwork::Open, AgentNetwork::Https] {
        assert_eq!(linux_network_gap(network, host(true), tcp, &[]), line);
    }
    assert_eq!(none(true, tcp, &[]), None);
    assert_eq!(
        none(true, tcp, &[port(8080), AccessGrant::TempFolder]),
        None
    );
    assert_eq!(none(true, tcp, &[port(1024)]), None);
    for sockets in [
        Some(LinuxSockets::TcpPorts),
        Some(LinuxSockets::Refuse),
        None,
    ] {
        assert_eq!(none(true, sockets, &[]), line, "{sockets:?}");
    }
    let named = none(true, tcp, &[port(8080), port(443), port(80)]).unwrap();
    assert_eq!(named, crate::sandbox::linux_unbridged_line(80));
    assert!(named.contains("port 80 is below 1024"), "{named}");
    assert_eq!(
        none(false, tcp, &[port(80)]),
        line,
        "no nest, the plain line"
    );
    let stored = r#"{"network":"none","label":"None"}"#;
    let old: NetworkView = serde_json::from_str(stored).unwrap();
    assert_eq!(old.gap, None, "a view an older engine stored");
}

#[test]
fn a_supervised_linux_under_tcp_only_drops_the_ports_line_but_names_port_53_for_https() {
    use crate::sandbox::{
        LINUX_DNS_ANY_HOST_LINE, LINUX_PORTS_ANY_HOST_LINE, LinuxHost, LinuxSockets,
    };
    let host = |nested| {
        Some(LinuxHost {
            nested,
            scoped: false,
            supervised: true,
            root: false,
            armored: false,
            routed: false,
        })
    };
    let low = [
        AccessGrant::Port { port: 80 },
        AccessGrant::Port { port: 53 },
    ];
    let ports = Some(LINUX_PORTS_ANY_HOST_LINE.to_owned());
    let dns = Some(LINUX_DNS_ANY_HOST_LINE.to_owned());
    let tcp = Some(LinuxSockets::TcpOnly);
    for nested in [false, true] {
        let gap = |network, sockets| linux_network_gap(network, host(nested), sockets, &low);
        assert_eq!(gap(AgentNetwork::None, tcp), None, "nested {nested}");
        assert_eq!(gap(AgentNetwork::Https, tcp), dns, "nested {nested}");
        assert_eq!(gap(AgentNetwork::Open, tcp), ports, "nested {nested}");
        for sockets in [
            Some(LinuxSockets::TcpPorts),
            Some(LinuxSockets::Refuse),
            None,
        ] {
            for network in [AgentNetwork::None, AgentNetwork::Https] {
                assert_eq!(gap(network, sockets), ports, "{network:?} {sockets:?}");
            }
        }
    }
}

#[test]
fn a_routed_linux_drops_the_port_443_line_with_or_without_the_supervisor() {
    use crate::sandbox::{
        LINUX_DNS_ANY_HOST_LINE, LINUX_PORTS_ANY_HOST_LINE, LinuxHost, LinuxSockets,
    };
    let host = |supervised, routed| {
        Some(LinuxHost {
            nested: true,
            scoped: false,
            supervised,
            root: false,
            armored: false,
            routed,
        })
    };
    let ports = Some(LINUX_PORTS_ANY_HOST_LINE.to_owned());
    let tcp = Some(LinuxSockets::TcpOnly);
    let high = [AccessGrant::Port { port: 8080 }, AccessGrant::TempFolder];
    let low = [
        AccessGrant::Port { port: 8080 },
        AccessGrant::Port { port: 80 },
    ];
    for supervised in [false, true] {
        let gap = |routed, network, sockets, grants: &[AccessGrant]| {
            linux_network_gap(network, host(supervised, routed), sockets, grants)
        };
        let https = AgentNetwork::Https;
        assert_eq!(gap(true, https, tcp, &[]), None, "supervised {supervised}");
        assert_eq!(
            gap(true, https, tcp, &high),
            None,
            "supervised {supervised}"
        );
        assert_eq!(
            gap(true, https, tcp, &low),
            gap(false, https, tcp, &low),
            "a port below 1024 keeps the shared network: supervised {supervised}"
        );
        for sockets in [
            Some(LinuxSockets::TcpPorts),
            Some(LinuxSockets::Refuse),
            None,
        ] {
            assert_eq!(gap(true, https, sockets, &[]), ports, "{sockets:?}");
        }
        assert_eq!(gap(true, AgentNetwork::Open, tcp, &[]), ports);
        assert_eq!(
            gap(true, AgentNetwork::None, tcp, &high),
            gap(false, AgentNetwork::None, tcp, &high)
        );
    }
    let unrouted =
        |supervised| linux_network_gap(AgentNetwork::Https, host(supervised, false), tcp, &[]);
    assert_eq!(unrouted(false), ports, "today's line");
    assert_eq!(
        unrouted(true),
        Some(LINUX_DNS_ANY_HOST_LINE.to_owned()),
        "today's line"
    );
}

#[test]
fn a_level_says_what_this_linux_cannot_hold_for_it_and_nothing_elsewhere() {
    use crate::sandbox::{
        LINUX_NESTED_GIT_LINE, LINUX_PROCESS_LIST_LINE, LINUX_SIGNALS_LINE, LinuxHost,
    };
    let host = |nested, scoped| {
        Some(LinuxHost {
            nested,
            scoped,
            supervised: false,
            root: false,
            armored: false,
            routed: false,
        })
    };
    let gaps = |access, nested, scoped| linux_gaps(access, host(nested, scoped), false);
    let git = LINUX_NESTED_GIT_LINE;
    for access in [AgentAccess::Sandboxed, AgentAccess::OwnBranch] {
        assert!(linux_gaps(access, None, true).is_empty(), "not Linux");
        assert_eq!(gaps(access, true, true), [git]);
        assert_eq!(gaps(access, true, false), [git]);
        assert_eq!(gaps(access, false, true), [git, LINUX_PROCESS_LIST_LINE]);
        assert_eq!(gaps(access, false, false), [git, LINUX_SIGNALS_LINE]);
        let found = linux_gaps(access, host(false, false), true);
        assert_eq!(
            found,
            gaps(access, false, false),
            "unsupervised, a repository adds none"
        );
    }
    assert!(gaps(AgentAccess::Full, false, false).is_empty());
    let levels = LevelView::on(host(false, false), true);
    assert_eq!(levels.len(), AgentAccess::ALL.len());
    for level in &levels {
        let told: Vec<&str> = level.gaps.iter().map(String::as_str).collect();
        assert_eq!(told, gaps(level.access, false, false), "{:?}", level.access);
    }
    assert!(
        LevelView::on(None, true)
            .iter()
            .all(|level| level.gaps.is_empty())
    );
    let stored = r#"{"access":"sandboxed","label":"Sandboxed","unavailable":null}"#;
    let old: LevelView = serde_json::from_str(stored).unwrap();
    assert!(old.gaps.is_empty(), "a view an older engine stored");
}

#[test]
fn a_supervised_linux_says_its_narrower_lines_and_root_says_why_none_of_them_hold() {
    use crate::sandbox::{
        LINUX_KEPT_GIT_LINE, LINUX_NESTED_GIT_LINE, LINUX_PROCESS_LIST_LINE, LINUX_ROOT_LINE,
        LINUX_SIGNALS_LINE, LINUX_SUPERVISED_LINE, LinuxHost, LinuxSupervisor,
    };
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
    let unread = LevelView::other_git(Some(held(true, false)), Path::new("/nonexistent"));
    assert!(unread, "a folder that cannot be read rules out none");
    let quiet = LevelView::on(Some(held(true, false)), false);
    let told: Vec<&str> = quiet[0].gaps.iter().map(String::as_str).collect();
    assert_eq!(told, [said], "a project with no other repository");
}

/// A Linux where AppArmor keeps unprivileged programs out of user namespaces and agents
/// get no process space of their own says AppArmor is why, just before the process or
/// signals line it explains, at every limited level and never under Full; a Linux
/// without the restriction, or one where CrazyAgent runs as root, whom the restriction
/// does not cover, says nothing of AppArmor (DEC-179 SBX-3, addendum 7, rv-ds241 nit).
#[test]
fn an_apparmor_blocked_linux_says_why_agents_share_your_process_space() {
    use crate::sandbox::{
        LINUX_APPARMOR_LINE, LINUX_KEPT_GIT_LINE, LINUX_NESTED_GIT_LINE, LINUX_PROCESS_LIST_LINE,
        LINUX_ROOT_LINE, LINUX_SIGNALS_LINE, LINUX_SUPERVISED_LINE, LinuxHost,
    };
    let host = |scoped, supervised, root| LinuxHost {
        nested: false,
        scoped,
        supervised,
        root,
        armored: true,
        routed: false,
    };
    let (git, said, armor) = (
        LINUX_NESTED_GIT_LINE,
        LINUX_SUPERVISED_LINE,
        LINUX_APPARMOR_LINE,
    );
    let (listed, signals) = (LINUX_PROCESS_LIST_LINE, LINUX_SIGNALS_LINE);
    for access in [AgentAccess::Sandboxed, AgentAccess::OwnBranch] {
        let gaps = |host, other_git| linux_gaps(access, Some(host), other_git);
        assert_eq!(gaps(host(true, false, false), false), [git, armor, listed]);
        assert_eq!(
            gaps(host(false, false, false), false),
            [git, armor, signals]
        );
        assert_eq!(gaps(host(false, true, false), false), [said, armor, listed]);
        let kept = [LINUX_KEPT_GIT_LINE, said, armor, listed];
        assert_eq!(
            gaps(host(false, true, false), true),
            kept,
            "four lines at most"
        );
        let root = [LINUX_ROOT_LINE, git, signals];
        assert_eq!(
            gaps(host(false, false, true), false),
            root,
            "root: no AppArmor line"
        );
        let free = LinuxHost {
            armored: false,
            ..host(true, false, false)
        };
        assert_eq!(
            gaps(free, false),
            [git, listed],
            "no restriction, no AppArmor line"
        );
    }
    let full = linux_gaps(AgentAccess::Full, Some(host(false, false, false)), true);
    assert!(full.is_empty());
    let levels = LevelView::on(Some(host(true, false, false)), false);
    let told: Vec<&str> = levels[0].gaps.iter().map(String::as_str).collect();
    assert_eq!(told, [git, armor, listed], "both clients get the line");
    assert!(
        armor.contains("sudo dpkg-reconfigure crazy-agent"),
        "{armor}"
    );
}

/// The Agent access page reads the project folder for another git repository before its
/// database transaction, and only where the call supervisor runs; the view takes that
/// answer (rv-secreview linux-t4c note 1, rv-code4 linux-t4c N2, DEC-179 T4d).
#[test]
fn only_a_supervised_host_reads_the_project_for_another_repository_and_the_view_takes_it() {
    use crate::sandbox::{LINUX_KEPT_GIT_LINE, LinuxHost};
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
    assert!(LevelView::other_git(Some(host(true)), &project));
    assert!(LevelView::other_git(Some(host(true)), &missing));
    for unread in [Some(host(false)), None] {
        assert!(!LevelView::other_git(unread, &project), "{unread:?}");
        assert!(!LevelView::other_git(unread, &missing), "{unread:?}");
    }
    let lines = |other_git| LevelView::on(Some(host(true)), other_git).remove(0).gaps;
    assert!(lines(true).contains(&LINUX_KEPT_GIT_LINE.to_owned()));
    assert!(!lines(false).contains(&LINUX_KEPT_GIT_LINE.to_owned()));
}
