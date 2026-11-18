use super::icloud::{
    JOB_SKIPPED_LISTED_MAX, SKIPPED_LISTED_MAX, SKIPPED_PATH_MAX, SkippedFolders, dataless,
};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Component, Path};

#[cfg(all(test, unix))]
mod tests;

/// The most reinstall offers the engine keeps; past it the oldest goes first.
pub const OFFERS_MAX: usize = 64;

/// The most bytes one drafted message takes: a fixed head and tail, and per named folder its
/// path, the folder to run its command in, the command and the file that chose it.
pub const DRAFT_BYTES_MAX: usize = 1024 + SKIPPED_LISTED_MAX * (2 * SKIPPED_PATH_MAX + 200);
const _: () = assert!(DRAFT_BYTES_MAX <= crate::agents::PROMPT_BYTES_MAX);

/// The most named folders an offer's reply carries, as a job's line names three.
pub const OFFER_FOLDERS_MAX: usize = JOB_SKIPPED_LISTED_MAX;
const _: () = assert!(
    2 * (DRAFT_BYTES_MAX + OFFER_FOLDERS_MAX * SKIPPED_PATH_MAX) + 1024
        <= crate::protocol::FRAME_BYTES_MAX
);

/// The most bytes a kept record takes as stored JSON: each named path with every byte
/// escaped as JSON escapes a control character, six bytes each, and the fields around it.
pub const KEPT_BYTES_MAX: usize = SKIPPED_LISTED_MAX * (6 * SKIPPED_PATH_MAX + 64) + 64;

/// The command that rebuilds a left folder, chosen by a file beside it: a lockfile or
/// manifest for `node_modules` and `.venv`, and the project's build for `dist` or `build`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Hint {
    Pnpm,
    Yarn,
    Bun,
    BunBinary,
    NpmCi,
    Npm,
    Uv,
    Poetry,
    Pip,
    Python,
    Build,
}

/// One folder a job left out, by its path below the project's folder, with the command its
/// neighbours name, or None when none does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Left {
    pub path: String,
    pub hint: Option<Hint>,
}

/// What the engine keeps for one folder until the owner dismisses it: the named left folders,
/// at most `SKIPPED_LISTED_MAX`, and how many more the listing only counted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Kept {
    pub left: Vec<Left>,
    pub more: u64,
}

/// A project's reinstall offer as a client gets it: the first named folders, how many more
/// there are, and the message the master gets when the owner says yes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Offer {
    pub folders: Vec<String>,
    pub more: u64,
    pub draft: String,
}

/// The files that choose a `node_modules` folder's command, in the order they are looked for.
const NODE: [(&str, Hint); 6] = [
    ("pnpm-lock.yaml", Hint::Pnpm),
    ("yarn.lock", Hint::Yarn),
    ("bun.lock", Hint::Bun),
    ("bun.lockb", Hint::BunBinary),
    ("package-lock.json", Hint::NpmCi),
    ("package.json", Hint::Npm),
];

/// The files that choose a `.venv` folder's command, in the order they are looked for.
const VENV: [(&str, Hint); 4] = [
    ("uv.lock", Hint::Uv),
    ("poetry.lock", Hint::Poetry),
    ("requirements.txt", Hint::Pip),
    ("pyproject.toml", Hint::Python),
];

impl Hint {
    /// The command that rebuilds the folder, run in the folder that holds it, each command
    /// quoted in backticks.
    fn command(self) -> &'static str {
        match self {
            Self::Pnpm => "`pnpm install`",
            Self::Yarn => "`yarn install`",
            Self::Bun | Self::BunBinary => "`bun install`",
            Self::NpmCi => "`npm ci`",
            Self::Npm => "`npm install`",
            Self::Uv => "`uv sync`",
            Self::Poetry => "`poetry install`",
            Self::Pip => {
                "`python3 -m venv .venv`, then `.venv/bin/pip install -r requirements.txt`"
            }
            Self::Python => "the project's Python installer",
            Self::Build => "the project's build command",
        }
    }

    /// The file whose presence chose the command, None for the project's build.
    fn file(self) -> Option<&'static str> {
        NODE.iter()
            .chain(VENV.iter())
            .find(|(_, hint)| *hint == self)
            .map(|(file, _)| *file)
    }
}

/// What an offer keeps from a job's left folders: each named one but `__pycache__`, which
/// rebuilds itself, with the command the files beside it name, and the counted rest.
/// Inputs: the folder the job landed in, the folders its download left and the metadata
/// read, links not followed. Output: the record, with no named folder when none is left.
pub fn kept(
    landed: &Path,
    skipped: &SkippedFolders,
    look: &dyn Fn(&Path) -> std::io::Result<fs::Metadata>,
) -> Kept {
    assert!(landed.is_absolute());
    assert!(skipped.listed.len() <= SKIPPED_LISTED_MAX);
    let left: Vec<Left> = skipped
        .listed
        .iter()
        .filter(|folder| {
            Path::new(&folder.path).file_name() != Some(std::ffi::OsStr::new("__pycache__"))
        })
        .map(|folder| Left {
            path: folder.path.clone(),
            hint: hint(landed, &folder.path, look),
        })
        .collect();
    let more = match left.is_empty() {
        true => 0,
        false => skipped.more.map_or(0, |more| more.folders),
    };
    let kept = Kept { left, more };
    assert!(bounded(&kept));
    kept
}

/// The command for one left folder, from the files beside it in the landed folder, checked
/// by name only: nothing is read, listed or downloaded. Inputs: the landed folder, the
/// folder's path below it and the metadata read. Output: the hint, None for a path cut with
/// "...", one that leaves the folder, a parent that is a link, missing or only in iCloud,
/// or a folder no file names.
fn hint(
    landed: &Path,
    path: &str,
    look: &dyn Fn(&Path) -> std::io::Result<fs::Metadata>,
) -> Option<Hint> {
    let below = Path::new(path);
    let inside = below
        .components()
        .all(|part| matches!(part, Component::Normal(_)));
    if path.ends_with("...") || !inside {
        return None;
    }
    let files: &[(&str, Hint)] = match below.file_name()?.to_str()? {
        "node_modules" => &NODE,
        ".venv" => &VENV,
        "dist" | "build" => return Some(Hint::Build),
        _ => return None,
    };
    let parent = landed.join(below.parent()?);
    assert!(parent.starts_with(landed));
    let held = look(&parent).ok()?;
    if !held.is_dir() || held.file_type().is_symlink() || dataless(&held) {
        return None;
    }
    files
        .iter()
        .find(|(file, _)| look(&parent.join(file)).is_ok_and(|found| !found.is_dir()))
        .map(|(_, hint)| *hint)
}

/// A path as a reader sees it: control characters, line breaks among them, and backticks,
/// which would end the draft's quote, shown as "?", and characters a reader cannot see or
/// that reorder the line taken out. Input: the path. Output: the plain text, never longer
/// than the path.
fn plain(path: &str) -> String {
    let controls: String = path
        .chars()
        .map(|c| match c.is_control() || c == '`' {
            true => '?',
            false => c,
        })
        .collect();
    let shown = crate::memory::without_hidden(&controls);
    assert!(shown.len() <= path.len());
    assert!(!shown.contains('\n') && !shown.contains('`'));
    shown
}

/// One folder's line in the draft. Input: the folder. Output: the line, with no line break.
fn line(left: &Left) -> String {
    let path = plain(&left.path);
    let within = match path.rsplit_once('/') {
        Some((parent, _)) => format!("in `./{parent}`"),
        None => "in the project folder".to_owned(),
    };
    match left.hint {
        None => format!("- `./{path}`"),
        Some(Hint::Build) => format!("- `./{path}`: rebuild it with {}", Hint::Build.command()),
        Some(Hint::Python) => format!(
            "- `./{path}`: {within}, install it with {} (pyproject.toml found)",
            Hint::Python.command()
        ),
        Some(hint) => format!(
            "- `./{path}`: {within}, run {} ({} found)",
            hint.command(),
            hint.file().unwrap_or("a lockfile")
        ),
    }
}

/// The message the master gets when the owner says yes: each named folder with where and how
/// to rebuild it, then the counted rest. Input: the record. Output: the text, within
/// `DRAFT_BYTES_MAX`.
pub fn draft(kept: &Kept) -> String {
    assert!(bounded(kept));
    let mut text = String::from(
        "Some folders of this project were left out when it came into the projects folder, as \
         they can be rebuilt. Please reinstall them in the agent sandbox:",
    );
    for left in &kept.left {
        text.push('\n');
        text.push_str(&line(left));
    }
    if kept.more > 0 {
        text.push_str(&format!(
            "\nCrazyAgent also counted {} more rebuildable folders it did not name; check the \
             project for those too.",
            kept.more
        ));
    }
    assert!(text.len() <= DRAFT_BYTES_MAX);
    text
}

/// The offer a client gets for a record: its first `OFFER_FOLDERS_MAX` paths shown plain,
/// the rest counted, and the draft. Input: the record. Output: the offer.
pub fn offer(kept: &Kept) -> Offer {
    let folders: Vec<String> = kept
        .left
        .iter()
        .take(OFFER_FOLDERS_MAX)
        .map(|left| plain(&left.path))
        .collect();
    let rest = (kept.left.len() - folders.len()) as u64;
    Offer {
        more: kept.more.saturating_add(rest),
        draft: draft(kept),
        folders,
    }
}

/// Whether a stored record keeps its bounds: at most `SKIPPED_LISTED_MAX` named folders,
/// each a path of at most `SKIPPED_PATH_MAX` bytes. Input: the record. Output: the answer.
pub fn bounded(kept: &Kept) -> bool {
    kept.left.len() <= SKIPPED_LISTED_MAX
        && kept
            .left
            .iter()
            .all(|left| !left.path.is_empty() && left.path.len() <= SKIPPED_PATH_MAX)
}
