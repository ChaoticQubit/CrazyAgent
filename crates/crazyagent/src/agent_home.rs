use crate::protocol::ProjectId;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub const TEXT_BYTES_MAX: usize = 64 * 1024;
pub const APP_FILE: &str = "instructions.md";
pub const PROJECT_FILE: &str = "instructions.md";
const TITLE: &str = "# Instructions from CrazyAgent\n";
const APP_HEADING: &str = "\n## App instructions\n\n";
const PROJECT_HEADING: &str = "\n## Project instructions\n\n";
pub const PROCESS_BYTES_MAX: usize =
    TITLE.len() + APP_HEADING.len() + PROJECT_HEADING.len() + 2 * (TEXT_BYTES_MAX + 1);
pub const SKILLS_FILE: &str = "crazyagent-skills";
pub const SKILLS_RECORD_MAX: usize = 2 * crate::plugins::SKILLS_PER_SCOPE_MAX;
const SKILLS_RECORD_BYTES_MAX: usize =
    SKILLS_RECORD_MAX * (crate::plugins::skill_folder::NAME_BYTES_MAX + 1);

static TEMPORARY: AtomicU64 = AtomicU64::new(0);

/// The CrazyAgent default process the owner can seed the Process page with: the owner's
/// working rules for every master and sub-agent, without machine-only tools.
pub const DEFAULT_PROCESS: &str = "# CrazyAgent default process

## Priorities

1. Security of the codebase, its users and their data comes first, then performance, then testing. A lower priority never wins a tradeoff against a higher one.
2. Prefer quality, simplicity, robustness and long-term maintainability over speed of delivery.

## Work and branches

- Work only in the worktree CrazyAgent gave you and keep each change focused on your task. Preserve work you did not make.
- CrazyAgent owns branches and refs. Never create, switch, rename, delete or push a branch, and never rewrite history.
- Commit with conventional messages, such as `feat: ...`, `fix: ...`, `test: ...` or `refactor: ...`. Never add a Co-Authored-By trailer or an agent name.

## Building

- Write the minimum code that works. First ask whether it needs to exist, then reuse what the codebase, the standard library or an installed dependency already has. No speculative abstractions, configuration or scaffolding.
- Fix a bug at its root cause: reproduce it first and check every caller of the code you change.
- Validate untrusted input with ordinary errors at trust boundaries. Use runtime assertions for internal invariants that should be impossible.
- Never install tools or dependencies, change global configuration or reach a new service without asking the master.
- Never edit generated files or CHANGELOG.md.

## Tests

- Test first: write the test for the missing behavior, run it and watch it fail for the intended reason, write the minimum code, then run it again until it passes.
- Real tests only: no mocks, stubs or fabricated replies. Use the real functions, files, processes and databases; temporary files and synthetic input are fine.
- Never skip, disable or weaken a test or an assertion to get a green result. A missing prerequisite is a blocker you report.
- Run the tests for your change, not the whole suite, unless you are asked to.

## Review and reporting

- Before you finish, review your own diff once for correctness, edge cases, security and these rules, and fix what you find.
- Report honestly: say what you ran and what it printed. Never claim a check passed that you did not run.

## Code style

- No comments in code, except a short docstring directly above a declaration saying what it does, its inputs and its outputs.
- Never use an em dash; use \"-\".
- Match the naming and idiom of the surrounding code.

## Secrets

- Never put a secret, token or credential in source, commands, environment, logs, commit messages or reports, and never read credential files.

## Questions and escalation

- A sub-agent asks the master when a product or design decision is unclear, rather than guessing.
- The master answers its sub-agents and escalates to the owner only for security, destructive or irreversible actions, spending, and product or scope decisions.
";

/// A plain failure code from reading, saving or handing out the owner's instructions.
/// It never carries the text itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentHomeError(pub String);

fn failure(code: &str) -> AgentHomeError {
    AgentHomeError(code.into())
}

/// The harnesses whose own global instruction file the engine writes in an agent's
/// private home: Claude Code reads `CLAUDE.md` in its CLAUDE_CONFIG_DIR and Codex reads
/// `AGENTS.md` in its CODEX_HOME.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Harness {
    Claude,
    Codex,
}

impl Harness {
    /// Maps a harness id to the harness, none for a harness without a known global
    /// instruction file. Input: the id, such as "claude-code" or "codex".
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "claude-code" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            _ => None,
        }
    }

    /// Returns the file name of this harness's global instruction file in its home.
    pub fn instruction_file(self) -> &'static str {
        match self {
            Self::Claude => "CLAUDE.md",
            Self::Codex => "AGENTS.md",
        }
    }
}

/// The project one agent launch serves: the engine data folder holding `engine.db` and
/// the app-wide instructions, the project id, and the project folder whose
/// `.crazyagent/instructions.md` holds the project instructions, none when the project
/// has no recorded folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentProject {
    pub data: PathBuf,
    pub id: ProjectId,
    pub folder: Option<PathBuf>,
}

/// Checks owner text before it is saved: at most `TEXT_BYTES_MAX` bytes with no NUL.
/// Input: the text. Output: unit, or `process_text_too_large` / `process_text_invalid`.
pub fn validate(text: &str) -> Result<(), AgentHomeError> {
    if text.len() > TEXT_BYTES_MAX {
        return Err(failure("process_text_too_large"));
    }
    if text.contains('\0') {
        return Err(failure("process_text_invalid"));
    }
    Ok(())
}

/// Reads the app-wide instructions from the engine data folder; a missing file is empty.
/// Input: the data folder. Output: the text, or a plain code when the file is a link,
/// not plain text or over `TEXT_BYTES_MAX` bytes.
pub fn read_app(data: &Path) -> Result<String, AgentHomeError> {
    read_bounded(&data.join(APP_FILE))
}

/// Saves the app-wide instructions into the engine data folder, replacing the file whole.
/// Input: the data folder and the text. Output: unit, or a plain code.
pub fn save_app(data: &Path, text: &str) -> Result<(), AgentHomeError> {
    validate(text)?;
    real_folder(data)?;
    write_whole(data, APP_FILE, text.as_bytes())
}

/// Reads the project instructions from `<folder>/.crazyagent/instructions.md`; a missing
/// file or `.crazyagent/` folder is empty. Input: the project folder. Output: the text,
/// or a plain code when an entry is a link, not plain text or over `TEXT_BYTES_MAX` bytes.
pub fn read_project(folder: &Path) -> Result<String, AgentHomeError> {
    let state = folder.join(crate::projects_home::CRAZYAGENT_DIRECTORY);
    match fs::symlink_metadata(&state) {
        Err(problem) if problem.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(_) => Err(failure("process_text_unreadable")),
        Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
            Err(failure("process_text_unsafe"))
        }
        Ok(_) => read_bounded(&state.join(PROJECT_FILE)),
    }
}

/// Saves the project instructions into `<folder>/.crazyagent/instructions.md`, a folder git
/// ignores. A project made before `.crazyagent/` existed keeps its state in the project
/// folder itself, so it is refused rather than given a new state folder.
/// Input: the project folder and the text. Output: unit, `process_project_folder_missing`
/// without a real `.crazyagent/` folder, or another plain code.
pub fn save_project(folder: &Path, text: &str) -> Result<(), AgentHomeError> {
    validate(text)?;
    let state = folder.join(crate::projects_home::CRAZYAGENT_DIRECTORY);
    real_folder(&state).map_err(|_| failure("process_project_folder_missing"))?;
    write_whole(&state, PROJECT_FILE, text.as_bytes())
}

/// Builds what every agent of the project receives: the app-wide instructions, then the
/// project instructions, each under its heading; empty when both are blank.
/// Input: the project. Output: at most `PROCESS_BYTES_MAX` bytes of markdown, or a code.
pub fn text(project: &AgentProject) -> Result<String, AgentHomeError> {
    let app = read_app(&project.data)?;
    let own = match &project.folder {
        Some(folder) => read_project(folder)?,
        None => String::new(),
    };
    Ok(render(&app, &own))
}

/// Renders the app text then the project text under their headings, leaving out a blank
/// part and returning empty when both are blank. Input: both texts, each at most
/// `TEXT_BYTES_MAX` bytes. Output: at most `PROCESS_BYTES_MAX` bytes.
pub fn render(app: &str, project: &str) -> String {
    assert!(app.len() <= TEXT_BYTES_MAX && project.len() <= TEXT_BYTES_MAX);
    let mut text = String::new();
    for (heading, part) in [(APP_HEADING, app), (PROJECT_HEADING, project)] {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if text.is_empty() {
            text.push_str(TITLE);
        }
        text.push_str(heading);
        text.push_str(part);
        text.push('\n');
    }
    assert!(text.len() <= PROCESS_BYTES_MAX);
    text
}

/// Writes the owner's instructions into one agent's private home for one launch: the
/// harness's global instruction file holds exactly `text`, and is removed when `text`
/// is empty, so a stale copy never outlives a cleared page.
/// Input: the agent's 0700 home (its CLAUDE_CONFIG_DIR or CODEX_HOME), the harness, the
/// project and the text from `text`. Output: unit, or a plain code.
pub fn prepare(
    home: &Path,
    harness: Harness,
    project: &AgentProject,
    text: &str,
) -> Result<(), AgentHomeError> {
    assert!(project.data.is_absolute());
    assert!(project.folder.as_deref().is_none_or(Path::is_absolute));
    assert!(text.len() <= PROCESS_BYTES_MAX);
    if !home.is_absolute() {
        return Err(failure("agent_home_invalid"));
    }
    real_folder(home)?;
    let name = harness.instruction_file();
    remove_entry(&home.join(name))?;
    if text.is_empty() {
        return Ok(());
    }
    write_whole(home, name, text.as_bytes())
}

/// Reads the owner's own global Claude Code instructions, `<home>/.claude/CLAUDE.md`, for
/// a preview on the owner's explicit request; a link is followed because the owner's own
/// file often is one. Input: the owner's home. Output: the text, `process_import_missing`
/// when there is no file, or a plain code when it is not plain text or too large.
pub fn import_global(home: &Path) -> Result<String, AgentHomeError> {
    let path = home.join(".claude").join("CLAUDE.md");
    match fs::metadata(&path) {
        Err(problem) if problem.kind() == std::io::ErrorKind::NotFound => {
            Err(failure("process_import_missing"))
        }
        Err(_) => Err(failure("process_text_unreadable")),
        Ok(metadata) if !metadata.is_file() => Err(failure("process_text_unsafe")),
        Ok(_) => {
            let file = File::open(&path).map_err(|_| failure("process_text_unreadable"))?;
            read_text(file)
        }
    }
}

/// What one agent was given at its last launch, read back from its private home: the
/// text of its harness's instruction file and the names of the skills installed for it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Received {
    pub text: String,
    pub skills: Vec<String>,
}

/// Records the names of the skills one launch installed in the agent's home, one per
/// line, replacing the last launch's list and any entry planted under its name.
/// Inputs: the agent's 0700 home and the names. Output: unit, or `agent_skills_invalid`
/// for more than `SKILLS_RECORD_MAX` names or a name outside the skill name rule.
pub fn record_skills(home: &Path, names: &[&str]) -> Result<(), AgentHomeError> {
    if names.len() > SKILLS_RECORD_MAX || !names.iter().all(|name| skill_name(name)) {
        return Err(failure("agent_skills_invalid"));
    }
    if !home.is_absolute() {
        return Err(failure("agent_home_invalid"));
    }
    real_folder(home)?;
    remove_entry(&home.join(SKILLS_FILE))?;
    let record: String = names.iter().map(|name| format!("{name}\n")).collect();
    assert!(record.len() <= SKILLS_RECORD_BYTES_MAX);
    write_whole(home, SKILLS_FILE, record.as_bytes())
}

/// Reads back what one agent's last launch left in its home: its harness's instruction
/// file and the recorded skill names. Each must be a plain single-link file of this user,
/// opened without following a link or waiting on a pipe, and within its bound.
/// Inputs: the agent's home and its harness. Output: none before the first launch made
/// the home, else the text (empty when none was given) and the names, or a plain code.
pub fn received(home: &Path, harness: Harness) -> Result<Option<Received>, AgentHomeError> {
    match fs::symlink_metadata(home) {
        Err(problem) if problem.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(failure("agent_instructions_unreadable")),
        Ok(metadata) if !metadata.is_dir() => return Err(failure("agent_instructions_unsafe")),
        Ok(_) => {}
    }
    let text = read_home_file(&home.join(harness.instruction_file()), PROCESS_BYTES_MAX)?;
    let record = read_home_file(&home.join(SKILLS_FILE), SKILLS_RECORD_BYTES_MAX)?;
    let skills: Vec<String> = record.split_terminator('\n').map(str::to_owned).collect();
    if skills.len() > SKILLS_RECORD_MAX {
        return Err(failure("agent_instructions_too_large"));
    }
    if !skills.iter().all(|name| skill_name(name)) {
        return Err(failure("agent_instructions_invalid"));
    }
    Ok(Some(Received { text, skills }))
}

fn skill_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= crate::plugins::skill_folder::NAME_BYTES_MAX
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-')
}

fn read_home_file(path: &Path, bytes_max: usize) -> Result<String, AgentHomeError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32);
    #[cfg(not(unix))]
    if fs::symlink_metadata(path).is_ok_and(|metadata| !metadata.is_file()) {
        return Err(failure("agent_instructions_unsafe"));
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(problem) if problem.kind() == std::io::ErrorKind::NotFound => {
            return Ok(String::new());
        }
        Err(_) if fs::symlink_metadata(path).is_ok_and(|metadata| !metadata.is_file()) => {
            return Err(failure("agent_instructions_unsafe"));
        }
        Err(_) => return Err(failure("agent_instructions_unreadable")),
    };
    let held = file
        .metadata()
        .map_err(|_| failure("agent_instructions_unreadable"))?;
    if !held.is_file() || !sole_own_link(&held) {
        return Err(failure("agent_instructions_unsafe"));
    }
    let mut bytes = Vec::new();
    file.take(bytes_max as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| failure("agent_instructions_unreadable"))?;
    if bytes.len() > bytes_max {
        return Err(failure("agent_instructions_too_large"));
    }
    match String::from_utf8(bytes) {
        Ok(text) if !text.contains('\0') => Ok(text),
        _ => Err(failure("agent_instructions_invalid")),
    }
}

#[cfg(unix)]
fn sole_own_link(metadata: &fs::Metadata) -> bool {
    metadata.nlink() == 1 && metadata.uid() == rustix::process::geteuid().as_raw()
}

#[cfg(not(unix))]
fn sole_own_link(_metadata: &fs::Metadata) -> bool {
    true
}

fn real_folder(folder: &Path) -> Result<(), AgentHomeError> {
    let metadata = fs::symlink_metadata(folder).map_err(|_| failure("process_folder_missing"))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(failure("process_folder_unsafe"));
    }
    Ok(())
}

fn remove_entry(path: &Path) -> Result<(), AgentHomeError> {
    let removed = match fs::symlink_metadata(path) {
        Err(problem) if problem.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(failure("agent_home_unwritable")),
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
    };
    removed.map_err(|_| failure("agent_home_unwritable"))
}

fn read_bounded(path: &Path) -> Result<String, AgentHomeError> {
    let named = match fs::symlink_metadata(path) {
        Err(problem) if problem.kind() == std::io::ErrorKind::NotFound => {
            return Ok(String::new());
        }
        Err(_) => return Err(failure("process_text_unreadable")),
        Ok(metadata) => metadata,
    };
    if !named.is_file() {
        return Err(failure("process_text_unsafe"));
    }
    let file = File::open(path).map_err(|_| failure("process_text_unreadable"))?;
    let held = file
        .metadata()
        .map_err(|_| failure("process_text_unreadable"))?;
    if !held.is_file() || !same_file(&named, &held) {
        return Err(failure("process_text_unsafe"));
    }
    read_text(file)
}

fn read_text(file: File) -> Result<String, AgentHomeError> {
    let mut bytes = Vec::new();
    file.take(TEXT_BYTES_MAX as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| failure("process_text_unreadable"))?;
    if bytes.len() > TEXT_BYTES_MAX {
        return Err(failure("process_text_too_large"));
    }
    let text = String::from_utf8(bytes).map_err(|_| failure("process_text_invalid"))?;
    validate(&text)?;
    Ok(text)
}

#[cfg(unix)]
fn same_file(named: &fs::Metadata, held: &fs::Metadata) -> bool {
    named.dev() == held.dev() && named.ino() == held.ino()
}

#[cfg(not(unix))]
fn same_file(named: &fs::Metadata, held: &fs::Metadata) -> bool {
    named.len() == held.len()
}

fn write_whole(folder: &Path, name: &str, bytes: &[u8]) -> Result<(), AgentHomeError> {
    assert!(!name.contains('/') && !name.starts_with('.'));
    let temporary = folder.join(format!(
        ".{name}.{}.{}",
        std::process::id(),
        TEMPORARY.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let written = options.open(&temporary).and_then(|mut file| {
        file.write_all(bytes)?;
        file.sync_all()
    });
    let renamed = written.and_then(|()| fs::rename(&temporary, folder.join(name)));
    if renamed.is_err() {
        let _ = fs::remove_file(&temporary);
        return Err(failure("process_text_unwritable"));
    }
    Ok(())
}

#[cfg(test)]
#[path = "agent_home/tests.rs"]
mod tests;
