use super::*;
use crate::protocol::RequestId;
use crate::tui::client::OwnerEvent;
use crate::tui::screens::settings_accounts::tests::{check, dataset, fixture, list_ctx, render};
use crate::tui::seam::{ProjectCard, ResearchCounts};
use crate::tui::shell::Shell;

fn project(id: &str, name: &str) -> ProjectCard {
    ProjectCard {
        id: ProjectId::new(id).unwrap(),
        name: name.into(),
        about: String::new(),
        platforms: Vec::new(),
        pool: None,
        research: ResearchCounts::default(),
        created_ms: None,
    }
}

fn projects() -> Vec<ProjectCard> {
    vec![
        project("tidepool", "Tidepool"),
        project("ledgerline", "Ledgerline"),
        project("fieldnotes", "Fieldnotes Mobile"),
    ]
}

fn saved(ordinal: u64, text: &str, scope: MemoryScope, origin: MemoryOrigin) -> SavedMemory {
    SavedMemory {
        id: RequestId::new(format!("memory-{ordinal}")).unwrap(),
        ordinal,
        scope,
        origin,
        text: text.into(),
    }
}

fn promoted(project: &str) -> MemoryOrigin {
    MemoryOrigin::Promoted {
        project_id: ProjectId::new(project).unwrap(),
        entry_id: RequestId::new(format!("entry-{project}")).unwrap(),
        version: 1,
    }
}

fn only(project: &str) -> MemoryScope {
    MemoryScope::Project {
        project_id: ProjectId::new(project).unwrap(),
    }
}

fn memories() -> Vec<SavedMemory> {
    vec![
        saved(
            7,
            "Mira prefers Postgres for new services.",
            MemoryScope::Global,
            MemoryOrigin::Written,
        ),
        saved(
            6,
            "Buoy firmware sends readings every 90 s in CBOR.",
            MemoryScope::Global,
            promoted("tidepool"),
        ),
        saved(
            5,
            "Most labs already use ERDDAP for ocean data access.",
            only("tidepool"),
            MemoryOrigin::Written,
        ),
        saved(
            4,
            "Studios bill monthly, and most want PDF invoices.",
            MemoryScope::Global,
            promoted("ledgerline"),
        ),
        saved(
            3,
            "Field teams work offline for up to five days.",
            only("fieldnotes"),
            MemoryOrigin::Written,
        ),
        saved(
            2,
            "Keep agent replies short unless asked for detail.",
            MemoryScope::Global,
            MemoryOrigin::Written,
        ),
        saved(
            1,
            "Use Tauri, not Electron, for desktop apps.",
            only("ledgerline"),
            MemoryOrigin::Written,
        ),
    ]
}

fn memory_ctx(width: u16, height: u16) -> Ctx {
    let mut ctx = list_ctx(width, height, dataset(None).1);
    ctx.store.harnesses = Load::Ready((2, 9));
    ctx.store.projects = Load::Ready(projects());
    ctx
}

fn put(row: &str, column: usize, text: &str, room: usize) -> String {
    let mut cells: Vec<char> = row.chars().collect();
    let mut value: Vec<char> = text.chars().collect();
    value.resize(room, ' ');
    cells.splice(column..column + room, value);
    cells.into_iter().collect()
}

fn engine_rows(rows: &mut [String], from: usize, saved_at: usize) {
    rows[4] = rows[4].replacen("  41 saved", "   7 saved", 1);
    rows[8] = put(&rows[8], saved_at, "", 5);
    for row in 9..=15 {
        rows[row] = put(&rows[row], saved_at, "", 6);
    }
    for row in [9, 14] {
        rows[row] = put(&rows[row], from, "You", 7);
    }
}

#[test]
fn memory_frames_match_the_design() {
    let mut shell = Shell::with(memory_ctx(80, 24), Box::new(Memory::with(memories())));
    shell.key(Key::Down);
    let mut expected = fixture("memory-80", &[]);
    engine_rows(&mut expected, 50, 66);
    assert_eq!(render(&mut shell, 80, 24), expected);

    let mut shell = Shell::with(memory_ctx(120, 36), Box::new(Memory::with(memories())));
    shell.key(Key::Down);
    let mut expected = fixture("memory-120", &[]);
    engine_rows(&mut expected, 70, 92);
    expected[18] = format!("{:<120}", "  ✓ Saved from Tidepool for every project.");
    assert_eq!(render(&mut shell, 120, 36), expected);
}

#[test]
fn no_memories_shows_how_they_arrive() {
    let mut shell = Shell::with(memory_ctx(80, 24), Box::new(Memory::with(Vec::new())));
    check("memory-empty-80", &render(&mut shell, 80, 24), &[], None);
}

#[test]
fn where_a_memory_came_from_is_plain() {
    let ctx = memory_ctx(80, 24);
    let lines: Vec<(String, String)> = memories()
        .iter()
        .map(|memory| (source(memory, &ctx), saved_line(memory, &ctx)))
        .take(3)
        .collect();
    assert_eq!(
        lines,
        [
            (
                "You".to_owned(),
                "Saved by you for every project.".to_owned()
            ),
            (
                "Tidepool".to_owned(),
                "Saved from Tidepool for every project.".to_owned()
            ),
            ("Tidepool".to_owned(), "Saved for Tidepool only.".to_owned()),
        ]
    );
    let mut unknown = memory_ctx(80, 24);
    unknown.store.projects = Load::Ready(Vec::new());
    assert_eq!(source(&memories()[2], &unknown), "A project");
}

#[test]
fn filtering_narrows_by_text_and_source() {
    let mut ctx = memory_ctx(80, 24);
    let mut screen = Memory::with(memories());
    screen.key(&Key::Char('/'), &mut ctx);
    assert!(screen.typing());
    for character in "tidepool".chars() {
        screen.key(&Key::Char(character), &mut ctx);
    }
    assert_eq!(screen.visible(&ctx).len(), 2);
    screen.key(&Key::Enter, &mut ctx);
    assert!(!screen.typing());
    screen.key(&Key::Esc, &mut ctx);
    assert_eq!(screen.visible(&ctx).len(), 7);
}

#[test]
fn delete_asks_first_then_sends_one_change_and_reloads() {
    let mut ctx = memory_ctx(80, 24);
    let mut screen = Memory::with(memories());
    screen.key(&Key::Down, &mut ctx);
    screen.key(&Key::Enter, &mut ctx);
    assert_eq!(screen.modal(&ctx).unwrap().0.title, "Memory");
    screen.key(&Key::Char('d'), &mut ctx);
    assert_eq!(screen.modal(&ctx).unwrap().0.title, "Delete memory");
    screen.key(&Key::Enter, &mut ctx);
    assert!(screen.modal(&ctx).is_none(), "Cancel is focused first");
    assert_eq!(ctx.queued(), 0);
    screen.key(&Key::Char('d'), &mut ctx);
    screen.key(&Key::Right, &mut ctx);
    screen.key(&Key::Enter, &mut ctx);
    match ctx.next_request() {
        Some(Request::Settings(job)) => match *job {
            SettingsJob::DeleteMemory { memory, generation } => {
                assert_eq!(memory.as_str(), "memory-6");
                assert!(generation > 0);
            }
            other => panic!("delete expected, got {other:?}"),
        },
        other => panic!("settings request expected, got {other:?}"),
    }
    assert!(screen.busy(&ctx));
    screen.key(&Key::Char('d'), &mut ctx);
    assert!(screen.modal(&ctx).is_none(), "one delete at a time");
    let deleted = Arrival::Owner(Box::new(OwnerEvent::Settings(Box::new(
        SettingsEvent::MemoryDeleted(Ok(())),
    ))));
    screen.arrive(&deleted, &mut ctx);
    assert_eq!(ctx.message().unwrap().lead, "Memory deleted.");
    assert!(
        matches!(ctx.next_request(), Some(Request::Settings(_))),
        "the list reloads"
    );
}

#[test]
fn every_project_scope_is_read_and_a_new_project_reloads() {
    let mut ctx = memory_ctx(120, 36);
    let mut screen = Memory::new();
    screen.shown(&mut ctx);
    assert!(matches!(ctx.next_request(), Some(Request::Projects)));
    match ctx.next_request() {
        Some(Request::Settings(job)) => match *job {
            SettingsJob::Memories { scopes } => {
                assert_eq!(scopes.len(), 4);
                assert_eq!(scopes[0], MemoryScope::Global);
            }
            other => panic!("memories expected, got {other:?}"),
        },
        other => panic!("settings request expected, got {other:?}"),
    }
    let reply = Arrival::Owner(Box::new(OwnerEvent::Settings(Box::new(
        SettingsEvent::Memories(Ok(memories())),
    ))));
    screen.arrive(&reply, &mut ctx);
    assert_eq!(ctx.store.memory, Load::Ready(7));
    let mut more = projects();
    more.push(project("harbor", "Harbor"));
    ctx.store.projects = Load::Ready(more.clone());
    screen.arrive(&Arrival::Projects(Ok(more)), &mut ctx);
    match ctx.next_request() {
        Some(Request::Settings(job)) => {
            assert!(matches!(*job, SettingsJob::Memories { ref scopes } if scopes.len() == 5));
        }
        other => panic!("reload expected, got {other:?}"),
    }
}
