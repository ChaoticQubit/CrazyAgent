use crate::connectors::audit::{ActivityView, CallKind, CallView};
use crate::connectors::{ConnectorSettings, KeepLog};
use crate::engine::owner::ConnectorsSaved;
use crate::tui::client::connectors::{ConnectorsEvent, ConnectorsJob};
use crate::tui::client::settings::{SettingsEvent, SettingsJob};
use crate::tui::clock::stamp;
use crate::tui::ctx::{Ctx, Load};
use crate::tui::frame::{Hint, Message, draw_lines, hint};
use crate::tui::keys::Key;
use crate::tui::screens::project_time::{local_zone, zone_offset};
use crate::tui::screens::settings_accounts::{plain, send, settings_event};
use crate::tui::screens::{Flow, Place, Screen};
use crate::tui::seam::Arrival;
use crate::tui::text::{cut, list_row, wrap};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};

const GUTTER: u16 = 2;
/// The page's title, the words the desktop's Settings list and page use too.
pub(in crate::tui) const TITLE: &str = "Connector activity";
const PURPOSE: &str = "Every tool list and tool call an agent made through a connector. Arguments and results are never kept.";
const KEEP_LABEL: &str = "Activity log keeps";
const CONNECTOR_LABEL: &str = "Connector";
const PROJECT_LABEL: &str = "Project";
const ALL: &str = "All";
const KEEP_CHOICES: [(KeepLog, &str); 3] = [
    (KeepLog::Days7, "7 days"),
    (KeepLog::Days30, "30 days"),
    (KeepLog::Days90, "90 days"),
];
const CALLS: &str = "Calls";
const EMPTY: &str = "No connector calls yet.";
const NONE_MATCH: &str = "No calls match these filters.";
const MORE: &str = "Showing the newest calls. Filter by connector or project to see others.";
const SAVED: &str = "Connector settings saved.";
const STALE: &str = "The connector settings changed elsewhere. Check them and try again.";
const NOT_SAVED: &str = "The connector settings were not saved.";
const NOT_READ: &str = "Couldn't load the connector activity.";
const CONTROLS: usize = 3;

const _: () = assert!(KEEP_LABEL.len() >= CONNECTOR_LABEL.len());
const _: () = assert!(KEEP_LABEL.len() >= PROJECT_LABEL.len());

/// The word for a call's kind, the same in both apps. Input: the kind. Output: the word.
fn kind_word(kind: CallKind) -> &'static str {
    match kind {
        CallKind::Read => "Read",
        CallKind::Write => "Write",
        CallKind::List => "Tool list",
        CallKind::Unknown => "Unknown tool",
    }
}

fn bytes_word(bytes: i64) -> String {
    const KIB: i64 = 1024;
    const MIB: i64 = 1024 * 1024;
    let bytes = bytes.max(0);
    if bytes < KIB {
        return format!("{bytes} B");
    }
    let (unit, size) = if bytes < MIB {
        ("KiB", KIB)
    } else {
        ("MiB", MIB)
    };
    let tenths = (bytes.saturating_mul(10) + size / 2) / size;
    format!("{}.{} {unit}", tenths / 10, tenths % 10)
}

/// A call's sizes as both apps show them, such as "120 B in, 2.0 KiB out". Inputs: the
/// bytes sent and received. Output: the words.
pub(in crate::tui) fn size_words(bytes_in: i64, bytes_out: i64) -> String {
    format!("{} in, {} out", bytes_word(bytes_in), bytes_word(bytes_out))
}

/// The plain words for a refusal code: the engine's connector sentence with its first
/// letter raised when it has one, else the app's own. Input: the code. Output: the words.
pub(in crate::tui) fn problem_words(code: &str) -> String {
    match crate::connectors::plain_problem(code) {
        Some(sentence) => {
            let mut letters = sentence.chars();
            letters.next().map_or_else(String::new, |first| {
                first.to_uppercase().chain(letters).collect()
            })
        }
        None => plain(code).to_owned(),
    }
}

/// Which line the cursor rests on: the keep setting, one of the two filters, or a call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Row {
    Keep,
    Connector,
    Project,
    Call(usize),
}

/// The choice next to `now` in `options`, wrapping at both ends; the first one when `now`
/// is not offered. Inputs: the options, the current choice and the direction. Output: it.
pub(in crate::tui) fn turn<T: Clone + PartialEq>(options: &[T], now: &T, forward: bool) -> T {
    assert!(!options.is_empty());
    let count = options.len();
    let Some(at) = options.iter().position(|choice| choice == now) else {
        return options[0].clone();
    };
    let next = if forward {
        (at + 1) % count
    } else {
        (at + count - 1) % count
    };
    options[next].clone()
}

/// Settings, Connector activity (DEC-168 addendum 8): the newest connector calls with
/// who, what, when, how large and how each ended, filtered by connector and project, and
/// how long the log keeps calls beside them. A keep pick stays unsaved until Enter sends
/// the whole connector settings with the version read; Esc drops it first. Opened by Go to.
pub(in crate::tui) struct ConnectorActivityPage {
    stored: Load<ActivityView>,
    connector: Option<String>,
    project: Option<String>,
    pick: KeepLog,
    selected: usize,
    top: usize,
    reading: bool,
    saving: bool,
}

impl ConnectorActivityPage {
    /// Opens the page with nothing read yet. Input: none. Output: the page; the read goes
    /// out when it is shown.
    pub fn new() -> Self {
        Self {
            stored: Load::Idle,
            connector: None,
            project: None,
            pick: ConnectorSettings::DEFAULT.keep_log,
            selected: 0,
            top: 0,
            reading: false,
            saving: false,
        }
    }

    fn read(&mut self, ctx: &mut Ctx) {
        if self.reading {
            return;
        }
        if self.stored.ready().is_none() {
            self.stored = Load::Loading;
        }
        self.reading = true;
        send(
            ctx,
            SettingsJob::Connectors(Box::new(ConnectorsJob::Activity {
                connector: self.connector.clone(),
                project: self.project.clone(),
            })),
        );
    }

    fn edited(&self) -> bool {
        self.stored
            .ready()
            .is_some_and(|view| view.settings.keep_log != self.pick)
    }

    fn rows(&self) -> Vec<Row> {
        let calls = self.stored.ready().map_or(0, |view| view.calls.len());
        let mut rows = vec![Row::Keep, Row::Connector, Row::Project];
        rows.extend((0..calls).map(Row::Call));
        assert_eq!(rows.len(), CONTROLS + calls);
        rows
    }

    fn moved(&mut self, to: usize) {
        self.selected = to.min(self.rows().len().saturating_sub(1));
    }

    fn change(&mut self, forward: bool, ctx: &mut Ctx) {
        let Some(view) = self.stored.ready() else {
            return;
        };
        match self.rows()[self.selected] {
            Row::Keep => {
                let options = KEEP_CHOICES.map(|(keep, _)| keep);
                self.pick = turn(&options, &self.pick, forward);
            }
            Row::Connector => {
                let mut options = vec![None];
                options.extend(view.connectors.iter().cloned().map(Some));
                self.connector = turn(&options, &self.connector, forward);
                self.read(ctx);
            }
            Row::Project => {
                let mut options = vec![None];
                options.extend(view.projects.iter().map(|choice| Some(choice.id.clone())));
                self.project = turn(&options, &self.project, forward);
                self.read(ctx);
            }
            Row::Call(_) => {}
        }
    }

    fn save(&mut self, ctx: &mut Ctx) {
        let Some(view) = self.stored.ready() else {
            return;
        };
        if !self.edited() || self.saving {
            return;
        }
        let job = ConnectorsJob::SaveSettings {
            settings: ConnectorSettings {
                keep_log: self.pick,
                ..view.settings
            },
            version: view.version,
        };
        self.saving = true;
        send(ctx, SettingsJob::Connectors(Box::new(job)));
    }

    fn take(&mut self, event: &ConnectorsEvent, ctx: &mut Ctx) {
        match event {
            ConnectorsEvent::Activity(result) => {
                if !self.reading {
                    return;
                }
                self.reading = false;
                match result {
                    Ok(view) => {
                        if view.connector != self.connector || view.project != self.project {
                            self.read(ctx);
                            return;
                        }
                        if !self.edited() {
                            self.pick = view.settings.keep_log;
                        }
                        self.stored = Load::Ready(view.clone());
                        self.moved(self.selected);
                    }
                    Err(code) => match self.stored.ready() {
                        Some(view) => {
                            self.connector = view.connector.clone();
                            self.project = view.project.clone();
                            ctx.say(Message::error(NOT_READ, &problem_words(code)));
                        }
                        None => self.stored = Load::Failed(code.clone()),
                    },
                }
            }
            ConnectorsEvent::Saved(result) => {
                if !self.saving {
                    return;
                }
                self.saving = false;
                match result {
                    Ok(ConnectorsSaved::Saved(_)) => {
                        if let Load::Ready(view) = &mut self.stored {
                            view.settings.keep_log = self.pick;
                        }
                        ctx.say(Message::success("Saved.", SAVED));
                    }
                    Ok(ConnectorsSaved::Stale(_)) => {
                        if let Some(view) = self.stored.ready() {
                            self.pick = view.settings.keep_log;
                        }
                        ctx.say(Message::warning("Not saved.", STALE));
                    }
                    Err(code) => {
                        ctx.say(Message::error(NOT_SAVED, &problem_words(code)));
                        return;
                    }
                }
                self.read(ctx);
            }
            ConnectorsEvent::Page(_)
            | ConnectorsEvent::Preview { .. }
            | ConnectorsEvent::Approval(_)
            | ConnectorsEvent::Writes(_) => {}
        }
    }

    fn label(text: &str) -> Span<'static> {
        Span::raw(format!("{text:<width$}  ", width = KEEP_LABEL.len()))
    }

    fn control_line(
        &self,
        row: Row,
        view: &ActivityView,
        width: usize,
        ctx: &Ctx,
    ) -> Line<'static> {
        let mut spans = Vec::new();
        match row {
            Row::Keep => {
                spans.push(Self::label(KEEP_LABEL));
                for (keep, word) in KEEP_CHOICES {
                    if keep == self.pick {
                        spans.push(Span::styled(format!("[{word}] "), ctx.theme.heading));
                    } else {
                        spans.push(Span::styled(format!(" {word}  "), ctx.theme.muted));
                    }
                }
            }
            Row::Connector => {
                spans.push(Self::label(CONNECTOR_LABEL));
                spans.push(Span::raw(
                    self.connector.clone().unwrap_or_else(|| ALL.into()),
                ));
            }
            Row::Project => {
                spans.push(Self::label(PROJECT_LABEL));
                let shown = match &self.project {
                    None => ALL.to_owned(),
                    Some(id) => view
                        .projects
                        .iter()
                        .find(|choice| choice.id == *id)
                        .map_or_else(|| id.clone(), |choice| choice.name.clone()),
                };
                spans.push(Span::raw(shown));
            }
            Row::Call(_) => return Line::default(),
        }
        list_row(
            Line::from(spans),
            width,
            self.rows()[self.selected] == row,
            true,
            &ctx.theme,
            ctx.glyphs,
        )
    }

    fn call_lines(
        &self,
        at: usize,
        call: &CallView,
        zone: Option<&[u8]>,
        width: usize,
        ctx: &Ctx,
    ) -> [Line<'static>; 2] {
        let mut head = vec![
            stamp(call.started_ms, zone_offset(zone, call.started_ms)),
            call.connector.clone(),
        ];
        if !call.tool.is_empty() {
            head.push(call.tool.clone());
        }
        head.push(kind_word(call.kind).to_owned());
        head.push(call.outcome_text.clone());
        let head = cut(
            &head.join("  "),
            width.saturating_sub(2),
            ctx.glyphs.ellipsis,
        );
        let detail = format!(
            "{PROJECT_LABEL}: {}  Agent: {}  {}  {} ms",
            call.project_name,
            call.agent,
            size_words(call.bytes_in, call.bytes_out),
            call.ms
        );
        let detail = cut(&detail, width.saturating_sub(4), ctx.glyphs.ellipsis);
        [
            list_row(
                Line::raw(head),
                width,
                self.rows()[self.selected] == Row::Call(at),
                true,
                &ctx.theme,
                ctx.glyphs,
            ),
            Line::styled(format!("    {detail}"), ctx.theme.muted),
        ]
    }

    /// The page's lines and, for each row, the first and last line that must show with it:
    /// the keep row brings the title, each call its two lines. Inputs: the view, the width
    /// and the context. Output: the lines and one span per row.
    fn body(
        &self,
        view: &ActivityView,
        width: usize,
        ctx: &Ctx,
    ) -> (Vec<Line<'static>>, Vec<(usize, usize)>) {
        let theme = &ctx.theme;
        let room = width.max(8);
        let mut lines = vec![Line::styled(TITLE, theme.heading)];
        lines.extend(
            wrap(PURPOSE, room)
                .into_iter()
                .map(|part| Line::styled(part, theme.muted)),
        );
        lines.push(Line::default());
        let mut spans = Vec::new();
        for row in [Row::Keep, Row::Connector, Row::Project] {
            spans.push((if row == Row::Keep { 0 } else { lines.len() }, lines.len()));
            lines.push(self.control_line(row, view, width, ctx));
        }
        lines.push(Line::default());
        lines.push(Line::styled(
            format!("{CALLS}  {}", view.calls.len()),
            theme.heading,
        ));
        if view.calls.is_empty() {
            let filtered = view.connector.is_some() || view.project.is_some();
            lines.push(Line::styled(
                if filtered { NONE_MATCH } else { EMPTY },
                theme.muted,
            ));
        }
        if view.more {
            lines.extend(
                wrap(MORE, room)
                    .into_iter()
                    .map(|part| Line::styled(part, theme.muted)),
            );
        }
        let zone = local_zone();
        for (at, call) in view.calls.iter().enumerate() {
            spans.push((lines.len(), lines.len() + 1));
            lines.extend(self.call_lines(at, call, zone.as_deref(), width, ctx));
        }
        assert_eq!(spans.len(), CONTROLS + view.calls.len());
        (lines, spans)
    }
}

impl Default for ConnectorActivityPage {
    fn default() -> Self {
        Self::new()
    }
}

impl Screen for ConnectorActivityPage {
    fn crumbs(&self) -> Vec<String> {
        vec!["Settings".into(), TITLE.into()]
    }

    fn status(&self, ctx: &Ctx) -> Line<'static> {
        if self.saving {
            return Line::styled(format!("Saving{}", ctx.glyphs.ellipsis), ctx.theme.muted);
        }
        Line::default()
    }

    fn wraps_hints(&self) -> bool {
        true
    }

    fn hints(&self, ctx: &Ctx) -> Vec<Hint> {
        match &self.stored {
            Load::Failed(_) => return vec![hint("r", "Try again"), hint("Esc", "Back")],
            Load::Ready(_) => {}
            Load::Idle | Load::Loading | Load::NotReady => return vec![hint("Esc", "Back")],
        }
        if self.saving {
            return Vec::new();
        }
        let mut hints = vec![hint(
            &format!("{}{}", ctx.glyphs.up, ctx.glyphs.down),
            "Move",
        )];
        if self.selected < CONTROLS {
            hints.push(hint(
                &format!("{}{}", ctx.glyphs.left, ctx.glyphs.right),
                "Change",
            ));
        }
        if self.edited() {
            hints.push(hint("Enter", "Save changes"));
            hints.push(hint("Esc", "Discard changes"));
        } else {
            hints.push(hint("r", "Read again"));
            hints.push(hint("Esc", "Back"));
        }
        hints
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        if matches!(self.stored, Load::Failed(_)) && *key == Key::Char('r') {
            self.read(ctx);
            return Flow::Handled;
        }
        if self.stored.ready().is_none() {
            return Flow::Ignored;
        }
        if self.saving {
            return Flow::Handled;
        }
        let last = self.rows().len() - 1;
        match key {
            Key::Up => self.moved(self.selected.saturating_sub(1)),
            Key::Down => self.moved(self.selected + 1),
            Key::Home => self.moved(0),
            Key::End => self.moved(last),
            Key::Right => self.change(true, ctx),
            Key::Left => self.change(false, ctx),
            Key::Enter => self.save(ctx),
            Key::Esc if self.edited() => {
                if let Some(view) = self.stored.ready() {
                    self.pick = view.settings.keep_log;
                }
            }
            Key::Char('r') if !self.edited() => self.read(ctx),
            _ => return Flow::Ignored,
        }
        Flow::Handled
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        if let Some(SettingsEvent::Connectors(event)) = settings_event(arrival) {
            self.take(event, ctx);
        }
        Flow::Handled
    }

    fn shown(&mut self, ctx: &mut Ctx) {
        if matches!(self.stored, Load::Idle) {
            self.read(ctx);
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let inner = Rect {
            x: area.x.saturating_add(GUTTER),
            width: area.width.saturating_sub(2 * GUTTER),
            ..area
        };
        let width = usize::from(inner.width);
        let room = usize::from(inner.height);
        let lines = match &self.stored {
            Load::Ready(view) => {
                let (lines, spans) = self.body(view, width, ctx);
                let (start, end) = spans[self.selected];
                if start < self.top {
                    self.top = start;
                }
                if room > 0 && end >= self.top + room {
                    self.top = end + 1 - room;
                }
                self.top = self.top.min(lines.len().saturating_sub(1));
                lines.into_iter().skip(self.top).take(room).collect()
            }
            Load::Failed(code) => vec![
                Line::styled(NOT_READ, ctx.theme.warning),
                Line::styled(problem_words(code), ctx.theme.muted),
            ],
            Load::Idle | Load::Loading | Load::NotReady => vec![Line::styled(
                format!("Loading the connector activity{}", ctx.glyphs.ellipsis),
                ctx.theme.muted,
            )],
        };
        draw_lines(frame.buffer_mut(), inner, &lines);
        None
    }

    fn place(&self) -> Option<Place> {
        Some(Place::ConnectorActivity)
    }
}

#[cfg(all(test, unix))]
mod tests;
