use super::*;

const HOOKS: PolicyHooks = PolicyHooks {
    local_folders: |_| Vec::new(),
};

#[test]
fn no_network_grants_a_model_only_on_loopback_and_never_a_host_port() {
    use crate::net::{SandboxNetwork, SandboxRemote};
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

/// A policy saved before the tool cache setting reads Shared, a pinned policy keeps the owner's
/// choice, a save names it, and the view lists every choice, the default first.
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
    let view = own.view(
        &HOOKS,
        None,
        folder.path(),
        &ToolHomes::at(folder.path()),
        1,
        None,
        true,
    );
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

/// A policy saved before the access change settings reads When the agent is idle and no grants, a
/// pinned policy keeps both, the view lists every Apply choice, the default first, and the saved
/// grants with their words.
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
    let view = own.view(
        &HOOKS,
        None,
        folder.path(),
        &ToolHomes::at(folder.path()),
        1,
        None,
        true,
    );
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

/// At most two sockets, four ports and one temp folder, each plain and named once. A socket grant
/// is a Unix socket path, so off Unix every socket is refused.
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

/// A running agent is widened by a wider level or network, a toolchain its profile lacks or a grant
/// it does not hold, and revoked by a grant the saved policy dropped; a record that kept no level,
/// network or toolchains never asks for a restart.
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

/// A new project records every family it uses or the owner has installed, so an empty project still
/// gets the owner's toolchains later; a family neither used nor installed keeps following the
/// project.
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
fn full_access_on_windows_is_refused_with_its_own_reason_and_code() {
    for access in AgentAccess::ALL {
        assert_eq!(
            access.refusal().map(|(reason, _)| reason),
            access.unavailable()
        );
    }
    #[cfg(target_os = "windows")]
    {
        assert_eq!(
            AgentAccess::Full.refusal(),
            Some((FULL_WINDOWS_UNAVAILABLE, SANDBOX_FULL_UNAVAILABLE_WINDOWS))
        );
        for access in [AgentAccess::Sandboxed, AgentAccess::OwnBranch] {
            assert_eq!(
                access.refusal(),
                Some((WINDOWS_UNAVAILABLE, AGENT_ACCESS_UNAVAILABLE))
            );
        }
    }
    #[cfg(target_os = "linux")]
    for access in [AgentAccess::OwnBranch, AgentAccess::Full] {
        assert_eq!(
            access.refusal(),
            Some((LINUX_UNAVAILABLE, AGENT_ACCESS_UNAVAILABLE))
        );
    }
    #[cfg(target_os = "macos")]
    assert!(
        AgentAccess::ALL
            .iter()
            .all(|access| access.refusal().is_none())
    );
}

#[test]
fn a_windows_model_only_agent_with_tools_starts_no_proxy_and_reaches_the_models_port_on_any_host() {
    use crate::net::{SandboxNetwork, SandboxRemote};
    let hosted = SandboxRemote::AnyHost(443);
    let local = SandboxRemote::Loopback(11_434);
    assert!(!AgentNetwork::None.proxied("windows", true, hosted));
    assert_eq!(
        AgentNetwork::None
            .reach("windows", true, hosted, None)
            .unwrap(),
        SandboxNetwork::Only(vec![hosted])
    );
    for os in ["macos", "linux"] {
        assert!(AgentNetwork::None.proxied(os, true, hosted));
        assert_eq!(
            AgentNetwork::None
                .reach(os, true, hosted, Some(41_300))
                .unwrap(),
            SandboxNetwork::Only(vec![SandboxRemote::Loopback(41_300)])
        );
        assert!(!AgentNetwork::None.proxied(os, true, local));
        assert_eq!(
            AgentNetwork::None.reach(os, true, local, None).unwrap(),
            SandboxNetwork::Only(vec![local])
        );
    }
    assert!(!AgentNetwork::None.proxied("windows", true, local));
    for os in ["windows", "macos", "linux"] {
        assert!(!AgentNetwork::None.proxied(os, false, hosted));
        assert_eq!(
            AgentNetwork::None.reach(os, false, hosted, None).unwrap(),
            SandboxNetwork::Only(vec![hosted])
        );
        for network in [AgentNetwork::Open, AgentNetwork::Https] {
            assert!(!network.proxied(os, true, hosted));
            assert_eq!(
                network.reach(os, true, hosted, None).unwrap(),
                network.sandbox(Some(hosted))
            );
        }
    }
}

#[test]
fn a_loopback_model_on_windows_is_refused_under_every_network_choice() {
    use crate::net::SandboxRemote;
    let local = SandboxRemote::Loopback(11_434);
    for network in AgentNetwork::ALL {
        for tools in [true, false] {
            let refusal = network.reach("windows", tools, local, None);
            assert!(
                refusal
                    .as_ref()
                    .is_err_and(crate::lines::model_loopback_windows),
                "{network:?} tools={tools}: {refusal:?}"
            );
        }
    }
    for os in ["macos", "linux"] {
        assert!(AgentNetwork::Open.reach(os, true, local, None).is_ok());
    }
}

#[test]
fn on_windows_a_limited_network_choice_says_it_reaches_the_whole_internet() {
    for network in [AgentNetwork::Https, AgentNetwork::None] {
        assert_eq!(
            windows_network_gap("windows", network),
            Some(WINDOWS_NETWORK_WIDE_LINE)
        );
    }
    assert_eq!(windows_network_gap("windows", AgentNetwork::Open), None);
    for os in ["macos", "linux"] {
        for network in AgentNetwork::ALL {
            assert_eq!(windows_network_gap(os, network), None);
        }
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
            [AgentAccess::Sandboxed, AgentAccess::OwnBranch]
                .iter()
                .all(|access| access.unavailable() == Some(WINDOWS_UNAVAILABLE))
        );
        assert_eq!(
            AgentAccess::Full.unavailable(),
            Some(FULL_WINDOWS_UNAVAILABLE)
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

/// Allow all changes adds no rule. No delete or rename denies every unlink and Read only every
/// write at and below each agent folder, in any letter case, and both keep the agents root from
/// being removed or moved aside. The older root is named only while agents stay there.
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
    let root_text = crate::paths::any_case("/tmp/ca-proj");
    let new = format!(
        "^{root_text}{}/{}",
        crate::paths::any_case("/.crazyagent"),
        crate::paths::any_case("worktrees")
    );
    let both = format!(
        "^{root_text}({})?/{}",
        crate::paths::any_case("/.crazyagent"),
        crate::paths::any_case("worktrees")
    );
    let state = format!("^{root_text}{}$", crate::paths::any_case("/.crazyagent"));
    for (older_agents, agents) in [(false, &new), (true, &both)] {
        let keep_root = format!("(deny file-write-unlink\n  (regex #\"{agents}$\"))\n");
        assert_eq!(
            rule(IdeAgentFiles::NoDeleteOrRename, older_agents),
            format!(
                "(deny file-write-unlink\n  (regex #\"{agents}/[^/]+(/|$)\"))\n{keep_root}(deny file-write-unlink\n  (regex #\"{state}\"))\n"
            )
        );
        assert_eq!(
            rule(IdeAgentFiles::ReadOnly, older_agents),
            format!(
                "(deny file-write*\n  (regex #\"{agents}/[^/]+(/|$)\"))\n{keep_root}(deny file-write*\n  (regex #\"{state}\"))\n"
            )
        );
    }
    assert!(state.contains("[cC][rR][aA][zZ][yY][aA][gG][eE][nN][tT]$"));
    assert!(new.contains("[wW][oO][rR](k|K|\u{212a})[tT][rR][eE][eE](s|S|\u{17f})"));
}

#[test]
fn only_a_none_network_this_linux_bridges_drops_the_ports_line() {
    use crate::host::LinuxHost;
    use crate::lines::LINUX_PORTS_ANY_HOST_LINE;
    use crate::settings::LinuxSockets;
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
    assert_eq!(named, crate::lines::linux_unbridged_line(80));
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
    use crate::host::LinuxHost;
    use crate::lines::{LINUX_DNS_ANY_HOST_LINE, LINUX_PORTS_ANY_HOST_LINE};
    use crate::settings::LinuxSockets;
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
    use crate::host::LinuxHost;
    use crate::lines::{LINUX_DNS_ANY_HOST_LINE, LINUX_PORTS_ANY_HOST_LINE};
    use crate::settings::LinuxSockets;
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
    use crate::host::LinuxHost;
    use crate::lines::{LINUX_NESTED_GIT_LINE, LINUX_PROCESS_LIST_LINE, LINUX_SIGNALS_LINE};
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

/// A Linux where AppArmor keeps unprivileged programs out of user namespaces and agents get no
/// process space of their own says AppArmor is why, just before the process or signals line it
/// explains, at every limited level and never under Full; a Linux without the restriction, or one
/// where CrazyAgent runs as root, whom the restriction does not cover, says nothing of AppArmor.
#[test]
fn an_apparmor_blocked_linux_says_why_agents_share_your_process_space() {
    use crate::host::LinuxHost;
    use crate::lines::{
        LINUX_APPARMOR_LINE, LINUX_KEPT_GIT_LINE, LINUX_NESTED_GIT_LINE, LINUX_PROCESS_LIST_LINE,
        LINUX_ROOT_LINE, LINUX_SIGNALS_LINE, LINUX_SUPERVISED_LINE,
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

/// The level list and a project's view show the Linux host their caller read, under the
/// owner's call supervisor setting, and the other-repository answer runs the caller's
/// bounded folder read only for a supervised host.
#[test]
fn levels_views_and_other_git_read_the_host_their_caller_passes() {
    use crate::settings::LinuxSupervisor;
    let host = LinuxHost {
        nested: false,
        scoped: false,
        supervised: true,
        root: false,
        armored: false,
        routed: false,
    };
    let calls = Some(LinuxSupervisor::Off);
    let levels = LevelView::all(Some(host), calls);
    assert_eq!(levels, LevelView::on(Some(host.under(calls)), true));
    assert_ne!(levels, LevelView::all(None, calls));
    assert_eq!(LevelView::all(None, calls), LevelView::on(None, true));
    let folder = tempfile::tempdir().unwrap();
    let homes = ToolHomes::at(folder.path());
    let own = AgentPolicy::default();
    let view = own.view(&HOOKS, Some(host), folder.path(), &homes, 1, None, true);
    assert_eq!(view.levels, LevelView::on(Some(host.under(None)), true));
    let unhosted = own.view(&HOOKS, None, folder.path(), &homes, 1, None, true);
    assert_eq!(unhosted.levels, LevelView::on(None, true));
    let unsupervised = LinuxHost {
        supervised: false,
        ..host
    };
    assert!(LevelView::other_git(Some(host), folder.path(), |_| true));
    assert!(!LevelView::other_git(Some(host), folder.path(), |_| false));
    assert!(!LevelView::other_git(None, folder.path(), |_| panic!(
        "read without a host"
    )));
    assert!(!LevelView::other_git(
        Some(unsupervised),
        folder.path(),
        |_| panic!("read without a supervised host")
    ));
}
