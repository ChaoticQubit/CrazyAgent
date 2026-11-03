use super::*;
use crate::agent_git::Layout;
use crate::config::ModelAccess;
use crate::project_setup::{HarnessSlot, StoredSetup, TeamPlan};
use crate::protocol::ProjectId;
use crate::team::master::{Answering, answer_sub, serve_calls};
use crate::team::tool::{QUESTION_TOOLS, SPAWN_TOOL, TEAM_TOOLS};
use crate::tracker::settings::{Scope, TicketOverride};
use rusqlite::{Connection, Transaction, TransactionBehavior, params};
use serde_json::{Value, json};
use std::io::Cursor;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use trackwright_core::catalog::{catalog, op_schema};
use trackwright_core::model::{ClosePolicy, ProjectKey};

const PROJECT: &str = "ticket-tools";
const KEY: &str = "TT";
const MASTER: i64 = 41;
const AGENT: i64 = 7;
const NOW: i64 = 1_800_000_000_000;
const ENGINE: &str = "/usr/local/bin/crazyagent";

/// A private engine root whose `engine.db` holds every authority schema and the team and
/// setup tables, with one project attached to the tracker as `TT`, its live master and one
/// sub-agent with tools, and the mailbox folder a team launch leaves.
struct Case {
    _root: tempfile::TempDir,
    base: PathBuf,
    mailbox: PathBuf,
    db: Connection,
    setup: StoredSetup,
    access: ModelAccess,
}

impl Case {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let base = root.path().canonicalize().unwrap();
        let folder = base.join("project");
        let mailbox = base.join("tmp").join(crate::team::tool::MAILBOX_FOLDER);
        for path in [&folder, &mailbox] {
            std::fs::create_dir_all(path).unwrap();
        }
        std::fs::create_dir(folder.join(crate::projects_home::CRAZYAGENT_DIRECTORY)).unwrap();
        let db = Connection::open(base.join("engine.db")).unwrap();
        crate::storage::initialize_authority_schemas(&db).unwrap();
        crate::team::initialize(&db).unwrap();
        crate::project_setup::initialize(&db).unwrap();
        crate::ide::workspace::tests::add_project(&db, PROJECT, &folder);
        for (id, role) in [(MASTER, "master"), (AGENT, "sub")] {
            db.execute(
                "INSERT INTO team_agents(id,project,role,harness,tools,connection,status,
                 created_ms,heartbeat_ms) VALUES (?1,?2,?3,'codex',1,'local-a','working',?4,?4)",
                params![id, PROJECT, role, NOW],
            )
            .unwrap();
        }
        db.execute(
            "INSERT INTO team_live_masters(project,live,agent) VALUES (?1,1,?2)",
            params![PROJECT, MASTER],
        )
        .unwrap();
        let tx = Transaction::new_unchecked(&db, TransactionBehavior::Immediate).unwrap();
        crate::tracker::attach(
            &tx,
            &project(),
            &ProjectKey::new(KEY).unwrap(),
            "Tools",
            NOW,
        )
        .unwrap();
        tx.commit().unwrap();
        let setup = StoredSetup {
            project_id: project(),
            name: "Tools".into(),
            about: String::new(),
            tech_stack: vec![],
            platforms: vec![],
            research: vec![],
            pool: None,
            team: TeamPlan {
                master: "claude-code".into(),
                master_tools: true,
                sub_harnesses: vec![HarnessSlot {
                    harness: "codex".into(),
                    agents: 2,
                    tools: true,
                }],
                min_agents: 1,
                max_agents: 3,
            },
            documents: vec![],
            folder: folder.to_str().unwrap().into(),
            created_ms: 1,
            revision: 1,
        };
        Self {
            _root: root,
            base,
            mailbox,
            db,
            setup,
            access: ModelAccess::default(),
        }
    }

    /// Runs the team server on the requests while the engine answers its mailbox the way a
    /// runtime tick does: as the live master, or as the sub-agent the mailbox belongs to.
    /// Inputs: whether the master calls and the requests. Output: the server's answers.
    fn served(&mut self, master: bool, requests: &[Value]) -> Vec<Value> {
        let mut input = Vec::new();
        for request in requests {
            input.extend(serde_json::to_vec(request).unwrap());
            input.push(b'\n');
        }
        let mailbox = self.mailbox.clone();
        let server = std::thread::spawn(move || {
            let mut output = Vec::new();
            crate::team::tool::serve_live(Cursor::new(input), &mut output, &mailbox, &[]).unwrap();
            output
        });
        let access = self.access.clone();
        let setup = self.setup.clone();
        let landing = self.base.clone();
        let answering = Answering {
            access: &access,
            stored: &setup,
            landing: &landing,
            agents: &landing,
            events: None,
            host: None,
        };
        let deadline = Instant::now() + Duration::from_secs(60);
        while !server.is_finished() {
            assert!(Instant::now() < deadline, "the server never finished");
            if master {
                serve_calls(&mut self.db, &answering, MASTER, &self.mailbox).unwrap();
            } else {
                crate::team::history::serve_agent_calls(
                    &mut self.db,
                    &project(),
                    (AGENT, "Agent 7"),
                    &landing,
                    &self.mailbox,
                    &mut |db, _, _| {
                        crate::team::runtime::team_caller(
                            db,
                            None,
                            &project(),
                            (AGENT, "Agent 7"),
                            None,
                        )
                    },
                    &mut |db, caller, call| answer_sub(db, &answering, caller, call).unwrap(),
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let answers: Vec<Value> = server
            .join()
            .unwrap()
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice(line).unwrap())
            .collect();
        assert_eq!(answers.len(), requests.len(), "{answers:#?}");
        answers
    }

    fn count(&self, sql: &str) -> i64 {
        self.db.query_row(sql, [], |row| row.get(0)).unwrap()
    }

    fn text(&self, sql: &str) -> String {
        self.db.query_row(sql, [], |row| row.get(0)).unwrap()
    }
}

fn project() -> ProjectId {
    ProjectId::new(PROJECT).unwrap()
}

fn tool_call(id: u64, name: &str, arguments: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": {"name": name, "arguments": arguments}})
}

fn text(answer: &Value) -> (&str, bool) {
    (
        answer["result"]["content"][0]["text"].as_str().unwrap(),
        answer["result"]["isError"].as_bool().unwrap(),
    )
}

/// Reads a ticket tool's answer as the core's reply envelope. Input: the server's answer.
/// Output: the envelope and whether the tool result is an error.
fn envelope(answer: &Value) -> (Value, bool) {
    let (text, error) = text(answer);
    (serde_json::from_str(text).unwrap(), error)
}

fn listed(serve: impl FnOnce(Cursor<Vec<u8>>, &mut Vec<u8>)) -> Vec<Value> {
    let mut input =
        serde_json::to_vec(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})).unwrap();
    input.push(b'\n');
    let mut output = Vec::new();
    serve(Cursor::new(input), &mut output);
    let answer: Value = serde_json::from_slice(output.trim_ascii()).unwrap();
    answer["result"]["tools"].as_array().unwrap().clone()
}

#[test]
fn an_agent_tool_call_runs_the_core_op_as_that_agent() {
    let mut case = Case::new();
    let answers = case.served(
        false,
        &[
            tool_call(
                1,
                "ticket_create",
                json!({"project": KEY, "title": "Fix the login page"}),
            ),
            tool_call(
                2,
                "ticket_create",
                json!({"project": KEY, "title": "Pose as the owner", "actor": "owner"}),
            ),
            tool_call(3, "ticket_show", json!({"id": "TT-1"})),
            tool_call(
                4,
                "ticket_policy_set",
                json!({"project": KEY, "key": "close", "value": "agents"}),
            ),
        ],
    );

    let (created, error) = envelope(&answers[0]);
    assert!(!error, "{created}");
    assert_eq!(created["ok"], true, "{created}");
    assert_eq!(created["data"]["id"], "TT-1");
    assert_eq!(created["data"]["created_by"], "agent:7");
    assert_eq!(case.text("SELECT created_by FROM tk_tickets"), "agent:7");
    assert_eq!(
        case.text("SELECT actor FROM tk_events WHERE op = 'create'"),
        "agent:7"
    );

    let (posed, error) = envelope(&answers[1]);
    assert!(error, "{posed}");
    assert_eq!(posed["ok"], false);
    assert_eq!(posed["code"], trackwright_core::model::codes::UNKNOWN_FIELD);
    assert_eq!(case.count("SELECT COUNT(*) FROM tk_tickets"), 1);

    let (shown, error) = envelope(&answers[2]);
    assert!(!error, "{shown}");
    assert_eq!(shown["data"]["ticket"]["created_by"], "agent:7", "{shown}");

    assert_eq!(answers[3]["error"]["code"], -32602, "{:#?}", answers[3]);
    assert_eq!(std::fs::read_dir(&case.mailbox).unwrap().count(), 0);
}

#[test]
fn team_tools_match_the_catalog() {
    let case = Case::new();
    let mailbox = case.mailbox.clone();
    let live = listed(|input, output| {
        crate::team::tool::serve_live(input, output, &mailbox, &[]).unwrap()
    });
    let names: Vec<&str> = live
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    let offered: Vec<&Value> = live
        .iter()
        .filter(|tool| tool["name"].as_str().unwrap().starts_with(TOOL_PREFIX))
        .collect();
    let expected: Vec<_> = catalog()
        .iter()
        .filter(|spec| !WITHHELD.contains(&spec.kind))
        .collect();
    assert_eq!(expected.len(), catalog().len() - WITHHELD.len());
    for kind in WITHHELD {
        assert!(catalog().iter().any(|spec| spec.kind == kind), "{kind}");
        assert_eq!(tool_name(kind), None, "{kind}");
        assert!(!names.contains(&format!("{TOOL_PREFIX}{kind}").as_str()));
    }
    assert_eq!(offered.len(), expected.len());
    for (tool, spec) in offered.iter().zip(&expected) {
        let name = format!("{TOOL_PREFIX}{}", spec.kind);
        assert_eq!(tool["name"], name.as_str());
        assert_eq!(tool_name(spec.kind), Some(name.as_str()));
        assert!(name.len() <= 64);
        assert!(
            name.bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
        );
        assert_eq!(tool["description"], spec.summary, "{name}");
        assert_eq!(tool["inputSchema"], op_schema(spec)["args"], "{name}");
    }
    for team in TEAM_TOOLS.iter().chain(&QUESTION_TOOLS) {
        assert!(!team.starts_with(TOOL_PREFIX), "{team}");
        assert_eq!(
            names.iter().filter(|name| **name == *team).count(),
            1,
            "{team}"
        );
    }
    assert_eq!(
        names.len(),
        TEAM_TOOLS.len() + QUESTION_TOOLS.len() + expected.len()
    );

    let drop = case.base.join("tmp").join(crate::team::tool::DROP_FILE);
    let reply_end = listed(|input, output| crate::team::tool::serve(input, output, &drop).unwrap());
    assert!(
        reply_end
            .iter()
            .all(|tool| !tool["name"].as_str().unwrap().starts_with(TOOL_PREFIX)),
        "a master whose calls wait for its reply's end gets no ticket tools"
    );
    let spawn = live.iter().find(|tool| tool["name"] == SPAWN_TOOL).unwrap();
    assert_eq!(
        spawn["inputSchema"]["properties"]["ticket"]["type"], "string",
        "{spawn}"
    );
}

#[test]
fn delegate_with_a_ticket_records_the_task_link() {
    let mut case = Case::new();
    let answers = case.served(
        true,
        &[
            tool_call(
                1,
                "ticket_create",
                json!({"project": KEY, "title": "Write the notes page"}),
            ),
            tool_call(
                2,
                SPAWN_TOOL,
                json!({"task": "Write notes.md", "harness": "codex", "ticket": "TT-1"}),
            ),
            tool_call(
                3,
                SPAWN_TOOL,
                json!({"task": "Write more.md", "harness": "codex", "ticket": "TT-9"}),
            ),
            tool_call(
                4,
                SPAWN_TOOL,
                json!({"task": "Write more.md", "ticket": "notes"}),
            ),
            tool_call(5, SPAWN_TOOL, json!({"task": "Write more.md", "ticket": 1})),
        ],
    );
    let (created, error) = envelope(&answers[0]);
    assert!(!error, "{created}");
    assert_eq!(created["data"]["created_by"], "master:41");

    let task: i64 = case
        .db
        .query_row(
            "SELECT id FROM team_tasks WHERE project=?1 AND prompt='Write notes.md'",
            [PROJECT],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        text(&answers[1]),
        (
            format!(
                "Task {task} is queued for a codex sub-agent. It waits until the pool has a \
                 ready account for that harness. The engine tells you when it ends."
            )
            .as_str(),
            false
        )
    );
    let link: (String, String, String) = case
        .db
        .query_row(
            "SELECT l.kind, l.target, l.created_by FROM tk_links l
             JOIN tk_tickets t ON t.id = l.ticket WHERE t.number = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        link,
        (
            "task".to_owned(),
            task.to_string(),
            DELEGATE_ACTOR.to_owned()
        )
    );
    assert_eq!(case.count("SELECT COUNT(*) FROM tk_links"), 1);

    let (missing, error) = text(&answers[2]);
    assert!(error, "{missing}");
    assert!(missing.starts_with("Not handed off: "), "{missing}");
    assert!(
        missing.contains(trackwright_core::model::codes::TICKET_UNKNOWN),
        "{missing}"
    );
    assert_eq!(text(&answers[3]), (TICKET_ID_INVALID, true));
    assert_eq!(text(&answers[4]), (TICKET_ID_INVALID, true));
    assert_eq!(
        case.count("SELECT COUNT(*) FROM team_tasks"),
        1,
        "a refused link leaves no task behind"
    );
    assert_eq!(case.count("SELECT COUNT(*) FROM tk_links"), 1);
}

const CODEX_PROFILE: &str = "codex-managed-profile";

/// The project's pool as a lead's `spawn` needs it: one native managed Codex profile whose
/// latest sign-in, recorded as the native sign-in flow stores it, observed an account.
/// Input: the case. Output: the access holding that pool.
fn signed_in_codex(case: &Case) -> ModelAccess {
    use crate::config::{
        AccountPool, ModelAuthentication, ModelConnection, ModelEndpoint, ModelReference,
    };
    use crate::protocol::CredentialRef;
    let reference = |value: &str| CredentialRef::new(value).unwrap();
    case.db
        .execute(
            "INSERT INTO native_auth_operations(id,profile_key,generation,phase,request,fact)
             VALUES ('operation-1',?1,1,'authenticated_observed',X'01',X'01')",
            [crate::memory::hash(CODEX_PROFILE.as_bytes())],
        )
        .unwrap();
    ModelAccess {
        accounts: vec![reference("local-account")],
        connections: vec![ModelConnection {
            id: reference("codex-managed"),
            provider: reference("openai"),
            harness: reference("codex"),
            protocol: reference("native"),
            account_ref: reference("local-account"),
            endpoint: ModelEndpoint::Native {},
            model: ModelReference {
                id: "gpt-5.5".into(),
                author: reference("openai"),
            },
            auth: ModelAuthentication::NativeManaged {
                profile_ref: reference(CODEX_PROFILE),
            },
        }],
        pools: vec![AccountPool {
            id: reference("team-pool"),
            name: None,
            members: vec![reference("codex-managed")],
            projects: vec![reference(PROJECT)],
            roles: vec![reference("project_leader")],
            fallback_allowed: false,
            provider_quota_policy: Default::default(),
        }],
    }
}

/// Attaches a second engine project to the tracker as `TO` and gives it the owner's ticket
/// `TO-1`, so a lead of `TT` can name a real ticket of another project. Input: the case.
fn other_project_ticket(case: &Case) {
    let folder = case.base.join("other");
    std::fs::create_dir_all(folder.join(crate::projects_home::CRAZYAGENT_DIRECTORY)).unwrap();
    crate::ide::workspace::tests::add_project(&case.db, "other-tools", &folder);
    let tx = Transaction::new_unchecked(&case.db, TransactionBehavior::Immediate).unwrap();
    let other = ProjectId::new("other-tools").unwrap();
    crate::tracker::attach(&tx, &other, &ProjectKey::new("TO").unwrap(), "Other", NOW).unwrap();
    let request = trackwright_core::requests::Request::parse(
        "create",
        br#"{"project": "TO", "title": "Their own work"}"#,
    )
    .unwrap();
    let owner = trackwright_core::model::Actor::new("owner").unwrap();
    let reply = trackwright_core::ops::dispatch(&tx, &crate::tracker::Host, &owner, request);
    assert!(reply.is_ok(), "{}", reply.json());
    tx.commit().unwrap();
}

#[test]
fn a_lead_spawn_with_a_ticket_links_its_task_and_a_refused_ticket_leaves_no_task() {
    let mut case = Case::new();
    case.access = signed_in_codex(&case);
    other_project_ticket(&case);
    let answers = case.served(
        false,
        &[
            tool_call(
                1,
                "ticket_create",
                json!({"project": KEY, "title": "Write the parser"}),
            ),
            tool_call(
                2,
                SPAWN_TOOL,
                json!({"task": "Write parser.rs", "harness": "codex", "ticket": "TT-1"}),
            ),
            tool_call(
                3,
                SPAWN_TOOL,
                json!({"task": "Write lexer.rs", "harness": "codex", "ticket": "TT-9"}),
            ),
            tool_call(
                4,
                SPAWN_TOOL,
                json!({"task": "Write lexer.rs", "harness": "codex", "ticket": "TO-1"}),
            ),
        ],
    );
    let (created, error) = envelope(&answers[0]);
    assert!(!error, "{created}");
    assert_eq!(created["data"]["id"], "TT-1", "{created}");

    let (task, parent): (i64, i64) = case
        .db
        .query_row(
            "SELECT id, parent FROM team_tasks WHERE project=?1 AND prompt='Write parser.rs'",
            [PROJECT],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(parent, AGENT);
    let (queued, error) = text(&answers[1]);
    assert!(!error, "{queued}");
    assert!(
        queued.starts_with(&format!(
            "Task {task} is queued for a codex worker of yours"
        )),
        "{queued}"
    );
    let link: (String, String, String, String) = case
        .db
        .query_row(
            "SELECT p.key, l.kind, l.target, l.created_by FROM tk_links l
             JOIN tk_tickets t ON t.id = l.ticket JOIN tk_projects p ON p.id = t.project",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        link,
        (
            KEY.to_owned(),
            "task".to_owned(),
            task.to_string(),
            DELEGATE_ACTOR.to_owned()
        )
    );

    let (missing, error) = text(&answers[2]);
    assert!(error, "{missing}");
    assert!(missing.starts_with("Not started: "), "{missing}");
    assert!(
        missing.contains(trackwright_core::model::codes::TICKET_UNKNOWN),
        "{missing}"
    );
    assert_eq!(
        text(&answers[3]),
        (
            "Not started: TO-1 is not a ticket of this project (link_cross_project).",
            true
        )
    );
    assert_eq!(
        case.count("SELECT COUNT(*) FROM team_tasks"),
        1,
        "a refused link leaves no task behind"
    );
    assert_eq!(case.count("SELECT COUNT(*) FROM tk_links"), 1);
}

#[test]
fn master_instructions_name_the_ticket_tools_and_the_close_setting() {
    let case = Case::new();
    let rules = crate::setup::pack::TICKET_RULES;
    let shipped = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/pack/rules/tickets.md");
    assert_eq!(std::fs::read_to_string(shipped).unwrap(), rules);
    assert!(!rules.contains("<!--"));
    let offered: Vec<&str> = catalog()
        .iter()
        .filter_map(|spec| tool_name(spec.kind))
        .collect();
    for named in rules.split('`').skip(1).step_by(2) {
        assert!(
            offered.contains(&named)
                || TEAM_TOOLS.contains(&named)
                || [crate::team::tool::SERVER_NAME, "ticket"].contains(&named),
            "the rule names {named}, which no tool offers"
        );
    }
    for tool in [
        "ticket_ready",
        "ticket_claim",
        "ticket_evidence_record",
        SPAWN_TOOL,
    ] {
        assert!(rules.contains(&format!("`{tool}`")), "{tool}");
    }
    assert!(!rules.contains('\u{2014}'));

    let engine = Path::new(ENGINE);
    let base = || crate::launch_context::for_project(&case.db, &project(), Layout::Linked).unwrap();
    let live = base()
        .with_live_team_tool(engine, &case.mailbox, &[])
        .unwrap()
        .with_ticket_rules(&case.db, &project())
        .unwrap();
    let told = live.instructions();
    assert_eq!(told.matches(rules).count(), 1, "{told}");
    assert!(told.contains(&format!("This project's tickets use the key `{KEY}`.")));
    assert!(told.contains("Its close setting is `evidence`:"), "{told}");
    assert!(told.len() <= crate::launch_context::MASTER_INSTRUCTIONS_BYTES_MAX);

    let tx = Transaction::new_unchecked(&case.db, TransactionBehavior::Immediate).unwrap();
    let owner_closes = TicketOverride {
        close: Some(ClosePolicy::Owner),
        ..TicketOverride::default()
    };
    crate::tracker::settings::set(&tx, &Scope::Project(project()), &owner_closes, 0, NOW)
        .unwrap()
        .unwrap();
    tx.commit().unwrap();
    let agent = base()
        .with_agent_team_tool(engine, &case.mailbox, &[])
        .unwrap()
        .with_ticket_rules(&case.db, &project())
        .unwrap();
    let heard = agent.instructions();
    assert_eq!(heard.matches(rules).count(), 1);
    assert!(heard.contains("Its close setting is `owner`:"), "{heard}");
    assert!(!heard.contains("`evidence`:"));

    for close in [
        ClosePolicy::Agents,
        ClosePolicy::Evidence,
        ClosePolicy::Owner,
    ] {
        let text = section(&ProjectKey::new("ABCDEFGH").unwrap(), close);
        assert!(text.contains(&format!("Its close setting is `{close}`:")));
        assert_eq!(text.matches(rules).count(), 1);
        assert!(text.len() <= RULES_BYTES_MAX, "{close}");
    }

    crate::ide::workspace::tests::add_project(&case.db, "bare", &case.base.join("project"));
    let bare = ProjectId::new("bare").unwrap();
    let plain = crate::launch_context::for_project(&case.db, &bare, Layout::Linked)
        .unwrap()
        .with_agent_team_tool(engine, &case.mailbox, &[])
        .unwrap();
    let before = plain.instructions().to_owned();
    let unattached = plain.with_ticket_rules(&case.db, &bare).unwrap();
    assert_eq!(
        unattached.instructions(),
        before,
        "a project without tickets is told nothing about them"
    );
}
