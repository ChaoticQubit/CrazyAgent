use super::launch::{LaunchError, failure};
use crate::team::events::screen_rows;
use std::path::Path;
use std::time::Duration;

/// How long a launch Codex opens behind its folder access screen waits for that screen and
/// then for the input box. Nothing can be typed until the screen is answered, so unlike an
/// ordinary first screen this wait ends the launch when it runs out.
pub(super) const SCREEN_WAIT: Duration = Duration::from_secs(60);
/// The failure of a launch whose folder access screen never showed.
pub(super) const SCREEN_MISSING: &str = "launch_folder_screen_missing";
/// The failure of a launch whose input box never came after its screen was answered.
pub(super) const BOX_MISSING: &str = "launch_folder_box_missing";

/// The title row of Codex's folder access screen.
const MENU_TITLE: &str = "Folder access";
/// The last three rows of each folder access screen Codex 0.164 draws: the first option
/// with the selection on it, the second option, and the footer. Enter picks the first.
const MENUS: [[&str; 3]; 3] = [
    [
        "\u{203a} 1. Open restricted",
        "2. Quit",
        "enter continue \u{b7} esc quit",
    ],
    [
        "\u{203a} 1. Open restricted",
        "2. Back to Agent Command Center",
        "enter continue \u{b7} esc back",
    ],
    [
        "\u{203a} 1. Open existing task",
        "2. Back to Agent Command Center",
        "enter continue \u{b7} esc back",
    ],
];
/// The mark Codex draws at the start of its input box and of the selected option.
const BOX_MARK: char = '\u{203a}';
/// How a `projects` setting that records its folders as untrusted ends.
const UNTRUSTED_END: &str = "={trust_level=\"untrusted\"}}";

/// What a pane shows of Codex's way past its folder access screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Screen {
    /// Exactly the folder access screen, the selection on its first option.
    Menu,
    /// Codex's input box, with no folder access screen over it.
    InputBox,
    /// Anything else, which gets no key.
    Other,
}

/// The folders Codex reads a project's `.codex` from for a launch in `cwd`, nearest first:
/// `cwd` and each parent up to and including the first one holding a `.git` that exists,
/// file or folder, or up to the root when none does. Only asks whether `.git` exists.
/// Input: the absolute working folder. Output: the folders, never empty.
fn project_folders(cwd: &Path) -> Vec<&Path> {
    let mut folders = Vec::new();
    for folder in cwd.ancestors() {
        folders.push(folder);
        if std::fs::metadata(folder.join(".git")).is_ok() {
            break;
        }
    }
    folders
}

/// Writes the `projects` setting that records `folders` at `level`.
/// Inputs: at least one folder and the trust level. Output: the setting, or
/// `launch_working_dir_unavailable` for a folder that is not text.
fn entry(folders: &[&Path], level: &str) -> Result<String, LaunchError> {
    assert!(!folders.is_empty());
    let mut parts = Vec::with_capacity(folders.len());
    for folder in folders {
        let name = folder
            .to_str()
            .and_then(|name| serde_json::to_string(name).ok())
            .ok_or_else(|| failure("launch_working_dir_unavailable"))?;
        parts.push(format!("{name}={{trust_level=\"{level}\"}}"));
    }
    Ok(format!("projects={{{}}}", parts.join(",")))
}

/// The setting that records every folder of the walk as untrusted, so no repository's
/// `.codex` loads and no trust line a Codex home saved earlier counts, and an app server
/// has no folder left to trust by itself. Input: the absolute working folder. Output: the
/// setting, or `launch_working_dir_unavailable`.
pub(crate) fn untrusted(cwd: &Path) -> Result<String, LaunchError> {
    entry(&project_folders(cwd), "untrusted")
}

/// The trust setting of a terminal launch: untrusted on every folder of the walk when any
/// of them holds a `.codex` folder, which only asks whether that folder exists, else
/// trusted on the working folder, so an ordinary project never meets the folder access
/// screen. Input: the absolute working folder. Output: the setting, or
/// `launch_working_dir_unavailable`.
pub(super) fn terminal_entry(cwd: &Path) -> Result<String, LaunchError> {
    let folders = project_folders(cwd);
    let found = folders.iter().any(|folder| {
        std::fs::metadata(folder.join(".codex")).is_ok_and(|metadata| metadata.is_dir())
    });
    if found {
        entry(&folders, "untrusted")
    } else {
        entry(&[cwd], "trusted")
    }
}

/// Tells whether a launch's arguments record its folders as untrusted, which makes Codex
/// open behind its folder access screen. Input: the arguments. Output: whether they do.
pub(super) fn behind_a_screen(argv: &[String]) -> bool {
    argv.windows(2).any(|pair| {
        pair[0] == "-c" && pair[1].starts_with("projects={") && pair[1].ends_with(UNTRUSTED_END)
    })
}

/// Reads a pane's screen for the folder access step. The menu counts only when its title
/// and its exact last three rows are there; the input box only with no menu title on the
/// screen. Input: the screen bytes as a host draws them. Output: what it shows.
pub(super) fn read_screen(screen: &[u8]) -> Screen {
    let rows: Vec<String> = screen_rows(screen)
        .iter()
        .map(|row| row.trim().to_owned())
        .filter(|row| !row.is_empty())
        .collect();
    let titled = rows.iter().any(|row| row == MENU_TITLE);
    if titled && rows.len() > 3 {
        let (above, last) = rows.split_at(rows.len() - 3);
        if above.iter().any(|row| row == MENU_TITLE) && MENUS.iter().any(|menu| last == menu) {
            return Screen::Menu;
        }
    }
    let input_box = rows.iter().any(|row| {
        row.strip_prefix(BOX_MARK)
            .is_some_and(|rest| rest.starts_with(' ') && !numbered(rest))
    });
    if input_box && !titled {
        Screen::InputBox
    } else {
        Screen::Other
    }
}

/// Tells whether the text after a selection mark is a numbered option.
/// Input: the text. Output: whether it starts with digits and a full stop.
fn numbered(text: &str) -> bool {
    text.trim_start()
        .split_once('.')
        .is_some_and(|(number, _)| {
            !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
        })
}

/// Tells whether a launch failure is one of the folder access step's, which no later look
/// can mend. Input: the failure. Output: whether it is.
pub(crate) fn ended_the_launch(problem: &LaunchError) -> bool {
    problem.0 == SCREEN_MISSING || problem.0 == BOX_MISSING
}

#[cfg(test)]
mod tests;
