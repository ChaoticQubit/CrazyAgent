use super::*;
use crate::policy::AccessGrant;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[cfg(unix)]
#[test]
fn the_temp_folder_is_given_only_when_it_is_the_owner_s_private_per_user_folder() {
    assert_eq!(private_temp(Path::new("/tmp")), Err("temp_folder_shared"));
    let open = tempfile::tempdir_in("/tmp").unwrap();
    std::fs::set_permissions(open.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        private_temp(open.path()),
        Err("temp_folder_shared"),
        "a private folder outside /private/var/folders is not the owner's temp folder"
    );
    if cfg!(target_os = "macos") {
        let own = std::env::temp_dir().canonicalize().unwrap();
        if own.starts_with(PER_USER_TEMP) {
            assert_eq!(owner_temp_folder(), Ok(own));
        }
    } else {
        assert_eq!(owner_temp_folder(), Err("temp_folder_needs_macos"));
    }
}

#[test]
fn a_port_the_hooks_name_as_an_ide_port_is_refused_and_held_now_drops_it() {
    fn ide_ports(_data_root: &Path) -> Vec<u16> {
        vec![20_123]
    }
    fn harness_folders() -> Vec<&'static str> {
        Vec::new()
    }
    let hooks = GrantHooks {
        harness_folders,
        ide_ports,
    };
    let data = Path::new("/nonexistent-engine-data");
    let ide = crate::policy::AccessGrant::Port { port: 20_123 };
    let free = crate::policy::AccessGrant::Port { port: 20_124 };
    assert_eq!(checked(&hooks, &ide, data), Err("grant_port_reserved"));
    assert_eq!(checked(&hooks, &free, data), Ok(free.clone()));
    assert_eq!(
        held_now(&hooks, &[ide, free.clone()], &[], data),
        vec![free.clone()]
    );
    assert!(held_now(&hooks, &[free], &[20_124], data).is_empty());
}

const HOOKS: GrantHooks = GrantHooks {
    harness_folders: no_harness_folders,
    ide_ports: no_ide_ports,
};

fn no_harness_folders() -> Vec<&'static str> {
    Vec::new()
}

fn no_ide_ports(_data_root: &Path) -> Vec<u16> {
    Vec::new()
}

#[test]
fn a_port_grant_refuses_zero_the_owner_ports_and_the_model_proxy_ports() {
    let data = Path::new("/nonexistent-engine-data");
    for port in [0, 1455, 11_434, *crate::ports::PROXY_PORTS.start()] {
        assert_eq!(
            checked(&HOOKS, &AccessGrant::Port { port }, data),
            Err("grant_port_reserved"),
            "port {port}"
        );
    }
    assert_eq!(
        checked(&HOOKS, &AccessGrant::Port { port: 47_821 }, data),
        Ok(AccessGrant::Port { port: 47_821 })
    );
}

#[cfg(target_os = "macos")]
#[test]
fn a_socket_grant_is_kept_canonical_and_refused_with_credentials_engine_data_listeners_and_tmux() {
    let tmp = tempfile::Builder::new()
        .prefix("ca-gs-")
        .tempdir_in("/tmp")
        .unwrap();
    let typed = tmp.path().join("discord-ipc-0");
    assert!(typed.starts_with("/tmp"));
    let _listener = kernel_processes::spawn_gate::bind_unix_std(&typed).unwrap();
    let kept = checked(
        &HOOKS,
        &AccessGrant::Socket {
            path: typed.clone(),
        },
        Path::new("/nonexistent"),
    )
    .unwrap();
    let real = tmp.path().canonicalize().unwrap().join("discord-ipc-0");
    assert_eq!(kept, AccessGrant::Socket { path: real.clone() });
    assert!(real.starts_with("/private/tmp"), "{}", real.display());
    let engine = AccessGrant::Socket {
        path: typed.clone(),
    };
    assert_eq!(
        checked(&HOOKS, &engine, tmp.path()),
        Err("grant_socket_engine")
    );
    let keys = AccessGrant::Socket {
        path: PathBuf::from("/Library/Keychains/x.sock"),
    };
    assert_eq!(
        checked(&HOOKS, &keys, Path::new("/nonexistent")),
        Err("grant_socket_secret")
    );
    let tmux = tmp.path().join("tmux-501");
    std::fs::create_dir(&tmux).unwrap();
    let pane = AccessGrant::Socket {
        path: tmux.join("default"),
    };
    assert_eq!(
        checked(&HOOKS, &pane, Path::new("/nonexistent")),
        Err("grant_socket_agent")
    );
    let launchd = tempfile::Builder::new()
        .prefix("com.apple.launchd.")
        .tempdir_in("/tmp")
        .unwrap();
    let agent = AccessGrant::Socket {
        path: launchd.path().join("Listeners"),
    };
    assert_eq!(
        checked(&HOOKS, &agent, Path::new("/nonexistent")),
        Err("grant_socket_agent")
    );
    for path in [
        "relative.sock",
        "/tmp/a b",
        "/tmp/../etc/x",
        "/no-such-folder-ca/x.sock",
    ] {
        assert_eq!(
            checked(
                &HOOKS,
                &AccessGrant::Socket {
                    path: PathBuf::from(path)
                },
                Path::new("/nonexistent")
            ),
            Err("grant_socket_not_plain"),
            "{path}"
        );
    }
    let long = AccessGrant::Socket {
        path: PathBuf::from(format!("/tmp/{}", "a".repeat(100))),
    };
    assert_eq!(
        checked(&HOOKS, &long, Path::new("/nonexistent")),
        Err("grant_socket_not_plain")
    );
}

/// Leader security add (1): a socket that controls a container or VM host, or a key or
/// password agent, is refused by its folder or file name even when it exists and is the
/// user's own, one row per class.
#[cfg(target_os = "macos")]
#[test]
fn host_escape_and_key_agent_sockets_are_refused_by_class() {
    let tmp = tempfile::Builder::new()
        .prefix("ca-gx-")
        .tempdir_in("/tmp")
        .unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let data = Path::new("/nonexistent");
    let rows: [(&str, &str); 14] = [
        ("docker.sock", "grant_socket_host"),
        (".docker/run/docker.sock", "grant_socket_host"),
        ("containerd/containerd.sock", "grant_socket_host"),
        ("podman/podman.sock", "grant_socket_host"),
        (".colima/default/x.sock", "grant_socket_host"),
        (".lima/default/sock/x.sock", "grant_socket_host"),
        (".orbstack/run/x.sock", "grant_socket_host"),
        (".rd/x.sock", "grant_socket_host"),
        (".gnupg/S.gpg-agent", "grant_socket_keys"),
        ("S.gpg-agent.ssh", "grant_socket_keys"),
        ("2BUA8C4S2C.com.1password/t/agent.sock", "grant_socket_keys"),
        (".1password/agent.sock", "grant_socket_keys"),
        ("agent.sock", "grant_socket_keys"),
        ("ssh-x/s.sock", "grant_socket_keys"),
    ];
    for (relative, code) in rows {
        let path = base.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let _listener = kernel_processes::spawn_gate::bind_unix_std(&path).unwrap();
        assert_eq!(
            checked(&HOOKS, &AccessGrant::Socket { path }, data),
            Err(code),
            "{relative}"
        );
    }
    let plain = base.join("lima-notes").join("app.sock");
    assert_eq!(escape_class(&base.join("climate").join("app.sock")), None);
    assert_eq!(escape_class(&plain), Some("grant_socket_host"));
}

/// Leader security add (1): only an existing socket the user owns, in a folder the user
/// owns that no one else can write or in a sticky folder of the user or root, is held; a
/// socket of root or another user, a plain file and a missing socket are refused.
#[test]
fn a_socket_must_exist_be_the_user_s_own_and_sit_where_no_one_else_can_swap_it() {
    let me = 501;
    let closed = (me, 0o040_700);
    assert_eq!(ownership(Some((true, me)), closed, me), Ok(()));
    assert_eq!(
        ownership(Some((true, 0)), closed, me),
        Err("grant_socket_owner"),
        "root's socket"
    );
    assert_eq!(
        ownership(Some((true, me + 1)), closed, me),
        Err("grant_socket_owner"),
        "another user's socket"
    );
    assert_eq!(
        ownership(Some((true, me)), (me, 0o040_777), me),
        Err("grant_socket_owner"),
        "a folder others can write"
    );
    assert_eq!(
        ownership(Some((true, me)), (me + 1, 0o040_700), me),
        Err("grant_socket_owner"),
        "another user's folder"
    );
    assert_eq!(
        ownership(Some((true, me)), (0, 0o041_777), me),
        Ok(()),
        "/tmp"
    );
    assert_eq!(
        ownership(Some((true, me)), (me + 1, 0o041_777), me),
        Err("grant_socket_owner"),
        "another user's sticky folder"
    );
    assert_eq!(
        ownership(Some((false, me)), closed, me),
        Err("grant_socket_missing"),
        "a plain file"
    );
    assert_eq!(ownership(None, closed, me), Err("grant_socket_missing"));
    #[cfg(target_os = "macos")]
    {
        let rooted = Path::new("/private/var/run/mDNSResponder");
        if std::fs::symlink_metadata(rooted).is_ok() {
            assert_eq!(
                checked(
                    &HOOKS,
                    &AccessGrant::Socket {
                        path: rooted.to_path_buf()
                    },
                    Path::new("/nonexistent")
                ),
                Err("grant_socket_owner"),
                "a real root-owned socket"
            );
        }
        let tmp = tempfile::tempdir_in("/tmp").unwrap();
        let gone = tmp.path().join("app.sock");
        assert_eq!(
            checked(
                &HOOKS,
                &AccessGrant::Socket { path: gone },
                Path::new("/nonexistent")
            ),
            Err("grant_socket_missing")
        );
    }
}

/// L2 r1 F1: a socket that drives the owner's terminal or editor (Emacs, Neovim, screen,
/// Zellij, kitty, WezTerm, Alacritty, iTerm2, VS Code-family IPC, Cursor, Hammerspoon, or
/// tmux started with -S anywhere) runs commands as the owner, so it is refused by name even
/// when it exists and is the user's own; plain names that only look alike stay allowed.
#[cfg(target_os = "macos")]
#[test]
fn editor_and_terminal_control_sockets_are_refused_by_class() {
    let tmp = tempfile::Builder::new()
        .prefix("ca-gc-")
        .tempdir_in("/tmp")
        .unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let data = Path::new("/nonexistent");
    let rows: [&str; 17] = [
        "emacs501/server",
        "nvim.me/a1b2/nvim.4242.0",
        "uscreens/S-me/4242.main",
        ".screen/4242.main",
        "zellij-501/0.41.2/main",
        "kitty-4242",
        "wezterm/gui-sock-4242",
        "Alacritty-4242.sock",
        "iTerm2/private/socket",
        "vscode-ipc-0a1b.sock",
        "vscode-git-0a1b.sock",
        "Code/1.95-main.sock",
        "VSCodium/1.95-main.sock",
        "Cursor/0.42-main.sock",
        "Hammerspoon/ipc.sock",
        "vim/server.sock",
        "pair/work.tmux",
    ];
    for relative in rows {
        let path = base.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let _listener = kernel_processes::spawn_gate::bind_unix_std(&path).unwrap();
        assert_eq!(
            checked(&HOOKS, &AccessGrant::Socket { path }, data),
            Err("grant_socket_agent"),
            "{relative}"
        );
    }
    for plain in [
        "screenshots/app.sock",
        "unicode/app.sock",
        "codec.sock",
        "code/app/dev.sock",
    ] {
        assert_eq!(escape_class(&base.join(plain)), None, "{plain}");
    }
}

/// L2 r1 F1 (hard link): a socket with a second name is refused at both names, so a hard
/// link to a refused socket at a plain name never passes; a socket with one name is kept.
#[cfg(target_os = "macos")]
#[test]
fn a_socket_with_a_second_name_is_refused_at_both_names() {
    let tmp = tempfile::Builder::new()
        .prefix("ca-gl-")
        .tempdir_in("/tmp")
        .unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let data = Path::new("/nonexistent");
    let lone = base.join("lone.sock");
    let _lone = kernel_processes::spawn_gate::bind_unix_std(&lone).unwrap();
    assert_eq!(
        checked(&HOOKS, &AccessGrant::Socket { path: lone.clone() }, data),
        Ok(AccessGrant::Socket { path: lone })
    );
    let first = base.join("first.sock");
    let _first = kernel_processes::spawn_gate::bind_unix_std(&first).unwrap();
    let second = base.join("second.sock");
    std::fs::hard_link(&first, &second).unwrap();
    for path in [first, second] {
        assert_eq!(
            checked(&HOOKS, &AccessGrant::Socket { path: path.clone() }, data),
            Err("grant_socket_owner"),
            "{}",
            path.display()
        );
    }
}

/// L2 r1 F1 (ports): Docker's TCP API (2375, 2376) and the Chrome DevTools and Node
/// inspector ports (9222, 9229) run anything as the owner, so a port grant never reaches
/// them.
#[test]
fn a_port_grant_never_reaches_docker_s_api_or_a_browser_or_node_debugger() {
    let data = Path::new("/nonexistent-engine-data");
    for port in [2375, 2376, 9222, 9229] {
        assert_eq!(
            checked(&HOOKS, &AccessGrant::Port { port }, data),
            Err("grant_port_reserved"),
            "port {port}"
        );
    }
    assert_eq!(
        checked(&HOOKS, &AccessGrant::Port { port: 9_223 }, data),
        Ok(AccessGrant::Port { port: 9_223 })
    );
}

/// L2 r2 L2-M1 floor: Kakoune, dtach and abduco sessions and an ssh ControlMaster socket
/// named by its `%C` hash are refused as control sockets; a shorter hex name is kept.
#[cfg(target_os = "macos")]
#[test]
fn the_floor_also_refuses_kakoune_dtach_abduco_and_ssh_mux_sockets() {
    let tmp = tempfile::Builder::new()
        .prefix("ca-gf-")
        .tempdir_in("/tmp")
        .unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let data = Path::new("/nonexistent");
    let rows: [&str; 5] = [
        "kakoune/me/work",
        "work.dtach",
        "abduco/main",
        "mux/0123456789abcdef0123456789abcdef01234567",
        "cm-0123456789ABCDEF0123456789ABCDEF01234567",
    ];
    for relative in rows {
        let path = base.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let _listener = kernel_processes::spawn_gate::bind_unix_std(&path).unwrap();
        assert_eq!(
            checked(&HOOKS, &AccessGrant::Socket { path }, data),
            Err("grant_socket_agent"),
            "{relative}"
        );
    }
    let short = base.join("0123456789abcdef0123456789abcdef0123456");
    assert_eq!(escape_class(&short), None);
}

/// L2 r2 L2-M1 floor: the Java (JDWP 5005), Python (debugpy 5678) and Go (Delve 2345)
/// debugger ports are reserved like Docker's and the browser's.
#[test]
fn a_port_grant_never_reaches_the_java_python_or_go_debugger_ports() {
    let data = Path::new("/nonexistent-engine-data");
    for port in [5005, 5678, 2345] {
        assert_eq!(
            checked(&HOOKS, &AccessGrant::Port { port }, data),
            Err("grant_port_reserved"),
            "port {port}"
        );
    }
    assert_eq!(
        checked(&HOOKS, &AccessGrant::Port { port: 5_432 }, data),
        Ok(AccessGrant::Port { port: 5_432 }),
        "a database port stays the owner's informed choice"
    );
}

/// SA241 vid 9: a port grant never opens a model relay port to an agent, at either end of
/// the relay block.
#[test]
fn a_port_grant_refuses_the_model_relay_ports() {
    let data = Path::new("/nonexistent-engine-data");
    let block = crate::ports::RELAY_PORTS;
    for port in [*block.start(), *block.end()] {
        assert_eq!(
            checked(&HOOKS, &AccessGrant::Port { port }, data),
            Err("grant_port_reserved"),
            "port {port}"
        );
    }
}
