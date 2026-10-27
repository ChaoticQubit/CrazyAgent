use super::settings::{draw_page, step};
use crate::agent_home::TEXT_BYTES_MAX;
use crate::protocol::ProjectId;
use crate::tui::client::OwnerEvent;
use crate::tui::client::process::{ProcessEvent, ProcessQuery};
use crate::tui::ctx::Ctx;
use crate::tui::frame::{Hint, Message, draw_lines, hint, sections_hint};
use crate::tui::keys::Key;
use crate::tui::screens::{Flow, Place, Screen, SettingsSection};
use crate::tui::seam::{Arrival, Request};
use crate::tui::text::{char_width, coded, indent, row, styled, wrap};
use crate::tui::theme::Role;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};

#[cfg(test)]
mod tests;

const PAGE_STEP: usize = 10;
const EDITOR_ROWS_MIN: usize = 3;
const EDITOR_CHROME_ROWS: usize = 2;

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
    Ready { saved: String },
}

/// The one plain sentence for a Process refusal. Input: the code.
/// Output: the sentence, or the fallback naming the code.
fn message(code: &str) -> String {
    match code {
        "process_text_too_large" => "This text is over 64 KB. Shorten it, then save again.".into(),
        "process_text_invalid" => {
            "This text has characters CrazyAgent can't save. Remove them, then save again.".into()
        }
        "process_text_unsafe" => "The instructions file is a link or not a plain file, so CrazyAgent won't read it. Replace it with a plain file.".into(),
        "process_text_unreadable" => "The instructions file couldn't be read. Check it, then try again.".into(),
        "process_text_unwritable" => "The instructions couldn't be saved. Check the folder, then try again.".into(),
        "process_project_folder_missing" => "This project keeps its files without a .crazyagent folder, so it can't hold project instructions.".into(),
        "process_project_unknown" => "This project has no setup yet, so it can't hold instructions.".into(),
        "process_import_missing" => "There is no ~/.claude/CLAUDE.md to import.".into(),
        "process_folder_missing" => "CrazyAgent's data folder is missing. Restart the engine, then try again.".into(),
        "process_folder_unsafe" => "CrazyAgent's data folder isn't a plain folder, so nothing was saved.".into(),
        "process_unavailable" => "Instructions aren't ready in this engine yet.".into(),
        "trusted_connection_unavailable" => "Not connected to the engine. Try again once it reconnects.".into(),
        other => coded("CrazyAgent couldn't finish that.", other),
    }
}

fn size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes} bytes")
    } else {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    }
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

    fn insert(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let mut parts = text.split('\n');
        let first = parts.next().unwrap_or_default();
        let at = byte_at(&self.lines[self.row], self.column);
        let rest = self.lines[self.row].split_off(at);
        self.lines[self.row].push_str(first);
        self.column += first.chars().count();
        for part in parts {
            self.lines.insert(self.row + 1, part.to_owned());
            self.row += 1;
            self.column = part.chars().count();
        }
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
        let cells = |from: usize, to: usize| -> usize {
            line[from..to].iter().map(|c| char_width(shown(*c))).sum()
        };
        self.left = self.left.min(self.column);
        while cells(self.left, self.column) >= width {
            self.left += 1;
        }
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
        let cursor = (cells(self.left, self.column), self.row - self.top);
        assert!(cursor.0 < width && cursor.1 < rows);
        (shown_lines, cursor)
    }
}

/// The Process page: the owner's instructions every agent gets, app-wide in Settings or
/// for one project. Reads the saved text on opening, edits it in place and saves it only on
/// Ctrl-S. At app level Ctrl-G reads the owner's global instructions and Ctrl-D the default
/// process, each into a preview first. One request runs at a time.
pub(in crate::tui) struct ProcessPage {
    scope: Scope,
    state: State,
    editor: Editor,
    problem: Option<String>,
    pending: Option<ProcessQuery>,
    preview: Option<(Seed, String)>,
    preview_top: usize,
}

impl ProcessPage {
    /// The page for one scope, loading until the engine answers.
    /// Input: where the text applies. Output: the screen.
    pub fn new(scope: Scope) -> Self {
        Self {
            scope,
            state: State::Loading,
            editor: Editor::new(""),
            problem: None,
            pending: None,
            preview: None,
            preview_top: 0,
        }
    }

    fn project(&self) -> Option<ProjectId> {
        match &self.scope {
            Scope::App => None,
            Scope::Project { id, .. } => Some(id.clone()),
        }
    }

    fn send(&mut self, query: ProcessQuery, ctx: &mut Ctx) {
        if self.pending.is_some() {
            return;
        }
        self.pending = Some(query.clone());
        ctx.request(Request::Process(Box::new(query)));
    }

    fn changed(&self) -> bool {
        matches!(&self.state, State::Ready { saved } if *saved != self.editor.text())
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
        let query = ProcessQuery::Save {
            project: self.project(),
            text: self.editor.text(),
        };
        self.send(query, ctx);
    }

    fn apply(&mut self, event: &ProcessEvent, ctx: &mut Ctx) {
        if self.pending.as_ref() != Some(&event.query) {
            return;
        }
        self.pending = None;
        match (&event.query, &event.result) {
            (ProcessQuery::Read { .. }, Ok(text)) => {
                self.editor = Editor::new(text);
                self.state = State::Ready {
                    saved: text.clone(),
                };
            }
            (ProcessQuery::Read { .. }, Err(code)) => self.state = State::Failed(message(code)),
            (ProcessQuery::Save { text, .. }, Ok(_)) => {
                if let State::Ready { saved } = &mut self.state {
                    saved.clone_from(text);
                }
                self.problem = None;
                ctx.say(Message::success(
                    "Instructions saved.",
                    "Agents started from now on get them.",
                ));
            }
            (ProcessQuery::Save { .. }, Err(code)) => self.problem = Some(message(code)),
            (ProcessQuery::ImportGlobal, Ok(text)) => self.open_preview(Seed::Import, text),
            (ProcessQuery::Default, Ok(text)) => self.open_preview(Seed::Default, text),
            (ProcessQuery::ImportGlobal | ProcessQuery::Default, Err(code)) => {
                ctx.say(Message::error(&message(code), ""));
            }
        }
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
        let seeds = "Ctrl-G imports your ~/.claude/CLAUDE.md and Ctrl-D the CrazyAgent default process, each as a preview first.";
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
        let rows = height
            .saturating_sub(lines.len() + EDITOR_CHROME_ROWS)
            .max(EDITOR_ROWS_MIN);
        if let Some((seed, text)) = &self.preview {
            let (title, note) = match seed {
                Seed::Import => (
                    "Your global instructions",
                    "Read from ~/.claude/CLAUDE.md. Enter puts it in the editor in place of its text; nothing is saved until Ctrl-S.",
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
            let body: Vec<String> = crate::tui::screens::project_markdown::sanitize(text)
                .split('\n')
                .flat_map(|line| {
                    let wrapped = wrap(line, width.saturating_sub(2).max(1));
                    if wrapped.is_empty() {
                        vec![String::new()]
                    } else {
                        wrapped
                    }
                })
                .collect();
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
        for text in shown_lines {
            lines.push(Line::from(vec![
                Span::styled(format!("{} ", ctx.glyphs.vline), theme.accent),
                Span::raw(text),
            ]));
        }
        if let Some(problem) = &self.problem {
            lines.push(styled(problem.clone(), Role::Error, theme));
        } else if let Some(pending) = &self.pending {
            let doing = match pending {
                ProcessQuery::Save { .. } => "Saving...",
                _ => "Reading...",
            };
            lines.push(styled(doing, Role::Muted, theme));
        }
        let cursor = Position::new((gutter + column) as u16, (first + line) as u16);
        (lines, Some(cursor))
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
        let mut hints = vec![hint("Ctrl-S", "Save")];
        if self.scope == Scope::App {
            hints.push(hint("Ctrl-G", "Import global"));
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
                self.send(ProcessQuery::ImportGlobal, ctx);
                Flow::Handled
            }
            (State::Ready { .. }, Key::Ctrl('d')) if self.scope == Scope::App => {
                self.send(ProcessQuery::Default, ctx);
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
        Flow::Handled
    }

    fn shown(&mut self, ctx: &mut Ctx) {
        if self.state == State::Loading {
            let query = ProcessQuery::Read {
                project: self.project(),
            };
            self.send(query, ctx);
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
