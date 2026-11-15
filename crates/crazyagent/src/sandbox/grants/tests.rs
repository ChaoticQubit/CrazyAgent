use super::*;
use crate::questions::{Answer, Settlement};
use std::os::unix::fs::PermissionsExt;

const PROJECT: &str = "alpha";

/// One engine root with a set-up project and the engine database with the team, setup and
/// questions tables, as the questions rows use.
struct Case {
    _tmp: tempfile::TempDir,
    db: Connection,
    project: ProjectId,
}

impl Case {
    fn new() -> Self {
        let tmp = tempfile::Builder::new()
            .prefix("ca-grants-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let root = base.join("engine");
        let folder = base.join("project");
        for path in [&root, &folder] {
            std::fs::create_dir(path).unwrap();
        }
        let db = crate::questions::open(&root).unwrap();
        crate::storage::initialize_authority_schemas(&db).unwrap();
        crate::team::initialize(&db).unwrap();
        crate::ide::workspace::tests::add_project(&db, PROJECT, &folder);
        Self {
            _tmp: tmp,
            db,
            project: ProjectId::new(PROJECT).unwrap(),
        }
    }

    fn policy(&self) -> AgentPolicy {
        super::super::policy::load(&self.db, PROJECT).unwrap()
    }

    fn version(&self) -> u64 {
        let item = crate::storage::versioned::agent_policy_item(&self.project);
        crate::storage::versioned::current(&self.db, &item)
            .unwrap()
            .version
    }

    fn status(&self) -> Vec<String> {
        let mut statement = self
            .db
            .prepare("SELECT status FROM access_requests WHERE project=?1 ORDER BY id")
            .unwrap();
        statement
            .query_map([PROJECT], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    fn ask_port(&self, port: u16) -> u64 {
        match request(
            &self.db,
            &self.project,
            &Asker::Master(7),
            &AccessGrant::Port { port },
            "the OpenAI gateway the app talks to",
        )
        .unwrap()
        {
            Requested::Asked(question) => question,
            other => panic!("no question was raised: {other:?}"),
        }
    }
}

#[test]
fn a_port_grant_refuses_zero_the_owner_ports_and_the_model_proxy_ports() {
    let data = Path::new("/nonexistent-engine-data");
    for port in [
        0,
        1455,
        11_434,
        *crate::sandbox::model_proxy::PROXY_PORTS.start(),
    ] {
        assert_eq!(
            checked(&AccessGrant::Port { port }, data),
            Err("grant_port_reserved"),
            "port {port}"
        );
    }
    assert_eq!(
        checked(&AccessGrant::Port { port: 47_821 }, data),
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
    let _listener = crate::spawn_gate::bind_unix_std(&typed).unwrap();
    let kept = checked(
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
    assert_eq!(checked(&engine, tmp.path()), Err("grant_socket_engine"));
    let keys = AccessGrant::Socket {
        path: PathBuf::from("/Library/Keychains/x.sock"),
    };
    assert_eq!(
        checked(&keys, Path::new("/nonexistent")),
        Err("grant_socket_secret")
    );
    let tmux = tmp.path().join("tmux-501");
    std::fs::create_dir(&tmux).unwrap();
    let pane = AccessGrant::Socket {
        path: tmux.join("default"),
    };
    assert_eq!(
        checked(&pane, Path::new("/nonexistent")),
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
        checked(&agent, Path::new("/nonexistent")),
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
        checked(&long, Path::new("/nonexistent")),
        Err("grant_socket_not_plain")
    );
}

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
fn a_request_raises_an_engine_question_the_owner_s_first_option_alone_allows() {
    let case = Case::new();
    let before = case.version();
    let question = case.ask_port(47_821);
    let pending = crate::questions::pending(&case.db, &case.project).unwrap();
    let asked = pending.iter().find(|q| q.id == question).unwrap();
    assert_eq!(asked.options, [ALLOW, DONT_ALLOW]);
    assert_eq!(asked.recommended.as_deref(), Some(DONT_ALLOW));
    assert!(asked.asker.starts_with(ASKER_PREFIX), "{}", asked.asker);
    assert!(
        asked.question.contains("Port 47821 on this computer"),
        "{}",
        asked.question
    );
    assert!(case.policy().grants.is_empty(), "asking grants nothing");
    let allow = Answer {
        picks: vec![0],
        comment: Some("fine for today".into()),
    };
    assert_eq!(
        crate::questions::answer(&case.db, question, 0, &allow).unwrap(),
        Settlement::Settled
    );
    assert_eq!(case.policy().grants, [AccessGrant::Port { port: 47_821 }]);
    assert_eq!(
        case.version(),
        before + 1,
        "the Allow is its own versioned write"
    );
    assert_eq!(case.status(), ["allowed"]);
    let log = history(&case.db, PROJECT).unwrap();
    assert_eq!(log.len(), 1);
    assert_eq!(
        (log[0].action.as_str(), log[0].grant.as_str()),
        ("allowed", "Port 47821 on this computer")
    );
    assert_eq!(log[0].by, format!("question {question}"));
}

#[test]
fn don_t_allow_a_comment_alone_or_a_dismissal_adds_nothing_and_is_logged_denied() {
    let case = Case::new();
    let refused = case.ask_port(47_821);
    let pick = Answer {
        picks: vec![1],
        comment: None,
    };
    crate::questions::answer(&case.db, refused, 0, &pick).unwrap();
    let commented = case.ask_port(47_822);
    let comment = Answer {
        picks: Vec::new(),
        comment: Some("allow it".into()),
    };
    crate::questions::answer(&case.db, commented, 0, &comment).unwrap();
    let dismissed = case.ask_port(47_823);
    crate::questions::dismiss(&case.db, dismissed, 0).unwrap();
    assert!(case.policy().grants.is_empty());
    assert_eq!(case.status(), ["denied", "denied", "denied"]);
    let log = history(&case.db, PROJECT).unwrap();
    assert!(log.iter().all(|row| row.action == "denied"), "{log:?}");
    assert_eq!(log.len(), 3);
}

#[test]
fn the_master_cannot_settle_an_access_question_from_its_chat() {
    let case = Case::new();
    let question = case.ask_port(47_821);
    let recorded = crate::questions::record_from_chat(&case.db, &case.project, 7, question, 0, 1);
    assert_eq!(recorded.unwrap_err().0, "questions_not_found");
    assert!(case.policy().grants.is_empty());
    assert_eq!(case.status(), ["pending"]);
}

/// RV-DS241-13: a master cannot close the owner's access question by relaying an answer
/// typed in its terminal either, so the request stays for the owner to decide, and the
/// owner's dismissal still denies it.
#[test]
fn the_master_cannot_relay_an_answer_to_an_access_question() {
    let case = Case::new();
    let question = case.ask_port(47_821);
    let key = crate::team::now_ms() + 60_000;
    let relayed =
        crate::questions::relay(&case.db, &case.project, 7, question, 0, "Allow", Some(key));
    assert_eq!(relayed.unwrap_err().0, "questions_not_found");
    assert!(case.policy().grants.is_empty());
    assert_eq!(case.status(), ["pending"]);
    let dismissed = crate::questions::dismiss(&case.db, question, 0).unwrap();
    assert_eq!(dismissed, Settlement::Settled);
    assert_eq!(case.status(), ["denied"]);
}

#[test]
fn a_held_grant_or_full_access_asks_nothing_and_requests_are_bounded() {
    let case = Case::new();
    let held = AgentPolicy {
        grants: vec![AccessGrant::Port { port: 47_821 }],
        ..AgentPolicy::default()
    };
    super::super::policy::save(&case.db, PROJECT, &held).unwrap();
    let again = request(
        &case.db,
        &case.project,
        &Asker::Agent(3),
        &AccessGrant::Port { port: 47_821 },
        "gateway",
    );
    assert_eq!(again, Ok(Requested::AlreadyAllowed));
    for port in 1..=REQUESTS_PENDING_MAX as u16 {
        case.ask_port(40_000 + port);
    }
    let over = request(
        &case.db,
        &case.project,
        &Asker::Agent(3),
        &AccessGrant::Port { port: 40_100 },
        "one more",
    );
    assert_eq!(over, Err("grant_requests_full".to_owned()));
    let blank = request(
        &case.db,
        &case.project,
        &Asker::Agent(3),
        &AccessGrant::Port { port: 40_101 },
        " ",
    );
    assert_eq!(blank, Err("grant_reason_invalid".to_owned()));
    let full = AgentPolicy {
        access: AgentAccess::Full,
        ..AgentPolicy::default()
    };
    super::super::policy::save(&case.db, PROJECT, &full).unwrap();
    let reached = request(
        &case.db,
        &case.project,
        &Asker::Agent(3),
        &AccessGrant::Port { port: 40_102 },
        "gateway",
    );
    assert_eq!(
        reached,
        Ok(Requested::AlreadyAllowed),
        "Full access reaches every port already"
    );
}

#[test]
fn an_owner_save_logs_each_grant_it_adds_and_removes_newest_first() {
    let case = Case::new();
    let a = AccessGrant::Port { port: 47_821 };
    let b = AccessGrant::TempFolder;
    log_changes(&case.db, PROJECT, &[], &[a.clone(), b.clone()], "owner", 1).unwrap();
    log_changes(&case.db, PROJECT, &[a.clone(), b], &[a], "owner", 2).unwrap();
    let rows: Vec<(String, String)> = history(&case.db, PROJECT)
        .unwrap()
        .into_iter()
        .map(|row| (row.action, row.grant))
        .collect();
    assert_eq!(
        rows,
        [
            ("revoked".to_owned(), "Your temp folder".to_owned()),
            ("granted".to_owned(), "Your temp folder".to_owned()),
            (
                "granted".to_owned(),
                "Port 47821 on this computer".to_owned()
            ),
        ]
    );
    for at in 0..(LOG_KEPT_MAX + 5) {
        log(
            &case.db,
            PROJECT,
            "granted",
            &AccessGrant::TempFolder,
            "owner",
            at,
        )
        .unwrap();
    }
    let kept: i64 = case
        .db
        .query_row(
            "SELECT COUNT(*) FROM access_grant_log WHERE project=?1",
            [PROJECT],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(kept, LOG_KEPT_MAX);
    assert_eq!(history(&case.db, PROJECT).unwrap().len(), LOG_LISTED_MAX);
}

/// Leader security add (3): a port grant never reaches a port the system lends out (where
/// the IDE's server and proxy ports land) nor a port the engine recorded for a project's
/// IDE, and a launch leaves out a saved port it keeps free for the owner's model endpoint.
#[cfg(unix)]
#[test]
fn a_port_grant_never_reaches_the_ide_the_dynamic_range_or_a_kept_model_port() {
    let data = tempfile::tempdir().unwrap();
    let ide = data.path().join(crate::ide::FOLDER).join(PROJECT);
    std::fs::create_dir_all(&ide).unwrap();
    std::fs::set_permissions(&ide, std::fs::Permissions::from_mode(0o700)).unwrap();
    crate::ide::ports::record(
        &ide,
        crate::ide::ports::Ports {
            server: 41_001,
            proxy: 41_002,
        },
    )
    .unwrap();
    for port in [41_001, 41_002] {
        assert_eq!(
            checked(&AccessGrant::Port { port }, data.path()),
            Err("grant_port_reserved"),
            "IDE port {port}"
        );
    }
    for port in [*DYNAMIC_PORTS.start(), 60_000, u16::MAX] {
        assert_eq!(
            checked(&AccessGrant::Port { port }, data.path()),
            Err("grant_port_dynamic"),
            "port {port}"
        );
    }
    assert_eq!(
        checked(&AccessGrant::Port { port: 49_151 }, data.path()),
        Ok(AccessGrant::Port { port: 49_151 })
    );
    let saved = [
        AccessGrant::Port { port: 8_080 },
        AccessGrant::Port { port: 8_081 },
    ];
    assert_eq!(
        held_now(&saved, &[8_081], data.path()),
        [AccessGrant::Port { port: 8_080 }],
        "a kept model port is left out at launch"
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
        let _listener = crate::spawn_gate::bind_unix_std(&path).unwrap();
        assert_eq!(
            checked(&AccessGrant::Socket { path }, data),
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
                &AccessGrant::Socket { path: gone },
                Path::new("/nonexistent")
            ),
            Err("grant_socket_missing")
        );
    }
}

/// Leader security add (2): the agent's reason has its control characters made spaces, is
/// cut to 300 bytes and shows as one quoted string; the question names the exact port.
#[test]
fn a_request_reason_is_stripped_cut_and_quoted_and_the_question_names_the_port() {
    let case = Case::new();
    let long = format!("line one\n\u{1b}[31mred\t\"quoted\" {}", "é".repeat(400));
    let Requested::Asked(question) = request(
        &case.db,
        &case.project,
        &Asker::Agent(3),
        &AccessGrant::Port { port: 47_900 },
        &long,
    )
    .unwrap() else {
        panic!("no question was raised");
    };
    let stored: String = case
        .db
        .query_row(
            "SELECT reason FROM access_requests WHERE question=?1",
            [question as i64],
            |row| row.get(0),
        )
        .unwrap();
    assert!(stored.len() <= REASON_BYTES_MAX);
    assert!(!stored.chars().any(char::is_control), "{stored:?}");
    assert!(
        stored.starts_with("line one [31mred \"quoted\" é"),
        "{stored:?}"
    );
    let pending = crate::questions::pending(&case.db, &case.project).unwrap();
    let asked = pending.iter().find(|q| q.id == question).unwrap();
    assert!(
        asked.question.contains("Port 47900 on this computer"),
        "{}",
        asked.question
    );
    let context = asked.context.as_deref().unwrap();
    assert!(
        context.contains(&serde_json::to_string(&stored).unwrap()),
        "{context}"
    );
    assert_eq!(
        request(
            &case.db,
            &case.project,
            &Asker::Agent(3),
            &AccessGrant::Port { port: 47_901 },
            "\u{7}\n\t ",
        ),
        Err("grant_reason_invalid".to_owned())
    );
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
        let _listener = crate::spawn_gate::bind_unix_std(&path).unwrap();
        assert_eq!(
            checked(&AccessGrant::Socket { path }, data),
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
    let _lone = crate::spawn_gate::bind_unix_std(&lone).unwrap();
    assert_eq!(
        checked(&AccessGrant::Socket { path: lone.clone() }, data),
        Ok(AccessGrant::Socket { path: lone })
    );
    let first = base.join("first.sock");
    let _first = crate::spawn_gate::bind_unix_std(&first).unwrap();
    let second = base.join("second.sock");
    std::fs::hard_link(&first, &second).unwrap();
    for path in [first, second] {
        assert_eq!(
            checked(&AccessGrant::Socket { path: path.clone() }, data),
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
            checked(&AccessGrant::Port { port }, data),
            Err("grant_port_reserved"),
            "port {port}"
        );
    }
    assert_eq!(
        checked(&AccessGrant::Port { port: 9_223 }, data),
        Ok(AccessGrant::Port { port: 9_223 })
    );
}

/// L2 r1 F2: a request the questions cap refuses leaves no row behind, so an agent that
/// fills the project's questions and keeps asking cannot grow the requests table.
#[test]
fn a_request_the_questions_cap_refuses_leaves_no_row() {
    let case = Case::new();
    for n in 0..crate::questions::PENDING_PER_PROJECT_MAX {
        let ask = Ask {
            question: format!("Filler {n}?"),
            kind: QuestionKind::Single,
            options: vec!["Yes".to_owned(), "No".to_owned()],
            recommended: Some("Yes".to_owned()),
            context: None,
        };
        crate::questions::raise(
            &case.db,
            &case.project,
            &EngineSource::Research(format!("fill{n}")),
            &ask,
        )
        .unwrap();
    }
    for port in 40_001..=40_010 {
        let refused = request(
            &case.db,
            &case.project,
            &Asker::Agent(3),
            &AccessGrant::Port { port },
            "the gateway",
        );
        assert_eq!(refused, Err("questions_full".to_owned()), "port {port}");
    }
    assert!(case.status().is_empty(), "{:?}", case.status());
}

/// The warning every socket and port Allow carries: the list of refused names is a floor,
/// so the owner is told that a program behind the grant can act on agents' requests as the
/// owner, outside the sandbox.
const RUN_AS_YOU: &str = "If that program can run commands or change files (a database, a debugger, an editor, a terminal or a remote shell), agents can then do anything you can, outside their sandbox.";

/// L2 r2 L2-M1: every socket and port meaning, and so the owner question an access request
/// raises, says the program behind it can let agents act as the owner outside their sandbox;
/// Don't allow stays the recommended answer and the temp folder keeps its own meaning.
#[test]
fn every_socket_and_port_allow_warns_the_agent_can_act_as_you() {
    for grant in [
        AccessGrant::Socket {
            path: PathBuf::from("/private/tmp/app/app.sock"),
        },
        AccessGrant::Port { port: 5_432 },
    ] {
        assert!(meaning(&grant).contains(RUN_AS_YOU), "{grant:?}");
    }
    assert!(!meaning(&AccessGrant::TempFolder).contains(RUN_AS_YOU));
    let case = Case::new();
    let question = case.ask_port(5_432);
    let pending = crate::questions::pending(&case.db, &case.project).unwrap();
    let asked = pending.iter().find(|q| q.id == question).unwrap();
    let context = asked.context.as_deref().unwrap_or_default();
    assert!(context.contains(RUN_AS_YOU), "{context}");
    assert_eq!(asked.recommended.as_deref(), Some(DONT_ALLOW));
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
        let _listener = crate::spawn_gate::bind_unix_std(&path).unwrap();
        assert_eq!(
            checked(&AccessGrant::Socket { path }, data),
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
            checked(&AccessGrant::Port { port }, data),
            Err("grant_port_reserved"),
            "port {port}"
        );
    }
    assert_eq!(
        checked(&AccessGrant::Port { port: 5_432 }, data),
        Ok(AccessGrant::Port { port: 5_432 }),
        "a database port stays the owner's informed choice"
    );
}

/// RV-DS241-83: on Linux, where Landlock limits ports and not hosts, a port's access question
/// says the port is open to agents on every host; a socket's and the temp folder's do not,
/// and no other system's question says it.
#[test]
fn only_on_linux_does_a_port_question_say_the_port_is_open_on_every_host() {
    let port = AccessGrant::Port { port: 5_432 };
    let line = crate::sandbox::LINUX_PORTS_ANY_HOST_LINE;
    assert_eq!(asked_meaning(&port, false), meaning(&port));
    assert_eq!(
        asked_meaning(&port, true),
        format!("{} {line}", meaning(&port))
    );
    for grant in [
        AccessGrant::Socket {
            path: PathBuf::from("/private/tmp/app/app.sock"),
        },
        AccessGrant::TempFolder,
    ] {
        assert_eq!(asked_meaning(&grant, true), meaning(&grant), "{grant:?}");
    }
    let case = Case::new();
    let question = case.ask_port(5_432);
    let pending = crate::questions::pending(&case.db, &case.project).unwrap();
    let asked = pending.iter().find(|q| q.id == question).unwrap();
    let context = asked.context.as_deref().unwrap_or_default();
    assert_eq!(
        context.contains(line),
        cfg!(target_os = "linux"),
        "{context}"
    );
}

/// SA241 vid 9: a port grant never opens a model relay port to an agent, at either end of
/// the relay block.
#[test]
fn a_port_grant_refuses_the_model_relay_ports() {
    let data = Path::new("/nonexistent-engine-data");
    let block = crate::sandbox::model_proxy::RELAY_PORTS;
    for port in [*block.start(), *block.end()] {
        assert_eq!(
            checked(&AccessGrant::Port { port }, data),
            Err("grant_port_reserved"),
            "port {port}"
        );
    }
}

/// Asks for the owner's Allow on a socket an agent named in the project folder, with a
/// second socket of the owner's beside it in a folder of its own. Input: the case. Output:
/// the asked socket's path, the other socket's path, both listeners and the question.
#[cfg(target_os = "macos")]
fn asked_socket(
    case: &Case,
) -> (
    PathBuf,
    PathBuf,
    std::os::unix::net::UnixListener,
    std::os::unix::net::UnixListener,
    u64,
) {
    let base = case._tmp.path().canonicalize().unwrap();
    let asked = base.join("project").join("app.sock");
    let owner = base.join("owner");
    std::fs::create_dir(&owner).unwrap();
    let other = owner.join("store.sock");
    let asked_listener = crate::spawn_gate::bind_unix_std(&asked).unwrap();
    let other_listener = crate::spawn_gate::bind_unix_std(&other).unwrap();
    assert_eq!(
        checked(
            &AccessGrant::Socket {
                path: other.clone()
            },
            &base.join("engine")
        ),
        Ok(AccessGrant::Socket {
            path: other.clone()
        }),
        "the owner's other socket passes the check on its own"
    );
    let Requested::Asked(question) = request(
        &case.db,
        &case.project,
        &Asker::Master(7),
        &AccessGrant::Socket {
            path: asked.clone(),
        },
        "the app's own socket",
    )
    .unwrap() else {
        panic!("no question was raised");
    };
    (asked, other, asked_listener, other_listener, question)
}

/// Answers a question with the owner's Allow and checks that it grants nothing: the
/// policy and its version stay and the request ends denied. Inputs: the case and the
/// question.
#[cfg(target_os = "macos")]
fn allow_grants_nothing(case: &Case, question: u64) {
    let before = case.version();
    let allow = Answer {
        picks: vec![0],
        comment: None,
    };
    assert_eq!(
        crate::questions::answer(&case.db, question, 0, &allow).unwrap(),
        Settlement::Settled
    );
    assert_eq!(case.policy().grants, Vec::<AccessGrant>::new());
    assert_eq!(case.version(), before, "nothing was written");
    assert_eq!(case.status(), ["denied"]);
}

/// The owner's Allow binds to the socket the question showed: an agent that swaps the
/// socket it asked for for a link to another of the owner's sockets after asking gets
/// nothing, and the swapped path is never saved (RV-DS241-82).
#[cfg(target_os = "macos")]
#[test]
fn an_allow_after_the_asked_socket_became_a_link_to_another_socket_grants_nothing() {
    let case = Case::new();
    let (asked, other, _asked, _other, question) = asked_socket(&case);
    std::fs::remove_file(&asked).unwrap();
    std::os::unix::fs::symlink(&other, &asked).unwrap();
    allow_grants_nothing(&case, question);
}

/// The owner's Allow binds to the very socket file the question showed, not only its
/// path: a different socket made at the same path after asking gets nothing
/// (RV-DS241-82).
#[cfg(target_os = "macos")]
#[test]
fn an_allow_after_another_socket_took_the_asked_path_grants_nothing() {
    let case = Case::new();
    let (asked, _, asked_listener, _other, question) = asked_socket(&case);
    drop(asked_listener);
    std::fs::remove_file(&asked).unwrap();
    let _again = crate::spawn_gate::bind_unix_std(&asked).unwrap();
    allow_grants_nothing(&case, question);
}

/// Asks for a socket at `path` as an agent and checks that the request gets the one
/// refusal every failed check of a socket path gives an agent, and raises nothing.
/// Inputs: the case and the path.
#[cfg(target_os = "macos")]
fn refused_alike(case: &Case, path: &Path) {
    let asked = request(
        &case.db,
        &case.project,
        &Asker::Master(7),
        &AccessGrant::Socket {
            path: path.to_path_buf(),
        },
        "the app's own socket",
    );
    assert_eq!(
        asked.map(|_| ()),
        Err("grant_socket_refused".to_owned()),
        "{}",
        path.display()
    );
    assert!(
        crate::questions::pending(&case.db, &case.project)
            .unwrap()
            .is_empty()
    );
}

/// The project folder of a case, where an agent names its sockets. Input: the case.
/// Output: the folder.
#[cfg(target_os = "macos")]
fn project_folder(case: &Case) -> PathBuf {
    case._tmp.path().canonicalize().unwrap().join("project")
}

/// An agent asking for a socket in a folder that does not exist learns nothing about the
/// folder: the refusal is the one every failed socket check gives (RV-DS241-87).
#[cfg(target_os = "macos")]
#[test]
fn a_socket_request_in_a_missing_folder_gets_the_one_socket_refusal() {
    let case = Case::new();
    refused_alike(&case, &project_folder(&case).join("gone").join("app.sock"));
}

/// An agent asking for a socket that is not there in a folder that is learns nothing
/// either (RV-DS241-87).
#[cfg(target_os = "macos")]
#[test]
fn a_socket_request_for_a_missing_file_gets_the_one_socket_refusal() {
    let case = Case::new();
    refused_alike(&case, &project_folder(&case).join("app.sock"));
}

/// An agent asking for a plain file as a socket learns nothing about what the file is
/// (RV-DS241-87).
#[cfg(target_os = "macos")]
#[test]
fn a_socket_request_for_a_plain_file_gets_the_one_socket_refusal() {
    let case = Case::new();
    let file = project_folder(&case).join("app.sock");
    std::fs::write(&file, "not a socket").unwrap();
    refused_alike(&case, &file);
}

/// An agent asking for a socket with a second name learns nothing about its owner or its
/// names (RV-DS241-87).
#[cfg(target_os = "macos")]
#[test]
fn a_socket_request_for_a_socket_with_a_second_name_gets_the_one_socket_refusal() {
    let case = Case::new();
    let first = project_folder(&case).join("app.sock");
    let _first = crate::spawn_gate::bind_unix_std(&first).unwrap();
    std::fs::hard_link(&first, project_folder(&case).join("two.sock")).unwrap();
    refused_alike(&case, &first);
}

/// An agent asking for a socket the floor refuses by its class learns nothing about which
/// class it is (RV-DS241-87).
#[cfg(target_os = "macos")]
#[test]
fn a_socket_request_for_a_key_agent_socket_gets_the_one_socket_refusal() {
    let case = Case::new();
    let keys = project_folder(&case).join("S.gpg-agent");
    let _keys = crate::spawn_gate::bind_unix_std(&keys).unwrap();
    refused_alike(&case, &keys);
}

/// An agent asking for a socket in the engine's own data folder learns nothing about that
/// folder (RV-DS241-87).
#[cfg(target_os = "macos")]
#[test]
fn a_socket_request_in_the_engine_data_folder_gets_the_one_socket_refusal() {
    let case = Case::new();
    let engine = case
        ._tmp
        .path()
        .canonicalize()
        .unwrap()
        .join("engine")
        .join("app.sock");
    let _engine = crate::spawn_gate::bind_unix_std(&engine).unwrap();
    refused_alike(&case, &engine);
}
