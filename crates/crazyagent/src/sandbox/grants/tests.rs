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
        .tempdir_in("/private/tmp")
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
        case.ask_port(50_000 + port);
    }
    let over = request(
        &case.db,
        &case.project,
        &Asker::Agent(3),
        &AccessGrant::Port { port: 50_100 },
        "one more",
    );
    assert_eq!(over, Err("grant_requests_full".to_owned()));
    let blank = request(
        &case.db,
        &case.project,
        &Asker::Agent(3),
        &AccessGrant::Port { port: 50_101 },
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
        &AccessGrant::Port { port: 50_102 },
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
