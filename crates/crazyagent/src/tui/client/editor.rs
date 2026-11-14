use crate::tui::ctx::Ctx;
use crate::tui::frame::Message;
use crossterm::cursor::Hide;
use crossterm::event::{EnableBracketedPaste, EnableFocusChange};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, disable_raw_mode, enable_raw_mode, is_raw_mode_enabled,
};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(all(test, unix))]
mod tests;

/// The plain line shown when the owner's terminal names no editor.
pub(in crate::tui) const EDITOR_UNSET: &str = "EDITOR isn't set in this terminal.";
/// The plain line shown when the editor ran and exited with a failure.
pub(in crate::tui) const EDITOR_FAILED: &str = "Your editor stopped with an error.";
/// The plain line shown when the editor named by EDITOR could not be started.
pub(in crate::tui) const EDITOR_MISSING: &str = "Your editor didn't start.";
/// The longest EDITOR value the TUI runs; a longer one counts as unset.
pub(in crate::tui) const EDITOR_BYTES_MAX: usize = 4096;
const UNSET_HINT: &str = "Set EDITOR, for example to vim, then start CrazyAgent again.";
const CHECK_HINT: &str = "Check what EDITOR names in this terminal.";
const SHELL_NOT_FOUND: i32 = 127;

/// One file waiting for the owner's editor, and the line shown once the TUI is back.
/// Holds the checked absolute file and an optional note.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) struct Edit {
    pub file: PathBuf,
    pub note: Option<&'static str>,
}

/// Reads the owner's editor from this terminal's EDITOR value.
/// Takes the value, if set. Returns it, or none when it is blank or longer
/// than `EDITOR_BYTES_MAX`.
pub(in crate::tui) fn configured(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty() && value.len() <= EDITOR_BYTES_MAX)
}

/// Queues one file for the owner's editor, which the run loop opens next, or says
/// plainly that EDITOR is unset. Takes the context, the checked absolute file and the
/// note for after. Returns nothing.
pub(in crate::tui) fn ask(ctx: &mut Ctx, file: PathBuf, note: Option<&'static str>) {
    assert!(file.is_absolute());
    if ctx.editor.is_none() {
        ctx.say(Message::error(EDITOR_UNSET, UNSET_HINT));
        return;
    }
    ctx.editing = Some(Edit { file, note });
}

/// Opens the file waiting for the owner's editor, if any, and says how it ended. Marks
/// the editor running meanwhile, so the TUI ignores the Ctrl-C a waiting GUI editor's
/// terminal sends. Takes the context and the terminal writer. Returns whether the editor
/// ran, so the caller redraws the whole screen, or the error of a terminal that could
/// not be handed over or taken back.
pub(in crate::tui) fn open_waiting(ctx: &mut Ctx, out: &mut impl Write) -> std::io::Result<bool> {
    open_waiting_with(ctx, out, &crate::tui::run::KEYBOARD_PUSHED)
}

/// Opens the waiting file as `open_waiting` does, with the flag that says whether this TUI
/// pushed the keyboard protocol. Takes the context, the terminal writer and the pushed
/// flag. Returns whether the editor ran, or the terminal error.
fn open_waiting_with(
    ctx: &mut Ctx,
    out: &mut impl Write,
    pushed: &AtomicBool,
) -> std::io::Result<bool> {
    let Some(edit) = ctx.editing.take() else {
        return Ok(false);
    };
    let editor = ctx
        .editor
        .clone()
        .expect("a file waits for the editor only while EDITOR is set");
    assert!(
        !ctx.editor_running.swap(true, Ordering::Relaxed),
        "one editor runs at a time"
    );
    let ran = run(out, &mut command(&editor, &edit.file), pushed);
    ctx.editor_running.store(false, Ordering::Relaxed);
    match ran? {
        Ok(()) => {
            if let Some(note) = edit.note {
                ctx.say(Message::success(note, ""));
            }
        }
        Err(problem) => ctx.say(Message::error(problem, CHECK_HINT)),
    }
    Ok(true)
}

/// Builds the command that runs the owner's editor on one file the way git does: EDITOR
/// is shell text, so `code --wait` works, and the file is its own argument, never part
/// of that text. Takes the EDITOR value and the file. Returns the command.
#[cfg(unix)]
fn command(editor: &str, file: &Path) -> Command {
    assert!(configured(Some(editor.to_owned())).is_some());
    let mut command = Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(format!("{editor} \"$@\""))
        .arg(editor)
        .arg(file);
    command
}

/// Builds the command that runs the owner's editor on one file: EDITOR's words, then
/// the file as its own argument. Takes the EDITOR value and the file. Returns the command.
#[cfg(windows)]
fn command(editor: &str, file: &Path) -> Command {
    assert!(configured(Some(editor.to_owned())).is_some());
    let mut words = editor.split_whitespace();
    let mut command = Command::new(words.next().expect("a configured editor has a word"));
    command.args(words).arg(file);
    command
}

/// Hands the terminal to the editor and takes it back: leaves raw mode, if on, and the
/// alternate screen the way the TUI exits, runs the editor through the spawn gate with
/// the terminal's own stdio and waits for it, then enters both again as at start.
/// Takes the terminal writer, the command and the keyboard pushed flag. Returns the
/// editor's outcome as a plain problem, inside the error of a terminal that could not be
/// handed over or taken back.
fn run(
    out: &mut impl Write,
    command: &mut Command,
    pushed: &AtomicBool,
) -> std::io::Result<Result<(), &'static str>> {
    let raw = is_raw_mode_enabled()?;
    if raw {
        disable_raw_mode()?;
    }
    crate::tui::run::restore_with(out, pushed)?;
    let ran = crate::spawn_gate::status(command);
    if raw {
        enable_raw_mode()?;
    }
    execute!(
        out,
        EnterAlternateScreen,
        Hide,
        EnableBracketedPaste,
        EnableFocusChange
    )?;
    Ok(match ran {
        Ok(status) if status.success() => Ok(()),
        Ok(status) if status.code() == Some(SHELL_NOT_FOUND) => Err(EDITOR_MISSING),
        Ok(_) => Err(EDITOR_FAILED),
        Err(_) => Err(EDITOR_MISSING),
    })
}
