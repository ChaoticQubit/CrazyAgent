use super::*;
use crate::config::{EffectiveConfig, ModelAccess, resolve_config_bytes};
use crate::memory::library::{AGENT_SUGGESTIONS_MAX, AGENT_VERSIONS_MAX, NOTE_BYTES_MAX};
use crate::memory::{
    HumanReview, KnowledgeAction, KnowledgeEntry, KnowledgeStatus, ReviewDecision,
};
use crate::project_setup::{HarnessSlot, StoredSetup, TeamPlan};
use crate::protocol::{Outcome, ProjectId, RequestId};
use crate::team::master::{Answering, serve_calls};
use crate::team::runtime::ConfigSource;
use rusqlite::{Connection, TransactionBehavior, params};
use std::io::Cursor;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const PROJECT: &str = "stale-memory";
const MASTER: i64 = 41;

struct Fixture {
    _root: tempfile::TempDir,
    base: PathBuf,
    mailbox: PathBuf,
    connection: Connection,
    setup: StoredSetup,
    config: EffectiveConfig,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-stale-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let base = root.path().canonicalize().unwrap();
        let connection = Connection::open(base.join("engine.db")).unwrap();
        crate::storage::initialize_authority_schemas(&connection).unwrap();
        connection
            .execute(
                "INSERT INTO team_live_masters(project,live,agent) VALUES (?1,1,?2)",
                params![PROJECT, MASTER],
            )
            .unwrap();
        let folder = base.join("project");
        std::fs::create_dir(&folder).unwrap();
        let mailbox = base.join(MAILBOX_FOLDER);
        std::fs::create_dir(&mailbox).unwrap();
        let setup = StoredSetup {
            project_id: ProjectId::new(PROJECT).unwrap(),
            name: "Stale".into(),
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
                    agents: 1,
                    tools: true,
                }],
                min_agents: 1,
                max_agents: 2,
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
            connection,
            setup,
            config: resolve_config_bytes("", "").unwrap(),
        }
    }

    fn call(&mut self, name: &str, arguments: Value) -> (String, bool) {
        let line = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": name, "arguments": arguments}});
        let mut input = serde_json::to_vec(&line).unwrap();
        input.push(b'\n');
        let served = self.mailbox.clone();
        let server = std::thread::spawn(move || {
            let mut output = Vec::new();
            serve_live(Cursor::new(input), &mut output, &served).unwrap();
            output
        });
        let access = ModelAccess::default();
        let setup = self.setup.clone();
        let landing = self.base.clone();
        let settings = self.config.clone();
        let config: ConfigSource = Box::new(move || Some(settings.clone()));
        let answering = Answering {
            access: &access,
            stored: &setup,
            landing: &landing,
            events: None,
            host: None,
            config: Some(&config),
        };
        let deadline = Instant::now() + Duration::from_secs(30);
        while !server.is_finished() {
            assert!(
                Instant::now() < deadline,
                "the engine never answered the call"
            );
            serve_calls(&mut self.connection, &answering, &self.mailbox).unwrap();
            std::thread::sleep(Duration::from_millis(20));
        }
        let output = server.join().unwrap();
        let answers: Vec<Value> = output
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice(line).unwrap())
            .collect();
        assert_eq!(answers.len(), 1, "{answers:#?}");
        (
            answers[0]["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .to_owned(),
            answers[0]["result"]["isError"].as_bool().unwrap(),
        )
    }

    fn memory(&mut self) -> KnowledgeEntry {
        let tx = self.connection.transaction().unwrap();
        let inbox = KnowledgeAction::Owner {
            action: HumanReview::Inbox {
                project_id: ProjectId::new(PROJECT).unwrap(),
                after: None,
            },
        };
        let outcome = crate::memory::execute(&tx, &self.config, &inbox, 1).unwrap();
        let Outcome::Knowledge { entries } = outcome else {
            panic!("{outcome:?}");
        };
        assert_eq!(entries.len(), 1, "{entries:#?}");
        entries[0].clone()
    }

    fn owner_edits(&mut self, entry: &RequestId, version: u64, text: &str) {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let review = KnowledgeAction::Owner {
            action: HumanReview::Review {
                entry_id: entry.clone(),
                version,
                decision: ReviewDecision::Edit { text: text.into() },
            },
        };
        assert_eq!(
            crate::memory::execute(&tx, &self.config, &review, 1).unwrap(),
            Outcome::Applied
        );
        tx.commit().unwrap();
    }

    fn review(&mut self, entry: &RequestId, decision: ReviewDecision) -> Outcome {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let review = KnowledgeAction::Owner {
            action: HumanReview::Review {
                entry_id: entry.clone(),
                version: 1,
                decision,
            },
        };
        let outcome = crate::memory::execute(&tx, &self.config, &review, 1)
            .unwrap_or_else(|problem| panic!("the owner review failed: {}", problem.0));
        tx.commit().unwrap();
        outcome
    }

    fn entry(&self, entry: &RequestId) -> KnowledgeEntry {
        let body: String = self
            .connection
            .query_row(
                "SELECT e.body FROM knowledge_entries e JOIN knowledge_heads h
                 ON e.id=h.id AND e.version=h.version WHERE h.id=?1",
                [entry.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        serde_json::from_str(&body).unwrap()
    }

    fn rows(&self, table: &str) -> usize {
        let count: i64 = self
            .connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        usize::try_from(count).unwrap()
    }
}

#[test]
fn agent_stale_memory_write_gets_the_reread_instruction() {
    let mut fixture = Fixture::new();
    let (listed, error) = fixture.call(MEMORIES_TOOL, json!({}));
    assert!(!error, "{listed}");
    assert!(listed.contains("no memories"), "{listed}");

    let first = "Dinners are planned on Sundays.";
    let (saved, error) = fixture.call(REMEMBER_TOOL, json!({"text": first}));
    assert!(!error, "{saved}");
    let opened = fixture.memory();
    assert_eq!(
        (opened.version, opened.text.as_str(), &opened.status),
        (1, first, &KnowledgeStatus::Provisional),
        "a new agent memory is a suggestion the owner still reviews"
    );
    assert!(!opened.human_reviewed);
    assert!(
        saved.contains(opened.id.as_str()) && saved.contains("version 1"),
        "{saved}"
    );
    let (listed, error) = fixture.call(MEMORIES_TOOL, json!({}));
    assert!(!error, "{listed}");
    assert!(
        listed.contains(opened.id.as_str())
            && listed.contains("version 1")
            && listed.contains(first),
        "the agent reads each memory with the version it names: {listed}"
    );

    let second = "Dinners are planned on Sundays and Wednesdays.";
    let (saved, error) = fixture.call(
        REMEMBER_TOOL,
        json!({"memory": opened.id.as_str(), "version": 1, "text": second}),
    );
    assert!(!error, "{saved}");
    assert_eq!(fixture.memory().version, 2);

    let late = "Dinners are planned on Fridays.";
    let (stale, error) = fixture.call(
        REMEMBER_TOOL,
        json!({"memory": opened.id.as_str(), "version": 1, "text": late}),
    );
    assert!(error, "{stale}");
    assert!(stale.starts_with("Stale: re-read and redo."), "{stale}");
    assert!(
        stale.contains("version 2") && stale.contains("team-41"),
        "the stale reply names the current version and who wrote it: {stale}"
    );
    let current = fixture.memory();
    assert_eq!(
        (current.version, current.text.as_str()),
        (2, second),
        "the stale write changed nothing"
    );
    let (forced, error) = fixture.call(
        REMEMBER_TOOL,
        json!({"memory": opened.id.as_str(), "version": 1, "text": late, "force": true}),
    );
    assert!(
        error && forced.starts_with("Stale: re-read and redo."),
        "an agent has no way to force a stale write: {forced}"
    );
    assert_eq!(fixture.memory(), current);

    let (listed, _) = fixture.call(MEMORIES_TOOL, json!({}));
    assert!(
        listed.contains("version 2") && listed.contains(second),
        "{listed}"
    );
    let (redone, error) = fixture.call(
        REMEMBER_TOOL,
        json!({"memory": opened.id.as_str(), "version": 2, "text": late}),
    );
    assert!(!error, "the redo on the re-read version lands: {redone}");
    let redone = fixture.memory();
    assert_eq!(
        (
            redone.version,
            redone.text.as_str(),
            &redone.status,
            redone.human_reviewed
        ),
        (3, late, &KnowledgeStatus::Provisional, false)
    );

    let owner = "Dinners are planned on Saturdays.";
    fixture.owner_edits(&redone.id, 3, owner);
    let (stale, error) = fixture.call(
        REMEMBER_TOOL,
        json!({"memory": opened.id.as_str(), "version": 3, "text": late}),
    );
    assert!(
        error && stale.starts_with("Stale: re-read and redo."),
        "{stale}"
    );
    assert!(
        stale.contains("version 4") && stale.contains("the owner"),
        "{stale}"
    );
    let (reviewed, error) = fixture.call(
        REMEMBER_TOOL,
        json!({"memory": opened.id.as_str(), "version": 4, "text": late}),
    );
    assert!(error, "owner edits win over agents: {reviewed}");
    assert!(
        reviewed.contains("owner reviewed") && reviewed.contains("remember"),
        "{reviewed}"
    );
    let held = fixture.memory();
    assert_eq!(
        (
            held.version,
            held.text.as_str(),
            &held.status,
            held.human_reviewed
        ),
        (4, owner, &KnowledgeStatus::Confirmed, true)
    );
    let (listed, _) = fixture.call(MEMORIES_TOOL, json!({}));
    assert!(
        listed.contains("version 4") && listed.contains("reviewed by the owner"),
        "{listed}"
    );

    for refused in [
        json!({"memory": opened.id.as_str(), "text": late}),
        json!({"version": 4, "text": late}),
        json!({"memory": opened.id.as_str(), "version": 0, "text": late}),
        json!({"memory": opened.id.as_str(), "version": "4", "text": late}),
        json!({"memory": "", "version": 4, "text": late}),
        json!({"text": "x".repeat(NOTE_BYTES_MAX + 1)}),
        json!({"text": " "}),
    ] {
        let (text, error) = fixture.call(REMEMBER_TOOL, refused.clone());
        assert!(error && text.starts_with("Not saved"), "{refused}: {text}");
    }
    let (missing, error) = fixture.call(
        REMEMBER_TOOL,
        json!({"memory": "agent-0000", "version": 1, "text": late}),
    );
    assert!(error && missing.starts_with("Not saved"), "{missing}");
    assert_eq!(
        fixture.memory(),
        held,
        "no refused write changed the memory"
    );
}

#[test]
fn agent_memory_writes_stop_at_their_bound_and_the_owner_still_reviews_a_full_store() {
    let mut fixture = Fixture::new();
    let mut suggested = Vec::new();
    for index in 0..AGENT_SUGGESTIONS_MAX {
        let (saved, error) = fixture.call(
            REMEMBER_TOOL,
            json!({"text": format!("Suggested memory number {index}.")}),
        );
        assert!(!error, "{saved}");
        let id = saved.split_whitespace().nth(2).unwrap();
        suggested.push(RequestId::new(id).unwrap());
    }
    let (full, error) = fixture.call(REMEMBER_TOOL, json!({"text": "One suggestion too many."}));
    assert!(error, "{full}");
    assert!(
        full.starts_with("Not saved")
            && full.contains(&format!("{AGENT_SUGGESTIONS_MAX} suggested memories")),
        "past the bound the agent is told why: {full}"
    );
    assert_eq!(
        (
            fixture.rows("knowledge_heads"),
            fixture.rows("knowledge_activity")
        ),
        (AGENT_SUGGESTIONS_MAX, AGENT_SUGGESTIONS_MAX),
        "a suggestion past the bound writes nothing"
    );

    let first = suggested[0].clone();
    for version in 1..AGENT_VERSIONS_MAX {
        let (saved, error) = fixture.call(
            REMEMBER_TOOL,
            json!({"memory": first.as_str(), "version": version,
                "text": format!("Changed memory at version {}.", version + 1)}),
        );
        assert!(
            !error,
            "a suggestion changes while under its bound: {saved}"
        );
    }
    let (spent, error) = fixture.call(
        REMEMBER_TOOL,
        json!({"memory": first.as_str(), "version": AGENT_VERSIONS_MAX, "text": "One change too many."}),
    );
    assert!(error, "{spent}");
    assert!(
        spent.starts_with("Not saved") && spent.contains(&format!("{AGENT_VERSIONS_MAX} versions")),
        "{spent}"
    );
    let kept = fixture.entry(&first);
    assert_eq!(
        (kept.version, kept.text),
        (
            AGENT_VERSIONS_MAX,
            format!("Changed memory at version {AGENT_VERSIONS_MAX}.")
        ),
        "a change past the bound changes nothing"
    );

    assert_eq!(
        fixture.review(&suggested[1], ReviewDecision::Reject),
        Outcome::Applied
    );
    let (freed, error) = fixture.call(
        REMEMBER_TOOL,
        json!({"text": "A suggestion after the owner rejected one."}),
    );
    assert!(!error, "an owner review frees a place: {freed}");

    let rows = fixture.rows("knowledge_entries");
    fixture.config.settings.knowledge.records_max = rows;
    let (stored, error) = fixture.call(
        REMEMBER_TOOL,
        json!({"memory": suggested[2].as_str(), "version": 1, "text": "A change into a full store."}),
    );
    assert!(error, "{stored}");
    assert!(
        stored.starts_with("Not saved") && stored.contains("memory is full"),
        "{stored}"
    );
    assert_eq!(fixture.rows("knowledge_entries"), rows);

    for (memory, decision, status) in [
        (
            &suggested[2],
            ReviewDecision::Reject,
            KnowledgeStatus::Rejected,
        ),
        (
            &suggested[3],
            ReviewDecision::Remove,
            KnowledgeStatus::Removed,
        ),
        (
            &suggested[4],
            ReviewDecision::Keep,
            KnowledgeStatus::Confirmed,
        ),
        (
            &suggested[5],
            ReviewDecision::Edit {
                text: "The owner's wording.".into(),
            },
            KnowledgeStatus::Confirmed,
        ),
    ] {
        assert_eq!(
            fixture.review(memory, decision),
            Outcome::Applied,
            "the owner reviews a store the agents filled"
        );
        let reviewed = fixture.entry(memory);
        assert_eq!(
            (reviewed.version, reviewed.status, reviewed.human_reviewed),
            (2, status, true)
        );
    }
    assert_eq!(fixture.rows("knowledge_entries"), rows + 4);
    let (still, error) = fixture.call(
        REMEMBER_TOOL,
        json!({"memory": suggested[6].as_str(), "version": 1, "text": "Still a full store."}),
    );
    assert!(
        error && still.contains("memory is full"),
        "owner reviews past the bound leave no room to agents: {still}"
    );
}
