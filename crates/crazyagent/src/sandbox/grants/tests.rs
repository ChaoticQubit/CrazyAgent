use super::*;
use crate::questions::{Answer, Settlement};
use crate::sandbox::GRANT_HOOKS;
use crate::sandbox::grants_store::*;
use kernel_sandbox::grants::*;
use kernel_sandbox::policy::AgentPolicy;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

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
        crate::sandbox::policy_store::load(&self.db, PROJECT).unwrap()
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
    crate::sandbox::policy_store::save(&case.db, PROJECT, &held).unwrap();
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
    crate::sandbox::policy_store::save(&case.db, PROJECT, &full).unwrap();
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
            checked(&GRANT_HOOKS, &AccessGrant::Port { port }, data.path()),
            Err("grant_port_reserved"),
            "IDE port {port}"
        );
    }
    for port in [*DYNAMIC_PORTS.start(), 60_000, u16::MAX] {
        assert_eq!(
            checked(&GRANT_HOOKS, &AccessGrant::Port { port }, data.path()),
            Err("grant_port_dynamic"),
            "port {port}"
        );
    }
    assert_eq!(
        checked(
            &GRANT_HOOKS,
            &AccessGrant::Port { port: 49_151 },
            data.path()
        ),
        Ok(AccessGrant::Port { port: 49_151 })
    );
    let saved = [
        AccessGrant::Port { port: 8_080 },
        AccessGrant::Port { port: 8_081 },
    ];
    assert_eq!(
        held_now(&GRANT_HOOKS, &saved, &[8_081], data.path()),
        [AccessGrant::Port { port: 8_080 }],
        "a kept model port is left out at launch"
    );
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

/// RV-DS241-83: on Linux, where Landlock limits ports and not hosts, a port's access question
/// says the port is open to agents on every host; a socket's and the temp folder's do not,
/// and no other system's question says it.
#[test]
fn only_on_linux_does_a_port_question_say_the_port_is_open_on_every_host() {
    let port = AccessGrant::Port { port: 5_432 };
    let line = kernel_sandbox::lines::LINUX_PORTS_ANY_HOST_LINE;
    assert_eq!(asked_meaning(&port, None), meaning(&port));
    assert_eq!(
        asked_meaning(&port, Some(line)),
        format!("{} {line}", meaning(&port))
    );
    let named = kernel_sandbox::lines::linux_unbridged_line(80);
    assert_eq!(
        asked_meaning(&port, Some(&named)),
        format!("{} {named}", meaning(&port))
    );
    for grant in [
        AccessGrant::Socket {
            path: PathBuf::from("/private/tmp/app/app.sock"),
        },
        AccessGrant::TempFolder,
    ] {
        assert_eq!(
            asked_meaning(&grant, Some(line)),
            meaning(&grant),
            "{grant:?}"
        );
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
    let asked_listener = kernel_processes::spawn_gate::bind_unix_std(&asked).unwrap();
    let other_listener = kernel_processes::spawn_gate::bind_unix_std(&other).unwrap();
    assert_eq!(
        checked(
            &GRANT_HOOKS,
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
    let _again = kernel_processes::spawn_gate::bind_unix_std(&asked).unwrap();
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
    let _first = kernel_processes::spawn_gate::bind_unix_std(&first).unwrap();
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
    let _keys = kernel_processes::spawn_gate::bind_unix_std(&keys).unwrap();
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
    let _engine = kernel_processes::spawn_gate::bind_unix_std(&engine).unwrap();
    refused_alike(&case, &engine);
}
