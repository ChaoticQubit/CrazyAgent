use super::settings::{draw_page, step};
use super::settings_accounts::{send as send_settings, settings_event};
use super::settings_plugins::SkillDetailPage;
use crate::agent_home::TEXT_BYTES_MAX;
use crate::coord::adoption::AgentAdoption;
use crate::coord::settings::Nudges;
use crate::engine::owner::{
    Adoption, InstructionSettings, InstructionView, ProcessText, SavedText,
};
use crate::instruction_files::{EditDelivery, Mode};
use crate::plugins::SkillView;
use crate::protocol::ProjectId;
use crate::tui::client::OwnerEvent;
use crate::tui::client::conflict::{CHOICES, Conflict, LINES_SHOWN_MAX};
use crate::tui::client::process::{ProcessEvent, ProcessQuery, ProcessReply};
use crate::tui::client::settings::{SettingsEvent, SettingsJob};
use crate::tui::ctx::Ctx;
use crate::tui::frame::{Hint, Message, draw_lines, hint, sections_hint};
use crate::tui::keys::Key;
use crate::tui::screens::{Flow, Place, Screen, SettingsSection};
use crate::tui::seam::{Arrival, Request};
use crate::tui::text::{char_width, coded, indent, row, styled, wrap};
use crate::tui::theme::Role;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

#[cfg(all(test, unix))]
mod tests;

const PAGE_STEP: usize = 10;
const NAME_REACH: usize = crate::plugins::skill_folder::NAME_BYTES_MAX + 2;
const EDITOR_ROWS_MIN: usize = 3;
const EDITOR_CHROME_ROWS: usize = 2;
const READ_REFUSED: &str = "The instructions weren't read: too many requests are waiting.";
const ADOPTION_ROWS_MAX: usize = 6;
const PATH_BYTES_MAX: usize = 4096;

/// Where an instructions text applies: every project, or one project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) enum Scope {
    App,
    Project { id: ProjectId, name: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Seed {
    Import,
    Default,
}

#[derive(Debug, PartialEq, Eq)]
enum State {
    Loading,
    Failed(String),
    Ready { saved: String, version: u64 },
}

/// One catalog skill the editor tags: its name as characters, its engine id and its name.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Skill {
    name: Vec<char>,
    id: String,
    title: String,
}

/// The skills the editor tags, from the engine's catalog: the first skill of each name,
/// longest names first so a longer name wins over a name it contains.
/// Input: the catalog. Output: the tags.
fn catalog(skills: &[SkillView]) -> Vec<Skill> {
    let mut tags: Vec<Skill> = Vec::new();
    for skill in skills {
        let usable = !skill.name.is_empty()
            && skill.name.len() <= crate::plugins::skill_folder::NAME_BYTES_MAX
            && !tags.iter().any(|tag| tag.title == skill.name);
        if usable {
            tags.push(Skill {
                name: skill.name.chars().collect(),
                id: skill.id.clone(),
                title: skill.name.clone(),
            });
        }
    }
    tags.sort_by_key(|tag| std::cmp::Reverse(tag.name.len()));
    tags
}

fn name_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '_' | '.' | ':' | '-')
}

fn ends_at(chars: &[char], end: usize) -> bool {
    match chars.get(end) {
        None => true,
        Some(next) if !name_char(*next) => true,
        Some(next) => {
            !(next.is_ascii_alphanumeric() || *next == '_')
                && chars.get(end + 1).is_none_or(|after| !name_char(*after))
        }
    }
}

/// Finds the catalog skill names in one line that touch a range of characters: a whole
/// name, not part of a longer name, with sentence punctuation after it allowed.
/// Inputs: the line's characters, the range and the tags, longest first.
/// Output: each name's first character, the character after it and its tag, in order.
fn skill_ranges(
    chars: &[char],
    from: usize,
    to: usize,
    skills: &[Skill],
) -> Vec<(usize, usize, usize)> {
    let to = to.min(chars.len());
    let mut found = Vec::new();
    let mut at = from.saturating_sub(NAME_REACH);
    while at < to {
        let free = at == 0 || !name_char(chars[at - 1]);
        let hit = free
            .then(|| {
                skills.iter().position(|skill| {
                    chars[at..].starts_with(&skill.name) && ends_at(chars, at + skill.name.len())
                })
            })
            .flatten();
        match hit {
            Some(index) => {
                let end = at + skills[index].name.len();
                found.push((at, end, index));
                at = end;
            }
            None => at += 1,
        }
    }
    found
}

/// Splits one drawn editor line into plain text and tagged skill names.
/// Inputs: the drawn characters, the first line character on screen, the names found
/// and the tag style. Output: the spans, which join back to the drawn text.
fn tagged(
    text: &str,
    left: usize,
    ranges: &[(usize, usize, usize)],
    tag: Style,
) -> Vec<Span<'static>> {
    let chars: Vec<char> = text.chars().collect();
    let mut spans = Vec::new();
    let mut at = 0;
    for &(start, end, _) in ranges {
        if end <= left {
            continue;
        }
        let start = (start.max(left) - left).min(chars.len());
        let end = (end - left).min(chars.len());
        if start < at || start >= end {
            continue;
        }
        if at < start {
            spans.push(Span::raw(chars[at..start].iter().collect::<String>()));
        }
        spans.push(Span::styled(
            chars[start..end].iter().collect::<String>(),
            tag,
        ));
        at = end;
    }
    if at < chars.len() {
        spans.push(Span::raw(chars[at..].iter().collect::<String>()));
    }
    spans
}

/// The one plain sentence for a Process refusal, also for an agent's Instructions view.
/// Input: the code. Output: the sentence, or the fallback naming the code.
pub(in crate::tui) fn message(code: &str) -> String {
    match code {
        "process_text_too_large" => "This text is over 64 KB. Shorten it, then save again.".into(),
        "process_text_invalid" => {
            "This text has characters CrazyAgent can't save. Remove them, then save again.".into()
        }
        "process_text_unsafe" => "The instructions file is a link or not a plain file, so CrazyAgent won't read it. Replace it with a plain file.".into(),
        "process_text_unreadable" => "The instructions file couldn't be read. Check it, then try again.".into(),
        "process_text_unwritable" => "The instructions couldn't be saved. Check the folder, then try again.".into(),
        "process_text_stale" => "These instructions changed since you opened them, so yours were not saved.".into(),
        "process_project_folder_missing" => "This project keeps its files without a .crazyagent folder, so it can't hold project instructions.".into(),
        "process_project_unknown" => "This project has no setup yet, so it can't hold instructions.".into(),
        "process_import_missing" => "That file doesn't exist. Check the path.".into(),
        "process_import_unsafe" => "That file isn't a plain file you own, so CrazyAgent won't read it.".into(),
        "process_import_unreadable" => "That file couldn't be read. Check it, then try again.".into(),
        "process_instructions_would_be_empty" => "That would leave agents with no instructions, so nothing was saved; write the instructions they should get first.".into(),
        "process_import_outside" => "That file links outside the project folder, so it stays where it is.".into(),
        "process_import_unmovable" => "The instruction files couldn't be moved aside, so nothing changed. Check the project folder, then try again.".into(),
        "process_request_invalid" => "Type the file's full path, starting with / or ~/.".into(),
        "process_commit_staged" => "Other files are staged in this project's checkout, so nothing was committed. Unstage or commit them, then try again.".into(),
        "process_commit_none" => "No moved instruction file waits for a commit.".into(),
        "process_commit_busy" => "Git is busy in this project's checkout (a merge, a rebase or a locked index), so nothing was committed. Finish it, then try again.".into(),
        "process_commit_detached" => "This project's checkout isn't on a branch, so nothing was committed. Check out a branch, then try again.".into(),
        "process_commit_moved" => "The branch changed while committing, so nothing was committed. Try again.".into(),
        "process_commit_identity" => "Git has no user.name or user.email for this project, so nothing was committed. Set them, then try again.".into(),
        "process_commit_failed" => "Git couldn't commit the move. Check the project's checkout, then try again.".into(),
        "process_folder_missing" => "CrazyAgent's data folder is missing. Restart the engine, then try again.".into(),
        "process_folder_unsafe" => "CrazyAgent's data folder isn't a plain folder, so nothing was saved.".into(),
        "process_unavailable" => "Instructions aren't ready in this engine yet.".into(),
        "process_save_unavailable" => "The engine couldn't save right now, so nothing was saved. Save again.".into(),
        "process_agent_unknown" => "This agent isn't part of this project's team.".into(),
        "process_agent_unsupported" => "CrazyAgent doesn't write instructions for this agent's harness.".into(),
        "agent_instructions_unsafe" => "This agent's instructions file is a link or not a plain file, so CrazyAgent won't read it.".into(),
        "agent_instructions_too_large" => "This agent's instructions file is larger than CrazyAgent writes, so it isn't shown.".into(),
        "agent_instructions_invalid" => "This agent's instructions file has characters CrazyAgent doesn't write, so it isn't shown.".into(),
        "agent_instructions_unreadable" => "This agent's instructions file couldn't be read. Try again.".into(),
        "trusted_connection_unavailable" => "Not connected to the engine. Try again once it reconnects.".into(),
        "instruction_settings_invalid" => "This project's agent settings hold a value CrazyAgent doesn't know. Pick an option to replace it.".into(),
        "instruction_settings_unavailable" => "This project's agent settings couldn't be read. Try again.".into(),
        "instruction_settings_save_unavailable" => "The engine couldn't save right now, so nothing was saved. Change it again.".into(),
        "process_adoption_unavailable" => "Workflow adoption couldn't be read. Try again.".into(),
        other => coded("CrazyAgent couldn't finish that.", other),
    }
}

/// The owner's words for one instructions option. Input: the option. Output: its name.
pub(in crate::tui) fn mode_label(mode: Mode) -> &'static str {
    match mode {
        Mode::ProjectOnly => "Project only",
        Mode::AppOnly => "App only",
        Mode::Both => "Both (app, then project appended)",
    }
}

/// The option Ctrl-F moves to from one option, in the order the page lists them.
/// Input: the option. Output: the next one, wrapping to the first.
fn next_mode(mode: Mode) -> Mode {
    match mode {
        Mode::ProjectOnly => Mode::AppOnly,
        Mode::AppOnly => Mode::Both,
        Mode::Both => Mode::ProjectOnly,
    }
}

/// What one instructions option hands the project's agents, in one sentence.
/// Input: the option. Output: the sentence.
fn mode_note(mode: Mode) -> &'static str {
    match mode {
        Mode::ProjectOnly => "Agents get only this project's instructions.md.",
        Mode::AppOnly => "Agents get only the app instructions from Settings.",
        Mode::Both => {
            "Agents get the app instructions, then this project's instructions.md appended."
        }
    }
}

/// The owner's words for one way an instruction edit reaches running agents. Input: the
/// choice. Output: its name.
fn edits_label(edits: EditDelivery) -> &'static str {
    match edits {
        EditDelivery::NextMessage => "Next task",
        EditDelivery::Restart => "Restart when idle",
        EditDelivery::Badge => "Badge only",
    }
}

/// The choice Ctrl-R moves to from one choice, in the order the page lists them.
/// Input: the choice. Output: the next one, wrapping to the first.
fn next_edits(edits: EditDelivery) -> EditDelivery {
    match edits {
        EditDelivery::NextMessage => EditDelivery::Restart,
        EditDelivery::Restart => EditDelivery::Badge,
        EditDelivery::Badge => EditDelivery::NextMessage,
    }
}

/// What one choice does when the owner edits the instructions, in one sentence.
/// Input: the choice. Output: the sentence.
fn edits_note(edits: EditDelivery) -> &'static str {
    match edits {
        EditDelivery::NextMessage => {
            "A running agent's next task starts with the sections you changed, sent once."
        }
        EditDelivery::Restart => {
            "A running agent restarts when it is next idle and resumes its conversation with the new text."
        }
        EditDelivery::Badge => {
            "Nothing is sent. A running agent shows Instructions changed until it starts again."
        }
    }
}

/// Writes a byte count the way the Process pages show sizes. Input: bytes. Output: words.
pub(in crate::tui) fn size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes} bytes")
    } else {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    }
}

/// Splits untrusted text into printable lines that fit beside a one-cell gutter, keeping
/// blank lines. Inputs: the text and the width in cells. Output: the lines.
fn quoted(text: &str, width: usize) -> Vec<String> {
    crate::tui::screens::project_markdown::sanitize(text)
        .split('\n')
        .flat_map(|line| {
            let wrapped = wrap(line, width.saturating_sub(2).max(1));
            if wrapped.is_empty() {
                vec![String::new()]
            } else {
                wrapped
            }
        })
        .collect()
}

/// One agent's grounding and skill counts as one line of words: graphify, `ctx_*`, then
/// every skill its project enabled by name with zero included, then the other skills it
/// called. Inputs: the view's skill names and the agent's row. Output: the words.
fn skill_words(skills: &[String], agent: &AgentAdoption) -> String {
    let mut words = vec![
        format!("graphify {}", agent.graphify),
        format!("ctx_* {}", agent.context_mode),
        if skills.is_empty() {
            "Skills: none enabled".to_owned()
        } else {
            "Skills:".to_owned()
        },
    ];
    words.extend(
        skills
            .iter()
            .zip(&agent.skills)
            .map(|(skill, count)| format!("{skill} {count}")),
    );
    words.extend(
        agent
            .other_skills
            .iter()
            .map(|other| format!("{} {} (not enabled)", other.name, other.count)),
    );
    words.join("  ")
}

/// The character the editor draws for one text character: a tab as a space, and a
/// control, escape or zero-width character as `?`, so stored text never reaches the
/// terminal as a command. Input: the character. Output: a printable character.
fn shown(character: char) -> char {
    match character {
        '\t' => ' ',
        c if c.is_control() || char_width(c) == 0 => '?',
        c => c,
    }
}

fn byte_at(line: &str, column: usize) -> usize {
    line.char_indices()
        .nth(column)
        .map_or(line.len(), |(index, _)| index)
}

/// A plain multi-line editor for one instructions text: its lines, the cursor as a line
/// and a character column, and the first line and column on screen. Every edit costs one
/// line, so a 64 KB text stays quick. The text keeps every character it was given.
#[derive(Debug)]
struct Editor {
    lines: Vec<String>,
    row: usize,
    column: usize,
    top: usize,
    left: usize,
}

impl Editor {
    fn new(text: &str) -> Self {
        let editor = Self {
            lines: text.split('\n').map(str::to_owned).collect(),
            row: 0,
            column: 0,
            top: 0,
            left: 0,
        };
        editor.assert_consistent();
        editor
    }

    fn text(&self) -> String {
        self.lines.join("\n")
    }

    fn bytes(&self) -> usize {
        self.lines.iter().map(String::len).sum::<usize>() + self.lines.len() - 1
    }

    fn width(&self, row: usize) -> usize {
        self.lines[row].chars().count()
    }

    fn assert_consistent(&self) {
        assert!(!self.lines.is_empty());
        assert!(self.row < self.lines.len());
        assert!(self.column <= self.width(self.row));
    }

    /// Inserts text at the cursor and leaves the cursor after it. The new lines go in with
    /// one pass over the lines below, so a many-line paste never shifts them once per line.
    /// Input: the text, with CR LF and CR read as LF.
    fn insert(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let mut parts = text.split('\n');
        let first = parts.next().unwrap_or_default();
        let at = byte_at(&self.lines[self.row], self.column);
        let rest = self.lines[self.row].split_off(at);
        self.lines[self.row].push_str(first);
        self.column += first.chars().count();
        let added: Vec<String> = parts.map(str::to_owned).collect();
        if let Some(last) = added.last() {
            self.column = last.chars().count();
        }
        let below = self.row + 1;
        self.row += added.len();
        self.lines.splice(below..below, added);
        self.lines[self.row].push_str(&rest);
    }

    fn backspace(&mut self) {
        if self.column > 0 {
            let at = byte_at(&self.lines[self.row], self.column - 1);
            self.lines[self.row].remove(at);
            self.column -= 1;
        } else if self.row > 0 {
            let line = self.lines.remove(self.row);
            self.row -= 1;
            self.column = self.width(self.row);
            self.lines[self.row].push_str(&line);
        }
    }

    fn delete(&mut self) {
        if self.column < self.width(self.row) {
            let at = byte_at(&self.lines[self.row], self.column);
            self.lines[self.row].remove(at);
        } else if self.row + 1 < self.lines.len() {
            let line = self.lines.remove(self.row + 1);
            self.lines[self.row].push_str(&line);
        }
    }

    fn go(&mut self, row: usize) {
        self.row = row.min(self.lines.len() - 1);
        self.column = self.column.min(self.width(self.row));
    }

    /// Applies one editing key. Input: the key. Output: true when the key edited the text
    /// or moved the cursor; keys the editor does not use return false.
    fn key(&mut self, key: &Key) -> bool {
        match key {
            Key::Char(character) => self.insert(character.encode_utf8(&mut [0u8; 4])),
            Key::Paste(text) => self.insert(text),
            Key::Enter | Key::Ctrl('j') => self.insert("\n"),
            Key::Backspace => self.backspace(),
            Key::Delete => self.delete(),
            Key::Left if self.column > 0 => self.column -= 1,
            Key::Left if self.row > 0 => {
                self.row -= 1;
                self.column = self.width(self.row);
            }
            Key::Right if self.column < self.width(self.row) => self.column += 1,
            Key::Right if self.row + 1 < self.lines.len() => {
                self.row += 1;
                self.column = 0;
            }
            Key::Left | Key::Right => {}
            Key::Up => self.go(self.row.saturating_sub(1)),
            Key::Down => self.go(self.row + 1),
            Key::PageUp => self.go(self.row.saturating_sub(PAGE_STEP)),
            Key::PageDown => self.go(self.row + PAGE_STEP),
            Key::Home | Key::Ctrl('a') => self.column = 0,
            Key::End | Key::Ctrl('e') => self.column = self.width(self.row),
            _ => return false,
        }
        self.assert_consistent();
        true
    }

    /// The lines on screen and the cursor in them, scrolling so the cursor shows.
    /// Inputs: the width in cells and the number of rows, each at least 1.
    /// Output: the printable lines and the cursor column and row.
    fn view(&mut self, width: usize, rows: usize) -> (Vec<String>, (usize, usize)) {
        assert!(width > 1 && rows > 0);
        if self.row < self.top {
            self.top = self.row;
        }
        if self.row >= self.top + rows {
            self.top = self.row + 1 - rows;
        }
        let line: Vec<char> = self.lines[self.row].chars().collect();
        self.left = self.left.min(self.column);
        let mut start = self.column;
        let mut used = 0;
        while start > self.left && used + char_width(shown(line[start - 1])) < width {
            used += char_width(shown(line[start - 1]));
            start -= 1;
        }
        self.left = start;
        let shown_lines = self.lines[self.top..(self.top + rows).min(self.lines.len())]
            .iter()
            .map(|text| {
                let mut out = String::new();
                let mut used = 0;
                for character in text.chars().skip(self.left).map(shown) {
                    let cost = char_width(character);
                    if used + cost > width {
                        break;
                    }
                    used += cost;
                    out.push(character);
                }
                out
            })
            .collect();
        let cursor = (used, self.row - self.top);
        assert!(cursor.0 < width && cursor.1 < rows);
        (shown_lines, cursor)
    }
}

/// The Process page: the owner's instructions every agent gets, app-wide in Settings or
/// for one project. Reads the saved text on opening, edits it in place and saves it only on
/// Ctrl-S. At app level Ctrl-G reads the owner's global instructions and Ctrl-D the default
/// process, each into a preview first. For one project it also shows the agent settings:
/// Ctrl-F picks the instruction files its agents get, Ctrl-T whether headless Claude runs
/// keep their transcript and Ctrl-R how an instruction edit reaches running agents, each
/// saved at once at the version read; one refused because the settings changed since reads
/// them again. A stale save of the text shows the conflict and keeps the draft until the
/// owner reloads the saved text (Ctrl-R, which reloads while the conflict shows) or keeps
/// their own (Ctrl-Y). One request runs at a time.
pub(in crate::tui) struct ProcessPage {
    scope: Scope,
    state: State,
    settings: Option<Result<InstructionView, String>>,
    adoption: Option<Result<Adoption, String>>,
    editor: Editor,
    problem: Option<String>,
    conflict: Option<Conflict>,
    pending: Option<ProcessQuery>,
    preview: Option<(Seed, String)>,
    preview_top: usize,
    path: Option<String>,
    skills: Vec<Skill>,
}

impl ProcessPage {
    /// The page for one scope, loading until the engine answers.
    /// Input: where the text applies. Output: the screen.
    pub fn new(scope: Scope) -> Self {
        Self {
            scope,
            state: State::Loading,
            settings: None,
            adoption: None,
            editor: Editor::new(""),
            problem: None,
            conflict: None,
            pending: None,
            preview: None,
            preview_top: 0,
            path: None,
            skills: Vec::new(),
        }
    }

    fn project(&self) -> Option<ProjectId> {
        match &self.scope {
            Scope::App => None,
            Scope::Project { id, .. } => Some(id.clone()),
        }
    }

    /// Sends one request unless another is running. A read the full queue refuses leaves
    /// the page failed with a retry, never loading for a reply that cannot come.
    /// Inputs: the query and the context. Output: none.
    fn send(&mut self, query: ProcessQuery, ctx: &mut Ctx) {
        if self.pending.is_some() {
            return;
        }
        if ctx.try_request(Request::Process(Box::new(query.clone()))) {
            self.pending = Some(query);
        } else if matches!(query, ProcessQuery::Read { .. }) {
            self.state = State::Failed(READ_REFUSED.into());
        }
    }

    fn read_adoption(&mut self, ctx: &mut Ctx) {
        if let Some(project) = self.project() {
            self.send(ProcessQuery::Adoption { project }, ctx);
        }
    }

    fn changed(&self) -> bool {
        matches!(&self.state, State::Ready { saved, .. } if *saved != self.editor.text())
    }

    fn invalid(&self) -> Option<String> {
        (self.editor.bytes() > TEXT_BYTES_MAX).then(|| message("process_text_too_large"))
    }

    fn save(&mut self, ctx: &mut Ctx) {
        if let Some(problem) = self.invalid() {
            self.problem = Some(problem);
            return;
        }
        if !self.changed() {
            return;
        }
        let State::Ready { version, .. } = self.state else {
            return;
        };
        let query = ProcessQuery::Save {
            project: self.project(),
            text: self.editor.text(),
            version,
            opened: None,
        };
        self.send(query, ctx);
    }

    fn keep_mine(&mut self, ctx: &mut Ctx) {
        if let Some(problem) = self.invalid() {
            self.problem = Some(problem);
            return;
        }
        let Some(conflict) = &self.conflict else {
            return;
        };
        let (version, opened) = conflict.keep_mine();
        let query = ProcessQuery::Save {
            project: self.project(),
            text: self.editor.text(),
            version,
            opened,
        };
        self.send(query, ctx);
    }

    fn reload(&mut self) {
        if self.pending.is_some() {
            return;
        }
        let Some(conflict) = self.conflict.take() else {
            return;
        };
        self.editor = Editor::new(&conflict.current);
        self.state = State::Ready {
            saved: conflict.current,
            version: conflict.version,
        };
        self.problem = self.invalid();
    }

    /// Saves one change to the project's agent settings at the version read, starting from
    /// the stored settings, or from the defaults with no version when they could not be read,
    /// so the owner can replace them. Inputs: the change and the context. Output: unit.
    fn change_settings(&mut self, change: fn(&mut InstructionSettings), ctx: &mut Ctx) {
        let Some(project) = self.project() else {
            return;
        };
        let Some(stored) = &self.settings else {
            return;
        };
        let (mut settings, version) = match stored {
            Ok(view) => (view.settings, Some(view.version)),
            Err(_) => (InstructionSettings::default(), None),
        };
        change(&mut settings);
        self.send(
            ProcessQuery::SaveSettings {
                project,
                settings,
                version,
            },
            ctx,
        );
    }

    fn skill_at_cursor(&self) -> Option<&Skill> {
        let chars: Vec<char> = self.editor.lines[self.editor.row].chars().collect();
        let column = self.editor.column;
        skill_ranges(&chars, column, column + 1, &self.skills)
            .into_iter()
            .find(|&(start, end, _)| start <= column && column <= end)
            .map(|(_, _, index)| &self.skills[index])
    }

    fn open_skill(&self, ctx: &mut Ctx) -> Flow {
        let Some(skill) = self.skill_at_cursor() else {
            ctx.say(Message::warning(
                "No skill name at the cursor.",
                "Move the cursor onto an underlined skill name, then press Ctrl-O.",
            ));
            return Flow::Handled;
        };
        if self.changed() {
            ctx.say(Message::warning(
                "Save first.",
                "Ctrl-S saves your instructions; opening a skill leaves this page.",
            ));
            return Flow::Handled;
        }
        Flow::Replace(Box::new(SkillDetailPage::new(
            skill.id.clone(),
            skill.title.clone(),
        )))
    }

    fn apply(&mut self, event: &ProcessEvent, ctx: &mut Ctx) {
        if self.pending.as_ref() != Some(&event.query) {
            return;
        }
        self.pending = None;
        match (&event.query, &event.result) {
            (
                ProcessQuery::Read { .. },
                Ok(ProcessReply::Current(ProcessText { text, version })),
            ) => {
                self.editor = Editor::new(text);
                self.state = State::Ready {
                    saved: text.clone(),
                    version: *version,
                };
                self.conflict = None;
                if let (Some(project), None) = (self.project(), &self.settings) {
                    self.send(ProcessQuery::Settings { project }, ctx);
                }
            }
            (ProcessQuery::Read { .. }, Err(code)) => self.state = State::Failed(message(code)),
            (
                ProcessQuery::Save { text, .. },
                Ok(ProcessReply::Saved(reply @ SavedText::Saved { version: next, .. })),
            ) => {
                if let State::Ready { saved, version } = &mut self.state {
                    assert!(*next > *version);
                    saved.clone_from(text);
                    *version = *next;
                }
                self.problem = None;
                self.conflict = None;
                ctx.say(match reply.claude_warning() {
                    None => Message::success(
                        "Instructions saved.",
                        "Agents started from now on get them.",
                    ),
                    Some(warning) => Message::warning("Instructions saved.", &warning),
                });
            }
            (
                ProcessQuery::Save { .. },
                Ok(ProcessReply::Saved(SavedText::Stale {
                    version,
                    current,
                    writer,
                })),
            ) => {
                self.problem = Some(message("process_text_stale"));
                self.conflict = Some(Conflict {
                    opened: *version,
                    version: current.version,
                    current: current.text.clone(),
                    writer: writer.clone(),
                });
            }
            (ProcessQuery::Save { .. }, Err(code)) => self.problem = Some(message(code)),
            (ProcessQuery::ReadFile { .. }, Ok(ProcessReply::Text(text))) => {
                self.open_preview(Seed::Import, text);
            }
            (ProcessQuery::Default, Ok(ProcessReply::Text(text))) => {
                self.open_preview(Seed::Default, text);
            }
            (ProcessQuery::ReadFile { .. } | ProcessQuery::Default, Err(code)) => {
                ctx.say(Message::error(&message(code), ""));
            }
            (ProcessQuery::Settings { .. }, Ok(ProcessReply::Settings(settings))) => {
                self.settings = Some(Ok(*settings));
                self.read_adoption(ctx);
            }
            (ProcessQuery::Settings { .. }, Err(code)) => {
                self.settings = Some(Err(message(code)));
                self.read_adoption(ctx);
            }
            (ProcessQuery::Adoption { .. }, Ok(ProcessReply::Adoption(view))) => {
                self.adoption = Some(Ok((**view).clone()));
            }
            (ProcessQuery::Adoption { .. }, Err(code)) => {
                self.adoption = Some(Err(message(code)));
            }
            (ProcessQuery::SaveSettings { .. }, Ok(ProcessReply::Settings(settings))) => {
                self.settings = Some(Ok(*settings));
                ctx.say(Message::success(
                    "Agent settings saved.",
                    "Agents started from now on follow them.",
                ));
            }
            (ProcessQuery::SaveSettings { project, .. }, Err(code))
                if code == crate::storage::versioned::STALE_CODE
                    || code == "process_outcome_unknown" =>
            {
                self.settings = None;
                self.send(
                    ProcessQuery::Settings {
                        project: project.clone(),
                    },
                    ctx,
                );
                ctx.say(match code == crate::storage::versioned::STALE_CODE {
                    true => Message::warning(
                        "These agent settings changed since you opened them.",
                        "The page now shows the saved ones; make your change again.",
                    ),
                    false => Message::warning(
                        "CrazyAgent couldn't confirm these agent settings were saved.",
                        "The page now shows the saved ones; check them before you change them again.",
                    ),
                });
            }
            (ProcessQuery::SaveSettings { .. }, Err(code)) => {
                ctx.say(Message::error(&message(code), ""));
            }
            _ => {}
        }
    }

    /// Edits the path of the file to import while its field is open: typed characters up to
    /// `PATH_BYTES_MAX` bytes, Backspace, Enter to read the file as a preview (a leading `~/`
    /// is the owner's home folder) and Esc to close the field. Inputs: the key and the
    /// context. Output: handled, so no key reaches the editor meanwhile.
    fn path_key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let Some(path) = self.path.as_mut() else {
            return Flow::Ignored;
        };
        match key {
            Key::Char(character) if path.len() + character.len_utf8() <= PATH_BYTES_MAX => {
                path.push(*character);
            }
            Key::Backspace => {
                path.pop();
            }
            Key::Esc => self.path = None,
            Key::Enter if !path.trim().is_empty() => {
                let typed = path.trim().to_owned();
                self.path = None;
                let path = match typed.strip_prefix("~/") {
                    Some(rest) => crate::projects_home::owner_home()
                        .map(|home| home.join(rest))
                        .unwrap_or_else(|_| std::path::PathBuf::from(&typed)),
                    None => std::path::PathBuf::from(&typed),
                };
                self.send(ProcessQuery::ReadFile { path }, ctx);
            }
            _ => {}
        }
        assert!(
            self.path
                .as_ref()
                .is_none_or(|path| path.len() <= PATH_BYTES_MAX)
        );
        Flow::Handled
    }

    fn open_preview(&mut self, seed: Seed, text: &str) {
        self.preview = Some((seed, text.to_owned()));
        self.preview_top = 0;
    }

    fn preview_key(&mut self, key: &Key) -> Flow {
        match key {
            Key::Enter => {
                if let Some((_, text)) = self.preview.take() {
                    self.editor = Editor::new(&text);
                    self.problem = self.invalid();
                }
            }
            Key::Esc => self.preview = None,
            Key::Up => self.preview_top = self.preview_top.saturating_sub(1),
            Key::Down => self.preview_top += 1,
            Key::PageUp => self.preview_top = self.preview_top.saturating_sub(PAGE_STEP),
            Key::PageDown => self.preview_top += PAGE_STEP,
            _ => {}
        }
        Flow::Handled
    }

    fn content(
        &mut self,
        width: usize,
        height: usize,
        ctx: &Ctx,
    ) -> (Vec<Line<'static>>, Option<Position>) {
        let theme = &ctx.theme;
        let mut lines = vec![match &self.scope {
            Scope::App => styled("Process", Role::Accent, theme),
            Scope::Project { name, .. } => {
                styled(format!("Process for {name}"), Role::Accent, theme)
            }
        }];
        let intro = match &self.scope {
            Scope::App => "Instructions every master and sub-agent gets, in every project. A project's own instructions come after these.".to_owned(),
            Scope::Project { name, .. } => format!("Instructions every agent in {name} gets, after the app-wide ones. Kept in the project's .crazyagent/instructions.md, which git ignores."),
        };
        let seeds = "Ctrl-G imports an instructions file you name, such as a CLAUDE.md, and Ctrl-D the CrazyAgent default process, each as a preview first.";
        let intro = match self.scope {
            Scope::App => format!("{intro} {seeds}"),
            Scope::Project { .. } => intro,
        };
        lines.extend(
            wrap(&intro, width)
                .into_iter()
                .map(|text| styled(text, Role::Muted, theme)),
        );
        lines.push(Line::default());
        match &self.state {
            State::Loading => {
                lines.push(ctx.loading("Loading the instructions"));
                return (lines, None);
            }
            State::Failed(problem) => {
                lines.push(styled(problem.clone(), Role::Error, theme));
                lines.push(styled("Press r to try again.", Role::Muted, theme));
                return (lines, None);
            }
            State::Ready { .. } => {}
        }
        if let Scope::Project { .. } = self.scope {
            self.settings_lines(width, &mut lines, ctx);
            self.adoption_lines(width, &mut lines, ctx);
        }
        if let Some(conflict) = &self.conflict {
            lines.extend(
                wrap(&conflict.heading(), width)
                    .into_iter()
                    .map(|text| styled(text, Role::Warning, theme)),
            );
            let body = quoted(&conflict.current, width);
            let more = body.len().saturating_sub(LINES_SHOWN_MAX);
            lines.extend(body.into_iter().take(LINES_SHOWN_MAX).map(|text| {
                Line::from(vec![
                    Span::styled(format!("{} ", ctx.glyphs.vline), theme.muted),
                    Span::raw(text),
                ])
            }));
            if more > 0 {
                lines.push(styled(format!("{more} more lines"), Role::Muted, theme));
            }
            lines.extend(
                wrap(CHOICES, width)
                    .into_iter()
                    .map(|text| styled(text, Role::Muted, theme)),
            );
            lines.push(Line::default());
        }
        let rows = height
            .saturating_sub(lines.len() + EDITOR_CHROME_ROWS)
            .max(EDITOR_ROWS_MIN);
        if let Some(path) = &self.path {
            lines.push(styled("Import from file", Role::Heading, theme));
            lines.extend(
                wrap("Type the file's full path; ~/ is your home folder. Enter reads it as a preview, Esc cancels. Nothing is saved until Ctrl-S.", width)
                    .into_iter()
                    .map(|text| styled(text, Role::Muted, theme)),
            );
            let room = width.saturating_sub(3).max(1);
            let mut used = 0;
            let mut tail: Vec<char> = path
                .chars()
                .rev()
                .map(shown)
                .take_while(|character| {
                    used += char_width(*character);
                    used <= room
                })
                .collect();
            tail.reverse();
            let column = 2 + tail.iter().copied().map(char_width).sum::<usize>();
            assert!(column <= room + 2);
            let tail: String = tail.into_iter().collect();
            let at = lines.len();
            lines.push(Line::from(vec![
                Span::styled(format!("{} ", ctx.glyphs.vline), theme.accent),
                Span::raw(tail),
            ]));
            return (lines, Some(Position::new(column as u16, at as u16)));
        }
        if let Some((seed, text)) = &self.preview {
            let (title, note) = match seed {
                Seed::Import => (
                    "Instructions from your file",
                    "Read once from the file you named; afterwards the instructions live here. Enter puts it in the editor in place of its text; nothing is saved until Ctrl-S.",
                ),
                Seed::Default => (
                    "CrazyAgent default process",
                    "Enter puts it in the editor in place of its text; nothing is saved until Ctrl-S.",
                ),
            };
            lines.push(styled(title, Role::Heading, theme));
            lines.extend(
                wrap(note, width)
                    .into_iter()
                    .map(|text| styled(text, Role::Muted, theme)),
            );
            let body = quoted(text, width);
            self.preview_top = self.preview_top.min(body.len().saturating_sub(1));
            lines.extend(
                body.into_iter()
                    .skip(self.preview_top)
                    .take(rows)
                    .map(|text| {
                        Line::from(vec![
                            Span::styled(format!("{} ", ctx.glyphs.vline), theme.muted),
                            Span::raw(text),
                        ])
                    }),
            );
            return (lines, None);
        }
        let right = format!(
            "{} of {}{}",
            size(self.editor.bytes()),
            size(TEXT_BYTES_MAX),
            if self.changed() {
                ". Not saved yet"
            } else {
                ""
            }
        );
        lines.push(row(
            width,
            styled("Instructions for agents", Role::Heading, theme),
            styled(right, Role::Muted, theme),
            ctx.glyphs,
        ));
        let gutter = 2;
        let (shown_lines, (column, line)) =
            self.editor.view(width.saturating_sub(gutter).max(2), rows);
        let first = lines.len();
        let (top, left) = (self.editor.top, self.editor.left);
        let tag = theme.accent.add_modifier(Modifier::UNDERLINED);
        for (offset, text) in shown_lines.into_iter().enumerate() {
            let chars: Vec<char> = self.editor.lines[top + offset].chars().collect();
            let drawn = text.chars().count();
            let ranges = skill_ranges(&chars, left, left + drawn, &self.skills);
            let mut spans = vec![Span::styled(format!("{} ", ctx.glyphs.vline), theme.accent)];
            spans.extend(tagged(&text, left, &ranges, tag));
            lines.push(Line::from(spans));
        }
        if let Some(problem) = &self.problem {
            lines.push(styled(problem.clone(), Role::Error, theme));
        } else if let Some(pending) = &self.pending {
            let doing = match pending {
                ProcessQuery::Save { .. } | ProcessQuery::SaveSettings { .. } => "Saving...",
                _ => "Reading...",
            };
            lines.push(styled(doing, Role::Muted, theme));
        }
        let cursor = Position::new((gutter + column) as u16, (first + line) as u16);
        (lines, Some(cursor))
    }

    /// Adds the project's agent settings above the editor: each setting with its option and
    /// key, what the chosen instruction files option hands agents, or why they could not be
    /// read. Inputs: the width, the lines so far and the context. Output: unit.
    fn settings_lines(&self, width: usize, lines: &mut Vec<Line<'static>>, ctx: &Ctx) {
        let theme = &ctx.theme;
        let settings = match &self.settings {
            None => {
                lines.push(ctx.loading("Loading the agent settings"));
                lines.push(Line::default());
                return;
            }
            Some(Err(problem)) => {
                lines.extend(
                    wrap(problem, width)
                        .into_iter()
                        .map(|text| styled(text, Role::Error, theme)),
                );
                lines.push(Line::default());
                return;
            }
            Some(Ok(view)) => view.settings,
        };
        let transcripts = if settings.transcripts { "On" } else { "Off" };
        for (name, option, key) in [
            ("Agent instructions", mode_label(settings.mode), "Ctrl-F"),
            (
                "Keep transcripts for headless Claude runs",
                transcripts,
                "Ctrl-T",
            ),
            ("Instruction edits", edits_label(settings.edits), "Ctrl-R"),
        ] {
            lines.push(row(
                width,
                Line::from(vec![
                    Span::raw(format!("{name}: ")),
                    Span::styled(option.to_owned(), theme.accent),
                ]),
                styled(format!("{key} changes"), Role::Muted, theme),
                ctx.glyphs,
            ));
        }
        for note in [mode_note(settings.mode), edits_note(settings.edits)] {
            lines.extend(
                wrap(note, width)
                    .into_iter()
                    .map(|text| styled(text, Role::Muted, theme)),
            );
        }
        lines.push(Line::default());
    }
}

impl ProcessPage {
    /// Adds the project's workflow adoption below its settings: its nudges option, then
    /// the newest agents, at most `ADOPTION_ROWS_MAX`, each with its count per counted tool
    /// and its flags in red, marked when the agent was reminded. Inputs: the width, the
    /// lines so far and the context. Output: unit.
    fn adoption_lines(&self, width: usize, lines: &mut Vec<Line<'static>>, ctx: &Ctx) {
        let theme = &ctx.theme;
        let adoption = match &self.adoption {
            None => return,
            Some(Err(problem)) => {
                lines.extend(
                    wrap(problem, width)
                        .into_iter()
                        .map(|text| styled(text, Role::Error, theme)),
                );
                lines.push(Line::default());
                return;
            }
            Some(Ok(adoption)) => adoption,
        };
        let nudges = match adoption.nudges {
            Nudges::Off => "Nudges off: flags show here, agents get no reminder.",
            Nudges::TenMinutes => {
                "Nudges every 10 minutes: each flag sends that agent one reminder."
            }
            Nudges::FirstEdit => {
                "Nudges on first edit: an edit before any graph query gets one reminder."
            }
        };
        lines.push(row(
            width,
            styled("Workflow adoption", Role::Heading, theme),
            styled(nudges, Role::Muted, theme),
            ctx.glyphs,
        ));
        if adoption.agents.is_empty() {
            lines.push(styled("No agents yet.", Role::Muted, theme));
        }
        for agent in adoption.agents.iter().take(ADOPTION_ROWS_MAX) {
            let counts: Vec<String> = adoption
                .counted
                .iter()
                .zip(&agent.counts)
                .map(|(tool, count)| format!("{tool} {count}"))
                .collect();
            let name = format!("#{} {} {}", agent.agent, agent.role, agent.harness);
            let role = if agent.active {
                Role::Accent
            } else {
                Role::Muted
            };
            lines.push(styled(name, role, theme));
            lines.extend(
                wrap(&counts.join("  "), width.saturating_sub(2).max(1))
                    .into_iter()
                    .map(|text| styled(format!("  {text}"), Role::Muted, theme)),
            );
            let skills = skill_words(&adoption.skills, agent);
            lines.extend(
                wrap(&skills, width.saturating_sub(2).max(1))
                    .into_iter()
                    .map(|text| styled(format!("  {text}"), Role::Muted, theme)),
            );
            for flag in &agent.flags {
                let reminded = if agent.reminded.contains(flag) {
                    ", reminded"
                } else {
                    ""
                };
                lines.push(styled(
                    format!("  {}{reminded}", flag.words()),
                    Role::Error,
                    theme,
                ));
            }
        }
        if adoption.agents.len() > ADOPTION_ROWS_MAX || adoption.hidden > 0 {
            let more = adoption.agents.len().saturating_sub(ADOPTION_ROWS_MAX)
                + usize::try_from(adoption.hidden).unwrap_or(usize::MAX);
            lines.push(styled(
                format!("and {more} older agents, shown in the desktop app"),
                Role::Muted,
                theme,
            ));
        }
        lines.push(Line::default());
    }
}

impl Screen for ProcessPage {
    fn crumbs(&self) -> Vec<String> {
        match &self.scope {
            Scope::App => vec!["Settings".into(), "Process".into()],
            Scope::Project { name, .. } => vec![name.clone(), "Process".into()],
        }
    }

    fn hints(&self, ctx: &Ctx) -> Vec<Hint> {
        if self.path.is_some() {
            return vec![hint("Enter", "Read file"), hint("Esc", "Cancel")];
        }
        if self.preview.is_some() {
            return vec![
                hint("Enter", "Put in the editor"),
                hint(&format!("{}{}", ctx.glyphs.up, ctx.glyphs.down), "Scroll"),
                hint("Esc", "Cancel"),
            ];
        }
        let back = match self.scope {
            Scope::App => hint("Esc", "Home"),
            Scope::Project { .. } => hint("Esc", "Back"),
        };
        match self.state {
            State::Ready { .. } => {}
            State::Failed(_) => return vec![hint("r", "Try again"), back],
            State::Loading => return vec![back],
        }
        let mut hints = Vec::new();
        if self.conflict.is_some() {
            hints.push(hint("Ctrl-R", "Reload"));
            hints.push(hint("Ctrl-Y", "Keep mine"));
        }
        hints.push(hint("Ctrl-S", "Save"));
        if !self.skills.is_empty() {
            hints.push(hint("Ctrl-O", "Open skill"));
        }
        if matches!(self.scope, Scope::Project { .. }) && self.settings.is_some() {
            hints.push(hint("Ctrl-F", "Instructions"));
            hints.push(hint("Ctrl-T", "Transcripts"));
            if self.conflict.is_none() {
                hints.push(hint("Ctrl-R", "Instruction edits"));
            }
        }
        if self.scope == Scope::App {
            hints.push(hint("Ctrl-G", "Import from file"));
            hints.push(hint("Ctrl-D", "Default process"));
            hints.push(sections_hint(ctx.glyphs));
        }
        hints.push(back);
        hints
    }

    fn typing(&self) -> bool {
        matches!(self.state, State::Ready { .. }) && self.preview.is_none()
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        if self.path.is_some() {
            return self.path_key(key, ctx);
        }
        if self.preview.is_some() {
            return self.preview_key(key);
        }
        match (&self.state, key) {
            (State::Failed(_), Key::Char('r') | Key::Enter) => {
                self.state = State::Loading;
                let query = ProcessQuery::Read {
                    project: self.project(),
                };
                self.send(query, ctx);
                Flow::Handled
            }
            (State::Ready { .. }, Key::Ctrl('s')) => {
                self.save(ctx);
                Flow::Handled
            }
            (State::Ready { .. }, Key::Ctrl('g')) if self.scope == Scope::App => {
                if self.pending.is_none() {
                    self.path = Some(String::new());
                }
                Flow::Handled
            }
            (State::Ready { .. }, Key::Ctrl('d')) if self.scope == Scope::App => {
                self.send(ProcessQuery::Default, ctx);
                Flow::Handled
            }
            (State::Ready { .. }, Key::Ctrl('f')) if self.settings.is_some() => {
                self.change_settings(|settings| settings.mode = next_mode(settings.mode), ctx);
                Flow::Handled
            }
            (State::Ready { .. }, Key::Ctrl('t')) if self.settings.is_some() => {
                self.change_settings(|settings| settings.transcripts = !settings.transcripts, ctx);
                Flow::Handled
            }
            (State::Ready { .. }, Key::Ctrl('r')) if self.conflict.is_some() => {
                self.reload();
                Flow::Handled
            }
            (State::Ready { .. }, Key::Ctrl('r')) if self.settings.is_some() => {
                self.change_settings(|settings| settings.edits = next_edits(settings.edits), ctx);
                Flow::Handled
            }
            (State::Ready { .. }, Key::Ctrl('o')) => self.open_skill(ctx),
            (State::Ready { .. }, Key::Ctrl('y')) if self.conflict.is_some() => {
                self.keep_mine(ctx);
                Flow::Handled
            }
            (State::Ready { .. }, key) if self.editor.key(key) => {
                self.problem = self.invalid();
                Flow::Handled
            }
            _ => Flow::Ignored,
        }
    }

    fn step(&mut self, forward: bool, ctx: &mut Ctx) -> Flow {
        match self.scope {
            Scope::App => step(SettingsSection::Process, forward, ctx),
            Scope::Project { .. } => Flow::Ignored,
        }
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        if let Arrival::Owner(event) = arrival
            && let OwnerEvent::Process(event) = event.as_ref()
        {
            self.apply(event, ctx);
        }
        match (settings_event(arrival), &self.scope) {
            (Some(SettingsEvent::Plugins(Ok((skills, _)))), Scope::App) => {
                self.skills = catalog(skills);
            }
            (
                Some(SettingsEvent::ProjectPlugins(project, Ok((skills, _)))),
                Scope::Project { id, .. },
            ) if project == id => {
                self.skills = catalog(skills);
            }
            _ => {}
        }
        Flow::Handled
    }

    fn shown(&mut self, ctx: &mut Ctx) {
        let job = match &self.scope {
            Scope::App => SettingsJob::Plugins,
            Scope::Project { id, .. } => SettingsJob::ProjectPlugins {
                project: id.clone(),
            },
        };
        send_settings(ctx, job);
        if self.state == State::Loading {
            let query = ProcessQuery::Read {
                project: self.project(),
            };
            self.send(query, ctx);
        } else if self.settings.is_some() {
            self.read_adoption(ctx);
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let width = (area.width as usize).saturating_sub(4).max(8);
        match self.scope {
            Scope::App => {
                let height = (area.height as usize).saturating_sub(2);
                let (content, cursor) = self.content(width, height, ctx);
                draw_page(frame, area, SettingsSection::Process, content, cursor, ctx)
            }
            Scope::Project { .. } => {
                let (content, cursor) = self.content(width, area.height as usize, ctx);
                draw_lines(frame.buffer_mut(), area, &indent(content, 2));
                cursor.map(|at| Position::new(area.x + 2 + at.x, area.y + at.y))
            }
        }
    }

    fn busy(&self, _ctx: &Ctx) -> bool {
        self.pending.is_some()
    }

    fn draft(&self) -> bool {
        self.changed()
    }

    fn loss(&self) -> &'static str {
        "Your instructions changes will be lost."
    }

    fn place(&self) -> Option<Place> {
        Some(match &self.scope {
            Scope::App => Place::Settings(SettingsSection::Process),
            Scope::Project { id, name } => Place::ProjectProcess {
                id: id.clone(),
                name: name.clone(),
            },
        })
    }
}
