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
        pool_id: None,
        research: ResearchCounts::default(),
        created_ms: None,
        activity: Default::default(),
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

fn with_writes(rows: &mut Vec<String>, width: usize, hints: &str) {
    rows[5] = format!("{:<width$}", format!("  {SUBTITLE}"));
    let settings = if width >= 120 {
        "  Agent memory writes  ← Loading →  w to change   Saved in  ← Loading →  p to change"
    } else {
        "  Writes ← Loading →  w   Saved in ← Loading →  p"
    };
    rows.insert(6, format!("{settings:<width$}"));
    let spare = rows.len() - 2;
    assert!(
        rows[spare].trim().is_empty(),
        "a blank row makes room for the writes row"
    );
    rows.remove(spare);
    *rows.last_mut().unwrap() = hints.to_owned();
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
        kept: false,
    }
}

fn kept(project: &str, text: &str) -> Suggestion {
    Suggestion {
        kept: true,
        ..suggestion(project, text)
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
    with_writes(
        &mut expected,
        80,
        " Shift-←→ Sections  Esc Home  p Where agents save                        ? Help ",
    );
    expected[22] = format!(
        "{:<80}",
        " Enter View  d Delete  a Add  w Agent memory writes  i Import  / Filter"
    );
    assert_eq!(render(&mut shell, 80, 24), expected);

    let mut shell = Shell::with(memory_ctx(120, 36), Box::new(Memory::with(memories())));
    shell.key(Key::Down);
    let mut expected = fixture("memory-120", &[]);
    engine_rows(&mut expected, 70, 92);
    expected[18] = format!("{:<120}", "  ✓ Saved from Tidepool for every project.");
    with_writes(
        &mut expected,
        120,
        " Enter View   d Delete   a Add   w Agent memory writes   i Import   / Filter   Shift-←→ Sections   Esc Home      ? Help ",
    );
    assert_eq!(render(&mut shell, 120, 36), expected);
}

#[test]
fn no_memories_shows_how_they_arrive() {
    let mut shell = Shell::with(memory_ctx(80, 24), Box::new(Memory::with(Vec::new())));
    let mut expected = fixture("memory-empty-80", &[]);
    expected[22] = format!(
        "{:<80}",
        " a Add  w Agent memory writes  i Import  Shift-←→ Sections  Esc Home"
    );
    expected[23] =
        " p Where agents save                                                     ? Help ".into();
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
    assert!(matches!(
        job(&mut ctx),
        SettingsJob::AgentWrites { set: None }
    ));
    assert!(matches!(
        job(&mut ctx),
        SettingsJob::AgentScope { set: None }
    ));
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
    assert!(matches!(
        job(&mut ctx),
        SettingsJob::AgentWrites { set: None }
    ));
    assert!(matches!(
        job(&mut ctx),
        SettingsJob::AgentScope { set: None }
    ));
    match job(&mut ctx) {
        SettingsJob::Suggestions { projects } => assert_eq!(projects.len(), 3),
        other => panic!("suggestions expected, got {other:?}"),
    }
    screen.arrive(
        &settings(SettingsEvent::Memories(
            screen.scoped.clone(),
            Ok(memories()),
        )),
        &mut ctx,
    );
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
    assert!(shown[6].contains("Labs want hourly exports.") && shown[6].contains("Suggested"));
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
    assert_eq!(ctx.message().unwrap().lead, "Saved to memory.");
    let (lines, _) = screen.body(116, &ctx);
    assert!(texts(&lines)[0].contains("1 suggested"));
    assert!(matches!(job(&mut ctx), SettingsJob::Memories { .. }));
    assert!(matches!(
        job(&mut ctx),
        SettingsJob::AgentWrites { set: None }
    ));
    assert!(matches!(
        job(&mut ctx),
        SettingsJob::AgentScope { set: None }
    ));
    assert!(matches!(job(&mut ctx), SettingsJob::Suggestions { .. }));
}

#[test]
fn a_suggestion_can_be_dismissed_and_a_refusal_is_plain() {
    let mut ctx = memory_ctx(80, 24);
    let mut screen = Memory::new();
    screen.shown(&mut ctx);
    while ctx.next_request().is_some() {}
    screen.arrive(
        &settings(SettingsEvent::Memories(
            screen.scoped.clone(),
            Ok(Vec::new()),
        )),
        &mut ctx,
    );
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
    assert!(matches!(
        job(&mut ctx),
        SettingsJob::AgentWrites { set: None }
    ));
    assert!(matches!(
        job(&mut ctx),
        SettingsJob::AgentScope { set: None }
    ));
    assert!(matches!(job(&mut ctx), SettingsJob::Suggestions { .. }));
    screen.arrive(
        &settings(SettingsEvent::Memories(
            screen.scoped.clone(),
            Ok(Vec::new()),
        )),
        &mut ctx,
    );
    screen.arrive(
        &settings(SettingsEvent::Suggestions(Ok(found.clone()))),
        &mut ctx,
    );
    assert!(!screen.busy(&ctx));

    screen.key(&Key::Enter, &mut ctx);
    screen.key(&Key::Esc, &mut ctx);
    assert!(screen.modal(&ctx).is_none());
    assert_eq!(ctx.queued(), 0, "Esc decides nothing");

    screen.key(&Key::Enter, &mut ctx);
    screen.key(&Key::Char('n'), &mut ctx);
    assert!(matches!(
        job(&mut ctx),
        SettingsJob::Suggestion { keep: false, .. }
    ));
    screen.arrive(
        &settings(SettingsEvent::SuggestionDone(found[0].clone(), Ok(()))),
        &mut ctx,
    );
    assert_eq!(ctx.message().unwrap().lead, "Suggestion dismissed.");
    let (lines, _) = screen.body(76, &ctx);
    assert!(texts(&lines).iter().any(|line| line == "No memories yet."));
}

#[test]
fn suggestions_without_engine_support_stay_quiet() {
    let mut ctx = memory_ctx(80, 24);
    let mut screen = Memory::new();
    screen.shown(&mut ctx);
    while ctx.next_request().is_some() {}
    screen.arrive(
        &settings(SettingsEvent::Memories(
            screen.scoped.clone(),
            Ok(memories()),
        )),
        &mut ctx,
    );
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
    assert!(matches!(
        job(&mut ctx),
        SettingsJob::AgentWrites { set: None }
    ));
    assert!(matches!(
        job(&mut ctx),
        SettingsJob::AgentScope { set: None }
    ));
    match ctx.next_request() {
        Some(Request::Settings(job)) => match *job {
            SettingsJob::Suggestions { projects } => assert_eq!(
                projects,
                ["tidepool", "ledgerline", "fieldnotes"].map(|id| ProjectId::new(id).unwrap())
            ),
            other => panic!("suggestions expected, got {other:?}"),
        },
        other => panic!("settings request expected, got {other:?}"),
    }
    assert!(ctx.next_request().is_none());
    let reply = Arrival::Owner(Box::new(OwnerEvent::Settings(Box::new(
        SettingsEvent::Memories(screen.scoped.clone(), Ok(memories())),
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

#[test]
fn hidden_characters_in_memory_text_show_as_escapes() {
    let hidden = "Hourly\u{e0049}\u{200b} exports";
    let shown = "Hourly\\u{e0049}\\u200b exports";
    let raw = |lines: &[String]| {
        lines
            .iter()
            .any(|line| line.contains('\u{e0049}') || line.contains('\u{200b}'))
    };
    let mut ctx = memory_ctx(120, 36);
    let mut screen = Memory::new();
    screen.shown(&mut ctx);
    while ctx.next_request().is_some() {}
    screen.arrive(
        &settings(SettingsEvent::Memories(
            screen.scoped.clone(),
            Ok(vec![saved(
                1,
                hidden,
                MemoryScope::Global,
                MemoryOrigin::Written,
            )]),
        )),
        &mut ctx,
    );
    screen.arrive(
        &settings(SettingsEvent::Suggestions(Ok(vec![suggestion(
            "tidepool", hidden,
        )]))),
        &mut ctx,
    );
    let (lines, _) = screen.body(116, &ctx);
    let rows = texts(&lines);
    assert!(
        rows.iter().filter(|line| line.contains(shown)).count() >= 3,
        "{rows:#?}"
    );
    assert!(!raw(&rows), "{rows:#?}");
    screen.key(&Key::Enter, &mut ctx);
    let (modal, _) = screen.modal(&ctx).unwrap();
    let body = texts(&modal.body);
    assert!(body.iter().any(|line| line.contains(shown)), "{body:#?}");
    assert!(!raw(&body));
    screen.key(&Key::Esc, &mut ctx);
    screen.key(&Key::Down, &mut ctx);
    screen.key(&Key::Enter, &mut ctx);
    let (modal, _) = screen.modal(&ctx).unwrap();
    assert_eq!(modal.title, "Memory");
    let body = texts(&modal.body);
    assert!(body.iter().any(|line| line.contains(shown)), "{body:#?}");
    assert!(!raw(&body));
}

#[test]
fn a_memory_kept_in_a_project_can_be_used_in_all_projects() {
    let mut ctx = memory_ctx(120, 36);
    let mut screen = Memory::new();
    screen.shown(&mut ctx);
    while ctx.next_request().is_some() {}
    screen.arrive(
        &settings(SettingsEvent::Memories(
            screen.scoped.clone(),
            Ok(memories()),
        )),
        &mut ctx,
    );
    let found = vec![
        kept(
            "tidepool",
            "Buoy firmware sends readings every 90 s in CBOR.",
        ),
        kept("fieldnotes", "Crews sync at dawn."),
    ];
    screen.arrive(
        &settings(SettingsEvent::Suggestions(Ok(found.clone()))),
        &mut ctx,
    );
    let (lines, _) = screen.body(116, &ctx);
    let shown = texts(&lines);
    assert!(
        shown[0].contains("7 saved   1 kept in a project"),
        "one already used in all projects is not offered again"
    );
    assert!(shown[6].contains("Crews sync at dawn.") && shown[6].contains("Fieldnotes Mobile"));
    assert!(
        shown
            .iter()
            .any(|line| line.contains("Kept for Fieldnotes Mobile only."))
    );

    screen.key(&Key::Enter, &mut ctx);
    assert_eq!(screen.modal(&ctx).unwrap().0.title, "Use in all projects");
    screen.key(&Key::Char('n'), &mut ctx);
    assert!(
        screen.modal(&ctx).is_none(),
        "a kept memory is never dismissed"
    );
    screen.key(&Key::Enter, &mut ctx);
    screen.key(&Key::Left, &mut ctx);
    screen.key(&Key::Enter, &mut ctx);
    assert!(screen.modal(&ctx).is_none(), "Cancel closes");
    assert_eq!(ctx.queued(), 0);

    screen.key(&Key::Enter, &mut ctx);
    screen.key(&Key::Char('y'), &mut ctx);
    match job(&mut ctx) {
        SettingsJob::Suggestion {
            suggestion: picked,
            keep,
            ..
        } => {
            assert_eq!(picked, found[1]);
            assert!(keep);
        }
        other => panic!("suggestion expected, got {other:?}"),
    }
    screen.arrive(
        &settings(SettingsEvent::SuggestionDone(found[1].clone(), Ok(()))),
        &mut ctx,
    );
    assert_eq!(ctx.message().unwrap().lead, "Now used in all projects.");
}

#[test]
fn an_imported_memory_names_its_file_and_project() {
    let ctx = memory_ctx(80, 24);
    let imported = saved(
        9,
        "Plan meals on Sunday.",
        only("tidepool"),
        MemoryOrigin::Imported {
            file: "meals.md".into(),
        },
    );
    assert_eq!(source(&imported, &ctx), "Tidepool");
    assert_eq!(
        saved_line(&imported, &ctx),
        "Imported from meals.md for Tidepool only."
    );
    for code in ["memory_import_folder_invalid", "memory_import_invalid"] {
        assert!(!memory_plain(code).contains('_'), "{code}");
    }
}

#[test]
fn w_changes_agent_memory_writes_and_y_approves_only_a_memory_an_agent_asked_for() {
    let mut ctx = memory_ctx(120, 36);
    let mut list = memories();
    list.insert(
        0,
        saved(
            8,
            "Ship on Fridays.",
            MemoryScope::Global,
            MemoryOrigin::Requested {
                agent: "Ada (@lead-a)".into(),
            },
        ),
    );
    let mut screen = Memory::with(list);

    screen.key(&Key::Char('w'), &mut ctx);
    assert!(matches!(
        job(&mut ctx),
        SettingsJob::AgentWrites {
            set: Some((AgentWrites::Ask, generation))
        } if generation > 0
    ));
    screen.key(&Key::Char('w'), &mut ctx);
    assert_eq!(ctx.queued(), 0, "one change at a time");
    screen.arrive(
        &settings(SettingsEvent::AgentWrites(Ok(AgentWrites::Ask))),
        &mut ctx,
    );
    assert_eq!(
        ctx.message().unwrap().lead,
        "Agents now ask you before a memory is saved."
    );
    screen.key(&Key::Char('w'), &mut ctx);
    assert!(matches!(
        job(&mut ctx),
        SettingsJob::AgentWrites {
            set: Some((AgentWrites::Save, _))
        }
    ));
    screen.arrive(
        &settings(SettingsEvent::AgentWrites(Ok(AgentWrites::Save))),
        &mut ctx,
    );
    assert_eq!(
        ctx.message().unwrap().lead,
        "Agents now save memories directly."
    );

    screen.key(&Key::Char('y'), &mut ctx);
    match job(&mut ctx) {
        SettingsJob::ApproveMemory { memory, generation } => {
            assert_eq!(memory.as_str(), "memory-8");
            assert!(generation > 0);
        }
        other => panic!("approve expected, got {other:?}"),
    }
    screen.key(&Key::Char('y'), &mut ctx);
    assert_eq!(ctx.queued(), 0, "one approval at a time");
    screen.arrive(&settings(SettingsEvent::MemoryApproved(Ok(()))), &mut ctx);
    assert_eq!(
        ctx.message().unwrap().lead,
        "Memory approved. Agents get it from their next launch."
    );
    assert!(
        matches!(job(&mut ctx), SettingsJob::Memories { .. }),
        "the list reloads"
    );
    while ctx.next_request().is_some() {}

    screen.key(&Key::Down, &mut ctx);
    screen.key(&Key::Char('y'), &mut ctx);
    assert_eq!(ctx.queued(), 0, "a memory nobody asked for is not approved");
}

#[test]
fn the_agent_memory_writes_row_shows_the_engine_choice_and_how_to_change_it() {
    let mut shell = Shell::with(memory_ctx(120, 36), Box::new(Memory::with(memories())));
    let row = |shell: &mut Shell| {
        render(shell, 120, 36)
            .into_iter()
            .find(|row| row.contains("Agent memory writes"))
            .unwrap_or_else(|| panic!("no Agent memory writes row"))
    };
    assert!(row(&mut shell).contains("Loading"), "{}", row(&mut shell));
    for (writes, chosen) in [
        (AgentWrites::Ask, "Ask me first"),
        (AgentWrites::Save, "Save directly"),
    ] {
        shell.arrive(settings(SettingsEvent::AgentWrites(Ok(writes))));
        let shown = row(&mut shell);
        assert!(shown.contains(chosen), "{shown}");
        assert!(shown.contains("w to change"), "{shown}");
    }
}

/// F34: `m` on an app-wide memory an agent saved opens Move memory, the arrows pick the
/// project and Enter sends one move; the list reloads once it lands, and an owner's own note
/// never opens it.
#[test]
fn m_moves_an_app_wide_agent_memory_into_the_chosen_project() {
    let mut ctx = memory_ctx(120, 36);
    let mut list = memories();
    list.insert(
        0,
        saved(
            8,
            "Parser gotcha.",
            MemoryScope::Global,
            MemoryOrigin::Agent {
                agent: "Master".into(),
            },
        ),
    );
    let mut screen = Memory::with(list);

    screen.key(&Key::Char('m'), &mut ctx);
    assert!(matches!(screen.mode, Mode::Move { project: 0 }));
    let shown = screen.modal(&ctx).unwrap().0;
    assert_eq!(shown.title, "Move memory");
    screen.key(&Key::Right, &mut ctx);
    assert!(matches!(screen.mode, Mode::Move { project: 1 }));
    screen.key(&Key::Enter, &mut ctx);
    match job(&mut ctx) {
        SettingsJob::MoveMemory {
            memory,
            project,
            generation,
        } => {
            assert_eq!(memory.as_str(), "memory-8");
            assert_eq!(project.as_str(), "ledgerline");
            assert!(generation > 0);
        }
        other => panic!("move expected, got {other:?}"),
    }
    screen.key(&Key::Char('m'), &mut ctx);
    assert!(matches!(screen.mode, Mode::List), "one move at a time");
    screen.arrive(&settings(SettingsEvent::MemoryMoved(Ok(()))), &mut ctx);
    assert_eq!(
        ctx.message().unwrap().lead,
        "Memory moved. Only agents in that project get it now."
    );
    assert!(
        matches!(job(&mut ctx), SettingsJob::Memories { .. }),
        "the list reloads"
    );
    while ctx.next_request().is_some() {}

    screen.key(&Key::Char('m'), &mut ctx);
    screen.key(&Key::Esc, &mut ctx);
    assert!(matches!(screen.mode, Mode::List), "Esc cancels");
    assert_eq!(ctx.queued(), 0);
    screen.key(&Key::Down, &mut ctx);
    screen.key(&Key::Char('m'), &mut ctx);
    assert!(
        matches!(screen.mode, Mode::List),
        "an owner's own note is never moved"
    );
}

/// F34: the memory settings row shows where agents save, `p` switches between This project
/// only and Agent chooses one change at a time, and each answer says what agents now do.
#[test]
fn p_switches_where_agents_save_memories_between_both_options() {
    let mut shell = Shell::with(memory_ctx(120, 36), Box::new(Memory::with(memories())));
    let row = |shell: &mut Shell| {
        render(shell, 120, 36)
            .into_iter()
            .find(|row| row.contains("Saved in"))
            .unwrap_or_else(|| panic!("no Saved in row"))
    };
    for (reach, chosen) in [
        (AgentScope::Project, "This project only"),
        (AgentScope::Agent, "Agent chooses"),
    ] {
        shell.arrive(settings(SettingsEvent::AgentScope(Ok(reach))));
        let shown = row(&mut shell);
        assert!(shown.contains(chosen), "{shown}");
        assert!(shown.contains("p to change"), "{shown}");
    }

    let mut ctx = memory_ctx(120, 36);
    let mut screen = Memory::with(memories());
    screen.key(&Key::Char('p'), &mut ctx);
    assert!(matches!(
        job(&mut ctx),
        SettingsJob::AgentScope {
            set: Some((AgentScope::Agent, generation))
        } if generation > 0
    ));
    screen.key(&Key::Char('p'), &mut ctx);
    assert_eq!(ctx.queued(), 0, "one change at a time");
    screen.arrive(
        &settings(SettingsEvent::AgentScope(Ok(AgentScope::Agent))),
        &mut ctx,
    );
    assert_eq!(
        ctx.message().unwrap().lead,
        "Agents now choose where a memory is saved, app-wide included."
    );
    screen.key(&Key::Char('p'), &mut ctx);
    assert!(matches!(
        job(&mut ctx),
        SettingsJob::AgentScope {
            set: Some((AgentScope::Project, _))
        }
    ));
    screen.arrive(
        &settings(SettingsEvent::AgentScope(Ok(AgentScope::Project))),
        &mut ctx,
    );
    assert_eq!(
        ctx.message().unwrap().lead,
        "Agents now save memories in their own project only."
    );
}

fn fieldnotes_memory(memories: Vec<SavedMemory>) -> Memory {
    let mut screen = Memory::with(memories);
    screen.project = Some((
        ProjectId::new("fieldnotes").unwrap(),
        "Fieldnotes Mobile".into(),
    ));
    screen
}

fn asked(ordinal: u64, text: &str) -> SavedMemory {
    saved(
        ordinal,
        text,
        only("fieldnotes"),
        MemoryOrigin::Requested {
            agent: "Ada (@lead-a)".into(),
        },
    )
}

#[test]
fn a_project_memory_reads_only_its_project_and_y_approves_a_requested_memory_in_place() {
    let mut ctx = memory_ctx(120, 36);
    let before = ctx.store.memory.clone();
    let fieldnotes = ProjectId::new("fieldnotes").unwrap();
    let mut screen = Memory::for_project(fieldnotes.clone(), "Fieldnotes Mobile".into());
    screen.shown(&mut ctx);
    assert!(matches!(ctx.next_request(), Some(Request::Projects)));
    match job(&mut ctx) {
        SettingsJob::Memories { scopes } => assert_eq!(scopes, [only("fieldnotes")]),
        other => panic!("memories expected, got {other:?}"),
    }
    assert!(
        ctx.next_request().is_none(),
        "the app-wide Agent memory writes and Where agents save settings and research suggestions stay in Settings"
    );

    let list = vec![
        asked(9, "Ship on Fridays."),
        saved(
            8,
            "Field crews sync at dusk.",
            only("fieldnotes"),
            MemoryOrigin::Written,
        ),
    ];
    screen.arrive(
        &settings(SettingsEvent::Memories(screen.scoped.clone(), Ok(list))),
        &mut ctx,
    );
    assert_eq!(
        ctx.store.memory, before,
        "one project's count never stands for every memory"
    );
    let shown = texts(&screen.body(110, &ctx).0).join("\n");
    assert!(shown.starts_with("Project memory"), "{shown}");
    assert!(
        shown.contains("1 memory waits for your approval. y approves the focused one."),
        "{shown}"
    );
    assert!(shown.contains(PROJECT_SUBTITLE), "{shown}");
    assert!(!shown.contains("Agent memory writes"), "{shown}");
    assert!(!shown.contains("Settings, Memory"), "{shown}");
    assert_eq!(
        screen.crumbs(),
        ["Fieldnotes Mobile".to_owned(), "Project memory".to_owned()]
    );
    assert_eq!(
        screen.place(),
        Some(Place::ProjectMemory {
            id: fieldnotes.clone(),
            name: "Fieldnotes Mobile".into(),
        })
    );
    assert_eq!(screen.project(), Some(fieldnotes));
    assert!(screen.hints(&ctx).contains(&hint("y", "Approve")));

    screen.key(&Key::Char('y'), &mut ctx);
    match job(&mut ctx) {
        SettingsJob::ApproveMemory { memory, generation } => {
            assert_eq!(memory.as_str(), "memory-9");
            assert!(generation > 0);
        }
        other => panic!("approve expected, got {other:?}"),
    }
    screen.arrive(&settings(SettingsEvent::MemoryApproved(Ok(()))), &mut ctx);
    assert_eq!(
        ctx.message().unwrap().lead,
        "Memory approved. Agents get it from their next launch."
    );
    match job(&mut ctx) {
        SettingsJob::Memories { scopes } => {
            assert_eq!(
                scopes,
                [only("fieldnotes")],
                "the reload stays in the project"
            )
        }
        other => panic!("reload expected, got {other:?}"),
    }
}

#[test]
fn a_project_memory_leaves_app_wide_keys_to_settings_and_s_opens_memory_settings() {
    let mut ctx = memory_ctx(120, 36);
    let mut screen = fieldnotes_memory(vec![asked(9, "Ship on Fridays.")]);
    for key in ['w', 'a', 'i', 'p', 'm'] {
        assert!(
            matches!(screen.key(&Key::Char(key), &mut ctx), Flow::Ignored),
            "{key}"
        );
        assert_eq!(ctx.queued(), 0, "{key}");
        assert!(matches!(screen.mode, Mode::List), "{key}");
    }
    assert!(matches!(screen.step(true, &mut ctx), Flow::Ignored));
    assert!(matches!(
        screen.key(&Key::Char('s'), &mut ctx),
        Flow::Open(Place::Settings(SettingsSection::Memory))
    ));
    let hints = screen.hints(&ctx);
    assert!(hints.contains(&hint("s", "Memory settings")), "{hints:?}");
    assert!(hints.contains(&hint("Esc", "Back")), "{hints:?}");
    for app_wide in ["a", "w", "i", "p", "m"] {
        assert!(
            !hints.iter().any(|shown| shown.key == app_wide),
            "{app_wide}: {hints:?}"
        );
    }

    let mut app = Memory::with(vec![asked(9, "Ship on Fridays.")]);
    assert!(matches!(app.key(&Key::Char('s'), &mut ctx), Flow::Ignored));
    assert!(app.hints(&ctx).contains(&hint("w", "Agent memory writes")));
}

#[test]
fn a_project_memory_counts_one_or_many_waiting_and_says_when_none_wait() {
    let ctx = memory_ctx(120, 36);
    let line = |memories: Vec<SavedMemory>| {
        texts(&fieldnotes_memory(memories).body(110, &ctx).0)[2].clone()
    };
    assert_eq!(
        line(vec![saved(
            1,
            "Kept.",
            only("fieldnotes"),
            MemoryOrigin::Written
        )]),
        "Nothing waits for your approval."
    );
    assert_eq!(
        line(vec![
            asked(2, "One."),
            saved(1, "Kept.", only("fieldnotes"), MemoryOrigin::Written)
        ]),
        "1 memory waits for your approval. y approves the focused one."
    );
    assert_eq!(
        line(vec![asked(3, "One."), asked(2, "Two.")]),
        "2 memories wait for your approval. y approves the focused one."
    );
    let empty = texts(&fieldnotes_memory(Vec::new()).body(110, &ctx).0).join(" ");
    assert!(empty.starts_with("Project memory"), "{empty}");
    assert!(empty.contains("No memories yet."), "{empty}");
    assert!(empty.contains("it waits here for your approval"), "{empty}");
}

#[test]
fn a_project_memory_keeps_only_its_own_reply_and_its_own_project_and_rereads_on_return() {
    let mut ctx = memory_ctx(120, 36);
    let mut screen = Memory::for_project(
        ProjectId::new("fieldnotes").unwrap(),
        "Fieldnotes Mobile".into(),
    );
    screen.shown(&mut ctx);
    while ctx.next_request().is_some() {}
    let mut mixed = memories();
    mixed.insert(0, asked(9, "Ship on Fridays."));
    screen.arrive(
        &settings(SettingsEvent::Memories(
            screen.scoped.clone(),
            Ok(mixed.clone()),
        )),
        &mut ctx,
    );
    assert!(
        screen
            .memories
            .iter()
            .all(|memory| memory.scope == only("fieldnotes"))
    );
    assert!(
        screen
            .memories
            .iter()
            .any(|memory| memory.id.as_str() == "memory-9")
    );
    assert!(screen.memories.len() < mixed.len());
    let ids = |screen: &Memory| -> Vec<String> {
        screen
            .memories
            .iter()
            .map(|memory| memory.id.as_str().to_owned())
            .collect()
    };
    let kept = ids(&screen);
    screen.arrive(
        &settings(SettingsEvent::Memories(
            screen.scoped.clone(),
            Ok(Vec::new()),
        )),
        &mut ctx,
    );
    assert_eq!(
        ids(&screen),
        kept,
        "a reply this screen never asked for belongs to another Memory screen"
    );
    screen.arrive(
        &settings(SettingsEvent::Memories(
            screen.scoped.clone(),
            Err("engine_unreachable".into()),
        )),
        &mut ctx,
    );
    assert!(matches!(screen.load, Load::Ready(())));
    assert!(ctx.message().is_none());

    screen.shown(&mut ctx);
    assert!(matches!(ctx.next_request(), Some(Request::Projects)));
    match job(&mut ctx) {
        SettingsJob::Memories { scopes } => assert_eq!(scopes, [only("fieldnotes")]),
        other => panic!("a fresh read expected on return, got {other:?}"),
    }
}

#[test]
fn stacked_memory_screens_each_take_only_the_reply_for_the_scopes_they_asked_for() {
    let mut ctx = memory_ctx(120, 36);
    let mut project = Memory::for_project(
        ProjectId::new("fieldnotes").unwrap(),
        "Fieldnotes Mobile".into(),
    );
    project.shown(&mut ctx);
    let mut app = Memory::new();
    app.shown(&mut ctx);
    while ctx.next_request().is_some() {}
    assert!(project.waiting && app.waiting);
    assert_ne!(project.scoped, app.scoped);

    let mine = settings(SettingsEvent::Memories(
        project.scoped.clone(),
        Ok(vec![asked(9, "Ship on Fridays.")]),
    ));
    app.arrive(&mine, &mut ctx);
    project.arrive(&mine, &mut ctx);
    assert!(
        app.waiting && app.memories.is_empty(),
        "Settings, Memory waits on for its own reply (PROJMEM-L2)"
    );
    assert!(!matches!(app.load, Load::Ready(())));
    assert_eq!(project.memories.len(), 1);

    let all = settings(SettingsEvent::Memories(app.scoped.clone(), Ok(memories())));
    app.arrive(&all, &mut ctx);
    project.arrive(&all, &mut ctx);
    assert_eq!(app.memories.len(), memories().len());
    assert_eq!(ctx.store.memory, Load::Ready(memories().len()));
    assert_eq!(
        project
            .memories
            .iter()
            .map(|memory| memory.id.as_str())
            .collect::<Vec<_>>(),
        ["memory-9"],
        "Project memory keeps its own list"
    );
}
