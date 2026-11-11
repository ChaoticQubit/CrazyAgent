use super::*;
use crate::tui::client::settings::{SettingsEvent, SettingsJob};
use crate::tui::screens::settings_accounts::tests::{ctx, render};
use crate::tui::shell::Shell;

const READ_VERSION: u64 = 4;

fn reply(query: ProcessQuery, result: Result<&str, &str>) -> Arrival {
    let result = result
        .map(|text| match &query {
            ProcessQuery::Read { .. } => ProcessReply::Current(ProcessText {
                text: text.to_owned(),
                version: READ_VERSION,
            }),
            ProcessQuery::Save { version, .. } => ProcessReply::Saved(SavedText::Saved {
                version: version + 1,
                size: None,
            }),
            _ => ProcessReply::Text(text.to_owned()),
        })
        .map_err(str::to_owned);
    answered(query, result)
}

fn answered(query: ProcessQuery, result: Result<ProcessReply, String>) -> Arrival {
    Arrival::Owner(Box::new(OwnerEvent::Process(Box::new(ProcessEvent {
        query,
        result,
    }))))
}

fn saved(query: ProcessQuery, bytes: u32) -> Arrival {
    let ProcessQuery::Save { version, .. } = &query else {
        panic!("only a save is saved: {query:?}");
    };
    let version = version + 1;
    Arrival::Owner(Box::new(OwnerEvent::Process(Box::new(ProcessEvent {
        query,
        result: Ok(ProcessReply::Saved(SavedText::Saved {
            version,
            size: Some(crate::engine::owner::ProcessSaved {
                bytes,
                claude_bytes_max: 28_159,
            }),
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
    #[cfg(not(unix))]
    assert!(
        project.is_none(),
        "a project page reads its adoption from the engine store, which is unix-only"
    );
    #[cfg(unix)]
    if let Some(project) = project {
        let query = ProcessQuery::Settings {
            project: project.clone(),
        };
        assert_eq!(requests(&mut shell), vec![query.clone()]);
        shell.arrive(settings(query, Ok(at(InstructionSettings::default(), 0))));
        assert_eq!(
            requests(&mut shell),
            vec![ProcessQuery::Adoption {
                project: project.clone()
            }]
        );
        shell.arrive(adoption_reply(&project, Ok(no_agents(&project))));
    }
    shell
}

fn at(settings: InstructionSettings, version: u64) -> InstructionView {
    InstructionView {
        settings,
        version,
        repeats: 0,
    }
}

/// A project's adoption view as the real coordination store reads it before any agent ran.
/// Input: the project. Output: the view, with no agents.
#[cfg(unix)]
fn no_agents(project: &ProjectId) -> Adoption {
    let root = tempfile::tempdir().unwrap();
    let db = rusqlite::Connection::open(root.path().join("engine.db")).unwrap();
    crate::storage::initialize_authority_schemas(&db).unwrap();
    let view = crate::coord::adoption::view(&db, project, &[], 1_800_000_000_000).unwrap();
    assert!(view.agents.is_empty());
    view
}

fn settings(query: ProcessQuery, result: Result<InstructionView, &str>) -> Arrival {
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
        version: READ_VERSION,
        opened: None,
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
        version: READ_VERSION,
        opened: None,
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
        version: READ_VERSION + 1,
        opened: None,
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
        version: READ_VERSION,
        opened: None,
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
fn a_save_the_store_keeps_busy_says_nothing_was_saved_and_the_same_ctrl_s_saves_again() {
    let mut shell = opened(Scope::App, "Use tdd.");
    shell.key(Key::End);
    typed(&mut shell, " Always.");
    shell.key(Key::Ctrl('s'));
    let save = ProcessQuery::Save {
        project: None,
        text: "Use tdd. Always.".into(),
        version: READ_VERSION,
        opened: None,
    };
    assert_eq!(requests(&mut shell), vec![save.clone()]);
    shell.arrive(reply(save.clone(), Err("process_save_unavailable")));
    let shown = screen(&mut shell);
    let flat = shown.lines().map(str::trim).collect::<Vec<_>>().join(" ");
    assert!(flat.contains("nothing was saved. Save again."), "{shown}");
    assert!(
        shown.contains("Not saved yet"),
        "a refused save keeps the draft"
    );
    shell.key(Key::Ctrl('s'));
    assert_eq!(
        requests(&mut shell),
        vec![save.clone()],
        "the same Ctrl-S saves again"
    );
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

    let folder = tempfile::tempdir().unwrap();
    let root = folder.path().canonicalize().unwrap();
    let file = root.join("CLAUDE.md");
    std::fs::write(&file, "OWNER-GLOBAL-RULE\n").unwrap();
    shell.key(Key::Ctrl('g'));
    assert_eq!(
        requests(&mut shell),
        vec![],
        "Ctrl-G opens a path field and reads nothing"
    );
    typed(&mut shell, &file.display().to_string());
    shell.key(Key::Enter);
    let query = ProcessQuery::ReadFile { path: file.clone() };
    assert_eq!(requests(&mut shell), vec![query.clone()]);
    let text = crate::instruction_files::read_picked(&file).unwrap();
    shell.arrive(reply(query, Ok(text.as_str())));
    let preview = screen(&mut shell);
    assert!(preview.contains("Instructions from your file"), "{preview}");
    assert!(
        preview.contains("Read once from the file you named;"),
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

    let missing = root.join("missing/CLAUDE.md");
    shell.key(Key::Ctrl('g'));
    typed(&mut shell, &missing.display().to_string());
    shell.key(Key::Enter);
    let query = ProcessQuery::ReadFile {
        path: missing.clone(),
    };
    assert_eq!(requests(&mut shell), vec![query.clone()]);
    let code = crate::instruction_files::read_picked(&missing)
        .unwrap_err()
        .0;
    shell.arrive(reply(query, Err(code.as_str())));
    assert!(screen(&mut shell).contains("That file doesn't exist. Check the path."));
}

#[cfg(unix)]
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
        version: READ_VERSION,
        opened: None,
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

#[cfg(unix)]
#[test]
fn a_stale_save_keeps_the_draft_and_never_adopts_the_newer_version() {
    let mut shell = opened(Scope::App, "Old rule.");
    typed(&mut shell, "Mine. ");
    shell.key(Key::Ctrl('s'));
    let save = ProcessQuery::Save {
        project: None,
        text: "Mine. Old rule.".into(),
        version: READ_VERSION,
        opened: None,
    };
    assert_eq!(requests(&mut shell), vec![save.clone()]);
    let stale = SavedText::Stale {
        version: READ_VERSION,
        current: ProcessText {
            text: "Desktop rule.".into(),
            version: READ_VERSION + 1,
        },
        writer: crate::storage::versioned::LastWriter::Owner,
    };
    shell.arrive(answered(save.clone(), Ok(ProcessReply::Saved(stale))));
    let shown = screen(&mut shell);
    assert!(
        shown.contains("changed since you opened"),
        "the owner learns the text changed: {shown}"
    );
    assert!(
        shown.contains("│ Mine. █ld rule."),
        "the draft stays, the cursor after the typed words: {shown}"
    );
    assert!(shown.contains("Not saved yet"), "{shown}");
    assert!(
        !shown.contains("Instructions saved."),
        "a stale save never reads as saved: {shown}"
    );

    shell.key(Key::Ctrl('s'));
    assert_eq!(
        requests(&mut shell),
        vec![save],
        "saving again still names the version the owner read, never the newer one"
    );
}

#[test]
fn stale_save_shows_the_conflict_choices() {
    let stale = |named: u64, text: &str| {
        Ok(ProcessReply::Saved(SavedText::Stale {
            version: named,
            current: ProcessText {
                text: text.into(),
                version: named + 1,
            },
            writer: crate::storage::versioned::LastWriter::Agent(
                crate::terminal::AgentId::team(3).unwrap(),
            ),
        }))
    };
    let mut shell = opened(Scope::App, "Old rule.");
    typed(&mut shell, "Mine. ");
    shell.key(Key::Ctrl('s'));
    let save = ProcessQuery::Save {
        project: None,
        text: "Mine. Old rule.".into(),
        version: READ_VERSION,
        opened: None,
    };
    assert_eq!(requests(&mut shell), vec![save.clone()]);
    shell.arrive(answered(save, stale(READ_VERSION, "Agent rule.")));
    let shown = screen(&mut shell);
    assert!(
        shown.contains("Changed by Agent 3 since you opened it."),
        "{shown}"
    );
    assert!(
        shown.contains("Agent rule."),
        "the text that landed: {shown}"
    );
    assert!(
        shown.contains("│ Mine. █ld rule."),
        "the draft stays, the cursor after the typed words: {shown}"
    );
    assert!(
        shown.contains("Ctrl-R Reload") && shown.contains("Ctrl-Y Keep mine"),
        "{shown}"
    );
    assert!(
        requests(&mut shell).is_empty(),
        "nothing is written until the owner chooses"
    );

    shell.key(Key::Ctrl('y'));
    let keep = ProcessQuery::Save {
        project: None,
        text: "Mine. Old rule.".into(),
        version: READ_VERSION + 1,
        opened: Some(READ_VERSION),
    };
    assert_eq!(
        requests(&mut shell),
        vec![keep.clone()],
        "Keep mine names the version shown and the one opened, so the engine records it"
    );
    shell.arrive(answered(keep, stale(READ_VERSION + 1, "Newer rule.")));
    let shown = screen(&mut shell);
    assert!(
        shown.contains("Newer rule.") && !shown.contains("Agent rule."),
        "a Keep mine refused again shows the newer text: {shown}"
    );

    shell.key(Key::Ctrl('r'));
    assert!(requests(&mut shell).is_empty(), "Reload writes nothing");
    let shown = screen(&mut shell);
    assert!(
        !shown.contains("Changed by") && !shown.contains("Keep mine"),
        "{shown}"
    );
    assert!(
        !shown.contains("Mine. Old rule.") && !shown.contains("Not saved yet"),
        "Reload puts the text that landed in the editor: {shown}"
    );
    typed(&mut shell, "Edit. ");
    shell.key(Key::Ctrl('s'));
    assert_eq!(
        requests(&mut shell),
        vec![ProcessQuery::Save {
            project: None,
            text: "Edit. Newer rule.".into(),
            version: READ_VERSION + 2,
            opened: None,
        }],
        "after Reload a save names the version that landed"
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
        shown.contains("Agent instructions: App and project"),
        "{shown}"
    );
    assert!(
        shown.contains("Keep transcripts for headless Claude runs: On"),
        "{shown}"
    );
    assert!(shown.contains("Ctrl-F changes"), "{shown}");
    assert!(shown.contains("Instruction edits: At next task"), "{shown}");
    assert!(shown.contains("Ctrl-R changes"), "{shown}");
    let flat = shown.lines().map(str::trim).collect::<Vec<_>>().join(" ");
    assert!(
        flat.contains("then this project's instructions.md appended."),
        "{shown}"
    );
    let mut stored = InstructionSettings::default();
    let mut version = 0;
    for (mode, words) in [
        (Mode::ProjectOnly, "Agent instructions: Project only"),
        (Mode::AppOnly, "Agent instructions: App only"),
        (Mode::Both, "Agent instructions: App and project"),
    ] {
        shell.key(Key::Ctrl('f'));
        stored.mode = mode;
        let save = ProcessQuery::SaveSettings {
            project: project.clone(),
            settings: stored,
            version: Some(version),
        };
        assert_eq!(requests(&mut shell), vec![save.clone()]);
        shell.key(Key::Ctrl('f'));
        assert_eq!(requests(&mut shell), vec![], "one request at a time");
        version += 1;
        shell.arrive(settings(save, Ok(at(stored, version))));
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
            version: Some(version),
        };
        assert_eq!(requests(&mut shell), vec![save.clone()]);
        version += 1;
        shell.arrive(settings(save, Ok(at(stored, version))));
        let words = if transcripts { ": On" } else { ": Off" };
        assert!(
            screen(&mut shell)
                .contains(&format!("Keep transcripts for headless Claude runs{words}"))
        );
    }
    for (edits, words, note) in [
        (
            crate::instruction_files::EditDelivery::Restart,
            "Instruction edits: Restart when idle",
            "restarts when it is next idle",
        ),
        (
            crate::instruction_files::EditDelivery::Badge,
            "Instruction edits: Badge only",
            "shows Instructions changed",
        ),
        (
            crate::instruction_files::EditDelivery::NextMessage,
            "Instruction edits: At next task",
            "next task starts with the sections you changed",
        ),
    ] {
        shell.key(Key::Ctrl('r'));
        stored.edits = edits;
        let save = ProcessQuery::SaveSettings {
            project: project.clone(),
            settings: stored,
            version: Some(version),
        };
        assert_eq!(requests(&mut shell), vec![save.clone()]);
        version += 1;
        shell.arrive(settings(save, Ok(at(stored, version))));
        let shown = screen(&mut shell);
        let flat = shown.lines().map(str::trim).collect::<Vec<_>>().join(" ");
        assert!(shown.contains(words), "{shown}");
        assert!(flat.contains(note), "{shown}");
    }
    shell.key(Key::Ctrl('t'));
    let refused = ProcessQuery::SaveSettings {
        project: project.clone(),
        settings: InstructionSettings {
            mode: Mode::Both,
            transcripts: false,
            edits: crate::instruction_files::EditDelivery::NextMessage,
            ..Default::default()
        },
        version: Some(version),
    };
    assert_eq!(requests(&mut shell), vec![refused.clone()]);
    shell.arrive(settings(
        refused,
        Err("instruction_settings_save_unavailable"),
    ));
    let shown = screen(&mut shell);
    let flat = shown.lines().map(str::trim).collect::<Vec<_>>().join(" ");
    assert!(
        shown.contains("Keep transcripts for headless Claude runs: On"),
        "{shown}"
    );
    assert!(
        flat.contains("nothing was saved. Change it again."),
        "{shown}"
    );
}

#[test]
fn a_project_page_offers_the_repeated_lines_choice_and_says_how_many_lines_repeat() {
    use crate::instruction_files::Repeated;
    let project = ProjectId::new("harbor-notes").unwrap();
    let scope = Scope::Project {
        id: project.clone(),
        name: "Harbor Notes".into(),
    };
    let mut shell = opened(scope, "");
    let flat = |shell: &mut Shell| {
        screen(shell)
            .lines()
            .map(str::trim)
            .collect::<Vec<_>>()
            .join(" ")
    };
    let shown = flat(&mut shell);
    assert!(
        shown.contains("Lines the project file repeats: Leave out"),
        "{shown}"
    );
    assert!(shown.contains("Ctrl-L changes"), "{shown}");
    assert!(
        shown.contains("No line of this project's file repeats the app instructions."),
        "{shown}"
    );
    let mut stored = InstructionSettings::default();
    for (version, repeated, repeats, words, note) in [
        (
            0,
            Repeated::Keep,
            3,
            "Lines the project file repeats: Keep",
            "3 lines of this project's file repeat the app instructions, and agents get them twice.",
        ),
        (
            1,
            Repeated::LeaveOut,
            1,
            "Lines the project file repeats: Leave out",
            "1 line of this project's file repeats the app instructions and is left out for agents.",
        ),
    ] {
        shell.key(Key::Ctrl('l'));
        stored.repeated = repeated;
        let save = ProcessQuery::SaveSettings {
            project: project.clone(),
            settings: stored,
            version: Some(version),
        };
        assert_eq!(requests(&mut shell), vec![save.clone()]);
        let view = InstructionView {
            settings: stored,
            version: version + 1,
            repeats,
        };
        shell.arrive(settings(save, Ok(view)));
        let shown = flat(&mut shell);
        assert!(shown.contains(words), "{shown}");
        assert!(shown.contains(note), "{shown}");
    }
    stored.mode = Mode::ProjectOnly;
    shell.key(Key::Ctrl('f'));
    let save = ProcessQuery::SaveSettings {
        project: project.clone(),
        settings: stored,
        version: Some(2),
    };
    assert_eq!(requests(&mut shell), vec![save.clone()]);
    shell.arrive(settings(save, Ok(at(stored, 3))));
    let shown = flat(&mut shell);
    assert!(
        shown.contains("Repeated lines only matter when agents get both"),
        "{shown}"
    );
}

#[cfg(unix)]
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
    assert_eq!(
        requests(&mut shell),
        vec![ProcessQuery::Adoption {
            project: project.clone()
        }],
        "unreadable settings still read workflow adoption"
    );
    shell.arrive(adoption_reply(&project, Ok(no_agents(&project))));
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
                edits: crate::instruction_files::EditDelivery::NextMessage,
                ..Default::default()
            },
            version: None,
        }],
        "settings no screen could read are replaced with no version"
    );
}

/// An agent setting saved at a version another window replaced is refused; the page reads
/// the settings again, says so, shows the saved ones and saves the next change at the
/// version just read (Q35).
#[test]
fn a_stale_agent_setting_reads_the_saved_settings_again_and_the_next_change_names_them() {
    let project = ProjectId::new("harbor-notes").unwrap();
    let mut shell = opened(
        Scope::Project {
            id: project.clone(),
            name: "Harbor Notes".into(),
        },
        "",
    );
    shell.key(Key::Ctrl('t'));
    let stale = ProcessQuery::SaveSettings {
        project: project.clone(),
        settings: InstructionSettings {
            mode: Mode::Both,
            transcripts: false,
            edits: EditDelivery::NextMessage,
            ..Default::default()
        },
        version: Some(0),
    };
    assert_eq!(requests(&mut shell), vec![stale.clone()]);
    shell.arrive(settings(stale, Err("stale_revision")));
    let read = ProcessQuery::Settings {
        project: project.clone(),
    };
    assert_eq!(requests(&mut shell), vec![read.clone()]);
    let shown = screen(&mut shell);
    assert!(shown.contains("changed since you opened them"), "{shown}");
    let saved = InstructionSettings {
        mode: Mode::ProjectOnly,
        transcripts: false,
        edits: EditDelivery::NextMessage,
        ..Default::default()
    };
    shell.arrive(settings(read, Ok(at(saved, 3))));
    assert_eq!(
        requests(&mut shell),
        vec![ProcessQuery::Adoption {
            project: project.clone()
        }],
        "settings read again read workflow adoption again"
    );
    shell.arrive(adoption_reply(&project, Ok(no_agents(&project))));
    let shown = screen(&mut shell);
    assert!(
        shown.contains("Agent instructions: Project only"),
        "{shown}"
    );
    assert!(
        shown.contains("Keep transcripts for headless Claude runs: Off"),
        "{shown}"
    );
    shell.key(Key::Ctrl('t'));
    assert_eq!(
        requests(&mut shell),
        vec![ProcessQuery::SaveSettings {
            project,
            settings: InstructionSettings {
                mode: Mode::ProjectOnly,
                transcripts: true,
                edits: EditDelivery::NextMessage,
                ..Default::default()
            },
            version: Some(3),
        }]
    );
}

/// A settings save the engine could not confirm may have landed, so the page reads the
/// settings and their version again before the next change, which then names that version.
#[test]
fn a_settings_save_the_engine_cannot_confirm_reads_the_settings_again_before_the_next_change() {
    let project = ProjectId::new("harbor-notes").unwrap();
    let mut shell = opened(
        Scope::Project {
            id: project.clone(),
            name: "Harbor Notes".into(),
        },
        "",
    );
    shell.key(Key::Ctrl('t'));
    let unconfirmed = ProcessQuery::SaveSettings {
        project: project.clone(),
        settings: InstructionSettings {
            mode: Mode::Both,
            transcripts: false,
            edits: EditDelivery::NextMessage,
            ..Default::default()
        },
        version: Some(0),
    };
    assert_eq!(requests(&mut shell), vec![unconfirmed.clone()]);
    shell.arrive(settings(unconfirmed, Err("process_outcome_unknown")));
    let read = ProcessQuery::Settings {
        project: project.clone(),
    };
    assert_eq!(
        requests(&mut shell),
        vec![read.clone()],
        "the page reads what is really stored"
    );
    let shown = screen(&mut shell);
    assert!(
        shown.contains("couldn't confirm these agent settings were saved"),
        "{shown}"
    );
    shell.key(Key::Ctrl('t'));
    assert_eq!(
        requests(&mut shell),
        vec![],
        "no change goes out before the settings are read again"
    );
    let landed = InstructionSettings {
        mode: Mode::Both,
        transcripts: false,
        edits: EditDelivery::NextMessage,
        ..Default::default()
    };
    shell.arrive(settings(read, Ok(at(landed, 1))));
    assert_eq!(
        requests(&mut shell),
        vec![ProcessQuery::Adoption {
            project: project.clone()
        }]
    );
    shell.arrive(adoption_reply(&project, Ok(no_agents(&project))));
    let shown = screen(&mut shell);
    assert!(
        shown.contains("Keep transcripts for headless Claude runs: Off"),
        "{shown}"
    );
    shell.key(Key::Ctrl('t'));
    assert_eq!(
        requests(&mut shell),
        vec![ProcessQuery::SaveSettings {
            project,
            settings: InstructionSettings {
                mode: Mode::Both,
                transcripts: true,
                edits: EditDelivery::NextMessage,
                ..Default::default()
            },
            version: Some(1),
        }],
        "the next change names the stored version, so it is never refused as stale"
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
        version: 0,
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

#[test]
fn import_from_file_reads_a_named_file_as_a_preview() {
    let folder = tempfile::tempdir().unwrap();
    let file = folder.path().canonicalize().unwrap().join("AGENTS.md");
    std::fs::write(&file, "Plan first.\n").unwrap();
    let mut shell = opened(Scope::App, "Saved rule.");
    assert!(
        render(&mut shell, 80, 24)[23].contains("Ctrl-G Import from file"),
        "{}",
        screen(&mut shell)
    );

    shell.key(Key::Ctrl('g'));
    typed(&mut shell, "~/never");
    let field = screen(&mut shell);
    assert!(field.contains("Import from file"), "{field}");
    assert!(field.contains("~/never"), "{field}");
    assert!(
        render(&mut shell, 80, 24)[23].contains("Enter Read file"),
        "{field}"
    );
    shell.key(Key::Esc);
    assert_eq!(
        requests(&mut shell),
        vec![],
        "Esc closes the field and reads nothing"
    );
    assert!(screen(&mut shell).contains("│ █aved rule."));

    shell.key(Key::Ctrl('g'));
    typed(&mut shell, "~/notes/CLAUDE.md");
    shell.key(Key::Enter);
    let home_read = ProcessQuery::ReadFile {
        path: crate::projects_home::owner_home()
            .unwrap()
            .join("notes/CLAUDE.md"),
    };
    assert_eq!(
        requests(&mut shell),
        vec![home_read.clone()],
        "~/ is the owner's home folder"
    );
    let missing = crate::instruction_files::read_picked(&folder.path().join("missing.md"))
        .unwrap_err()
        .0;
    shell.arrive(reply(home_read, Err(missing.as_str())));
    assert!(
        screen(&mut shell).contains("That file doesn't exist. Check the path."),
        "{}",
        screen(&mut shell)
    );

    shell.key(Key::Ctrl('g'));
    typed(&mut shell, &format!("{}x", file.display()));
    shell.key(Key::Backspace);
    shell.key(Key::Enter);
    let query = ProcessQuery::ReadFile { path: file.clone() };
    assert_eq!(requests(&mut shell), vec![query.clone()]);
    let text = crate::instruction_files::read_picked(&file).unwrap();
    shell.arrive(reply(query, Ok(text.as_str())));
    assert!(screen(&mut shell).contains("Instructions from your file"));
    shell.key(Key::Enter);
    assert!(screen(&mut shell).contains("│ █lan first."));
    assert!(screen(&mut shell).contains("Not saved yet"));
    assert_eq!(
        requests(&mut shell),
        vec![],
        "nothing is saved before Ctrl-S"
    );
    shell.key(Key::Ctrl('s'));
    assert_eq!(
        requests(&mut shell),
        vec![ProcessQuery::Save {
            project: None,
            text: "Plan first.\n".into(),
            version: READ_VERSION,
            opened: None,
        }]
    );
}

#[test]
fn each_import_from_file_refusal_reads_as_a_plain_sentence_and_keeps_the_editor() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path().canonicalize().unwrap();
    let large = root.join("large.md");
    std::fs::write(&large, "x".repeat(crate::agent_home::TEXT_BYTES_MAX + 1)).unwrap();
    let invalid = root.join("invalid.md");
    std::fs::write(&invalid, b"rule\xff\xfe").unwrap();
    let directory = root.join("folder.md");
    std::fs::create_dir(&directory).unwrap();
    for (path, code) in [
        (root.join("missing.md"), "process_import_missing"),
        (directory, "process_import_unsafe"),
        (large, "process_text_too_large"),
        (invalid, "process_text_invalid"),
        (
            std::path::PathBuf::from("relative/CLAUDE.md"),
            "process_request_invalid",
        ),
    ] {
        let mut shell = opened(Scope::App, "Saved rule.");
        shell.key(Key::Ctrl('g'));
        typed(&mut shell, &path.display().to_string());
        shell.key(Key::Enter);
        let query = ProcessQuery::ReadFile { path: path.clone() };
        assert_eq!(requests(&mut shell), vec![query.clone()]);
        let refused = crate::instruction_files::read_picked(&path).unwrap_err().0;
        assert_eq!(refused, code, "{}", path.display());
        shell.arrive(reply(query, Err(refused.as_str())));
        let shown = screen(&mut shell);
        let sentence: String = message(code).chars().take(40).collect();
        assert!(shown.contains(&sentence), "{code}: {shown}");
        assert!(
            shown.contains("│ █aved rule."),
            "a refusal keeps the editor: {shown}"
        );
    }
}

/// A project's adoption view read by the real coordination store: a master that called
/// nothing, a Codex sub-agent that edited with no declared work, queried the graph twice,
/// ran one context-mode call and read the tdd skill and a skill the project does not
/// enable, and was reminded of its flag with the project's nudges turned on, and a newer
/// agent whose launch fires no hooks that edited and read the tdd skill, reminded the same
/// way. Input: the project. Output: the view and the master, sub-agent and hookless ids.
#[cfg(unix)]
fn adoption_read(project: &ProjectId) -> (Adoption, i64, i64, i64) {
    const NOW: i64 = 1_800_000_000_000;
    let root = tempfile::tempdir().unwrap();
    let db = rusqlite::Connection::open(root.path().join("engine.db")).unwrap();
    crate::storage::initialize_authority_schemas(&db).unwrap();
    let body = serde_json::to_string(&crate::protocol::StoredProject {
        project_id: project.clone(),
        spec: crate::protocol::ProjectSpec {
            name: "Harbor Notes".into(),
            repositories: vec![],
        },
    })
    .unwrap();
    db.execute(
        "INSERT INTO projects(id, body) VALUES(?1, ?2)",
        rusqlite::params![project.as_str(), body],
    )
    .unwrap();
    let agent = |role: &str, harness: &str, hookless: bool| {
        db.execute(
            "INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms,hookless)
             VALUES (?1,?2,?3,1,'running',?4,?4,?5)",
            rusqlite::params![project.as_str(), role, harness, NOW, hookless],
        )
        .unwrap();
        db.last_insert_rowid()
    };
    let master = agent("master", "claude-code", false);
    let sub = agent("sub", "codex", false);
    let hookless = agent("sub", "claude-code", true);
    for tool in [
        "Edit",
        "graphify",
        "graphify",
        "ctx_execute",
        "Skill",
        "skill:tdd",
        "skill:other-one",
    ] {
        crate::coord::record_tool(&db, sub, tool, NOW).unwrap();
    }
    for tool in ["Edit", "Skill", "skill:tdd"] {
        crate::coord::record_tool(&db, hookless, tool, NOW).unwrap();
    }
    let nudges = crate::coord::settings::CoordSettings {
        nudges: crate::coord::settings::Nudges::TenMinutes,
        ..Default::default()
    };
    crate::coord::settings::write(&db, project, &nudges).unwrap();
    crate::coord::adoption::tick(&db, project, NOW).unwrap();
    let skills = ["zeta".to_owned(), "tdd".to_owned()];
    let view = crate::coord::adoption::view(&db, project, &skills, NOW).unwrap();
    (view, master, sub, hookless)
}

fn adoption_reply(project: &ProjectId, result: Result<Adoption, &str>) -> Arrival {
    Arrival::Owner(Box::new(OwnerEvent::Process(Box::new(ProcessEvent {
        query: ProcessQuery::Adoption {
            project: project.clone(),
        },
        result: result
            .map(|view| ProcessReply::Adoption(Box::new(view)))
            .map_err(str::to_owned),
    }))))
}

/// Opens a project's Process page on a tall terminal and answers its text and settings
/// reads, leaving the adoption read it sends next. Input: the project. Output: the shell.
fn opened_for_adoption(project: &ProjectId) -> Shell {
    let scope = Scope::Project {
        id: project.clone(),
        name: "Harbor Notes".into(),
    };
    let mut shell = Shell::with(ctx(140, 60), Box::new(ProcessPage::new(scope)));
    let read = ProcessQuery::Read {
        project: Some(project.clone()),
    };
    assert_eq!(requests(&mut shell), vec![read.clone()]);
    shell.arrive(reply(read, Ok("")));
    let query = ProcessQuery::Settings {
        project: project.clone(),
    };
    assert_eq!(requests(&mut shell), vec![query.clone()]);
    shell.arrive(settings(query, Ok(at(InstructionSettings::default(), 0))));
    assert_eq!(
        requests(&mut shell),
        vec![ProcessQuery::Adoption {
            project: project.clone()
        }],
        "the page reads workflow adoption once its settings arrive"
    );
    shell
}

#[cfg(unix)]
#[test]
fn a_project_page_shows_workflow_adoption_after_its_settings() {
    let project = ProjectId::new("harbor-notes").unwrap();
    let (view, master, sub, hookless) = adoption_read(&project);
    let mut shell = opened_for_adoption(&project);
    shell.arrive(adoption_reply(&project, Ok(view)));
    let rows = render(&mut shell, 140, 60);
    let shown = rows.join("\n");
    let at = |text: &str| {
        rows.iter()
            .position(|row| row.contains(text))
            .unwrap_or_else(|| panic!("{text:?} is missing:\n{shown}"))
    };
    let heading = at("Workflow adoption");
    assert!(
        rows[heading].contains("Nudges every 10 minutes: each flag sends that agent one reminder."),
        "{shown}"
    );
    let hookless_row = at(&format!("#{hookless} sub claude-code"));
    let sub_row = at(&format!("#{sub} sub codex"));
    let master_row = at(&format!("#{master} master claude-code"));
    assert!(heading < hookless_row && hookless_row < sub_row && sub_row < master_row);
    assert!(
        rows[hookless_row + 1].contains(
            "start_work 0  inspect_work 0  update_work 0  transfer_work 0  finish_work 0  ctx_* 0  graphify 0  Skill 1"
        ),
        "an agent without hooks shows its counts (OQ-8):\n{shown}"
    );
    assert!(
        rows[hookless_row + 2].contains("graphify 0  ctx_* 0  Skills:  tdd 1  zeta 0"),
        "{shown}"
    );
    assert!(
        rows[hookless_row + 3].contains("Edits without start_work, reminded"),
        "an agent without hooks shows its flag, reminded under the 10-minute nudges:\n{shown}"
    );
    assert!(!shown.contains("unavailable"), "{shown}");
    assert!(
        rows[sub_row + 1].contains(
            "start_work 0  inspect_work 0  update_work 0  transfer_work 0  finish_work 0  ctx_* 1  graphify 2  Skill 1"
        ),
        "{shown}"
    );
    assert!(
        rows[sub_row + 2]
            .contains("graphify 2  ctx_* 1  Skills:  tdd 1  zeta 0  other-one 1 (not enabled)"),
        "{shown}"
    );
    assert!(
        rows[sub_row + 3].contains("Edits without start_work, reminded"),
        "{shown}"
    );
    assert!(
        rows[master_row + 2].contains("graphify 0  ctx_* 0  Skills:  tdd 0  zeta 0"),
        "{shown}"
    );
    assert!(
        !rows[master_row + 3].contains("Edits without start_work"),
        "a master that called nothing has no flag:\n{shown}"
    );
}

#[test]
fn an_unreadable_adoption_says_so_on_the_project_page() {
    let project = ProjectId::new("harbor-notes").unwrap();
    let mut shell = opened_for_adoption(&project);
    shell.arrive(adoption_reply(
        &project,
        Err("process_adoption_unavailable"),
    ));
    let shown = render(&mut shell, 140, 60).join("\n");
    assert!(
        shown.contains("Workflow adoption couldn't be read. Try again."),
        "{shown}"
    );
}

#[test]
fn a_save_that_would_leave_agents_no_instructions_says_so_in_one_sentence() {
    assert_eq!(
        message("process_instructions_would_be_empty"),
        "That would leave agents with no instructions, so nothing was saved; write the instructions they should get first."
    );
}
