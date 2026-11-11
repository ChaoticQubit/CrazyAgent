use crate::protocol::ProjectId;
use sha2::{Digest, Sha256};
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
const AGENT_HEADING: &str = "\n## Instructions for this agent\n\n";
pub const PROCESS_BYTES_MAX: usize = TITLE.len()
    + APP_HEADING.len()
    + PROJECT_HEADING.len()
    + AGENT_HEADING.len()
    + 3 * (TEXT_BYTES_MAX + 1)
    + crate::instruction_files::SECTION_BYTES_MAX;
/// The most bytes of the owner's instructions an agent is given inline at a launch: the
/// app and project instructions and the instruction files together. A launch over it is
/// refused with a code naming the largest part, never cut or moved out of the prompt.
pub const GIVEN_BYTES_MAX: usize = 64 * 1024;
const STALE_CONTEXT_FOLDER: &str = "crazyagent-context";
const TOO_LARGE: &str = "agent_instructions_too_large_";
const SKILLS_FOLDER: &str = "agent-skills";
const FILES_FOLDER: &str = "agent-instruction-files";
const GIVEN_FOLDER: &str = "agent-instructions-given";
const FILES_RECORD_BYTES_MAX: usize =
    crate::instruction_files::FILES_MAX * ("unreadable\t18446744073709551615\t\n".len() + 32);
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
- Commit or leave your edits as the \"How your work lands\" section CrazyAgent gives you says; it follows the project's agent access. Never create, switch, rename, delete or push a branch, and never write inside the project folder's `.git` or rewrite history.
- The owner accepts your work in the project's Changes view.
- If the sandbox refuses a command, never retry it outside the sandbox or another way; report what was refused.

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
/// the app-wide instructions, the project id, the project folder whose
/// `.crazyagent/instructions.md` holds the project instructions, none when the project
/// has no recorded folder, and which of the two texts the owner chose for its agents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentProject {
    pub data: PathBuf,
    pub id: ProjectId,
    pub folder: Option<PathBuf>,
    pub mode: crate::instruction_files::Mode,
}

/// What every agent of one project receives at a launch: the owner's app and project
/// instructions the project's mode gives, the record of instruction files found, empty
/// since the engine reads no instruction file outside the app (DEC-106), and how many
/// bytes at the end of the text are an instruction files section, zero.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Given {
    pub text: String,
    pub files: Vec<crate::instruction_files::Record>,
    pub section_bytes: usize,
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

/// Builds what every agent of the project receives from the owner's two instruction texts
/// by the project's mode: the app-wide instructions, the project instructions, or the app
/// text with the project text appended, each under its heading; empty when the given texts
/// are blank. No instruction file outside the app is read. Everything is given inline,
/// never moved to a file the agent must open. Input: the project. Output: at most
/// `GIVEN_BYTES_MAX` bytes of markdown, or a code; past the limit it names the larger
/// part, such as `agent_instructions_too_large_project`.
pub fn text(project: &AgentProject) -> Result<Given, AgentHomeError> {
    let app = match project.mode.app() {
        true => read_app(&project.data)?,
        false => String::new(),
    };
    let own = match (&project.folder, project.mode.project()) {
        (Some(folder), true) => read_project(folder)?,
        _ => String::new(),
    };
    let text = render(&app, &own);
    if text.len() > GIVEN_BYTES_MAX {
        let largest = match app.trim().len() >= own.trim().len() {
            true => "app",
            false => "project",
        };
        return Err(too_large(largest));
    }
    assert!(text.len() <= GIVEN_BYTES_MAX && GIVEN_BYTES_MAX <= PROCESS_BYTES_MAX);
    assert!(project.mode.app() || app.is_empty());
    assert!(project.mode.project() || own.is_empty());
    Ok(Given {
        text,
        files: Vec::new(),
        section_bytes: 0,
    })
}

/// Names the refusal for owner instructions past `GIVEN_BYTES_MAX` after the part that
/// is largest or too large on its own, as a plain code the owner window shows.
/// Input: "app", "project" or an instruction file name such as `~/.claude/CLAUDE.md`.
/// Output: the error, such as `agent_instructions_too_large_home_claude_claude_md`.
fn too_large(part: &str) -> AgentHomeError {
    let mut slug = String::new();
    for word in part
        .replacen("~/", "home/", 1)
        .to_ascii_lowercase()
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
    {
        if !slug.is_empty() {
            slug.push('_');
        }
        slug.push_str(word);
    }
    let code = format!("{TOO_LARGE}{slug}");
    assert!(code.len() <= 96 && crate::protocol::plain_code(&code) == code);
    AgentHomeError(code)
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

/// Adds the owner's own instructions for one agent after the app and project text, under
/// their heading, leaving a blank part out. Inputs: the text from `render` and the agent's
/// own text, at most `TEXT_BYTES_MAX` bytes. Output: at most `PROCESS_BYTES_MAX` bytes.
pub fn with_agent(mut text: String, agent: &str) -> String {
    assert!(agent.len() <= TEXT_BYTES_MAX);
    let part = agent.trim();
    if part.is_empty() {
        return text;
    }
    if text.is_empty() {
        text.push_str(TITLE);
    }
    text.push_str(AGENT_HEADING);
    text.push_str(part);
    text.push('\n');
    assert!(text.len() <= PROCESS_BYTES_MAX);
    text
}

/// Writes the owner's instructions into one agent's private home for one launch: the
/// harness's global instruction file holds exactly the given text, and is removed when it
/// is empty, so a stale copy never outlives a cleared page; the engine's own record of the
/// instruction files it found, kept outside the home, replaces the last launch's.
/// Input: the agent's 0700 home (its CLAUDE_CONFIG_DIR or CODEX_HOME), the harness, the
/// project and what `text` built. Output: unit, or a plain code.
pub fn prepare(
    home: &Path,
    harness: Harness,
    project: &AgentProject,
    given: &Given,
) -> Result<(), AgentHomeError> {
    assert!(project.data.is_absolute());
    assert!(project.folder.as_deref().is_none_or(Path::is_absolute));
    assert!(given.text.len() <= PROCESS_BYTES_MAX);
    if !home.is_absolute() {
        return Err(failure("agent_home_invalid"));
    }
    real_folder(home)?;
    record_files(&project.data, home, &given.files)?;
    write_record(&project.data, GIVEN_FOLDER, home, &given.text)?;
    remove_entry(&home.join(STALE_CONTEXT_FOLDER))?;
    let name = harness.instruction_file();
    if given.text.is_empty() {
        return remove_entry(&home.join(name));
    }
    remove_folder(&home.join(name))?;
    write_whole(home, name, given.text.as_bytes())
}

/// Records the instruction files one launch found, one `state<TAB>bytes<TAB>name` line
/// each, in the engine's 0700 folder that no agent can write, replacing the last launch's
/// record for that home.
/// Inputs: the engine data root, the agent's 0700 home and the records. Output: unit, or
/// `agent_files_invalid` for more records than the engine reads or a name it does not read.
fn record_files(
    data: &Path,
    home: &Path,
    files: &[crate::instruction_files::Record],
) -> Result<(), AgentHomeError> {
    if files.len() > crate::instruction_files::FILES_MAX
        || !files
            .iter()
            .all(|file| crate::instruction_files::known_name(&file.name))
    {
        return Err(failure("agent_files_invalid"));
    }
    let record: String = files
        .iter()
        .map(|file| format!("{}\t{}\t{}\n", file.state.as_str(), file.bytes, file.name))
        .collect();
    assert!(record.len() <= FILES_RECORD_BYTES_MAX);
    write_record(data, FILES_FOLDER, home, &record)
}

/// Names where the engine keeps one kind of launch record for one agent home: a file
/// named by the SHA-256 of the home's real path in the folder `kind` of the engine data
/// root's real path, outside every folder an agent may write. Inputs: the engine data
/// root, the record's folder name and the agent's home. Output: the folder and the file
/// name, or `agent_instructions_unreadable`.
fn record_place(data: &Path, kind: &str, home: &Path) -> Result<(PathBuf, String), AgentHomeError> {
    assert!(kind == FILES_FOLDER || kind == SKILLS_FOLDER || kind == GIVEN_FOLDER);
    let real =
        |path: &Path| fs::canonicalize(path).map_err(|_| failure("agent_instructions_unreadable"));
    let home = real(home)?;
    let name = format!("{:x}", Sha256::digest(home.as_os_str().as_encoded_bytes()));
    assert_eq!(name.len(), 64);
    Ok((real(data)?.join(kind), name))
}

/// Writes one launch's record for an agent home into the engine's 0700 folder `kind`,
/// replacing the last launch's record and any entry planted under its name.
/// Inputs: the engine data root, the record's folder name, the agent's home and the
/// record text. Output: unit, or a plain code.
fn write_record(data: &Path, kind: &str, home: &Path, record: &str) -> Result<(), AgentHomeError> {
    let (folder, name) = record_place(data, kind, home)?;
    #[cfg(unix)]
    let created =
        std::os::unix::fs::DirBuilderExt::mode(&mut fs::DirBuilder::new(), 0o700).create(&folder);
    #[cfg(not(unix))]
    let created = fs::DirBuilder::new().create(&folder);
    match created {
        Err(problem) if problem.kind() != std::io::ErrorKind::AlreadyExists => {
            return Err(failure("process_text_unwritable"));
        }
        _ => real_folder(&folder)?,
    }
    remove_folder(&folder.join(&name))?;
    write_whole(&folder, &name, record.as_bytes())
}

/// Reads one launch record the engine keeps for an agent home, empty before a launch
/// wrote one. Inputs: the engine data root, the record's folder name, the agent's home and
/// the record's bound. Output: the text, or a plain code.
fn read_record(
    data: &Path,
    kind: &str,
    home: &Path,
    bytes_max: usize,
) -> Result<String, AgentHomeError> {
    let (folder, name) = record_place(data, kind, home)?;
    match fs::symlink_metadata(&folder) {
        Err(problem) if problem.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(_) => return Err(failure("agent_instructions_unreadable")),
        Ok(metadata) if !metadata.is_dir() => return Err(failure("agent_instructions_unsafe")),
        Ok(_) => {}
    }
    read_home_file(&folder.join(name), bytes_max)
}

/// Reads the engine's own record of the instruction files one agent's last launch found,
/// kept outside the agent's home, so nothing the agent writes changes what its card lists.
/// Inputs: the engine data root and the agent's home. Output: the records, empty before
/// a launch recorded any, or a plain code.
pub fn given_files(
    data: &Path,
    home: &Path,
) -> Result<Vec<crate::instruction_files::Record>, AgentHomeError> {
    parse_files(&read_record(
        data,
        FILES_FOLDER,
        home,
        FILES_RECORD_BYTES_MAX,
    )?)
}

/// Reads the engine's own record of the owner text one agent home was last given, at its
/// launch or by the last change block a running agent had, kept outside the agent's home so
/// nothing the agent writes changes what counts as an edit. Inputs: the engine data root and
/// the agent's home. Output: the text, none before any launch recorded one, or a plain code.
pub fn given_text(data: &Path, home: &Path) -> Result<Option<String>, AgentHomeError> {
    let (folder, name) = record_place(data, GIVEN_FOLDER, home)?;
    match fs::symlink_metadata(folder.join(name)) {
        Err(problem) if problem.kind() == std::io::ErrorKind::NotFound => Ok(None),
        _ => read_record(data, GIVEN_FOLDER, home, GIVEN_BYTES_MAX).map(Some),
    }
}

/// Replaces the engine's record of the owner text an agent home has, after a running agent
/// was sent the sections that changed. Inputs: the engine data root, the agent's home and
/// the text, at most `GIVEN_BYTES_MAX` bytes. Output: unit, or a plain code.
pub fn record_given(data: &Path, home: &Path, text: &str) -> Result<(), AgentHomeError> {
    if text.len() > GIVEN_BYTES_MAX {
        return Err(failure("agent_instructions_too_large"));
    }
    real_folder(home)?;
    write_record(data, GIVEN_FOLDER, home, text)
}

/// Reads back the instruction files record one launch left. Input: the record text.
/// Output: the records, or `agent_instructions_invalid` for a line the engine never writes.
fn parse_files(record: &str) -> Result<Vec<crate::instruction_files::Record>, AgentHomeError> {
    let files = record
        .split_terminator('\n')
        .map(|line| {
            let mut fields = line.splitn(3, '\t');
            let state = fields
                .next()
                .and_then(crate::instruction_files::State::parse)?;
            let bytes = fields.next()?.parse::<u64>().ok()?;
            let name = fields
                .next()
                .filter(|name| crate::instruction_files::known_name(name))?;
            Some(crate::instruction_files::Record {
                name: name.to_owned(),
                bytes,
                state,
            })
        })
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| failure("agent_instructions_invalid"))?;
    if files.len() > crate::instruction_files::FILES_MAX {
        return Err(failure("agent_instructions_too_large"));
    }
    Ok(files)
}

/// What one agent was given at its last launch, read back from its private home: the
/// text of its harness's instruction file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Received {
    pub text: String,
}

/// Records the names of the skills one launch installed for an agent home, one per line,
/// in the engine's 0700 folder that no agent can write, replacing the last launch's list.
/// Inputs: the engine data root, the agent's 0700 home and the names. Output: unit, or
/// `agent_skills_invalid` for more than `SKILLS_RECORD_MAX` names or a name outside the
/// skill name rule.
pub fn record_skills(data: &Path, home: &Path, names: &[&str]) -> Result<(), AgentHomeError> {
    if names.len() > SKILLS_RECORD_MAX || !names.iter().all(|name| skill_name(name)) {
        return Err(failure("agent_skills_invalid"));
    }
    assert!(data.is_absolute());
    if !home.is_absolute() {
        return Err(failure("agent_home_invalid"));
    }
    real_folder(home)?;
    let record: String = names.iter().map(|name| format!("{name}\n")).collect();
    assert!(record.len() <= SKILLS_RECORD_BYTES_MAX);
    write_record(data, SKILLS_FOLDER, home, &record)
}

/// Reads the engine's own record of the skills one agent's last launch installed, kept
/// outside the agent's home, so nothing the agent writes changes what its card lists.
/// Inputs: the engine data root and the agent's home. Output: the names, empty before a
/// launch recorded any, or a plain code for a record the engine never writes.
pub fn given_skills(data: &Path, home: &Path) -> Result<Vec<String>, AgentHomeError> {
    let record = read_record(data, SKILLS_FOLDER, home, SKILLS_RECORD_BYTES_MAX)?;
    let skills: Vec<String> = record.split_terminator('\n').map(str::to_owned).collect();
    if skills.len() > SKILLS_RECORD_MAX {
        return Err(failure("agent_instructions_too_large"));
    }
    if !skills.iter().all(|name| skill_name(name)) {
        return Err(failure("agent_instructions_invalid"));
    }
    Ok(skills)
}

/// Reads back the instruction file one agent's last launch left in its home. It must be
/// a plain single-link file of this user, opened without following a link or waiting on a
/// pipe, and within its bound.
/// Inputs: the agent's home and its harness. Output: none before the first launch made
/// the home, else the text (empty when none was given), or a plain code.
pub fn received(home: &Path, harness: Harness) -> Result<Option<Received>, AgentHomeError> {
    match fs::symlink_metadata(home) {
        Err(problem) if problem.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(failure("agent_instructions_unreadable")),
        Ok(metadata) if !metadata.is_dir() => return Err(failure("agent_instructions_unsafe")),
        Ok(_) => {}
    }
    let text = read_home_file(&home.join(harness.instruction_file()), PROCESS_BYTES_MAX)?;
    Ok(Some(Received { text }))
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

pub(crate) fn remove_entry(path: &Path) -> Result<(), AgentHomeError> {
    let removed = match fs::symlink_metadata(path) {
        Err(problem) if problem.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(failure("agent_home_unwritable")),
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
    };
    removed.map_err(|_| failure("agent_home_unwritable"))
}

/// Clears a directory planted where a file is about to be written; a file or link stays
/// for the rename to replace atomically, so a reader never finds the name missing.
/// Input: the entry path. Output: unit, or `agent_home_unwritable`.
fn remove_folder(path: &Path) -> Result<(), AgentHomeError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => remove_entry(path),
        _ => Ok(()),
    }
}

/// Reads an instruction file through one open that follows no link in its last part and
/// never waits on a FIFO, then checks the opened file itself, so nothing swapped in after
/// a check is read or waited on. Takes the path. Returns the text, empty for a missing
/// file, `process_text_unsafe` for a link or any other non-regular file, or another code.
fn read_bounded(path: &Path) -> Result<String, AgentHomeError> {
    let file = match crate::platform::open_plain(path) {
        Ok(file) => file,
        Err(problem) if problem.kind() == std::io::ErrorKind::NotFound => {
            return Ok(String::new());
        }
        Err(_) if fs::symlink_metadata(path).is_ok_and(|named| !named.is_file()) => {
            return Err(failure("process_text_unsafe"));
        }
        Err(_) => return Err(failure("process_text_unreadable")),
    };
    let held = file
        .metadata()
        .map_err(|_| failure("process_text_unreadable"))?;
    if !held.is_file() {
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

pub(crate) fn write_whole(folder: &Path, name: &str, bytes: &[u8]) -> Result<(), AgentHomeError> {
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

mod transcript;
pub use transcript::{LoadedFile, Observed, observed};

#[cfg(test)]
#[path = "agent_home/tests.rs"]
mod tests;
