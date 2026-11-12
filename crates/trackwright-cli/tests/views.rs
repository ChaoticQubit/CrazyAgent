use ratatui::backend::TestBackend;
use ratatui::{Frame, Terminal};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use trackwright_cli::tui::Local;
use trackwright_cli::views::{Client, Key, Page, Theme, Views};
use trackwright_core::model::{Actor, OpKind, ProjectKey};

const LONG_TITLE: &str =
    "Board view moves a ticket between columns and keeps the version it read for the stale check";

struct Recorder<'a> {
    client: &'a mut Local,
    sent: Vec<(OpKind, Value)>,
    received: Vec<Value>,
}

impl<'a> Recorder<'a> {
    fn new(client: &'a mut Local) -> Self {
        Self {
            client,
            sent: Vec::new(),
            received: Vec::new(),
        }
    }
}

impl Client for Recorder<'_> {
    fn call(&mut self, op: OpKind, input: Value) -> Value {
        self.sent.push((op, input.clone()));
        let reply = self.client.call(op, input);
        self.received.push(reply.clone());
        reply
    }
}

fn actor(text: &str) -> Actor {
    Actor::new(text).unwrap()
}

fn key() -> ProjectKey {
    ProjectKey::new("TW").unwrap()
}

fn database() -> (tempfile::TempDir, PathBuf) {
    let folder = tempfile::Builder::new()
        .prefix("tw-views-")
        .tempdir()
        .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(folder.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let path = folder.path().join("tracker.db");
    let mut store = trackwright_core::store::Store::open(&path).unwrap();
    let tx = store.transaction().unwrap();
    trackwright_core::store::create_project(&tx, &actor("owner"), &key(), "Trackwright", 1)
        .unwrap();
    tx.commit().unwrap();
    (folder, path)
}

fn data(client: &mut dyn Client, op: OpKind, input: Value) -> Value {
    let reply = client.call(op, input.clone());
    assert_eq!(
        reply["ok"],
        json!(true),
        "{} {input} replied {reply}",
        op.as_str()
    );
    reply["data"].clone()
}

fn version(client: &mut dyn Client, id: &str) -> i64 {
    data(client, OpKind::Show, json!({ "id": id }))["ticket"]["version"]
        .as_i64()
        .unwrap()
}

fn write(client: &mut dyn Client, op: OpKind, mut input: Value) -> Value {
    let id = input["id"].as_str().unwrap().to_owned();
    input["version"] = json!(version(client, &id));
    data(client, op, input)
}

fn create(client: &mut dyn Client, title: &str, kind: &str, priority: u8, release: &str) {
    data(
        client,
        OpKind::Create,
        json!({
            "project": "TW",
            "title": title,
            "body": "",
            "kind": kind,
            "priority": priority,
            "release": release,
        }),
    );
}

fn move_to(client: &mut dyn Client, id: &str, status: &str) {
    write(client, OpKind::Move, json!({ "id": id, "status": status }));
}

fn claim(path: &Path, who: &str, id: &str) {
    let mut agent = Local::open(path, actor(who)).unwrap();
    write(&mut agent, OpKind::Claim, json!({ "id": id }));
}

fn seed(path: &Path) -> Local {
    let mut owner = Local::open(path, actor("owner")).unwrap();
    let client: &mut dyn Client = &mut owner;
    data(
        client,
        OpKind::PolicySet,
        json!({ "project": "TW", "key": "close", "value": "owner" }),
    );
    data(
        client,
        OpKind::VersionUpsert,
        json!({
            "project": "TW",
            "name": "0.1 Core",
            "position": 1,
            "target_day": "2026-11-20",
            "status": "active",
        }),
    );
    data(
        client,
        OpKind::VersionUpsert,
        json!({
            "project": "TW",
            "name": "0.2 Hosts",
            "position": 2,
            "target_day": "2026-12-11",
            "status": "planned",
        }),
    );
    data(
        client,
        OpKind::CycleStart,
        json!({ "project": "TW", "release": "0.1 Core", "starts_day": "2026-11-02" }),
    );
    data(
        client,
        OpKind::Create,
        json!({
            "project": "TW",
            "title": "Open the standalone database",
            "body": "The core opens or creates its own SQLite file, 0600 inside a 0700 folder.\nA host migrates the same tables into its own database instead.",
            "kind": "feature",
            "priority": 1,
            "release": "0.1 Core",
            "cycle": 1,
        }),
    );
    create(
        client,
        "Ready queue orders by priority",
        "feature",
        2,
        "0.1 Core",
    );
    create(client, "Export escapes agent text", "bug", 0, "0.1 Core");
    create(
        client,
        "Roadmap import reads RM-1 sections",
        "feature",
        3,
        "0.2 Hosts",
    );
    create(
        client,
        "Schema v1 matches the catalog",
        "chore",
        2,
        "0.1 Core",
    );
    create(client, LONG_TITLE, "feature", 1, "0.1 Core");
    create(
        client,
        "Claim races leave exactly one winner",
        "spike",
        2,
        "0.1 Core",
    );
    move_to(client, "TW-1", "in_progress");
    move_to(client, "TW-2", "todo");
    move_to(client, "TW-3", "in_review");
    move_to(client, "TW-4", "backlog");
    move_to(client, "TW-6", "todo");
    move_to(client, "TW-7", "todo");
    write(
        client,
        OpKind::Close,
        json!({ "id": "TW-5", "reason": "Generated and checked" }),
    );
    write(
        client,
        OpKind::Depend,
        json!({ "id": "TW-7", "on": "TW-2" }),
    );
    data(
        client,
        OpKind::CriterionAdd,
        json!({ "id": "TW-1", "text": "The file is created 0600 in a 0700 folder" }),
    );
    data(
        client,
        OpKind::CriterionAdd,
        json!({ "id": "TW-1", "text": "Reopening the file keeps every row" }),
    );
    data(
        client,
        OpKind::CheckAdd,
        json!({
            "id": "TW-1",
            "position": 1,
            "kind": "test",
            "ref": "tests/store.rs::the_standalone_file_is_0600",
        }),
    );
    data(
        client,
        OpKind::CheckAdd,
        json!({ "id": "TW-1", "position": 2, "kind": "manual", "ref": "reopen check" }),
    );
    data(
        client,
        OpKind::EvidenceRecord,
        json!({
            "id": "TW-1",
            "position": 1,
            "kind": "test",
            "ref": "tests/store.rs::the_standalone_file_is_0600",
            "result": "pass",
            "evidence": "local run",
        }),
    );
    let shown = data(client, OpKind::Show, json!({ "id": "TW-1" }));
    data(
        client,
        OpKind::CriterionSet,
        json!({
            "id": "TW-1",
            "position": 1,
            "state": "met",
            "version": shown["criteria"][0]["version"],
        }),
    );
    data(
        client,
        OpKind::Link,
        json!({ "id": "TW-1", "kind": "branch", "target": "feat/v3a-at1-core" }),
    );
    claim(path, "agent:store", "TW-1");
    claim(path, "agent:export", "TW-3");
    owner
}

fn render(views: &Views, width: u16, height: u16) -> Vec<String> {
    screen(width, height, |frame| {
        let area = frame.area();
        views.render(area, frame.buffer_mut(), &Theme::plain());
    })
}

fn screen(width: u16, height: u16, draw: impl FnOnce(&mut Frame<'_>)) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(draw).unwrap();
    let buffer = terminal.backend().buffer().clone();
    let mut rows = Vec::new();
    for y in 0..height {
        let mut line = String::new();
        let mut skip = 0;
        for x in 0..width {
            if skip > 0 {
                skip -= 1;
                continue;
            }
            let symbol = buffer[(x, y)].symbol();
            line.push_str(symbol);
            skip = ratatui::text::Span::raw(symbol).width().saturating_sub(1);
        }
        rows.push(line);
    }
    rows
}

fn check(name: &str, actual: &[String]) {
    if let Ok(folder) = std::env::var("TRACKWRIGHT_FRAMES_OUT") {
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(format!("{folder}/{name}.txt"), actual.join("\n") + "\n").unwrap();
    }
    let path = format!("{}/tests/frames/{name}.txt", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("missing frame {path}"));
    let expected: Vec<&str> = text.lines().collect();
    let mut wrong = Vec::new();
    for (row, (want, got)) in expected.iter().zip(actual).enumerate() {
        if *want != got.as_str() {
            wrong.push(format!("row {row}\n want |{want}|\n  got |{got}|"));
        }
    }
    assert_eq!(expected.len(), actual.len(), "{name}: row count");
    assert!(wrong.is_empty(), "{name}:\n{}", wrong.join("\n"));
}

fn opened(page: Page) -> (tempfile::TempDir, Local, Views) {
    let (folder, path) = database();
    let mut owner = seed(&path);
    let mut views = Views::new(key());
    views.open(page, &mut owner);
    (folder, owner, views)
}

#[test]
fn the_list_view_renders_the_80_column_frame() {
    let (_folder, _owner, views) = opened(Page::List);
    check("list-80", &render(&views, 80, 24));
}

#[test]
fn the_list_view_renders_the_120_column_frame() {
    let (_folder, _owner, views) = opened(Page::List);
    check("list-120", &render(&views, 120, 36));
}

#[test]
fn the_ready_view_renders_the_80_column_frame() {
    let (_folder, _owner, views) = opened(Page::Ready);
    check("ready-80", &render(&views, 80, 24));
}

#[test]
fn the_ready_view_renders_the_120_column_frame() {
    let (_folder, _owner, views) = opened(Page::Ready);
    check("ready-120", &render(&views, 120, 36));
}

#[test]
fn the_board_view_renders_the_80_column_frame() {
    let (_folder, _owner, views) = opened(Page::Board);
    check("board-80", &render(&views, 80, 24));
}

#[test]
fn the_board_view_renders_the_120_column_frame() {
    let (_folder, _owner, views) = opened(Page::Board);
    check("board-120", &render(&views, 120, 36));
}

#[test]
fn the_roadmap_view_renders_the_80_column_frame() {
    let (_folder, _owner, views) = opened(Page::Roadmap);
    check("roadmap-80", &render(&views, 80, 24));
}

#[test]
fn the_roadmap_view_renders_the_120_column_frame() {
    let (_folder, _owner, views) = opened(Page::Roadmap);
    check("roadmap-120", &render(&views, 120, 36));
}

#[test]
fn the_detail_view_renders_the_80_column_frame() {
    let (_folder, mut owner, mut views) = opened(Page::List);
    assert!(views.key(Key::Enter, &mut owner));
    check("detail-80", &render(&views, 80, 24));
}

#[test]
fn the_detail_view_renders_the_120_column_frame() {
    let (_folder, mut owner, mut views) = opened(Page::List);
    assert!(views.key(Key::Enter, &mut owner));
    check("detail-120", &render(&views, 120, 36));
}

#[test]
fn back_from_the_detail_view_returns_to_the_list_it_came_from() {
    let (_folder, mut owner, mut views) = opened(Page::List);
    assert!(views.key(Key::Down, &mut owner));
    assert!(views.key(Key::Enter, &mut owner));
    assert!(render(&views, 80, 24)[0].starts_with(" TW-2  Ready queue orders by priority"));
    assert!(views.key(Key::Back, &mut owner));
    assert_eq!(views.page(), Page::List);
    assert!(render(&views, 80, 24)[4].starts_with(" > TW-2"));
    assert!(!views.key(Key::Back, &mut owner));
}

fn shown_version(replies: &[Value], id: &str) -> i64 {
    let listed = replies
        .iter()
        .filter_map(|reply| reply["data"]["tickets"].as_array())
        .flatten()
        .filter(|row| row["id"] == json!(id))
        .count();
    assert_eq!(listed, 1, "{id} must appear in exactly one board column");
    let shown: Vec<&Value> = replies
        .iter()
        .map(|reply| &reply["data"]["ticket"])
        .filter(|ticket| ticket["id"] == json!(id))
        .collect();
    assert_eq!(
        shown.len(),
        1,
        "the board reads {id} once, when it is selected"
    );
    shown[0]["version"].as_i64().unwrap()
}

#[test]
fn a_board_move_sends_the_version_it_read() {
    let (_folder, path) = database();
    let mut owner = seed(&path);
    let mut recorder = Recorder::new(&mut owner);
    let mut views = Views::new(key());
    views.open(Page::Board, &mut recorder);
    let read = shown_version(&recorder.received, "TW-2");
    recorder.sent.clear();
    assert!(views.key(Key::Char('>'), &mut recorder));
    let (op, input) = recorder.sent.first().unwrap().clone();
    assert_eq!(op, OpKind::Move);
    assert_eq!(
        input,
        json!({ "id": "TW-2", "status": "in_progress", "version": read })
    );
    assert!(views.notice().is_none(), "{:?}", views.notice());
    let after = data(&mut owner, OpKind::Show, json!({ "id": "TW-2" }));
    assert_eq!(after["ticket"]["status"], json!("in_progress"));
    assert!(after["ticket"]["version"].as_i64().unwrap() > read);
}

#[test]
fn a_moved_card_stays_selected_in_its_new_column() {
    let (_folder, mut owner, mut views) = opened(Page::Board);
    assert!(views.key(Key::Char('>'), &mut owner));
    let rows = render(&views, 80, 24);
    let marked: Vec<&String> = rows.iter().filter(|row| row.contains('>')).collect();
    assert_eq!(marked.len(), 1, "{rows:#?}");
    let column = marked[0].find('>').unwrap();
    assert_eq!(column, 20, "{rows:#?}");
    assert!(marked[0][column..].starts_with(">TW-2"), "{rows:#?}");
}

#[test]
fn a_stale_board_move_shows_one_line_and_changes_nothing() {
    let (_folder, path) = database();
    let mut owner = seed(&path);
    let mut views = Views::new(key());
    views.open(Page::Board, &mut owner);
    let mut other = Local::open(&path, actor("owner")).unwrap();
    let read = version(&mut other, "TW-2");
    data(
        &mut other,
        OpKind::Edit,
        json!({ "id": "TW-2", "priority": 1, "version": read }),
    );
    let mut recorder = Recorder::new(&mut owner);
    assert!(views.key(Key::Char('>'), &mut recorder));
    assert_eq!(recorder.sent[0].0, OpKind::Move);
    assert_eq!(recorder.sent[0].1["version"], json!(read));
    assert_eq!(recorder.received[0]["code"], json!("stale_version"));
    assert!(
        recorder.sent[1..].iter().any(|(op, _)| *op == OpKind::List),
        "the board reloads after a stale move"
    );
    let line = "TW-2 changed since it was read. Board reloaded; move it again if still wanted.";
    assert_eq!(
        views.notice().map(|notice| notice.text.as_str()),
        Some(line)
    );
    let rows = render(&views, 80, 24);
    assert_eq!(
        rows.iter().filter(|row| row.contains(line)).count(),
        1,
        "{rows:#?}"
    );
    assert_eq!(rows[23].trim_end(), format!(" {line}"));
    let after = data(&mut owner, OpKind::Show, json!({ "id": "TW-2" }));
    assert_eq!(after["ticket"]["status"], json!("todo"));
    assert_eq!(after["ticket"]["priority"], json!(1));
}

#[test]
fn a_board_move_into_done_closes_and_out_of_done_reopens_with_the_version_read() {
    let (_folder, path) = database();
    let mut owner = seed(&path);
    let mut recorder = Recorder::new(&mut owner);
    let mut views = Views::new(key());
    views.open(Page::Board, &mut recorder);
    for _ in 0..2 {
        assert!(views.key(Key::Right, &mut recorder));
    }
    let read = shown_version(&recorder.received, "TW-3");
    recorder.sent.clear();
    assert!(views.key(Key::Char('>'), &mut recorder));
    assert_eq!(
        recorder.sent[0],
        (
            OpKind::Close,
            json!({ "id": "TW-3", "version": read, "status": "done" })
        )
    );
    assert!(views.notice().is_none(), "{:?}", views.notice());
    let closed = data(&mut recorder, OpKind::Show, json!({ "id": "TW-3" }));
    assert_eq!(closed["ticket"]["status"], json!("done"));
    assert_eq!(closed["ticket"]["closed_by"], json!("owner"));
    let rows = render(&views, 80, 24);
    let marked: Vec<&String> = rows.iter().filter(|row| row.contains('>')).collect();
    assert_eq!(marked.len(), 1, "{rows:#?}");
    let column = marked[0].find('>').unwrap();
    assert_eq!(column, 60, "{rows:#?}");
    assert!(marked[0][column..].starts_with(">TW-3"), "{rows:#?}");

    let version = closed["ticket"]["version"].as_i64().unwrap();
    recorder.sent.clear();
    assert!(views.key(Key::Char('<'), &mut recorder));
    assert_eq!(
        recorder.sent[0],
        (
            OpKind::Reopen,
            json!({ "id": "TW-3", "version": version, "status": "in_review" })
        )
    );
    assert!(views.notice().is_none(), "{:?}", views.notice());
    let reopened = data(&mut recorder, OpKind::Show, json!({ "id": "TW-3" }));
    assert_eq!(reopened["ticket"]["status"], json!("in_review"));
    assert_eq!(reopened["ticket"]["closed_by"], Value::Null);
}

#[test]
fn a_board_close_the_policy_refuses_shows_one_line_and_changes_nothing() {
    let (_folder, path) = database();
    drop(seed(&path));
    let mut agent = Local::open(&path, actor("agent:export")).unwrap();
    let mut views = Views::new(key());
    views.open(Page::Board, &mut agent);
    for _ in 0..2 {
        assert!(views.key(Key::Right, &mut agent));
    }
    assert!(views.key(Key::Char('>'), &mut agent));
    let line = "close_owner_only: policy close is owner: only the owner may close";
    assert_eq!(
        views.notice().map(|notice| notice.text.as_str()),
        Some(line)
    );
    let rows = render(&views, 80, 24);
    assert_eq!(
        rows.iter().filter(|row| row.contains(line)).count(),
        1,
        "{rows:#?}"
    );
    assert_eq!(rows[23].trim_end(), format!(" {line}"));
    let after = data(&mut agent, OpKind::Show, json!({ "id": "TW-3" }));
    assert_eq!(after["ticket"]["status"], json!("in_review"));
    assert_eq!(after["ticket"]["closed_by"], Value::Null);
}

#[test]
fn the_list_loads_the_next_page_at_its_last_row() {
    let (_folder, path) = database();
    let mut writers: Vec<Local> = ["owner", "ci", "agent:seed"]
        .iter()
        .map(|who| Local::open(&path, actor(who)).unwrap())
        .collect();
    for number in 1..=60 {
        data(
            &mut writers[(number - 1) / 20],
            OpKind::Create,
            json!({
                "project": "TW",
                "title": format!("Ticket {number}"),
                "body": "",
                "kind": "chore",
                "priority": 2,
            }),
        );
    }
    let mut owner = writers.swap_remove(0);
    let mut recorder = Recorder::new(&mut owner);
    let mut views = Views::new(key());
    views.open(Page::List, &mut recorder);
    let cursor = recorder.received[0]["cursor"].clone();
    assert!(
        cursor.is_string(),
        "a 60-ticket list pages: {}",
        recorder.received[0]
    );
    assert_eq!(
        recorder.received[0]["data"]["tickets"]
            .as_array()
            .unwrap()
            .len(),
        50
    );
    for _ in 0..49 {
        assert!(views.key(Key::Down, &mut recorder));
    }
    assert_eq!(
        recorder.sent.len(),
        1,
        "no page is fetched before the last row"
    );
    assert!(views.key(Key::Down, &mut recorder));
    assert_eq!(recorder.sent.len(), 2);
    assert_eq!(recorder.sent[1].0, OpKind::List);
    assert_eq!(recorder.sent[1].1["cursor"], cursor);
    let rows = render(&views, 80, 24);
    assert!(
        rows.iter().any(|row| row.starts_with(" > TW-51 ")),
        "{rows:#?}"
    );
    assert!(rows[0].trim_end().ends_with("60 tickets"), "{rows:#?}");
}

#[test]
fn a_move_past_the_last_column_says_so_on_one_line() {
    let (_folder, path) = database();
    let mut owner = seed(&path);
    let mut views = Views::new(key());
    views.open(Page::Board, &mut owner);
    for _ in 0..3 {
        assert!(views.key(Key::Right, &mut owner));
    }
    assert!(views.key(Key::Char('>'), &mut owner));
    let rows = render(&views, 80, 24);
    let notice = views
        .notice()
        .expect("a move past the last column is refused");
    assert_eq!(notice.text, "Done is the last column.");
    assert_eq!(rows[23].trim_end(), " Done is the last column.");
}

#[test]
fn a_window_too_small_for_a_view_says_so_instead_of_drawing_it() {
    let (_folder, _owner, views) = opened(Page::Board);
    let rows = render(&views, 40, 6);
    assert_eq!(rows[0].trim_end(), " Make the window at least 60 x 8.");
    assert!(rows[1..].iter().all(|row| row.trim().is_empty()));
}

#[test]
fn the_standalone_tui_frames_the_board_with_tabs_and_hints() {
    let (_folder, _owner, views) = opened(Page::Board);
    let rows = screen(80, 24, |frame| {
        trackwright_cli::tui::draw(frame, &views, &Theme::plain());
    });
    check("tui-board-80", &rows);
}
