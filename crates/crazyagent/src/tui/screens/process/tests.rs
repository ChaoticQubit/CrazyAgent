use super::*;
use crate::tui::client::settings::{SettingsEvent, SettingsJob};
use crate::tui::screens::settings_accounts::tests::{ctx, render};
use crate::tui::shell::Shell;

fn reply(query: ProcessQuery, result: Result<&str, &str>) -> Arrival {
    Arrival::Owner(Box::new(OwnerEvent::Process(Box::new(ProcessEvent {
        query,
        result: result
            .map(|text| ProcessReply::Text(text.to_owned()))
            .map_err(str::to_owned),
    }))))
}

fn saved(query: ProcessQuery, bytes: u32) -> Arrival {
    Arrival::Owner(Box::new(OwnerEvent::Process(Box::new(ProcessEvent {
        query,
        result: Ok(ProcessReply::Saved(crate::engine::owner::ProcessSaved {
            bytes,
            claude_bytes_max: 28_159,
        })),
    }))))
}

fn requests(shell: &mut Shell) -> Vec<ProcessQuery> {
    let mut sent = Vec::new();
    while let Some(request) = shell.ctx.next_request() {
        if let Request::Process(query) = request {
            sent.push(*query);
        }
    }
    sent
}

fn opened(scope: Scope, saved: &str) -> Shell {
    let project = match &scope {
        Scope::App => None,
        Scope::Project { id, .. } => Some(id.clone()),
    };
    let mut shell = Shell::with(ctx(80, 24), Box::new(ProcessPage::new(scope)));
    let read = ProcessQuery::Read {
        project: project.clone(),
    };
    assert_eq!(requests(&mut shell), vec![read.clone()]);
    shell.arrive(reply(read, Ok(saved)));
    if let Some(project) = project {
        let query = ProcessQuery::Settings { project };
        assert_eq!(requests(&mut shell), vec![query.clone()]);
        shell.arrive(settings(query, Ok(InstructionSettings::default())));
    }
    shell
}

fn settings(query: ProcessQuery, result: Result<InstructionSettings, &str>) -> Arrival {
    Arrival::Owner(Box::new(OwnerEvent::Process(Box::new(ProcessEvent {
        query,
        result: result.map(ProcessReply::Settings).map_err(str::to_owned),
    }))))
}

fn screen(shell: &mut Shell) -> String {
    render(shell, 80, 24).join("\n")
}

/// Asks to quit and reads whether the shell warns about the unsaved instructions, then
/// stays. Input: the shell. Output: true when the warning showed.
fn asks_before_quit(shell: &mut Shell) -> bool {
    shell.key(Key::Ctrl('q'));
    let warned = screen(shell).contains("Your instructions changes will be lost.");
    shell.key(Key::Esc);
    warned
}

fn typed(shell: &mut Shell, text: &str) {
    for character in text.chars() {
        shell.key(Key::Char(character));
    }
}

#[test]
fn the_editor_keeps_every_character_and_edits_across_lines() {
    let original = "one\ttab\r\nkeep\n\nlast";
    let mut editor = Editor::new(original);
    assert_eq!(editor.text(), original);
    assert_eq!(editor.bytes(), original.len());

    editor.key(&Key::Down);
    editor.key(&Key::End);
    editor.key(&Key::Paste("s\r\nnew\rline".into()));
    assert_eq!(editor.text(), "one\ttab\r\nkeeps\nnew\nline\n\nlast");
    editor.key(&Key::Home);
    editor.key(&Key::Backspace);
    assert_eq!(editor.text(), "one\ttab\r\nkeeps\nnewline\n\nlast");
    editor.key(&Key::End);
    editor.key(&Key::Delete);
    editor.key(&Key::Delete);
    assert_eq!(editor.text(), "one\ttab\r\nkeeps\nnewlinelast");
    editor.key(&Key::Right);
    editor.key(&Key::Enter);
    assert_eq!(editor.text(), "one\ttab\r\nkeeps\nnewlinel\nast");
    for _ in 0..40 {
        editor.key(&Key::Left);
    }
    assert_eq!((editor.row, editor.column), (0, 0));
    assert!(!editor.key(&Key::Tab));
    assert_eq!(editor.bytes(), editor.text().len());
}

#[test]
fn the_editor_shows_control_and_escape_characters_as_marks_and_follows_the_cursor() {
    let mut editor = Editor::new("a\u{1b}[31mred\u{200b}\tz\nsecond");
    let (lines, cursor) = editor.view(40, 5);
    assert_eq!(lines, vec!["a?[31mred? z".to_owned(), "second".to_owned()]);
    assert_eq!(cursor, (0, 0));
    assert!(lines.iter().all(|line| !line.chars().any(char::is_control)));

    let long: String = (0..30).map(|row| format!("line {row}\n")).collect();
    let mut editor = Editor::new(&format!("{long}{}", "x".repeat(50)));
    editor.key(&Key::PageDown);
    editor.key(&Key::PageDown);
    editor.key(&Key::PageDown);
    editor.key(&Key::PageDown);
    editor.key(&Key::End);
    let (lines, (column, row)) = editor.view(20, 4);
    assert_eq!(lines.len(), 4);
    assert_eq!(row, 3, "the cursor line is the last one on screen");
    assert_eq!(lines[3], "x".repeat(19));
    assert_eq!(column, 19);
    assert_eq!(editor.top, 27);
}

#[test]
fn end_on_a_pasted_100k_character_line_scrolls_at_once_and_keeps_wide_characters_whole() {
    let mut editor = Editor::new("");
    editor.key(&Key::Paste("x".repeat(100_000)));
    editor.key(&Key::Home);
    let (_, cursor) = editor.view(80, 10);
    assert_eq!(cursor, (0, 0));
    let started = std::time::Instant::now();
    editor.key(&Key::End);
    let (lines, cursor) = editor.view(80, 10);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(1),
        "End on a long line took {:?}",
        started.elapsed()
    );
    assert_eq!(cursor, (79, 0));
    assert_eq!(editor.left, 100_000 - 79);
    assert_eq!(lines[0], "x".repeat(79));

    let mut wide = Editor::new(&"\u{4e2d}".repeat(50));
    wide.key(&Key::End);
    let (lines, (column, _)) = wide.view(21, 1);
    assert_eq!(
        column, 20,
        "ten wide characters fill the cells before the cursor"
    );
    assert_eq!(wide.left, 40);
    assert_eq!(lines[0], "\u{4e2d}".repeat(10));
}

#[test]
fn the_app_page_reads_the_saved_text_and_saves_only_on_ctrl_s() {
    let mut shell = opened(Scope::App, "Use tdd.\n");
    let shown = screen(&mut shell);
    assert!(shown.contains("Settings  /  Process"), "{shown}");
    assert!(
        shown.contains("│ █se tdd."),
        "the cursor starts at the top: {shown}"
    );
    assert!(shown.contains("9 bytes of 64.0 KB"), "{shown}");
    assert!(!shown.contains("Not saved yet"), "{shown}");

    shell.key(Key::Ctrl('s'));
    assert_eq!(
        requests(&mut shell),
        vec![],
        "nothing changed, nothing saved"
    );
    shell.key(Key::Down);
    typed(&mut shell, "Ask the master.");
    assert!(screen(&mut shell).contains("Not saved yet"));
    assert!(asks_before_quit(&mut shell));
    assert_eq!(requests(&mut shell), vec![], "typing never saves");

    shell.key(Key::Ctrl('s'));
    let save = ProcessQuery::Save {
        project: None,
        text: "Use tdd.\nAsk the master.".into(),
    };
    assert_eq!(requests(&mut shell), vec![save.clone()]);
    shell.key(Key::Ctrl('s'));
    assert_eq!(requests(&mut shell), vec![], "one request at a time");
    shell.arrive(saved(save, 24));
    assert!(!screen(&mut shell).contains("Not saved yet"));
}

#[test]
fn a_save_a_claude_master_may_refuse_warns_with_the_limit() {
    let mut shell = opened(Scope::App, "Use tdd.\n");
    typed(&mut shell, "Ask the master. ");
    shell.key(Key::Ctrl('s'));
    let save = ProcessQuery::Save {
        project: None,
        text: "Ask the master. Use tdd.\n".into(),
    };
    assert_eq!(requests(&mut shell), vec![save.clone()]);
    shell.arrive(saved(save, 40_000));
    let shown = screen(&mut shell);
    assert!(
        !shown.contains("Not saved yet"),
        "the text is saved: {shown}"
    );
    assert!(
        shown.contains("Instructions saved. Shorten: a Claude master takes 27.5 KB, not 39.1 KB."),
        "the save names what a Claude master takes and what agents get: {shown}"
    );
    assert!(!shown.contains("Agents started from now on"), "{shown}");

    typed(&mut shell, "Short. ");
    shell.key(Key::Ctrl('s'));
    let save = ProcessQuery::Save {
        project: None,
        text: "Ask the master. Short. Use tdd.\n".into(),
    };
    assert_eq!(requests(&mut shell), vec![save.clone()]);
    shell.arrive(saved(save, 28_159));
    let shown = screen(&mut shell);
    assert!(
        shown.contains("Instructions saved. Agents started from now on get them."),
        "exactly the limit is no warning: {shown}"
    );
    assert!(!shown.contains("Shorten"), "{shown}");
}

#[test]
fn a_save_the_full_queue_refuses_leaves_the_page_ready_to_save_again() {
    let mut shell = opened(Scope::App, "Use tdd.");
    shell.key(Key::End);
    typed(&mut shell, " Always.");
    let mut filled = 0;
    while filled < 64 && {
        shell.ctx.try_request(Request::EditPool {
            change: crate::tui::seam::PoolChange::Remove {
                id: crate::protocol::CredentialRef::new("pool-a").unwrap(),
                opened_projects: 0,
            },
        })
    } {
        filled += 1;
    }
    assert!(filled < 64, "the request queue is bounded");

    shell.key(Key::Ctrl('s'));
    assert_eq!(
        shell.ctx.message().map(|message| message.lead.as_str()),
        Some("Too many requests are waiting.")
    );
    assert!(!shell.busy(), "a refused save is not waiting on the engine");
    let shown = screen(&mut shell);
    assert!(!shown.contains("Saving..."), "{shown}");
    assert!(shown.contains("Not saved yet"), "{shown}");
    assert_eq!(
        requests(&mut shell),
        vec![],
        "the refused save never queued"
    );

    shell.key(Key::Ctrl('s'));
    let save = ProcessQuery::Save {
        project: None,
        text: "Use tdd. Always.".into(),
    };
    assert_eq!(
        requests(&mut shell),
        vec![save.clone()],
        "once the queue drains the same Ctrl-S saves"
    );
    assert!(shell.busy());
    shell.arrive(saved(save, 16));
    assert!(!screen(&mut shell).contains("Not saved yet"));
}

#[test]
fn a_first_read_the_full_queue_refuses_offers_a_retry_instead_of_loading_forever() {
    let mut ctx = ctx(80, 24);
    let mut filled = 0;
    while filled < 64 && {
        ctx.try_request(Request::EditPool {
            change: crate::tui::seam::PoolChange::Remove {
                id: crate::protocol::CredentialRef::new("pool-a").unwrap(),
                opened_projects: 0,
            },
        })
    } {
        filled += 1;
    }
    assert!(filled < 64, "the request queue is bounded");
    let mut shell = Shell::with(ctx, Box::new(ProcessPage::new(Scope::App)));
    assert_eq!(
        requests(&mut shell),
        vec![],
        "the refused read never queued"
    );
    assert!(!shell.busy(), "a refused read is not waiting on the engine");
    let shown = screen(&mut shell);
    assert!(!shown.contains("Loading the instructions"), "{shown}");
    assert!(shown.contains("Press r to try again."), "{shown}");

    shell.key(Key::Char('r'));
    let read = ProcessQuery::Read { project: None };
    assert_eq!(requests(&mut shell), vec![read.clone()]);
    shell.arrive(reply(read, Ok("Back.")));
    assert!(screen(&mut shell).contains("│ █ack."));
}

#[test]
fn a_many_line_paste_into_a_long_text_inserts_in_one_pass() {
    let mut editor = Editor::new(&"\n".repeat(99_999));
    assert_eq!(editor.lines.len(), 100_000);
    let started = std::time::Instant::now();
    editor.key(&Key::Paste("x\n".repeat(50_000)));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(1),
        "a 50,000-line paste took {:?}",
        started.elapsed()
    );
    assert_eq!(editor.lines.len(), 150_000);
    assert_eq!((editor.row, editor.column), (50_000, 0));
    assert!(editor.lines[..50_000].iter().all(|line| line == "x"));
    assert!(editor.lines[50_000..].iter().all(String::is_empty));
    assert_eq!(
        editor.text(),
        format!("{}{}", "x\n".repeat(50_000), "\n".repeat(99_999))
    );

    let mut middle = Editor::new("ab\ncd");
    middle.key(&Key::Right);
    middle.key(&Key::Paste("1\r\n2\r3".into()));
    assert_eq!(middle.text(), "a1\n2\n3b\ncd");
    assert_eq!((middle.row, middle.column), (2, 1));
}

#[test]
fn the_owners_global_instructions_are_read_only_on_ctrl_g_and_land_only_after_a_preview() {
    let mut shell = opened(Scope::App, "Saved rule.");
    typed(&mut shell, "gd");
    shell.key(Key::Ctrl('d'));
    assert_eq!(requests(&mut shell), vec![ProcessQuery::Default]);
    shell.arrive(reply(
        ProcessQuery::Default,
        Ok("# CrazyAgent default process\n"),
    ));
    assert!(screen(&mut shell).contains("CrazyAgent default process"));
    shell.key(Key::Esc);
    let kept = screen(&mut shell);
    assert!(
        kept.contains("gd█aved rule."),
        "Esc keeps the typed draft: {kept}"
    );
    assert!(kept.contains("Not saved yet"), "{kept}");

    shell.key(Key::Ctrl('g'));
    assert_eq!(
        requests(&mut shell),
        vec![ProcessQuery::ReadFile {
            path: "/owner/CLAUDE.md".into()
        }]
    );
    shell.arrive(reply(
        ProcessQuery::ReadFile {
            path: "/owner/CLAUDE.md".into(),
        },
        Ok("OWNER-GLOBAL-RULE\n"),
    ));
    let preview = screen(&mut shell);
    assert!(preview.contains("Your global instructions"), "{preview}");
    assert!(
        preview.contains("Read from ~/.claude/CLAUDE.md."),
        "{preview}"
    );
    assert!(preview.contains("OWNER-GLOBAL-RULE"), "{preview}");
    typed(&mut shell, "zz");
    shell.key(Key::Enter);
    assert!(screen(&mut shell).contains("│ █WNER-GLOBAL-RULE"));
    assert!(!screen(&mut shell).contains("Saved rule"));
    assert_eq!(
        requests(&mut shell),
        vec![],
        "putting it in the editor saves nothing"
    );

    shell.key(Key::Ctrl('g'));
    shell.arrive(reply(
        ProcessQuery::ReadFile {
            path: "/owner/CLAUDE.md".into(),
        },
        Err("process_import_missing"),
    ));
    assert!(screen(&mut shell).contains("There is no ~/.claude/CLAUDE.md to import."));
}

#[test]
fn a_project_page_has_no_seeds_and_reads_and_saves_its_own_text() {
    let project = ProjectId::new("harbor-notes").unwrap();
    let scope = Scope::Project {
        id: project.clone(),
        name: "Harbor Notes".into(),
    };
    let mut shell = opened(scope, "");
    let shown = screen(&mut shell);
    assert!(shown.contains("Harbor Notes  /  Process"), "{shown}");
    assert!(shown.contains("Process for Harbor Notes"), "{shown}");
    assert!(shown.contains(".crazyagent/instructions.md"), "{shown}");
    assert!(!shown.contains("Ctrl-G"), "{shown}");
    shell.key(Key::Ctrl('g'));
    shell.key(Key::Ctrl('d'));
    assert_eq!(requests(&mut shell), vec![]);
    typed(&mut shell, "Run the linter.");
    shell.key(Key::Ctrl('s'));
    let save = ProcessQuery::Save {
        project: Some(project.clone()),
        text: "Run the linter.".into(),
    };
    assert_eq!(requests(&mut shell), vec![save.clone()]);
    shell.arrive(reply(save, Err("process_project_folder_missing")));
    assert!(screen(&mut shell).contains("without a .crazyagent folder"));
    assert!(
        screen(&mut shell).contains("Not saved yet"),
        "a refused save keeps the draft"
    );
    assert_eq!(
        shell.place(),
        Some(Place::ProjectProcess {
            id: project,
            name: "Harbor Notes".into()
        })
    );
}

#[test]
fn a_project_page_offers_every_agent_setting_and_saves_each_change_at_once() {
    let project = ProjectId::new("harbor-notes").unwrap();
    let scope = Scope::Project {
        id: project.clone(),
        name: "Harbor Notes".into(),
    };
    let mut shell = opened(scope, "");
    let shown = screen(&mut shell);
    assert!(
        shown.contains("Agent instructions: Both (app, then project appended)"),
        "{shown}"
    );
    assert!(
        shown.contains("Keep transcripts for headless Claude runs: On"),
        "{shown}"
    );
    assert!(shown.contains("Ctrl-F changes"), "{shown}");
    assert!(shown.contains("instructions.md appended"), "{shown}");
    let mut stored = InstructionSettings::default();
    for (mode, words) in [
        (Mode::ProjectOnly, "Agent instructions: Project only"),
        (Mode::AppOnly, "Agent instructions: App only"),
        (
            Mode::Both,
            "Agent instructions: Both (app, then project appended)",
        ),
    ] {
        shell.key(Key::Ctrl('f'));
        stored.mode = mode;
        let save = ProcessQuery::SaveSettings {
            project: project.clone(),
            settings: stored,
        };
        assert_eq!(requests(&mut shell), vec![save.clone()]);
        shell.key(Key::Ctrl('f'));
        assert_eq!(requests(&mut shell), vec![], "one request at a time");
        shell.arrive(settings(save, Ok(stored)));
        let shown = screen(&mut shell);
        assert!(shown.contains(words), "{shown}");
        assert!(shown.contains("Agent settings saved."), "{shown}");
    }
    for transcripts in [false, true] {
        shell.key(Key::Ctrl('t'));
        stored.transcripts = transcripts;
        let save = ProcessQuery::SaveSettings {
            project: project.clone(),
            settings: stored,
        };
        assert_eq!(requests(&mut shell), vec![save.clone()]);
        shell.arrive(settings(save, Ok(stored)));
        let words = if transcripts { ": On" } else { ": Off" };
        assert!(
            screen(&mut shell)
                .contains(&format!("Keep transcripts for headless Claude runs{words}"))
        );
    }
    shell.key(Key::Ctrl('t'));
    let refused = ProcessQuery::SaveSettings {
        project: project.clone(),
        settings: InstructionSettings {
            mode: Mode::Both,
            transcripts: false,
        },
    };
    assert_eq!(requests(&mut shell), vec![refused.clone()]);
    shell.arrive(settings(refused, Err("instruction_settings_unavailable")));
    let shown = screen(&mut shell);
    assert!(
        shown.contains("Keep transcripts for headless Claude runs: On"),
        "{shown}"
    );
    assert!(shown.contains("couldn't be read"), "{shown}");
}

#[test]
fn unreadable_agent_settings_say_why_and_a_change_replaces_them_from_the_defaults() {
    let project = ProjectId::new("harbor-notes").unwrap();
    let mut shell = Shell::with(
        ctx(80, 24),
        Box::new(ProcessPage::new(Scope::Project {
            id: project.clone(),
            name: "Harbor Notes".into(),
        })),
    );
    let read = ProcessQuery::Read {
        project: Some(project.clone()),
    };
    assert_eq!(requests(&mut shell), vec![read.clone()]);
    shell.arrive(reply(read, Ok("")));
    let query = ProcessQuery::Settings {
        project: project.clone(),
    };
    assert_eq!(requests(&mut shell), vec![query.clone()]);
    shell.arrive(settings(query, Err("instruction_settings_invalid")));
    let shown = screen(&mut shell);
    assert!(shown.contains("value CrazyAgent doesn't know"), "{shown}");
    shell.key(Key::Ctrl('f'));
    assert_eq!(
        requests(&mut shell),
        vec![ProcessQuery::SaveSettings {
            project,
            settings: InstructionSettings {
                mode: Mode::ProjectOnly,
                transcripts: true,
            },
        }]
    );
}

#[test]
fn the_app_page_has_no_agent_settings() {
    let mut shell = opened(Scope::App, "");
    let shown = screen(&mut shell);
    assert!(!shown.contains("Agent instructions:"), "{shown}");
    shell.key(Key::Ctrl('f'));
    shell.key(Key::Ctrl('t'));
    assert_eq!(requests(&mut shell), vec![]);
}

#[test]
fn a_text_over_64_kb_says_why_and_is_never_sent() {
    let mut shell = opened(Scope::App, "");
    shell.key(Key::Paste("a".repeat(TEXT_BYTES_MAX + 1)));
    let shown = screen(&mut shell);
    assert!(shown.contains("This text is over 64 KB."), "{shown}");
    shell.key(Key::Ctrl('s'));
    assert_eq!(requests(&mut shell), vec![]);
    shell.key(Key::Backspace);
    assert!(!screen(&mut shell).contains("over 64 KB"));
}

#[test]
fn a_failed_read_offers_a_retry_and_a_stale_reply_is_ignored() {
    let mut shell = Shell::with(ctx(80, 24), Box::new(ProcessPage::new(Scope::App)));
    let read = ProcessQuery::Read { project: None };
    assert_eq!(requests(&mut shell), vec![read.clone()]);
    shell.arrive(reply(ProcessQuery::Default, Ok("stale")));
    assert!(screen(&mut shell).contains("Loading the instructions"));
    shell.arrive(reply(read.clone(), Err("process_text_unsafe")));
    let shown = screen(&mut shell);
    assert!(shown.contains("is a link or not a plain file"), "{shown}");
    assert!(shown.contains("Press r to try again."), "{shown}");
    shell.key(Key::Char('r'));
    assert_eq!(requests(&mut shell), vec![read.clone()]);
    shell.arrive(reply(read, Ok("Back.")));
    assert!(screen(&mut shell).contains("│ █ack."));
}

fn skill(id: &str, name: &str, line: &str) -> crate::plugins::SkillView {
    crate::plugins::SkillView {
        id: id.into(),
        name: name.into(),
        description: format!("{line} More detail."),
        line: line.into(),
        scope: crate::plugins::Scope::Global,
        scope_label: "Every project".into(),
        enabled: true,
        on_here: None,
        source: String::new(),
        folder: String::new(),
        refused: None,
    }
}

fn catalog(shell: &mut Shell) {
    let skills = vec![
        skill("skill-a", "tdd", "Write the failing test first."),
        skill("skill-b", "code-review", "Review a diff for bugs."),
    ];
    shell.arrive(Arrival::Owner(Box::new(OwnerEvent::Settings(Box::new(
        SettingsEvent::Plugins(Ok((skills, Vec::new()))),
    )))));
}

fn drawn(shell: &mut Shell) -> ratatui::buffer::Buffer {
    let mut screen = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
    screen.draw(|frame| shell.draw(frame)).unwrap();
    screen.backend().buffer().clone()
}

fn find(buffer: &ratatui::buffer::Buffer, text: &str) -> (u16, u16) {
    for y in 0..buffer.area.height {
        let row: String = (0..buffer.area.width)
            .map(|x| buffer[(x, y)].symbol())
            .collect();
        if let Some(at) = row.find(text) {
            return (row[..at].chars().count() as u16, y);
        }
    }
    panic!("{text} is not on screen");
}

fn settings_jobs(shell: &mut Shell) -> Vec<SettingsJob> {
    std::iter::from_fn(|| shell.ctx.next_request())
        .filter_map(|request| match request {
            Request::Settings(job) => Some(*job),
            _ => None,
        })
        .collect()
}

#[test]
fn catalog_skill_names_are_tagged_and_ctrl_o_opens_the_skill_at_the_cursor() {
    use ratatui::style::Modifier;
    let mut shell = Shell::with(ctx(80, 24), Box::new(ProcessPage::new(Scope::App)));
    assert!(
        settings_jobs(&mut shell)
            .iter()
            .any(|job| matches!(job, SettingsJob::Plugins)),
        "the page reads the skill catalog"
    );
    shell.arrive(reply(
        ProcessQuery::Read { project: None },
        Ok("Use tdd and code-review.\nNot tdd-ish or xtdd.\n"),
    ));
    catalog(&mut shell);

    let buffer = drawn(&mut shell);
    let tagged = |x: u16, y: u16| buffer[(x, y)].modifier.contains(Modifier::UNDERLINED);
    let (x, y) = find(&buffer, "Use tdd and code-review.");
    assert!((0..4).all(|at| !tagged(x + at, y)), "plain text is no tag");
    assert!((4..7).all(|at| tagged(x + at, y)), "tdd is a tag");
    assert!((7..12).all(|at| !tagged(x + at, y)));
    assert!((12..23).all(|at| tagged(x + at, y)), "code-review is a tag");
    assert!(!tagged(x + 23, y), "the full stop after a name is no tag");
    let (x, y) = find(&buffer, "Not tdd-ish or xtdd.");
    assert!(
        (0..20).all(|at| !tagged(x + at, y)),
        "a name inside a longer word is no tag"
    );
    assert!(
        screen(&mut shell).contains("Ctrl-O"),
        "{}",
        screen(&mut shell)
    );

    shell.key(Key::Ctrl('o'));
    assert!(
        screen(&mut shell).contains("No skill name at the cursor."),
        "{}",
        screen(&mut shell)
    );
    assert!(settings_jobs(&mut shell).is_empty());
    for _ in 0..5 {
        shell.key(Key::Right);
    }
    shell.key(Key::Ctrl('o'));
    assert!(
        settings_jobs(&mut shell)
            .iter()
            .any(|job| matches!(job, SettingsJob::SkillDetail { id, .. } if id == "skill-a")),
        "Ctrl-O opens that skill's detail page"
    );
    let rows = render(&mut shell, 80, 24);
    assert!(
        rows[0].contains("Plugins and skills  /  tdd"),
        "{}",
        rows[0]
    );
}

#[test]
fn ctrl_o_never_leaves_unsaved_instructions_behind() {
    let mut shell = opened(Scope::App, "Use tdd.\n");
    catalog(&mut shell);
    shell.key(Key::Char('x'));
    for _ in 0..4 {
        shell.key(Key::Right);
    }
    shell.key(Key::Ctrl('o'));
    let shown = screen(&mut shell);
    assert!(shown.contains("Save first."), "{shown}");
    assert!(
        !settings_jobs(&mut shell)
            .iter()
            .any(|job| matches!(job, SettingsJob::SkillDetail { .. }))
    );
    assert!(render(&mut shell, 80, 24)[0].contains("Settings  /  Process"));
}

#[test]
fn a_project_page_tags_the_skills_of_its_own_catalog() {
    let project = ProjectId::new("harbor-notes").unwrap();
    let scope = Scope::Project {
        id: project.clone(),
        name: "Harbor Notes".into(),
    };
    let mut shell = Shell::with(ctx(80, 24), Box::new(ProcessPage::new(scope)));
    assert!(
        settings_jobs(&mut shell).iter().any(
            |job| matches!(job, SettingsJob::ProjectPlugins { project: asked } if *asked == project)
        ),
        "a project page reads that project's catalog"
    );
}
