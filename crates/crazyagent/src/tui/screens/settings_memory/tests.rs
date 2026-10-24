use super::*;
use crate::protocol::RequestId;
use crate::tui::client::OwnerEvent;
use crate::tui::screens::settings_accounts::tests::{dataset, fixture, list_ctx, render};
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
    for row in rows.iter_mut().skip(9).take(7) {
        *row = put(row, saved_at, "", 6);
    }
    for row in [9, 14] {
        rows[row] = put(&rows[row], from, "You", 7);
    }
}

fn with_add(row: &str, before: &str, gap: &str) -> String {
    let added = format!("a Add{gap}");
    let row = row.replacen(before, &format!("{added}{before}"), 1);
    let room = format!("{}? Help", " ".repeat(added.len() + 1));
    assert!(row.contains(&room), "the help line has room for a Add");
    row.replacen(&room, " ? Help", 1)
}

fn settings(arrival: SettingsEvent) -> Arrival {
    Arrival::Owner(Box::new(OwnerEvent::Settings(Box::new(arrival))))
}

fn job(ctx: &mut Ctx) -> SettingsJob {
    match ctx.next_request() {
        Some(Request::Settings(job)) => *job,
        other => panic!("settings request expected, got {other:?}"),
    }
}

fn suggestion(project: &str, text: &str) -> Suggestion {
    Suggestion {
        project: ProjectId::new(project).unwrap(),
        entry: RequestId::new(format!("entry-{project}")).unwrap(),
        version: 3,
        text: text.into(),
    }
}

fn texts(lines: &[Line<'static>]) -> Vec<String> {
    lines.iter().map(ToString::to_string).collect()
}

#[test]
fn memory_frames_match_the_design() {
    let mut shell = Shell::with(memory_ctx(80, 24), Box::new(Memory::with(memories())));
    shell.key(Key::Down);
    let mut expected = fixture("memory-80", &[]);
    engine_rows(&mut expected, 50, 66);
    expected[23] = with_add(&expected[23], "/ Filter", "  ");
    assert_eq!(render(&mut shell, 80, 24), expected);

    let mut shell = Shell::with(memory_ctx(120, 36), Box::new(Memory::with(memories())));
    shell.key(Key::Down);
    let mut expected = fixture("memory-120", &[]);
    engine_rows(&mut expected, 70, 92);
    expected[18] = format!("{:<120}", "  ✓ Saved from Tidepool for every project.");
    expected[35] = with_add(&expected[35], "/ Filter", "   ");
    assert_eq!(render(&mut shell, 120, 36), expected);
}

#[test]
fn no_memories_shows_how_they_arrive() {
    let mut shell = Shell::with(memory_ctx(80, 24), Box::new(Memory::with(Vec::new())));
    let mut expected = fixture("memory-empty-80", &[]);
    expected[23] = with_add(&expected[23], "Shift-", "  ");
    assert_eq!(render(&mut shell, 80, 24), expected);
}

#[test]
fn memory_problems_read_as_sentences() {
    assert_eq!(
        memory_plain("memory_note_blank"),
        "Write something to remember first."
    );
    assert_eq!(
        memory_plain("knowledge_version_conflict"),
        "This suggestion changed. The list now shows the latest."
    );
    assert_eq!(
        memory_plain("memory_duplicate"),
        "This memory is already saved."
    );
    assert_eq!(
        memory_plain("controller_disconnected"),
        plain("controller_disconnected")
    );
}

#[test]
fn add_memory_saves_a_note_for_the_chosen_projects() {
    let mut ctx = memory_ctx(120, 36);
    let mut screen = Memory::with(memories());
    screen.key(&Key::Char('a'), &mut ctx);
    assert!(screen.typing());
    assert!(screen.modal(&ctx).is_none());
    let (lines, cursor) = screen.body(116, &ctx);
    let shown = texts(&lines);
    assert_eq!(shown[0], "Add memory");
    assert!(shown.iter().any(|line| line.contains("Every project")));
    assert!(cursor.is_some(), "the cursor sits in the note");

    screen.key(&Key::Enter, &mut ctx);
    assert_eq!(ctx.queued(), 0, "a blank note is not sent");
    assert_eq!(
        ctx.message().unwrap().lead,
        "Write something to remember first."
    );

    for character in "Ship small".chars() {
        screen.key(&Key::Char(character), &mut ctx);
    }
    screen.key(&Key::Ctrl('j'), &mut ctx);
    for character in "changes".chars() {
        screen.key(&Key::Char(character), &mut ctx);
    }
    screen.key(&Key::Tab, &mut ctx);
    let (lines, _) = screen.body(116, &ctx);
    assert!(
        texts(&lines)
            .iter()
            .any(|line| line.contains("Tidepool only"))
    );
    screen.key(&Key::Enter, &mut ctx);
    match job(&mut ctx) {
        SettingsJob::AddMemory {
            scope,
            text,
            generation,
        } => {
            assert_eq!(scope, only("tidepool"));
            assert_eq!(text, "Ship small\nchanges");
            assert!(generation > 0);
        }
        other => panic!("add memory expected, got {other:?}"),
    }
    assert!(screen.busy(&ctx));
    screen.key(&Key::Enter, &mut ctx);
    assert_eq!(ctx.queued(), 0, "one save at a time");

    screen.arrive(
        &settings(SettingsEvent::MemoryAdded(Err("memory_duplicate".into()))),
        &mut ctx,
    );
    assert_eq!(ctx.message().unwrap().rest, "This memory is already saved.");
    assert!(screen.typing(), "a refused note stays open to edit");

    screen.key(&Key::Enter, &mut ctx);
    assert!(matches!(job(&mut ctx), SettingsJob::AddMemory { .. }));
    screen.arrive(&settings(SettingsEvent::MemoryAdded(Ok(()))), &mut ctx);
    assert_eq!(ctx.message().unwrap().lead, "Memory saved.");
    assert!(!screen.typing());
    assert!(matches!(job(&mut ctx), SettingsJob::Memories { .. }));
    assert!(matches!(job(&mut ctx), SettingsJob::Suggestions { .. }));

    screen.key(&Key::Char('a'), &mut ctx);
    screen.key(&Key::Char('x'), &mut ctx);
    screen.key(&Key::Esc, &mut ctx);
    assert!(!screen.typing(), "Esc closes the note without saving");
    assert_eq!(ctx.queued(), 0);
}

#[test]
fn research_suggestions_come_first_and_save_for_every_project() {
    let mut ctx = memory_ctx(120, 36);
    let mut screen = Memory::new();
    screen.shown(&mut ctx);
    assert!(matches!(ctx.next_request(), Some(Request::Projects)));
    assert!(matches!(job(&mut ctx), SettingsJob::Memories { .. }));
    match job(&mut ctx) {
        SettingsJob::Suggestions { projects } => assert_eq!(projects.len(), 3),
        other => panic!("suggestions expected, got {other:?}"),
    }
    screen.arrive(&settings(SettingsEvent::Memories(Ok(memories()))), &mut ctx);
    let found = vec![
        suggestion("tidepool", "Labs want hourly exports."),
        suggestion("ledgerline", "Studios pay by card."),
    ];
    screen.arrive(
        &settings(SettingsEvent::Suggestions(Ok(found.clone()))),
        &mut ctx,
    );
    assert!(!screen.busy(&ctx));
    let (lines, _) = screen.body(116, &ctx);
    let shown = texts(&lines);
    assert!(shown[0].contains("7 saved   2 suggested"));
    assert!(shown[5].contains("Labs want hourly exports.") && shown[5].contains("Suggested"));
    assert!(
        shown
            .iter()
            .any(|line| line.contains("From research in Tidepool."))
    );

    screen.key(&Key::Char('d'), &mut ctx);
    assert!(screen.modal(&ctx).is_none(), "a suggestion is not deleted");
    screen.key(&Key::Enter, &mut ctx);
    let (modal, _) = screen.modal(&ctx).unwrap();
    assert_eq!(modal.title, "Suggested memory");
    assert!(
        texts(&modal.body)
            .iter()
            .any(|line| line.contains("From research in Tidepool."))
    );
    screen.key(&Key::Enter, &mut ctx);
    match job(&mut ctx) {
        SettingsJob::Suggestion {
            suggestion: picked,
            keep,
            generations,
        } => {
            assert_eq!(picked, found[0]);
            assert!(keep, "Save to memory is focused first");
            assert!(generations[0] > 0 && generations[0] < generations[1]);
        }
        other => panic!("suggestion expected, got {other:?}"),
    }
    assert!(screen.busy(&ctx));
    screen.key(&Key::Enter, &mut ctx);
    assert!(screen.modal(&ctx).is_none(), "one decision at a time");

    screen.arrive(
        &settings(SettingsEvent::SuggestionDone(found[0].clone(), Ok(()))),
        &mut ctx,
    );
    let (lines, _) = screen.body(116, &ctx);
    assert!(texts(&lines)[0].contains("1 suggested"));
    assert!(matches!(job(&mut ctx), SettingsJob::Memories { .. }));
    assert!(matches!(job(&mut ctx), SettingsJob::Suggestions { .. }));
}

#[test]
fn a_suggestion_can_be_dismissed_and_a_refusal_is_plain() {
    let mut ctx = memory_ctx(80, 24);
    let mut screen = Memory::new();
    screen.shown(&mut ctx);
    while ctx.next_request().is_some() {}
    screen.arrive(&settings(SettingsEvent::Memories(Ok(Vec::new()))), &mut ctx);
    let found = vec![suggestion("fieldnotes", "Crews sync at dawn.")];
    screen.arrive(
        &settings(SettingsEvent::Suggestions(Ok(found.clone()))),
        &mut ctx,
    );
    let (lines, _) = screen.body(76, &ctx);
    assert!(texts(&lines).iter().all(|line| line != "No memories yet."));

    screen.key(&Key::Enter, &mut ctx);
    screen.key(&Key::Char('n'), &mut ctx);
    assert!(matches!(
        job(&mut ctx),
        SettingsJob::Suggestion { keep: false, .. }
    ));
    screen.arrive(
        &settings(SettingsEvent::SuggestionDone(
            found[0].clone(),
            Err("knowledge_version_conflict".into()),
        )),
        &mut ctx,
    );
    let message = ctx.message().unwrap();
    assert_eq!(message.lead, "Could not finish with this suggestion.");
    assert_eq!(
        message.rest,
        "This suggestion changed. The list now shows the latest."
    );
    assert!(matches!(job(&mut ctx), SettingsJob::Memories { .. }));
    assert!(matches!(job(&mut ctx), SettingsJob::Suggestions { .. }));
    screen.arrive(&settings(SettingsEvent::Memories(Ok(Vec::new()))), &mut ctx);
    screen.arrive(
        &settings(SettingsEvent::Suggestions(Ok(found.clone()))),
        &mut ctx,
    );
    assert!(!screen.busy(&ctx));

    screen.key(&Key::Enter, &mut ctx);
    screen.key(&Key::Esc, &mut ctx);
    assert!(screen.modal(&ctx).is_none());
    assert_eq!(ctx.queued(), 0, "Esc decides nothing");
}

#[test]
fn suggestions_without_engine_support_stay_quiet() {
    let mut ctx = memory_ctx(80, 24);
    let mut screen = Memory::new();
    screen.shown(&mut ctx);
    while ctx.next_request().is_some() {}
    screen.arrive(&settings(SettingsEvent::Memories(Ok(memories()))), &mut ctx);
    screen.arrive(
        &settings(SettingsEvent::Suggestions(Err(NOT_READY.into()))),
        &mut ctx,
    );
    assert!(ctx.message().is_none());
    assert!(!screen.busy(&ctx));
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
    let selection = saved(
        8,
        "Readings arrive every 90 s.",
        only("tidepool"),
        MemoryOrigin::Selected {
            path: "context/research/market_research.md".into(),
        },
    );
    assert_eq!(source(&selection, &ctx), "Tidepool");
    assert_eq!(saved_line(&selection, &ctx), "Saved for Tidepool only.");
    let mut unknown = memory_ctx(80, 24);
    unknown.store.projects = Load::Ready(Vec::new());
    assert_eq!(source(&memories()[2], &unknown), "A project");
    let selected = |scope| {
        saved(
            9,
            "Ship the page before the market opens.",
            scope,
            MemoryOrigin::Selected {
                path: "context/brief.md".into(),
            },
        )
    };
    let kept = selected(only("ledgerline"));
    assert_eq!(
        (source(&kept, &ctx), saved_line(&kept, &ctx)),
        (
            "Ledgerline".to_owned(),
            "Saved for Ledgerline only.".to_owned()
        )
    );
    let everywhere = selected(MemoryScope::Global);
    assert_eq!(
        (source(&everywhere, &ctx), saved_line(&everywhere, &ctx)),
        (
            "You".to_owned(),
            "Saved by you for every project.".to_owned()
        )
    );
}

#[test]
fn a_memory_selected_from_a_project_file_names_that_project() {
    let ctx = memory_ctx(80, 24);
    let selected = |scope| {
        saved(
            8,
            "Readings arrive every 90 s.",
            scope,
            MemoryOrigin::Selected {
                path: "context/research/market_research.md".into(),
            },
        )
    };
    let memory = selected(only("tidepool"));
    assert_eq!(source(&memory, &ctx), "Tidepool");
    assert_eq!(saved_line(&memory, &ctx), "Saved for Tidepool only.");
    let global = selected(MemoryScope::Global);
    assert_eq!(source(&global, &ctx), "You");
    assert_eq!(saved_line(&global, &ctx), "Saved by you for every project.");
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
