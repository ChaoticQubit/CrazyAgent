use rusqlite::Transaction;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use trackwright_core::catalog;
use trackwright_core::criteria::{self, NewEvidence};
use trackwright_core::events;
use trackwright_core::export::{self, EXPORT_HEADER};
use trackwright_core::host;
use trackwright_core::import::{self, Change, IMPORT_BYTES_MAX, Preview, Unparsed, Why};
use trackwright_core::model::{
    Actor, CheckKind, EvidenceResult, OPEN_TICKETS_CREATED_PER_ACTOR_MAX, OpKind, PolicyKey,
    Priority, ProjectKey, ShortId, TicketKind, TicketStatus, VersionStatus, codes,
};
use trackwright_core::ops;
use trackwright_core::policy;
use trackwright_core::requests::Request;
use trackwright_core::standalone::Standalone;
use trackwright_core::store::{self, NewTicket, Store, StoreError, TicketChange};
use trackwright_core::text::TITLE_BYTES_MAX;

const PROJECT_EXPORT: &str = include_str!("fixtures/export-project.md");
const TICKET_EXPORT: &str = include_str!("fixtures/export-ticket.md");
const ROADMAP: &str = include_str!("fixtures/roadmap.md");
const ROADMAP_AGAIN: &str = include_str!("fixtures/roadmap-v2.md");
const CHECKLIST: &str = include_str!("fixtures/checklist.md");

const PLAN_ROWS: &str = "
INSERT INTO tk_versions(project, name, position, target_day, status, source, version)
    SELECT id, 'V1', 0, '2026-12-01', 'planned', 'test', 1 FROM tk_projects WHERE key = 'CA';
INSERT INTO tk_versions(project, name, position, target_day, status, source, version)
    SELECT id, 'V2', 1, NULL, 'planned', 'test', 1 FROM tk_projects WHERE key = 'CA';
INSERT INTO tk_cycles(project, version_id, number, starts_day, ends_day, status, version)
    SELECT p.id, v.id, 1, '2026-11-02', '2026-11-15', 'active', 1
    FROM tk_projects p JOIN tk_versions v ON v.project = p.id AND v.name = 'V1'
    WHERE p.key = 'CA';
UPDATE tk_tickets SET version_id = (SELECT id FROM tk_versions WHERE name = 'V1'),
    cycle_id = (SELECT id FROM tk_cycles WHERE number = 1) WHERE number IN (1, 3);
UPDATE tk_tickets SET version_id = (SELECT id FROM tk_versions WHERE name = 'V1')
    WHERE number = 2;";

fn opened(dir: &Path) -> Store {
    Store::open(&dir.join("tracker").join("tracker.db")).unwrap()
}

fn actor(name: &str) -> Actor {
    Actor::new(name).unwrap()
}

fn owner() -> Actor {
    actor("owner")
}

fn key() -> ProjectKey {
    ProjectKey::new("CA").unwrap()
}

fn other() -> ProjectKey {
    ProjectKey::new("CB").unwrap()
}

fn id(number: u32) -> ShortId {
    ShortId::new(key(), number).unwrap()
}

fn project(tx: &Transaction<'_>) {
    store::create_project(tx, &owner(), &key(), "CrazyAgent", 1_000).unwrap();
}

fn ticket(
    tx: &Transaction<'_>,
    title: &str,
    kind: TicketKind,
    status: TicketStatus,
    priority: u8,
    body: &str,
    at_ms: i64,
) -> ShortId {
    let new = NewTicket {
        title: title.to_owned(),
        body: body.to_owned(),
        kind,
        status,
        priority: Priority::new(priority).unwrap(),
    };
    store::create_ticket(tx, &owner(), &key(), &new, at_ms)
        .unwrap()
        .short_id
}

fn built(dir: &Path, at_ms: i64) -> Store {
    let mut store = opened(dir);
    let tx = store.transaction().unwrap();
    store::create_project(&tx, &owner(), &key(), "CrazyAgent", at_ms).unwrap();
    let first = ticket(
        &tx,
        "Sign in with a magic link",
        TicketKind::Feature,
        TicketStatus::Todo,
        2,
        "Magic links replace passwords.",
        at_ms,
    );
    let second = ticket(
        &tx,
        "Plan a week on one board",
        TicketKind::Feature,
        TicketStatus::Backlog,
        1,
        "",
        at_ms + 1,
    );
    let third = ticket(
        &tx,
        "Ship the first build",
        TicketKind::Chore,
        TicketStatus::Todo,
        2,
        "",
        at_ms + 2,
    );
    ticket(
        &tx,
        "Fix the *bold* title",
        TicketKind::Bug,
        TicketStatus::Todo,
        0,
        "",
        at_ms + 3,
    );
    tx.execute_batch(PLAN_ROWS).unwrap();
    for text in [
        "Links expire after 15 minutes",
        "The email names the device",
    ] {
        criteria::add_criterion(&tx, &Standalone, &owner(), &first, text, at_ms + 4).unwrap();
    }
    let checks = [
        (1, CheckKind::Test, "auth::expiry"),
        (2, CheckKind::Manual, "email-copy"),
    ];
    for (position, kind, reference) in checks {
        criteria::add_check(
            &tx,
            &Standalone,
            &owner(),
            &first,
            position,
            kind,
            reference,
            at_ms + 5,
        )
        .unwrap();
    }
    let proof = NewEvidence {
        ticket: first.clone(),
        criterion: 1,
        kind: CheckKind::Test,
        check: "auth::expiry".to_owned(),
        result: EvidenceResult::Pass,
        reference: Some("run 41".to_owned()),
    };
    let (_, outcome) =
        criteria::record_evidence(&tx, &Standalone, &owner(), &proof, at_ms + 6).unwrap();
    assert_eq!(outcome.ticket.status, TicketStatus::Todo);
    host::link(
        &tx,
        &Standalone,
        &owner(),
        &first,
        "branch",
        "feat/magic-link",
        at_ms + 7,
    )
    .unwrap();
    let read = store::ticket(&tx, &second).unwrap();
    let assigned = TicketChange {
        assignee: Some(Some(actor("agent:planner"))),
        ..TicketChange::default()
    };
    store::update_ticket(&tx, &owner(), &second, read.version, &assigned, at_ms + 8).unwrap();
    let read = store::ticket(&tx, &third).unwrap();
    policy::close(
        &tx,
        &Standalone,
        &owner(),
        &third,
        read.version,
        TicketStatus::Done,
        Some("shipped"),
        at_ms + 9,
    )
    .unwrap();
    tx.commit().unwrap();
    store
}

fn refused<T: std::fmt::Debug>(result: Result<T, StoreError>) -> &'static str {
    match result {
        Err(StoreError::Refused(refusal)) => refusal.code,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

fn counts(tx: &Transaction<'_>) -> [i64; 3] {
    ["tk_versions", "tk_tickets", "tk_events"].map(|table| {
        tx.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
    })
}

/// Runs one request through the core's dispatch on the standalone host, as MCP and the CLI
/// do. Inputs: the transaction, the actor, the operation and its arguments. Output: the
/// reply envelope.
fn call(tx: &Transaction<'_>, by: &Actor, op: &str, args: Value) -> Value {
    let request = Request::parse(op, args.to_string().as_bytes()).unwrap();
    let reply = ops::dispatch(tx, &Standalone, by, request);
    let envelope: Value = serde_json::from_str(reply.json()).unwrap();
    assert_eq!(envelope["ok"], json!(reply.is_ok()));
    envelope
}

/// The data of a reply that must succeed. Input: the envelope. Output: its data.
fn answered(envelope: &Value) -> &Value {
    assert_eq!(envelope["ok"], json!(true), "{envelope}");
    &envelope["data"]
}

/// The code of a reply that must be refused. Input: the envelope. Output: its code.
fn denied(envelope: &Value) -> &str {
    assert_eq!(envelope["ok"], json!(false), "{envelope}");
    envelope["code"].as_str().unwrap()
}

fn tracker(dir: &Path) -> PathBuf {
    dir.join("tracker")
}

fn written(dir: &Path, name: &str, text: &str) -> PathBuf {
    fs::write(tracker(dir).join(name), text).unwrap();
    PathBuf::from(name)
}

fn changes(preview: &Preview) -> Vec<Change> {
    preview
        .items
        .iter()
        .map(|item| item.change.clone())
        .collect()
}

fn keys(preview: &Preview) -> Vec<String> {
    let keys: Vec<String> = preview.items.iter().map(|item| item.key.clone()).collect();
    for key in &keys {
        assert!(!key.is_empty() && key.len() <= 64, "{key}");
        assert!(
            key.bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._:-".contains(&byte)),
            "{key}"
        );
    }
    let unique: BTreeSet<&String> = keys.iter().collect();
    assert_eq!(unique.len(), keys.len());
    keys
}

fn key_of(preview: &Preview, change: &Change) -> String {
    preview
        .items
        .iter()
        .find(|item| &item.change == change)
        .unwrap_or_else(|| panic!("no item {change:?}"))
        .key
        .clone()
}

fn version_add(name: &str, position: u32, target_day: Option<&str>) -> Change {
    Change::VersionAdd {
        name: name.to_owned(),
        position,
        target_day: target_day.map(str::to_owned),
    }
}

fn ticket_add(version: Option<&str>, kind: TicketKind, title: &str) -> Change {
    Change::TicketAdd {
        version: version.map(str::to_owned),
        kind,
        title: title.to_owned(),
    }
}

fn feature(version: &str, title: &str) -> Change {
    ticket_add(Some(version), TicketKind::Feature, title)
}

fn unparsed(heading: &str, lines: (u32, u32), why: Why, text: &str) -> Unparsed {
    Unparsed {
        heading: heading.to_owned(),
        line_first: lines.0,
        line_last: lines.1,
        why,
        text: text.to_owned(),
    }
}

fn replayed(tx: &Transaction<'_>, project: &ProjectKey) {
    assert_eq!(
        store::state(tx, project).unwrap(),
        events::replay(tx, project).unwrap()
    );
}

#[test]
fn export_is_byte_identical_for_the_same_state() {
    let early = tempfile::tempdir().unwrap();
    let late = tempfile::tempdir().unwrap();
    let mut first = built(early.path(), 1_000);
    let mut second = built(late.path(), 9_000_000);

    let tx = first.transaction().unwrap();
    let project = export::project(&tx, &key()).unwrap();
    assert_eq!(project, export::project(&tx, &key()).unwrap());
    assert_eq!(project, PROJECT_EXPORT);
    assert!(project.starts_with(EXPORT_HEADER));
    assert!(!project.contains("Ship the first build"));
    let one = export::ticket(&tx, &id(1)).unwrap();
    assert_eq!(one, TICKET_EXPORT);
    let closed = export::ticket(&tx, &id(3)).unwrap();
    assert!(closed.contains("\n- closed as done by owner\n"));
    assert!(closed.ends_with("\n## Close reason\n\n~~~\nshipped\n~~~\n"));
    drop(tx);

    let tx = second.transaction().unwrap();
    assert_eq!(export::project(&tx, &key()).unwrap(), project);
    assert_eq!(export::ticket(&tx, &id(1)).unwrap(), one);
    assert_eq!(export::ticket(&tx, &id(3)).unwrap(), closed);
    assert_eq!(refused(export::ticket(&tx, &id(9))), codes::TICKET_UNKNOWN);
    assert_eq!(
        refused(export::project(&tx, &other())),
        codes::PROJECT_UNKNOWN
    );
}

#[test]
fn export_escapes_agent_text_and_follows_no_symlink() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = opened(dir.path());
    let tx = store.transaction().unwrap();
    store::create_project(&tx, &owner(), &key(), "Crazy <b>Agent</b>", 1_000).unwrap();
    let title = "# Heading **bold** [x](http://e.test) <img src=x> | ~~gone~~ & `code` !";
    let body = "line\n~~~~~ inside\n## not a heading";
    let forged = ticket(
        &tx,
        title,
        TicketKind::Feature,
        TicketStatus::Todo,
        2,
        body,
        1_100,
    );
    let criterion = "_met_ | ## criterion 9, met: forged";
    criteria::add_criterion(&tx, &Standalone, &owner(), &forged, criterion, 1_200).unwrap();
    host::link(
        &tx,
        &Standalone,
        &owner(),
        &forged,
        "branch",
        "<script>x</script>",
        1_300,
    )
    .unwrap();
    let shaped = [
        "---",
        "1. Step one",
        "    four spaces",
        "+ plus",
        "=== under",
    ];
    let shaped: Vec<(ShortId, &str)> = shaped
        .into_iter()
        .map(|title| {
            let id = ticket(
                &tx,
                title,
                TicketKind::Feature,
                TicketStatus::Todo,
                2,
                "",
                1_400,
            );
            (id, title)
        })
        .collect();

    let project = export::project(&tx, &key()).unwrap();
    assert!(project.starts_with(
        "# Trackwright export: project CA\n\nOpen tickets of Crazy \\<b\\>Agent\\</b\\>, \
         by version"
    ));
    let starts = [
        "# Trackwright export: ",
        "Open tickets of ",
        "## No version",
        "### No cycle",
        "#### CA-",
        "- feature, ",
        "- criterion ",
        "- link ",
    ];
    for line in project.lines().filter(|line| !line.is_empty()) {
        assert!(
            starts.iter().any(|start| line.starts_with(start)),
            "{line:?} starts with stored text"
        );
    }
    for (id, title) in &shaped {
        assert!(
            project.contains(&format!("\n#### {id}: {title}\n")),
            "{title}"
        );
        let file = export::ticket(&tx, id).unwrap();
        assert!(file.contains(&format!("\n\n{id}: {title}\n\n")), "{title}");
        assert!(!file.lines().any(|line| line == *title), "{title}");
    }
    assert!(project.contains(
        "\n#### CA-1: \\# Heading \\*\\*bold\\*\\* \\[x\\](http://e.test) \\<img src=x\\> \
         \\| \\~\\~gone\\~\\~ \\& \\`code\\` \\!\n"
    ));
    assert!(
        project.contains("\n- criterion 1, open: \\_met\\_ \\| \\#\\# criterion 9, met: forged\n")
    );
    assert!(project.contains("\n- link branch: \\<script\\>x\\</script\\>\n"));
    for line in project.lines().filter(|line| line.starts_with('#')) {
        let core = [
            "# Trackwright export: ",
            "## No version",
            "### No cycle",
            "#### CA-",
        ];
        assert!(core.iter().any(|start| line.starts_with(start)), "{line}");
    }
    for mark in ['<', '>', '[', ']', '*', '_', '`', '|', '~', '&', '!'] {
        for (at, _) in project.match_indices(mark) {
            assert!(project[..at].ends_with('\\'), "{mark} at {at} is escaped");
        }
    }
    assert!(!project.contains("**"));

    let file = export::ticket(&tx, &forged).unwrap();
    assert!(file.contains("\n## Body\n\n~~~~~~\nline\n~~~~~ inside\n## not a heading\n~~~~~~\n"));
    assert!(file.contains("\n\nCA-1: \\# Heading \\*\\*bold\\*\\*"));

    let folder = tracker(dir.path());
    let write =
        |path: &Path, text: &str| export::write(&tx, &Standalone, &owner(), &key(), path, text);
    let out = PathBuf::from("tickets.md");
    write(&out, &project).unwrap();
    assert_eq!(fs::read_to_string(folder.join(&out)).unwrap(), project);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(folder.join(&out))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    write(&out, &file).unwrap();
    assert_eq!(fs::read_to_string(folder.join(&out)).unwrap(), file);
    fs::create_dir(folder.join("exports")).unwrap();
    write(Path::new("exports/tickets.md"), &project).unwrap();
    assert_eq!(
        fs::read_to_string(folder.join("exports").join("tickets.md")).unwrap(),
        project
    );

    let outside = dir.path().join("outside.md");
    fs::write(&outside, "keep me\n").unwrap();
    let escapes = [
        outside.clone(),
        folder.join("inside.md"),
        PathBuf::from("../outside.md"),
        PathBuf::from("../escape.md"),
        PathBuf::from("exports/../../escape.md"),
        PathBuf::from("./here.md"),
        PathBuf::from("missing/tickets.md"),
        PathBuf::from("a/b/c/d/e/f/g/h/tickets.md"),
        PathBuf::new(),
    ];
    for path in &escapes {
        assert_eq!(
            refused(write(path, &project)),
            codes::EXPORT_PATH_REFUSED,
            "{}",
            path.display()
        );
    }
    for name in [
        "CLAUDE.md",
        "claude.md",
        "CLAUDE.local.md",
        "AGENTS.md",
        "exports/agents.md",
        "GEMINI.md",
        "copilot-instructions.md",
    ] {
        assert_eq!(
            refused(write(Path::new(name), &project)),
            codes::EXPORT_PATH_REFUSED,
            "{name}"
        );
        assert!(!folder.join(name).exists(), "{name}");
    }
    assert_eq!(fs::read_to_string(&outside).unwrap(), "keep me\n");
    assert!(!dir.path().join("escape.md").exists());
    assert!(!folder.join("inside.md").exists());
    assert!(!folder.join("here.md").exists());

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let linked = folder.join("linked.md");
        symlink(&outside, &linked).unwrap();
        assert_eq!(
            refused(write(Path::new("linked.md"), &project)),
            codes::EXPORT_PATH_REFUSED
        );
        assert_eq!(fs::read_to_string(&outside).unwrap(), "keep me\n");
        assert!(
            fs::symlink_metadata(&linked)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let real = dir.path().join("real");
        fs::create_dir(&real).unwrap();
        symlink(&real, folder.join("folder")).unwrap();
        assert_eq!(
            refused(write(Path::new("folder/tickets.md"), &project)),
            codes::EXPORT_PATH_REFUSED
        );
        assert!(!real.join("tickets.md").exists());
    }
    let notes = written(dir.path(), "notes.md", "# Notes\n");
    assert_eq!(refused(write(&notes, &project)), codes::EXPORT_PATH_REFUSED);
    assert_eq!(
        fs::read_to_string(folder.join(&notes)).unwrap(),
        "# Notes\n"
    );
    assert_eq!(
        refused(write(Path::new("tickets.txt"), &project)),
        codes::EXPORT_PATH_REFUSED
    );
    fs::create_dir(folder.join("folder.md")).unwrap();
    assert_eq!(
        refused(write(Path::new("folder.md"), &project)),
        codes::EXPORT_PATH_REFUSED
    );
    assert!(!folder.join("tickets.txt").exists());
    for entry in fs::read_dir(&folder).unwrap() {
        let name = entry.unwrap().file_name().into_string().unwrap();
        assert!(!name.ends_with(".tmp"), "{name}");
    }
}

#[test]
fn import_turns_versions_and_scope_items_into_versions_and_backlog_tickets() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = opened(dir.path());
    let roadmap = written(dir.path(), "roadmap.md", ROADMAP);
    let tx = store.transaction().unwrap();
    project(&tx);

    let preview = import::preview(&tx, &Standalone, &owner(), &key(), &roadmap).unwrap();
    assert_eq!(
        changes(&preview),
        vec![
            version_add("V1", 0, Some("2026-12-01")),
            feature("V1", "Sign in with a magic link"),
            feature("V1", "Plan a week on one board"),
            version_add("V2", 1, None),
            feature("V2", "Share a board by link"),
        ]
    );
    assert_eq!(
        preview.unparsed,
        vec![unparsed(
            "Open questions",
            (20, 22),
            Why::NotAVersion,
            "- Which payment provider should V1 use?",
        )]
    );
    let accept = keys(&preview);
    let before = counts(&tx);
    assert_eq!(
        import::preview(&tx, &Standalone, &owner(), &key(), &roadmap).unwrap(),
        preview,
        "a preview is the same for the same file and state"
    );
    assert_eq!(counts(&tx), before, "a preview writes nothing");

    let applied =
        import::apply(&tx, &Standalone, &owner(), &key(), &roadmap, &accept, 2_000).unwrap();
    let versions: Vec<(&str, u32, Option<&str>)> = applied
        .versions
        .iter()
        .map(|version| {
            assert_eq!(version.status, VersionStatus::Planned);
            assert_eq!(version.source, "import");
            assert_eq!(version.version, 1);
            (
                version.name.as_str(),
                version.position,
                version.target_day.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        versions,
        vec![("V1", 0, Some("2026-12-01")), ("V2", 1, None)]
    );
    let tickets: Vec<(String, &str)> = applied
        .tickets
        .iter()
        .map(|ticket| {
            assert_eq!(ticket.status, TicketStatus::Backlog);
            assert_eq!(ticket.kind, TicketKind::Feature);
            assert_eq!(ticket.priority.get(), 2);
            assert_eq!(ticket.created_by, owner());
            (ticket.short_id.to_string(), ticket.title.as_str())
        })
        .collect();
    assert_eq!(
        tickets,
        vec![
            ("CA-1".to_owned(), "Sign in with a magic link"),
            ("CA-2".to_owned(), "Plan a week on one board"),
            ("CA-3".to_owned(), "Share a board by link"),
        ]
    );

    let again = import::preview(&tx, &Standalone, &owner(), &key(), &roadmap).unwrap();
    assert!(again.items.is_empty(), "{:?}", again.items);
    assert_eq!(again.unparsed, preview.unparsed);
    let view = export::project(&tx, &key()).unwrap();
    assert!(view.contains(
        "\n## Version V1 (planned, target 2026-12-01)\n\n### No cycle\n\n\
         #### CA-1: Sign in with a magic link\n"
    ));
    assert!(view.contains(
        "\n## Version V2 (planned, no target)\n\n### No cycle\n\n\
         #### CA-3: Share a board by link\n"
    ));
    replayed(&tx, &key());

    store::create_project(&tx, &owner(), &other(), "Other", 3_000).unwrap();
    let agent = actor("agent:planner");
    let before = counts(&tx);
    assert_eq!(
        refused(import::apply(
            &tx,
            &Standalone,
            &agent,
            &other(),
            &roadmap,
            &accept,
            3_100
        )),
        codes::AGENT_EDIT_REFUSED
    );
    assert_eq!(counts(&tx), before);
    policy::set(
        &tx,
        &Standalone,
        &owner(),
        &other(),
        PolicyKey::AgentEdit,
        "any",
        0,
        3_200,
    )
    .unwrap();
    let theirs = import::preview(&tx, &Standalone, &owner(), &other(), &roadmap).unwrap();
    assert_eq!(
        keys(&theirs),
        accept,
        "keys follow the file, not the project"
    );
    let applied =
        import::apply(&tx, &Standalone, &agent, &other(), &roadmap, &accept, 3_300).unwrap();
    assert_eq!(applied.versions.len(), 2);
    assert_eq!(applied.tickets.len(), 3);
    for ticket in &applied.tickets {
        assert_eq!(ticket.status, TicketStatus::Triage, "create = triage");
        assert_eq!(ticket.created_by, agent);
    }
    replayed(&tx, &other());
}

#[test]
fn reimport_shows_a_diff_and_applies_only_accepted_items() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = opened(dir.path());
    let roadmap = written(dir.path(), "roadmap.md", ROADMAP);
    let tx = store.transaction().unwrap();
    project(&tx);
    let first = import::preview(&tx, &Standalone, &owner(), &key(), &roadmap).unwrap();
    import::apply(
        &tx,
        &Standalone,
        &owner(),
        &key(),
        &roadmap,
        &keys(&first),
        2_000,
    )
    .unwrap();

    fs::write(&roadmap, ROADMAP_AGAIN).unwrap();
    let target = Change::VersionChange {
        name: "V1".to_owned(),
        position: 0,
        target_day: Some("2026-12-15".to_owned()),
        was_position: 0,
        was_target_day: Some("2026-12-01".to_owned()),
    };
    let export_item = feature("V1", "Export a week as Markdown");
    let teams = version_add("V3", 2, None);
    let invite = feature("V3", "Invite a teammate");
    let diff = import::preview(&tx, &Standalone, &owner(), &key(), &roadmap).unwrap();
    assert_eq!(
        changes(&diff),
        vec![
            target.clone(),
            export_item.clone(),
            teams.clone(),
            invite.clone()
        ]
    );
    assert!(diff.unparsed.is_empty(), "{:?}", diff.unparsed);
    keys(&diff);

    let before = counts(&tx);
    let gone = key_of(&first, &version_add("V1", 0, Some("2026-12-01")));
    assert_eq!(
        refused(import::apply(
            &tx,
            &Standalone,
            &owner(),
            &key(),
            &roadmap,
            &[gone],
            3_000
        )),
        codes::IMPORT_ITEM_UNKNOWN
    );
    assert_eq!(
        refused(import::apply(
            &tx,
            &Standalone,
            &owner(),
            &key(),
            &roadmap,
            &[key_of(&diff, &target), key_of(&diff, &invite)],
            3_000
        )),
        codes::IMPORT_ITEM_NEEDS_VERSION
    );
    assert_eq!(counts(&tx), before, "a refused apply writes nothing");

    let accept = [key_of(&diff, &target), key_of(&diff, &export_item)];
    let applied =
        import::apply(&tx, &Standalone, &owner(), &key(), &roadmap, &accept, 3_100).unwrap();
    assert_eq!(applied.versions.len(), 1);
    assert_eq!(applied.versions[0].name, "V1");
    assert_eq!(
        applied.versions[0].target_day.as_deref(),
        Some("2026-12-15")
    );
    assert_eq!(applied.versions[0].version, 2);
    assert_eq!(applied.tickets.len(), 1);
    assert_eq!(applied.tickets[0].short_id, id(4));
    assert_eq!(applied.tickets[0].title, "Export a week as Markdown");

    let rest = import::preview(&tx, &Standalone, &owner(), &key(), &roadmap).unwrap();
    assert_eq!(changes(&rest), vec![teams, invite]);
    assert_eq!(counts(&tx)[..2], [2, 4]);
    let view = export::project(&tx, &key()).unwrap();
    assert!(view.contains("\n## Version V1 (planned, target 2026-12-15)\n"));
    assert!(view.contains("\n#### CA-4: Export a week as Markdown\n"));
    replayed(&tx, &key());
}

#[test]
fn an_unparsed_section_is_returned_not_guessed() {
    let dir = tempfile::tempdir().unwrap();
    let long = format!("- {}", "x".repeat(TITLE_BYTES_MAX + 1));
    let messy = format!(
        "# Roadmap\n\nSome intro the importer cannot place.\n\n## Risks\n\
         - The board may be slow on phones.\n\n## V1: first release\nTarget: next Friday\n\
         Scope:\n- Sign in with a magic link\n{long}\n\n## V1: again\nScope:\n\
         - Plan a week on one board\n"
    );
    let mut store = opened(dir.path());
    let messy = written(dir.path(), "messy.md", &messy);
    let checklist = written(dir.path(), "checklist.md", CHECKLIST);
    let process = written(
        dir.path(),
        "release-process.md",
        "# Release process\n\n> Written by the CrazyAgent engine on 2026-10-27.\n\n\
         ## Branches\nTrunk with short branches and one review.\n",
    );
    let tx = store.transaction().unwrap();
    project(&tx);

    let preview = import::preview(&tx, &Standalone, &owner(), &key(), &messy).unwrap();
    assert_eq!(
        changes(&preview),
        vec![
            version_add("V1", 0, None),
            feature("V1", "Sign in with a magic link")
        ]
    );
    assert_eq!(
        preview.unparsed,
        vec![
            unparsed(
                "",
                (3, 3),
                Why::BeforeFirstSection,
                "Some intro the importer cannot place."
            ),
            unparsed(
                "Risks",
                (5, 6),
                Why::NotAVersion,
                "- The board may be slow on phones."
            ),
            unparsed(
                "V1: first release",
                (9, 9),
                Why::TargetNotADay,
                "Target: next Friday"
            ),
            unparsed("V1: first release", (12, 12), Why::ItemRefused, &long),
            unparsed(
                "V1: again",
                (14, 16),
                Why::SecondSection,
                "Scope:\n- Plan a week on one board"
            ),
        ]
    );

    let preview = import::preview(&tx, &Standalone, &owner(), &key(), &checklist).unwrap();
    let chore = ticket_add(None, TicketKind::Chore, "Run the full suite");
    assert_eq!(changes(&preview), vec![chore]);
    assert_eq!(
        preview.unparsed,
        vec![unparsed(
            "Notes",
            (8, 9),
            Why::NoChecklistItem,
            "Ask the owner before a major version."
        )]
    );
    let applied = import::apply(
        &tx,
        &Standalone,
        &owner(),
        &key(),
        &checklist,
        &keys(&preview),
        2_000,
    )
    .unwrap();
    assert!(applied.versions.is_empty());
    assert_eq!(applied.tickets.len(), 1);
    assert_eq!(applied.tickets[0].kind, TicketKind::Chore);
    assert_eq!(applied.tickets[0].status, TicketStatus::Backlog);
    assert!(
        export::project(&tx, &key())
            .unwrap()
            .contains("\n## No version\n\n### No cycle\n\n#### CA-1: Run the full suite\n")
    );

    let preview = import::preview(&tx, &Standalone, &owner(), &key(), &process).unwrap();
    assert!(preview.items.is_empty(), "{:?}", preview.items);
    assert_eq!(
        preview.unparsed,
        vec![unparsed(
            "Branches",
            (5, 6),
            Why::NoChecklistItem,
            "Trunk with short branches and one review."
        )]
    );
    replayed(&tx, &key());
}

#[test]
fn import_refuses_a_link_and_a_file_over_the_bound() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = opened(dir.path());
    let tx = store.transaction().unwrap();
    project(&tx);
    let roadmap = written(dir.path(), "roadmap.md", ROADMAP);
    let accept = keys(&import::preview(&tx, &Standalone, &owner(), &key(), &roadmap).unwrap());
    let before = counts(&tx);
    let apply = |path: &Path| {
        refused(import::apply(
            &tx,
            &Standalone,
            &owner(),
            &key(),
            path,
            &accept,
            2_000,
        ))
    };

    #[cfg(unix)]
    {
        let folder = tracker(dir.path());
        let linked = PathBuf::from("linked.md");
        std::os::unix::fs::symlink(folder.join(&roadmap), folder.join(&linked)).unwrap();
        assert_eq!(
            refused(import::preview(&tx, &Standalone, &owner(), &key(), &linked)),
            codes::IMPORT_PATH_REFUSED
        );
        assert_eq!(apply(&linked), codes::IMPORT_PATH_REFUSED);
        let real = dir.path().join("real");
        fs::create_dir(&real).unwrap();
        fs::write(real.join("roadmap.md"), ROADMAP).unwrap();
        std::os::unix::fs::symlink(&real, folder.join("plans")).unwrap();
        let through = Path::new("plans/roadmap.md");
        assert_eq!(
            refused(import::preview(&tx, &Standalone, &owner(), &key(), through)),
            codes::IMPORT_PATH_REFUSED
        );
        assert_eq!(apply(through), codes::IMPORT_PATH_REFUSED);
    }

    let outside = dir.path().join("outside.md");
    fs::write(&outside, ROADMAP).unwrap();
    for path in [
        outside.clone(),
        tracker(dir.path()).join(&roadmap),
        PathBuf::from("../outside.md"),
        PathBuf::from("./roadmap.md"),
    ] {
        assert_eq!(
            refused(import::preview(&tx, &Standalone, &owner(), &key(), &path)),
            codes::IMPORT_PATH_REFUSED,
            "{}",
            path.display()
        );
        assert_eq!(
            apply(&path),
            codes::IMPORT_PATH_REFUSED,
            "{}",
            path.display()
        );
    }

    let head = "# Roadmap\n\n## V1\n";
    let full = format!("{head}{}", "x".repeat(IMPORT_BYTES_MAX - head.len()));
    assert_eq!(full.len(), IMPORT_BYTES_MAX);
    let at_bound = written(dir.path(), "full.md", &full);
    assert_eq!(
        changes(&import::preview(&tx, &Standalone, &owner(), &key(), &at_bound).unwrap()),
        vec![version_add("V1", 0, None)]
    );
    let over = written(dir.path(), "over.md", &format!("{full}x"));
    assert_eq!(
        refused(import::preview(&tx, &Standalone, &owner(), &key(), &over)),
        codes::IMPORT_TOO_LARGE
    );
    assert_eq!(apply(&over), codes::IMPORT_TOO_LARGE);

    let folder = PathBuf::from("folder.md");
    fs::create_dir(tracker(dir.path()).join(&folder)).unwrap();
    let text = written(dir.path(), "roadmap.txt", ROADMAP);
    let missing = PathBuf::from("missing.md");
    for path in [&folder, &text, &missing] {
        assert_eq!(
            refused(import::preview(&tx, &Standalone, &owner(), &key(), path)),
            codes::IMPORT_PATH_REFUSED,
            "{}",
            path.display()
        );
    }

    let secret = written(dir.path(), "notes.md", "# Notes\nhunter2 is the password\n");
    match import::preview(&tx, &Standalone, &owner(), &key(), &secret) {
        Err(StoreError::Refused(refusal)) => {
            assert_eq!(refusal.code, codes::IMPORT_FORMAT_UNKNOWN);
            assert!(!refusal.message.contains("hunter2"), "{}", refusal.message);
            assert!(!refusal.message.contains("Notes"), "{}", refusal.message);
        }
        other => panic!("expected import_format_unknown, got {other:?}"),
    }
    let binary = PathBuf::from("binary.md");
    fs::write(
        tracker(dir.path()).join(&binary),
        [b'#', b' ', 0xff, 0xfe, b'\n'],
    )
    .unwrap();
    assert_eq!(
        refused(import::preview(&tx, &Standalone, &owner(), &key(), &binary)),
        codes::IMPORT_NOT_TEXT
    );
    let hidden = written(
        dir.path(),
        "hidden.md",
        "# Roadmap\n\n## V1\nScope:\n- Sign in \u{202e}gnihton\n",
    );
    assert_eq!(
        refused(import::preview(&tx, &Standalone, &owner(), &key(), &hidden)),
        codes::TEXT_HIDDEN_CHARACTERS
    );
    assert_eq!(apply(&hidden), codes::TEXT_HIDDEN_CHARACTERS);
    assert_eq!(counts(&tx), before, "a refused import writes nothing");
}

#[test]
fn import_counts_its_tickets_against_the_actors_open_ticket_bound() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = opened(dir.path());
    let tx = store.transaction().unwrap();
    project(&tx);
    policy::set(
        &tx,
        &Standalone,
        &owner(),
        &key(),
        PolicyKey::AgentEdit,
        "any",
        0,
        1_050,
    )
    .unwrap();
    let agent = actor("agent:planner");
    let bound = OPEN_TICKETS_CREATED_PER_ACTOR_MAX;
    let scope: String = (1..=bound + 1)
        .map(|number| format!("- Item {number}\n"))
        .collect();
    let roadmap = written(
        dir.path(),
        "roadmap.md",
        &format!("# Roadmap\n\n## V1: first release\nScope:\n{scope}"),
    );
    let preview = import::preview(&tx, &Standalone, &agent, &key(), &roadmap).unwrap();
    let all = keys(&preview);
    assert_eq!(all.len(), bound + 2, "one version and every scope item");
    let before = counts(&tx);
    let apply = |who: &Actor, accept: &[String], at_ms: i64| {
        import::apply(&tx, &Standalone, who, &key(), &roadmap, accept, at_ms)
    };

    assert_eq!(
        refused(apply(&agent, &all, 2_000)),
        codes::OPEN_TICKETS_FULL
    );
    assert_eq!(counts(&tx), before, "a refused import writes nothing");

    let applied = apply(&agent, &all[..bound + 1], 2_100).unwrap();
    assert_eq!(applied.versions.len(), 1);
    assert_eq!(applied.tickets.len(), bound);
    let rest = import::preview(&tx, &Standalone, &agent, &key(), &roadmap).unwrap();
    let last = keys(&rest);
    assert_eq!(
        changes(&rest),
        vec![feature("V1", &format!("Item {}", bound + 1))]
    );
    let full = counts(&tx);
    assert_eq!(
        refused(apply(&agent, &last, 2_200)),
        codes::OPEN_TICKETS_FULL
    );
    assert_eq!(counts(&tx), full);

    let by_owner = apply(&owner(), &last, 2_300).unwrap();
    assert_eq!(by_owner.tickets.len(), 1);
    assert_eq!(by_owner.tickets[0].created_by, owner());

    for number in 1..bound {
        ticket(
            &tx,
            &format!("Owner work {number}"),
            TicketKind::Feature,
            TicketStatus::Todo,
            2,
            "",
            2_400,
        );
    }
    let more = written(
        dir.path(),
        "more.md",
        &format!(
            "# Roadmap\n\n## V1: first release\nScope:\n- Item {}\n",
            bound + 2
        ),
    );
    let extra = import::preview(&tx, &Standalone, &owner(), &key(), &more).unwrap();
    assert_eq!(
        changes(&extra),
        vec![feature("V1", &format!("Item {}", bound + 2))]
    );
    let held = counts(&tx);
    assert_eq!(
        refused(import::apply(
            &tx,
            &Standalone,
            &owner(),
            &key(),
            &more,
            &keys(&extra),
            2_500
        )),
        codes::OPEN_TICKETS_FULL,
        "the owner has no exemption"
    );
    assert_eq!(counts(&tx), held);
    replayed(&tx, &key());
}

#[test]
fn export_and_import_answer_through_dispatch_inside_the_hosts_folder() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = opened(dir.path());
    let roadmap = written(dir.path(), "roadmap.md", ROADMAP);
    let path = roadmap.to_str().unwrap();
    let tx = store.transaction().unwrap();
    project(&tx);
    ticket(
        &tx,
        "Ship <it> | now",
        TicketKind::Feature,
        TicketStatus::Todo,
        2,
        "",
        1_100,
    );
    let me = owner();

    let text = export::project(&tx, &key()).unwrap();
    let whole = call(&tx, &me, "export", json!({"project": "CA"}));
    assert_eq!(answered(&whole), &json!({"text": text}));
    let one = call(&tx, &me, "export", json!({"project": "CA", "id": "CA-1"}));
    assert_eq!(
        answered(&one),
        &json!({"text": export::ticket(&tx, &id(1)).unwrap()})
    );
    let to_file = call(
        &tx,
        &me,
        "export",
        json!({"project": "CA", "path": "open.md"}),
    );
    assert_eq!(
        answered(&to_file),
        &json!({"path": "open.md", "bytes": text.len()})
    );
    assert_eq!(
        fs::read_to_string(tracker(dir.path()).join("open.md")).unwrap(),
        text
    );
    for refused_path in ["../open.md", "/tmp/open.md", "CLAUDE.md", "notes.txt"] {
        let refused = call(
            &tx,
            &me,
            "export",
            json!({"project": "CA", "path": refused_path}),
        );
        assert_eq!(denied(&refused), "export_path_refused", "{refused_path}");
    }
    assert!(!dir.path().join("open.md").exists());
    assert!(!tracker(dir.path()).join("CLAUDE.md").exists());

    let expected = import::preview(&tx, &Standalone, &me, &key(), &roadmap).unwrap();
    let previewed = call(
        &tx,
        &me,
        "import_preview",
        json!({"project": "CA", "path": path}),
    );
    assert_eq!(
        answered(&previewed),
        &serde_json::to_value(&expected).unwrap()
    );
    assert_eq!(
        denied(&call(
            &tx,
            &me,
            "import_preview",
            json!({"project": "CA", "path": "../roadmap.md"}),
        )),
        "import_path_refused"
    );
    let accept = keys(&expected);
    let by_agent = json!({"project": "CA", "path": path, "accept": accept.clone()});
    assert_eq!(
        denied(&call(&tx, &actor("agent:w1"), "import_apply", by_agent)),
        "agent_edit_refused"
    );
    let applied = call(
        &tx,
        &me,
        "import_apply",
        json!({"project": "CA", "path": path, "accept": accept}),
    );
    let data = answered(&applied);
    assert_eq!(data["versions"], json!(["V1", "V2"]));
    assert_eq!(data["tickets"], json!(["CA-2", "CA-3", "CA-4"]));
    let board = call(&tx, &me, "roadmap", json!({"project": "CA"}));
    assert_eq!(answered(&board)["versions"][0]["name"], "V1");
    assert_eq!(answered(&board)["versions"][0]["target_day"], "2026-12-01");
    let again = import::preview(&tx, &Standalone, &me, &key(), &roadmap).unwrap();
    assert!(again.items.is_empty(), "{:?}", again.items);
    assert_eq!(
        events::replay(&tx, &key()).unwrap(),
        store::state(&tx, &key()).unwrap()
    );
    tx.commit().unwrap();
}

#[test]
fn an_export_or_preview_larger_than_one_reply_is_refused_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = opened(dir.path());
    let scope: String = (1..=120)
        .map(|item| format!("- Scope item {item:03} for the first release of the board\n"))
        .collect();
    let long = written(
        dir.path(),
        "long.md",
        &format!("# Roadmap\n\n## V1: first release\nScope:\n{scope}"),
    );
    let tx = store.transaction().unwrap();
    project(&tx);
    for number in 1..=40 {
        let title = format!("{number:02} {}", "w".repeat(150));
        ticket(
            &tx,
            &title,
            TicketKind::Feature,
            TicketStatus::Todo,
            2,
            "",
            1_100,
        );
    }
    let me = owner();
    let text = export::project(&tx, &key()).unwrap();
    assert!(text.len() > 8 * 1024);

    let whole = call(&tx, &me, "export", json!({"project": "CA"}));
    assert_eq!(denied(&whole), "export_too_large");
    assert!(
        whole["message"].as_str().unwrap().contains("path"),
        "{whole}"
    );
    let to_file = call(
        &tx,
        &me,
        "export",
        json!({"project": "CA", "path": "open.md"}),
    );
    assert_eq!(answered(&to_file)["bytes"], text.len());

    let before = counts(&tx);
    let previewed = call(
        &tx,
        &me,
        "import_preview",
        json!({"project": "CA", "path": long.to_str().unwrap()}),
    );
    assert_eq!(denied(&previewed), "import_too_large");
    assert_eq!(counts(&tx), before);
    let preview = import::preview(&tx, &Standalone, &me, &key(), &long).unwrap();
    assert_eq!(preview.items.len(), 121);
    tx.commit().unwrap();
}

#[test]
fn the_largest_import_apply_reply_fits_the_default_reply_bound() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = opened(dir.path());
    let mut roadmap = String::from("# Roadmap\n\n");
    for section in 0..64_usize {
        roadmap.push_str(&format!(
            "## V1000.1000.{}: release {section}\nTarget: 2026-12-01\n",
            1000 + section
        ));
        if section < OPEN_TICKETS_CREATED_PER_ACTOR_MAX {
            roadmap.push_str(&format!("Scope:\n- Item {section:02} of the plan\n"));
        }
        roadmap.push('\n');
    }
    let path = written(dir.path(), "roadmap.md", &roadmap);
    let tx = store.transaction().unwrap();
    project(&tx);
    let preview = import::preview(&tx, &Standalone, &owner(), &key(), &path).unwrap();
    assert_eq!(preview.items.len(), 64 + OPEN_TICKETS_CREATED_PER_ACTOR_MAX);
    assert!(preview.unparsed.is_empty(), "{:?}", preview.unparsed);

    let applied = call(
        &tx,
        &owner(),
        "import_apply",
        json!({"project": "CA", "path": path.to_str().unwrap(), "accept": keys(&preview)}),
    );
    let data = answered(&applied);
    let versions = data["versions"].as_array().unwrap();
    assert_eq!(versions.len(), 64);
    assert_eq!(versions[0], "V1000.1000.1000");
    assert_eq!(versions[63], "V1000.1000.1063");
    let tickets = data["tickets"].as_array().unwrap();
    assert_eq!(tickets.len(), OPEN_TICKETS_CREATED_PER_ACTOR_MAX);
    assert_eq!(tickets[0], "CA-1");
    assert!(
        applied.to_string().len() <= 8 * 1024,
        "{}",
        applied.to_string().len()
    );
    tx.commit().unwrap();
}

#[test]
fn an_import_preview_is_a_read_and_takes_no_request_id() {
    assert!(!OpKind::ImportPreview.is_write());
    assert!(!OpKind::Export.is_write());
    assert!(OpKind::ImportApply.is_write());
    let spec = catalog::spec(OpKind::ImportPreview);
    assert!(spec.fields.iter().all(|field| field.name != "request_id"));
    let args = json!({"project": "CA", "path": "roadmap.md", "request_id": "r-1"});
    let refused = Request::parse("import_preview", args.to_string().as_bytes()).unwrap_err();
    assert_eq!(refused.code, codes::UNKNOWN_FIELD);
}
