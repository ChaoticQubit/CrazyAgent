use super::*;
use crate::tui::screens::settings_accounts::tests::{ctx, render};
use crate::tui::shell::Shell;

fn reply(query: ProcessQuery, result: Result<&str, &str>) -> Arrival {
    Arrival::Owner(Box::new(OwnerEvent::Process(Box::new(ProcessEvent {
        query,
        result: result.map(str::to_owned).map_err(str::to_owned),
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
    let read = ProcessQuery::Read { project };
    assert_eq!(requests(&mut shell), vec![read.clone()]);
    shell.arrive(reply(read, Ok(saved)));
    shell
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
    shell.arrive(reply(save, Ok("")));
    assert!(!screen(&mut shell).contains("Not saved yet"));
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
    assert_eq!(requests(&mut shell), vec![ProcessQuery::ImportGlobal]);
    shell.arrive(reply(ProcessQuery::ImportGlobal, Ok("OWNER-GLOBAL-RULE\n")));
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
        ProcessQuery::ImportGlobal,
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
