use super::*;
use crate::tui::frame::Tone;
use crate::tui::glyphs::UNICODE;
use crate::tui::theme::{Appearance, Colors, Depth};
use std::os::unix::fs::PermissionsExt;

const LEAVE: &str = "\x1b[?1049l";
const ENTER: &str = "\x1b[?1049h";
const EDITED: &str = "edited by the script\n";

fn ctx(editor: Option<&str>) -> Ctx {
    let mut ctx = Ctx::new(
        Depth::NoColor,
        Colors::Dark,
        Appearance::Dark,
        &UNICODE,
        "/private/tmp/engine.sock".into(),
    );
    ctx.editor = configured(editor.map(str::to_owned));
    ctx
}

fn private_root() -> tempfile::TempDir {
    let root = tempfile::Builder::new()
        .prefix("ca-tui-editor-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    root
}

/// Writes a real editor script that appends one line to the screen file the TUI writes
/// its terminal sequences to, and records every argument it got in the marker file.
/// Takes the folder. Returns the script path.
fn script(folder: &Path) -> PathBuf {
    let path = folder.join("fake editor.sh");
    std::fs::write(
        &path,
        "#!/bin/sh\ndir=$(dirname \"$2\")\nprintf 'edited by the script\\n' >> \"$dir/screen\"\nprintf '%s\\n' \"$@\" > \"$dir/marker\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path
}

fn screen(folder: &Path) -> std::fs::File {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(folder.join("screen"))
        .unwrap()
}

fn quoted(path: &Path) -> String {
    format!("'{}'", path.display())
}

#[test]
fn open_in_editor_runs_the_editor_with_the_file_and_restores_the_screen() {
    let root = private_root();
    let folder = root.path().canonicalize().unwrap();
    let file = folder.join("my notes.md");
    std::fs::write(&file, "# Notes\n").unwrap();
    let editor = format!("{} --wait", quoted(&script(&folder)));
    let mut ctx = ctx(Some(&editor));
    let mut out = screen(&folder);
    assert!(
        !open_waiting(&mut ctx, &mut out).unwrap(),
        "nothing waits yet"
    );
    assert_eq!(std::fs::read(folder.join("screen")).unwrap(), b"");

    ask(&mut ctx, file.clone(), Some("Back from the editor."));
    assert_eq!(
        ctx.editing,
        Some(Edit {
            file: file.clone(),
            note: Some("Back from the editor."),
        })
    );
    assert!(open_waiting(&mut ctx, &mut out).unwrap(), "the editor ran");

    assert_eq!(
        std::fs::read_to_string(folder.join("marker")).unwrap(),
        format!("--wait\n{}\n", file.display()),
        "EDITOR's own words come first and the file is one argument, spaces and all"
    );
    let written = std::fs::read_to_string(folder.join("screen")).unwrap();
    let edited = written
        .find(EDITED)
        .expect("the editor ran while the TUI waited");
    let left = written.find(LEAVE).expect("the alternate screen was left");
    let back = written
        .rfind(ENTER)
        .expect("the alternate screen was entered again");
    assert!(
        left < edited && edited < back,
        "the screen is handed over before the editor runs and taken back after: {written:?}"
    );
    let (before, after) = written.split_at(edited);
    for handed in ["\x1b[?1004l", "\x1b[?2004l", "\x1b[?25h", LEAVE] {
        assert!(before.contains(handed), "missing {handed:?} in {before:?}");
    }
    for taken in [ENTER, "\x1b[?25l", "\x1b[?2004h", "\x1b[?1004h"] {
        assert!(after.contains(taken), "missing {taken:?} in {after:?}");
    }
    assert!(!crossterm::terminal::is_raw_mode_enabled().unwrap());
    assert_eq!(ctx.editing, None, "the file is opened once");
    let message = ctx.message().expect("the note shows once the TUI is back");
    assert_eq!(
        (message.tone, message.lead.as_str()),
        (Tone::Success, "Back from the editor.")
    );
    assert!(!open_waiting(&mut ctx, &mut out).unwrap());
}

#[test]
fn unset_editor_shows_the_plain_line() {
    for unset in [None, Some(""), Some("   \t")] {
        let root = private_root();
        let folder = root.path().canonicalize().unwrap();
        let file = folder.join("notes.md");
        std::fs::write(&file, "# Notes\n").unwrap();
        let mut ctx = ctx(unset);
        assert_eq!(ctx.editor, None, "{unset:?} names no editor");
        ask(&mut ctx, file, None);
        assert_eq!(
            ctx.editing, None,
            "nothing waits for an editor that isn't there"
        );
        let message = ctx.message().expect("the owner is told");
        assert_eq!(
            (message.tone, message.lead.as_str()),
            (Tone::Error, EDITOR_UNSET)
        );
        assert_eq!(EDITOR_UNSET, "EDITOR isn't set in this terminal.");
        let mut out = screen(&folder);
        assert!(!open_waiting(&mut ctx, &mut out).unwrap());
        assert_eq!(
            std::fs::read(folder.join("screen")).unwrap(),
            b"",
            "the screen is never handed over"
        );
    }
    assert_eq!(configured(Some("x".repeat(EDITOR_BYTES_MAX + 1))), None);
    assert_eq!(configured(Some("vim".into())), Some("vim".into()));
}

#[test]
fn an_editor_that_fails_or_is_missing_still_gives_the_screen_back_and_says_so() {
    let root = private_root();
    let folder = root.path().canonicalize().unwrap();
    let file = folder.join("notes.md");
    std::fs::write(&file, "# Notes\n").unwrap();
    let missing = format!("{} --wait", quoted(&folder.join("no such editor")));
    for (editor, lead) in [
        ("exit 3;", EDITOR_FAILED),
        (missing.as_str(), EDITOR_MISSING),
    ] {
        std::fs::write(folder.join("screen"), "").unwrap();
        let mut ctx = ctx(Some(editor));
        let mut out = screen(&folder);
        ask(&mut ctx, file.clone(), Some("Back from the editor."));
        assert!(open_waiting(&mut ctx, &mut out).unwrap());
        let written = std::fs::read_to_string(folder.join("screen")).unwrap();
        let left = written.find(LEAVE).expect("handed over");
        let back = written.rfind(ENTER).expect("taken back");
        assert!(left < back, "{editor}: {written:?}");
        let message = ctx.message().expect("the owner is told");
        assert_eq!(
            (message.tone, message.lead.as_str()),
            (Tone::Error, lead),
            "{editor}"
        );
        assert_eq!(ctx.editing, None);
    }
}

#[test]
#[should_panic(expected = "assertion failed: file.is_absolute()")]
fn only_a_checked_absolute_file_waits_for_the_editor() {
    let mut ctx = ctx(Some("vim"));
    ask(&mut ctx, PathBuf::from("notes.md"), None);
}

#[test]
#[should_panic(expected = "a file waits for the editor only while EDITOR is set")]
fn a_file_never_waits_for_an_editor_that_isnt_set() {
    let mut ctx = ctx(None);
    ctx.editing = Some(Edit {
        file: PathBuf::from("/private/tmp/notes.md"),
        note: None,
    });
    let _ = open_waiting(&mut ctx, &mut Vec::new());
}

#[test]
#[should_panic(expected = "assertion failed: configured(Some(editor.to_owned())).is_some()")]
fn a_blank_editor_never_becomes_a_command() {
    let _ = command(" ", Path::new("/private/tmp/notes.md"));
}
