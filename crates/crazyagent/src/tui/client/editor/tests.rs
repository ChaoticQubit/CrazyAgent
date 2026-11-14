use super::*;
use crate::tui::frame::Tone;
use crate::tui::glyphs::UNICODE;
use crate::tui::theme::{Appearance, Colors, Depth};
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

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

/// Writes a real editor script that marks the file's folder once it starts, then waits
/// until the test drops a release file there, for at most 20 seconds.
/// Takes the folder. Returns the script path.
fn waiting_script(folder: &Path) -> PathBuf {
    let path = folder.join("waiting editor.sh");
    std::fs::write(
        &path,
        "#!/bin/sh\ndir=$(dirname \"$1\")\n: > \"$dir/started\"\nn=0\nwhile [ ! -e \"$dir/release\" ] && [ \"$n\" -lt 400 ]; do sleep 0.05; n=$((n + 1)); done\nrm -f \"$dir/started\" \"$dir/release\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path
}

/// Starts the TUI's real stop listener on its own runtime and thread, as `run_tui` does.
/// Takes the editor-running flag the listener reads. Returns the stop flag it sets on a
/// stop request and its thread.
fn listen(running: &Arc<AtomicBool>) -> (Arc<AtomicBool>, std::thread::JoinHandle<()>) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let requested = {
        let _enter = runtime.enter();
        crate::tui::stop_requested(running.clone()).unwrap()
    };
    let stopped = Arc::new(AtomicBool::new(false));
    let stop = stopped.clone();
    let thread = std::thread::spawn(move || {
        runtime.block_on(requested);
        stop.store(true, Ordering::Relaxed);
    });
    (stopped, thread)
}

/// Sends one real signal to this test process with `/bin/kill`.
/// Takes the signal name without SIG. Returns once `kill` delivered it.
fn signal(name: &str) {
    let status = crate::spawn_gate::status(
        Command::new("/bin/kill")
            .arg("-s")
            .arg(name)
            .arg(std::process::id().to_string()),
    )
    .unwrap();
    assert!(status.success(), "kill -s {name}");
}

/// Waits up to 10 seconds for a condition, checking every 20 ms.
/// Takes what is awaited, for the failure, and the condition. Panics on time out.
fn eventually(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn ctrl_c_while_the_editor_runs_keeps_the_tui_and_other_stop_signals_still_end_it() {
    let root = private_root();
    let folder = root.path().canonicalize().unwrap();
    let file = folder.join("notes.md");
    std::fs::write(&file, "# Notes\n").unwrap();
    let mut ctx = ctx(Some(&quoted(&waiting_script(&folder))));
    let running = ctx.editor_running.clone();
    let mut out = screen(&folder);

    let (stopped, listener) = listen(&running);
    let sender = {
        let (folder, running) = (folder.clone(), running.clone());
        std::thread::spawn(move || {
            eventually("the editor started", || folder.join("started").exists());
            assert!(
                running.load(Ordering::Relaxed),
                "the TUI marks the editor running while it waits"
            );
            signal("INT");
            std::thread::sleep(Duration::from_millis(300));
            std::fs::write(folder.join("release"), "").unwrap();
        })
    };
    ask(&mut ctx, file.clone(), None);
    assert!(open_waiting(&mut ctx, &mut out).unwrap());
    sender.join().unwrap();
    assert!(
        !stopped.load(Ordering::Relaxed),
        "Ctrl-C while the editor runs leaves the TUI running"
    );
    assert!(
        !running.load(Ordering::Relaxed),
        "the editor no longer counts as running once the TUI is back"
    );
    assert!(ctx.message().is_none(), "the editor ended cleanly");
    signal("INT");
    eventually("Ctrl-C outside the editor stops the TUI", || {
        stopped.load(Ordering::Relaxed)
    });
    listener.join().unwrap();

    for name in ["TERM", "HUP"] {
        let (stopped, listener) = listen(&running);
        let sender = {
            let (folder, stopped) = (folder.clone(), stopped.clone());
            let name = name.to_owned();
            std::thread::spawn(move || {
                eventually("the editor started", || folder.join("started").exists());
                signal(&name);
                eventually("the stop request ends the TUI mid-edit", || {
                    stopped.load(Ordering::Relaxed)
                });
                std::fs::write(folder.join("release"), "").unwrap();
            })
        };
        ask(&mut ctx, file.clone(), None);
        assert!(open_waiting(&mut ctx, &mut out).unwrap());
        sender.join().unwrap();
        listener.join().unwrap();
        assert!(
            stopped.load(Ordering::Relaxed),
            "SIG{name} still stops the TUI"
        );
        assert!(!running.load(Ordering::Relaxed));
    }
}

#[test]
#[should_panic(expected = "one editor runs at a time")]
fn a_second_editor_never_starts_while_one_runs() {
    let mut ctx = ctx(Some("vim"));
    ctx.editor_running.store(true, Ordering::Relaxed);
    ctx.editing = Some(Edit {
        file: PathBuf::from("/private/tmp/notes.md"),
        note: None,
    });
    let _ = open_waiting(&mut ctx, &mut Vec::new());
}

#[test]
fn the_keyboard_protocol_popped_for_the_editor_is_pushed_again_on_the_screen_it_returns_to() {
    let root = private_root();
    let folder = root.path().canonicalize().unwrap();
    let file = folder.join("notes.md");
    std::fs::write(&file, "# Notes\n").unwrap();
    let editor = format!("{} --wait", quoted(&script(&folder)));
    let mut ctx = ctx(Some(&editor));
    let mut out = screen(&folder);
    let pushed = AtomicBool::new(true);
    ask(&mut ctx, file.clone(), None);
    assert!(open_waiting_with(&mut ctx, &mut out, &pushed).unwrap());
    let written = std::fs::read_to_string(folder.join("screen")).unwrap();
    let (before, after) = written.split_at(written.find(EDITED).expect("the editor ran"));
    assert!(
        before.contains("\x1b[<1u"),
        "the protocol is popped before the editor: {before:?}"
    );
    let back = after
        .find(ENTER)
        .expect("the alternate screen was entered again");
    assert!(
        after[back..].contains("\x1b[>1u"),
        "Shift-Enter is its own key again on the TUI's screen: {after:?}"
    );
    assert!(
        pushed.load(Ordering::SeqCst),
        "the next restore pops it again"
    );

    let seen = written.len();
    let never = AtomicBool::new(false);
    ask(&mut ctx, file, None);
    assert!(open_waiting_with(&mut ctx, &mut out, &never).unwrap());
    let again = std::fs::read_to_string(folder.join("screen")).unwrap();
    assert!(
        !again[seen..].contains("\x1b[>"),
        "a protocol never pushed stays off: {:?}",
        &again[seen..]
    );
    assert!(!never.load(Ordering::SeqCst));
}
