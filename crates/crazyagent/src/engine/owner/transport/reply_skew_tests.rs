use super::owner_server_tests::Fixture;
use super::*;
use crate::engine::owner::{AccountProfileRequest, NativeAuthScope};
use crate::master_session::MasterOwnerAction;
use crate::protocol::{ConfigId, CredentialRef, ProjectId, SessionId};
use crate::scheduling::{OrderWhy, PoolRanking, RankedAccount, Standing, Strategy};
use crate::usage::metrics::{AgentRow, Metrics};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::os::unix::fs::PermissionsExt;

const NEWER_FIELD: &str = "field_from_a_newer_engine";
const MARKER: &str = "reply-value-that-must-stay-out-of-logs";
const MISMATCH: &str = "engine_version_mismatch";
const OWNER_UNKNOWN: &str = "owner_outcome_unknown_reconcile_same_identity";
const ASIDE: &str = "owner.sock.real";

/// A real engine whose owner socket is swapped for one that answers each connection with
/// the next queued JSON reply, so a reply only a newer engine could write reaches the real
/// client. Fields: the engine, its generation folder, the serving task and the client.
struct Skewed {
    fixture: Fixture,
    generation: PathBuf,
    server: tokio::task::JoinHandle<()>,
    client: OwnerClient,
}

impl Skewed {
    /// Starts the engine and the swapped socket. Input: the replies, in the order the
    /// client asks. Output: the running pair with a client attached to the swapped socket.
    async fn start(replies: Vec<Value>) -> Self {
        Self::start_raw(
            replies
                .iter()
                .map(|reply| serde_json::to_vec(reply).unwrap())
                .collect(),
        )
        .await
    }

    /// Starts the engine and the swapped socket. Input: the reply frames' bodies, as bytes
    /// that need not be JSON, in the order the client asks. Output: the running pair.
    async fn start_raw(replies: Vec<Vec<u8>>) -> Self {
        let fixture = Fixture::start().await;
        let generation = fixture.client().await.root.path.clone();
        let socket = generation.join(SOCKET);
        std::fs::rename(&socket, generation.join(ASIDE)).unwrap();
        let listener = crate::spawn_gate::bind_unix(&socket).unwrap();
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
        let server = tokio::spawn(serve(listener, replies));
        let client =
            OwnerClient::attach(&generation, OwnerClientKind::Tui, Duration::from_secs(20))
                .unwrap();
        Self {
            fixture,
            generation,
            server,
            client,
        }
    }

    /// Reads the client's unreadable-reply log. Output: its entries, none for no log.
    fn logged(&self) -> Vec<Value> {
        match std::fs::read_to_string(self.log()) {
            Ok(text) => text
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => panic!("the log could not be read: {error}"),
        }
    }

    /// The client's unreadable-reply log. Output: its path in the data root's logs folder.
    fn log(&self) -> PathBuf {
        self.fixture
            .root
            .path()
            .join("logs")
            .join("owner-reply.log")
    }

    /// Puts the engine's socket back and stops the engine. Input: none. Output: none.
    async fn stop(self) {
        self.server.abort();
        let socket = self.generation.join(SOCKET);
        std::fs::remove_file(&socket).unwrap();
        std::fs::rename(self.generation.join(ASIDE), &socket).unwrap();
        self.fixture.stop().await;
    }
}

/// Answers one connection per queued reply with the reply's own bytes.
/// Inputs: the listener and the frame bodies. Output: none; ends when the queue is empty.
async fn serve(listener: UnixListener, replies: Vec<Vec<u8>>) {
    let mut replies: VecDeque<Vec<u8>> = replies.into();
    while let Some(reply) = replies.pop_front() {
        let (mut stream, _) = crate::spawn_gate::accept_unix(&listener).await.unwrap();
        let mut auth = [0; 37];
        stream.read_exact(&mut auth).await.unwrap();
        read_frame(&mut stream).await.unwrap();
        let length = u32::try_from(reply.len()).unwrap();
        stream.write_all(&length.to_be_bytes()).await.unwrap();
        stream.write_all(&reply).await.unwrap();
    }
}

/// A usage metrics reply. Input: whether one agent row carries a field this client lacks.
/// Output: the reply.
fn metrics_reply(newer: bool) -> Value {
    let mut row = serde_json::to_value(AgentRow::default()).unwrap();
    if newer {
        row[NEWER_FIELD] = json!(1);
    }
    let mut metrics = serde_json::to_value(Metrics::default()).unwrap();
    metrics["agents"] = json!([row]);
    json!({ "kind": "metrics", "metrics": metrics })
}

/// A change set list reply. Input: whether the set carries a field this client lacks.
/// Output: the reply.
fn change_sets_reply(newer: bool) -> Value {
    let mut view = json!({
        "id": "0123456789abcdef0123456789abcdef",
        "agent": { "kind": "master", "id": "m", "label": "Master" },
        "state": "pending",
        "base": "a",
        "head": "b",
        "files": 1,
        "added": 1,
        "removed": 0,
        "last_outcome": "succeeded",
        "checked": true,
        "created_ms": 1,
        "updated_ms": 2,
        "restorable_until_ms": null,
        "conflicts": [],
    });
    if newer {
        view[NEWER_FIELD] = json!(1);
    }
    json!({ "kind": "list", "change_sets": [view] })
}

/// An uncommitted import reply. Input: whether it carries a field this client lacks.
/// Output: the reply.
fn uncommitted_reply(newer: bool) -> Value {
    let mut reply = json!({
        "kind": "uncommitted",
        "files": ["CLAUDE.md"],
        "branch": "imports",
        "checked_out": null,
    });
    if newer {
        reply[NEWER_FIELD] = json!(1);
    }
    reply
}

/// A reply kind no client knows. Output: the reply.
fn junk() -> Value {
    json!({ "kind": "reply_from_a_newer_engine" })
}

/// An owner request with an unreadable reply expected. Inputs: the id and the operation.
/// Output: the request.
fn owner_request(id: &str, operation: OwnerOperation) -> OwnerRequest {
    OwnerRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::new(id).unwrap(),
        expected_revision: Cursor::new(0),
        operation,
    }
}

/// The project every read in this file asks about. Output: the id.
fn project() -> ProjectId {
    ProjectId::new("p").unwrap()
}

/// An account profile request naming a synthetic connection. Output: the request.
fn account_request() -> AccountProfileRequest {
    AccountProfileRequest {
        request_id: RequestId::new("skew-account").unwrap(),
        scope: NativeAuthScope {
            expected_database_instance: "a".repeat(64),
            connection: CredentialRef::new("skew-connection").unwrap(),
            expected_config_id: ConfigId::new("b".repeat(64)).unwrap(),
            expected_source_hash: "c".repeat(64),
        },
    }
}

/// The refusal a call ended with. Input: the call's result. Output: its code.
fn refused<T: std::fmt::Debug>(result: Result<T, EngineError>) -> String {
    result.unwrap_err().0
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_read_reply_with_a_field_this_client_lacks_is_a_version_mismatch() {
    let skewed = Skewed::start(vec![
        metrics_reply(false),
        metrics_reply(true),
        change_sets_reply(false),
        change_sets_reply(true),
        uncommitted_reply(false),
        uncommitted_reply(true),
    ])
    .await;
    let client = &skewed.client;
    assert_eq!(client.usage_metrics(None, 0).await.unwrap().agents.len(), 1);
    assert_eq!(refused(client.usage_metrics(None, 0).await), MISMATCH);
    assert_eq!(client.change_sets(project()).await.unwrap().len(), 1);
    assert_eq!(refused(client.change_sets(project()).await), MISMATCH);
    assert_eq!(
        client
            .uncommitted_instruction_import(project())
            .await
            .unwrap()
            .files,
        ["CLAUDE.md"]
    );
    assert_eq!(
        refused(client.uncommitted_instruction_import(project()).await),
        MISMATCH
    );
    skewed.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_read_of_every_exchange_reports_an_unreadable_reply_as_a_version_mismatch() {
    let skewed = Skewed::start(vec![junk(); 14]).await;
    let client = &skewed.client;
    assert_eq!(refused(client.account_catalog().await), MISMATCH);
    assert_eq!(refused(client.agents(None, 0).await), MISMATCH);
    assert_eq!(refused(client.change_sets(project()).await), MISMATCH);
    assert_eq!(refused(client.finished_agents(project()).await), MISMATCH);
    assert_eq!(refused(client.github_status().await), MISMATCH);
    assert_eq!(refused(client.harness_catalog(false).await), MISMATCH);
    assert_eq!(
        refused(client.ide_extension_asks(project()).await),
        MISMATCH
    );
    assert_eq!(refused(client.ide_tab_state(false).await), MISMATCH);
    assert_eq!(refused(client.plugin_skills(None, 0).await), MISMATCH);
    assert_eq!(
        refused(client.uncommitted_instruction_import(project()).await),
        MISMATCH
    );
    assert_eq!(
        refused(client.questions_list(project(), None).await),
        MISMATCH
    );
    assert_eq!(refused(client.recovery_banner(project()).await), MISMATCH);
    assert_eq!(refused(client.usage_metrics(None, 0).await), MISMATCH);
    assert_eq!(
        refused(
            client
                .read_account_auth_capabilities(account_request())
                .await
        ),
        MISMATCH
    );
    assert_eq!(skewed.logged().len(), 14);
    skewed.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_owner_read_is_a_mismatch_and_a_mutation_or_reconcile_keeps_its_unknown_outcome() {
    let skewed = Skewed::start(vec![junk(), junk(), junk()]).await;
    let client = &skewed.client;
    let read = owner_request(
        "skew-read",
        OwnerOperation::Read {
            query: OwnerRead::Capabilities,
        },
    );
    assert_eq!(refused(client.request(read).await), MISMATCH);
    let reconcile = owner_request(
        "skew-reconcile",
        OwnerOperation::Reconcile {
            operation_id: RequestId::new("skew-landed").unwrap(),
            expected_hash: None,
        },
    );
    assert_eq!(refused(client.request(reconcile).await), OWNER_UNKNOWN);
    let apply = owner_request(
        "skew-apply",
        OwnerOperation::Apply {
            mutation: OwnerMutation::ProjectMaster {
                action: MasterOwnerAction::Cancel {
                    project_id: project(),
                    session_id: SessionId::new("skew-session").unwrap(),
                    turn_id: RequestId::new("skew-turn").unwrap(),
                },
            },
        },
    );
    assert_eq!(refused(client.request(apply).await), OWNER_UNKNOWN);
    assert_eq!(skewed.logged().len(), 1);
    skewed.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_mutation_exchange_keeps_its_unknown_outcome_and_logs_nothing() {
    let skewed = Skewed::start(vec![junk()]).await;
    assert_eq!(
        refused(skewed.client.usage_refresh().await),
        "usage_outcome_unknown"
    );
    assert!(skewed.logged().is_empty());
    skewed.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_skewed_read_back_of_a_configure_keeps_its_unknown_outcome() {
    let skewed = Skewed::start(vec![junk()]).await;
    assert_eq!(
        refused(skewed.client.read_account_profile(account_request()).await),
        "account_profile_outcome_unknown"
    );
    assert!(skewed.logged().is_empty());
    skewed.stop().await;
}

/// A wrong value inside a tagged reply has no position, because serde buffers the reply
/// before it reads the variant, so its line and column are 0.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_log_names_where_a_reply_broke_and_never_a_value_from_it() {
    let mut metrics = serde_json::to_value(Metrics::default()).unwrap();
    metrics["running"] = json!(MARKER);
    let parse = serde_json::from_value::<Metrics>(metrics.clone()).unwrap_err();
    assert!(
        parse.to_string().contains(MARKER),
        "serde must echo the value for this row to prove anything"
    );
    let skewed = Skewed::start(vec![json!({ "kind": "metrics", "metrics": metrics })]).await;
    assert_eq!(
        refused(skewed.client.usage_metrics(None, 0).await),
        MISMATCH
    );
    let raw = std::fs::read_to_string(skewed.log()).unwrap();
    assert!(!raw.contains(MARKER), "the reply's value reached the log");
    let entries = skewed.logged();
    assert_eq!(entries.len(), 1);
    let entry = entries[0].as_object().unwrap();
    let mut fields: Vec<&str> = entry.keys().map(String::as_str).collect();
    fields.sort_unstable();
    assert_eq!(fields, ["category", "column", "event", "kind", "line"]);
    assert_eq!(entry["event"], "owner_reply_unreadable");
    assert_eq!(entry["kind"], "usage.metrics");
    assert_eq!(entry["category"], "data");
    assert_eq!(entry["line"], 0);
    assert_eq!(entry["column"], 0);
    skewed.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reply_that_is_not_json_is_logged_with_its_real_position() {
    let bodies: [(&[u8], &str); 2] = [
        (br#"{"kind":"metrics",}"#, "syntax"),
        (br#"{"kind":"metrics","metr"#, "eof"),
    ];
    let skewed = Skewed::start_raw(bodies.iter().map(|(body, _)| body.to_vec()).collect()).await;
    for _ in bodies {
        assert_eq!(
            refused(skewed.client.usage_metrics(None, 0).await),
            MISMATCH
        );
    }
    let entries = skewed.logged();
    assert_eq!(entries.len(), bodies.len());
    for ((body, category), entry) in bodies.iter().zip(&entries) {
        let parse = serde_json::from_slice::<Value>(body).unwrap_err();
        assert_eq!(entry["category"], *category);
        assert_eq!(entry["line"], parse.line());
        assert_eq!(entry["column"], parse.column());
        assert!(parse.column() > 0);
    }
    skewed.stop().await;
}

#[test]
fn the_shared_refusal_text_explains_a_version_mismatch_as_the_desktop_app_does() {
    assert_eq!(
        crate::config::plain_problem(MISMATCH),
        Some("CrazyAgent's engine is a different version from this app. Restart the engine.")
    );
}

/// An account ranking reply. Input: the field an engine from before the why lines did not
/// write, `why` on the account or `own_order_projects` on the pool, or None for today's
/// shape. Output: the reply.
fn ranking_reply(lacking: Option<&str>) -> Value {
    let ranking = PoolRanking {
        pool: CredentialRef::new("skew-pool").unwrap(),
        strategy: Strategy::ManualRank,
        accounts: vec![RankedAccount {
            connection: CredentialRef::new("skew-connection").unwrap(),
            standing: Standing::Next {
                weekly_resets_ms: None,
                weekly_left_percent: None,
                use_last: false,
                why: OrderWhy::NoReading,
            },
            held_for: Vec::new(),
        }],
        own_order_projects: 0,
    };
    let mut reply = serde_json::to_value(OwnerReply {
        request_id: RequestId::new("skew-ranking").unwrap(),
        response: OwnerResponse::Read {
            state: OwnerReadReply {
                revision: Cursor::new(0),
                config_id: ConfigId::new("b".repeat(64)).unwrap(),
                data: OwnerData::AccountRanking {
                    pools: vec![ranking],
                },
            },
        },
    })
    .unwrap();
    let pool = &mut reply["response"]["state"]["data"]["pools"][0];
    match lacking {
        Some("why") => assert!(
            pool["accounts"][0]["standing"]
                .as_object_mut()
                .unwrap()
                .remove("why")
                .is_some()
        ),
        Some(field) => assert!(pool.as_object_mut().unwrap().remove(field).is_some()),
        None => {}
    }
    reply
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_ranking_reply_from_before_the_why_lines_is_a_version_mismatch() {
    let skewed = Skewed::start(vec![
        ranking_reply(None),
        ranking_reply(Some("why")),
        ranking_reply(Some("own_order_projects")),
    ])
    .await;
    let read = || {
        owner_request(
            "skew-ranking",
            OwnerOperation::Read {
                query: OwnerRead::AccountRanking,
            },
        )
    };
    let client = &skewed.client;
    assert!(matches!(
        client.request(read()).await.unwrap().response,
        OwnerResponse::Read { .. }
    ));
    assert_eq!(refused(client.request(read()).await), MISMATCH);
    assert_eq!(refused(client.request(read()).await), MISMATCH);
    skewed.stop().await;
}

/// PROTO-SKEW-READ-KIND: every exchange's `read_kind` lists exactly the reads named here and
/// ends in `_ => None`, so an arm dropped turns a read's skew back into an unknown outcome and
/// fails this row, and a mutation added to a read list, which would turn a mutation's unknown
/// outcome into a version mismatch, fails it too until it is added here on purpose.
#[test]
fn every_exchange_names_its_reads_and_only_its_reads() {
    let sources = [
        ("account_profile", include_str!("account_profile.rs")),
        ("accounts", include_str!("accounts.rs")),
        ("agents", include_str!("agents.rs")),
        ("changes", include_str!("changes.rs")),
        ("cleanup", include_str!("cleanup.rs")),
        ("github", include_str!("github.rs")),
        ("ide_extensions", include_str!("ide_extensions.rs")),
        ("ide_tab", include_str!("ide_tab.rs")),
        ("plugins", include_str!("plugins.rs")),
        ("process", include_str!("process.rs")),
        ("questions", include_str!("questions.rs")),
        ("recovery", include_str!("recovery.rs")),
        ("roadmap", include_str!("roadmap.rs")),
        ("usage", include_str!("usage.rs")),
    ];
    let expected: &[(&str, &[&str])] = &[
        ("account_profile", &["Capabilities"]),
        ("accounts", &["Catalog", "List", "LocalModels"]),
        ("agents", &["List", "Using", "Holders"]),
        (
            "changes",
            &["List", "Diff", "Branches", "Waiting", "OwnerDirty"],
        ),
        ("cleanup", &["Finished", "Removal"]),
        ("github", &["Status", "Repos"]),
        ("ide_extensions", &["Asks"]),
        ("ide_tab", &["State"]),
        (
            "plugins",
            &[
                "Skills",
                "SkillDetail",
                "Servers",
                "Server",
                "ImportScan",
                "Hooks",
                "Effective",
                "Tools",
            ],
        ),
        (
            "process",
            &[
                "Read",
                "PendingImport",
                "UncommittedImport",
                "ReadFile",
                "Default",
                "Given",
                "Agent",
                "AgentText",
                "Settings",
                "AppSettings",
                "Adoption",
            ],
        ),
        ("questions", &["List", "Counts"]),
        ("recovery", &["List"]),
        ("roadmap", &["ReadStatus"]),
        ("usage", &["Report", "Metrics"]),
    ];
    assert_eq!(sources.len(), expected.len());
    let head = "fn read_kind(action: &Action) -> Option<&'static str> {\n";
    for ((module, source), (named, reads)) in sources.iter().zip(expected) {
        assert_eq!(module, named);
        assert_eq!(source.matches(head).count(), 1, "{module}");
        let body = source
            .split(head)
            .nth(1)
            .unwrap()
            .split("\n}\n")
            .next()
            .unwrap();
        let arms: Vec<&str> = body
            .lines()
            .map(str::trim)
            .filter(|line| line.contains("=>"))
            .collect();
        assert_eq!(arms.last(), Some(&"_ => None,"), "{module}");
        let found: Vec<(String, String)> = arms[..arms.len() - 1]
            .iter()
            .map(|arm| {
                let variant = arm
                    .strip_prefix("Action::")
                    .unwrap_or_else(|| panic!("{module}: {arm}"))
                    .split([' ', '{', '='])
                    .next()
                    .unwrap();
                let kind = arm
                    .split("=> Some(\"")
                    .nth(1)
                    .and_then(|rest| rest.strip_suffix("\"),"))
                    .unwrap_or_else(|| panic!("{module}: {arm}"));
                (variant.to_owned(), kind.to_owned())
            })
            .collect();
        let wanted: Vec<(String, String)> = reads
            .iter()
            .map(|variant| {
                let mut kind = String::new();
                for (at, c) in variant.char_indices() {
                    if c.is_ascii_uppercase() && at > 0 {
                        kind.push('_');
                    }
                    kind.push(c.to_ascii_lowercase());
                }
                ((*variant).to_owned(), format!("{module}.{kind}"))
            })
            .collect();
        assert_eq!(found, wanted, "{module}");
    }
}
