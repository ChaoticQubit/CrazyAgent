use crate::connectors::approvals::AllowedWrites;
use crate::connectors::{ConnectorsView, Grant, Who, WritePolicy};
use crate::protocol::ProjectId;
use crate::tui::client::connectors::{ConnectorsEvent, ConnectorsJob};
use crate::tui::client::settings::{SettingsEvent, SettingsJob};
use crate::tui::ctx::{Ctx, Load};
use crate::tui::frame::{Hint, Message, draw_lines, hint};
use crate::tui::keys::Key;
use crate::tui::screens::settings::app_plugins::safe;
use crate::tui::screens::settings::connector_activity::turn;
use crate::tui::screens::settings::connectors::words::{READY, refusal, state};
use crate::tui::screens::settings_accounts::{send, settings_event};
use crate::tui::screens::{Flow, Place, Screen};
use crate::tui::seam::Arrival;
use crate::tui::text::{list_row, row, styled, wrap};
use crate::tui::theme::Role;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::Line;
use std::collections::HashSet;

#[cfg(all(test, unix))]
mod tests;

const GUTTER: u16 = 2;
/// The section's title (api-connectors.ts PROJECT_CONNECTORS.title).
pub(in crate::tui) const TITLE: &str = "Connectors";
/// The section's purpose (PROJECT_CONNECTORS.purpose).
pub(in crate::tui) const PURPOSE: &str = "Connectors from Settings, Connectors that this project's master and agents can use. Each is off until you turn it on here.";
/// The who row's label (PROJECT_CONNECTORS.who).
pub(in crate::tui) const WHO: &str = "Who can use it";
/// The line of a page with no connectors at all (PROJECT_CONNECTORS.none).
pub(in crate::tui) const NONE: &str = "No connectors yet. Add one in Settings, Connectors.";
/// The state word of an On switch whose connector is gone (PROJECT_CONNECTORS.notFound).
pub(in crate::tui) const NOT_FOUND: &str = "Not found";
/// The lead of a refused change (PROJECT_CONNECTORS.notChanged).
pub(in crate::tui) const NOT_CHANGED: &str = "The connector was not changed.";
/// Who a connector reaches, in the desktop's order with its words (WHO_CHOICES).
pub(in crate::tui) const WHO_CHOICES: [(Who, &str); 3] = [
    (Who::Both, "Master and agents with tools on"),
    (Who::Master, "Master only"),
    (Who::Agents, "Agents only"),
];
/// The Allowed writes block's title (PROJECT_CONNECTORS.writes).
pub(in crate::tui) const WRITES: &str = "Allowed writes";
/// The line under it (PROJECT_CONNECTORS.writesAbout).
pub(in crate::tui) const WRITES_ABOUT: &str =
    "Connectors that may change things in this project without asking again.";
/// The line of a project with no standing allow (PROJECT_CONNECTORS.writesNone).
pub(in crate::tui) const WRITES_NONE: &str = "No connector may change things here without asking.";
/// The lead of a failed read of them (PROJECT_CONNECTORS.writesFailed).
pub(in crate::tui) const WRITES_FAILED: &str = "Couldn't load the allowed writes.";
/// The action that takes one back (PROJECT_CONNECTORS.revoke).
pub(in crate::tui) const REVOKE: &str = "Revoke";
const NOT_READ: &str = "Couldn't load the connectors.";

/// The sentence after a revoke (writesRevoked). Input: the connector's shown title.
/// Output: the sentence.
pub(in crate::tui) fn writes_revoked(title: &str) -> String {
    format!("{title} asks again before its next write.")
}

/// The title an allowed write shows: the first listed connector's with that name, else
/// the name (writeRows). Inputs: the page and the name. Output: the shown title.
fn write_title(view: &ConnectorsView, name: &str) -> String {
    view.connectors
        .iter()
        .find(|connector| connector.name == name)
        .map_or_else(|| safe(name), |connector| safe(&connector.title))
}

/// The line naming every connector not listed, once (notReadyLine). Input: their titles.
/// Output: the line.
pub(in crate::tui) fn not_ready_line(titles: &[String]) -> String {
    assert!(!titles.is_empty(), "the line names at least one connector");
    format!(
        "Not ready yet: {}. Set them up in Settings, Connectors.",
        titles.join(", ")
    )
}

/// One listed switch: the connector's name and title, whether it is ready, its state word
/// and its switch row, if any.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Switch {
    name: String,
    title: String,
    ready: bool,
    word: &'static str,
    grant: Option<Grant>,
}

impl Switch {
    fn on(&self) -> bool {
        self.grant.as_ref().is_some_and(|grant| grant.enabled)
    }

    fn who(&self) -> Who {
        self.grant.as_ref().map_or(Who::Both, |grant| grant.who)
    }
}

/// The switches the page lists and the titles of the rest, the desktop's projectRows:
/// every Ready connector and every one whose switch is On, in the page's order, then an On
/// switch whose connector is gone. One row per name, the first listed winning, so a
/// built-in connector hides the owner's refused file it shadows. Input: the page. Output:
/// the switches and the others.
fn switches(view: &ConnectorsView) -> (Vec<Switch>, Vec<String>) {
    let grant_of = |name: &str| view.grants.iter().find(|grant| grant.connector == name);
    let mut seen = HashSet::new();
    let mut listed = Vec::new();
    let mut others = Vec::new();
    for connector in &view.connectors {
        if !seen.insert(connector.name.as_str()) {
            continue;
        }
        let grant = grant_of(&connector.name).cloned();
        let word = state(connector).0;
        let ready = word == READY;
        if ready || grant.as_ref().is_some_and(|grant| grant.enabled) {
            listed.push(Switch {
                name: connector.name.clone(),
                title: safe(&connector.title),
                ready,
                word,
                grant,
            });
        } else {
            others.push(safe(&connector.title));
        }
    }
    assert_eq!(listed.len() + others.len(), seen.len(), "one row or name per name");
    for grant in &view.grants {
        let known = view.connectors.iter().any(|c| c.name == grant.connector);
        if grant.enabled && !known {
            listed.push(Switch {
                name: grant.connector.clone(),
                title: safe(&grant.connector),
                ready: false,
                word: NOT_FOUND,
                grant: Some(grant.clone()),
            });
        }
    }
    assert!(listed.len() + others.len() <= seen.len() + view.grants.len());
    (listed, others)
}

/// The one request in flight: a read of the page, a change to one switch, a read of the
/// allowed writes or a revoke of one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pending {
    Read,
    Change,
    Writes,
    Revoke,
}

/// Which line the cursor rests on: a switch, the who row under an On one, or an allowed
/// write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Row {
    Switch(usize),
    Who(usize),
    Write(usize),
}

/// A project's Connectors page (DEC-168 addenda 18, 20 and 27): every ready connector and
/// every one turned on for this project, with its On switch and, while On and Ready, who
/// it reaches; the rest are named once. Space or Enter turns a switch on or off, Left and
/// Right change who; each change goes to the engine at once. Under Ask once per project
/// and connector the Allowed writes follow, read after the page, and x takes one back
/// (DEC-168 addendum 32). Opened with b on the project's Settings tab.
pub(in crate::tui) struct ProjectConnectorsPage {
    project: ProjectId,
    name: String,
    stored: Load<ConnectorsView>,
    writes: Load<AllowedWrites>,
    revoking: Option<String>,
    selected: usize,
    top: usize,
    pending: Option<Pending>,
}

impl ProjectConnectorsPage {
    /// Opens the page for one project with nothing read yet. Inputs: the project and its
    /// name. Output: the page; the read goes out when it is shown.
    pub fn new(project: ProjectId, name: String) -> Self {
        Self {
            project,
            name,
            stored: Load::Idle,
            writes: Load::Idle,
            revoking: None,
            selected: 0,
            top: 0,
            pending: None,
        }
    }

    fn ask(&mut self, job: ConnectorsJob, ctx: &mut Ctx) {
        assert!(self.pending.is_none(), "one request at a time");
        let pending = match &job {
            ConnectorsJob::View { project: Some(id) } if id == self.project.as_str() => {
                Pending::Read
            }
            ConnectorsJob::Grant { project, .. } if project == self.project.as_str() => {
                Pending::Change
            }
            ConnectorsJob::Writes { project } if project == self.project.as_str() => {
                Pending::Writes
            }
            ConnectorsJob::Revoke { project, .. } if project == self.project.as_str() => {
                Pending::Revoke
            }
            _ => panic!("the page asks only about its own project"),
        };
        self.pending = Some(pending);
        send(ctx, SettingsJob::Connectors(Box::new(job)));
    }

    fn read(&mut self, ctx: &mut Ctx) {
        if self.pending.is_some() {
            return;
        }
        if self.stored.ready().is_none() {
            self.stored = Load::Loading;
        }
        let project = Some(self.project.as_str().to_owned());
        self.ask(ConnectorsJob::View { project }, ctx);
    }

    fn read_writes(&mut self, ctx: &mut Ctx) {
        if !self.asks_once() || self.pending.is_some() {
            return;
        }
        if self.writes.ready().is_none() {
            self.writes = Load::Loading;
        }
        let project = self.project.as_str().to_owned();
        self.ask(ConnectorsJob::Writes { project }, ctx);
    }

    fn asks_once(&self) -> bool {
        self.stored
            .ready()
            .is_some_and(|view| view.settings.write == WritePolicy::AskOncePerProject)
    }

    fn allowed(&self) -> &[String] {
        match &self.writes {
            Load::Ready(writes) if self.asks_once() => &writes.connectors,
            _ => &[],
        }
    }

    fn all_rows(&self, switches: &[Switch]) -> Vec<Row> {
        let mut rows = Self::rows(switches);
        rows.extend((0..self.allowed().len()).map(Row::Write));
        rows
    }

    fn listed(&self) -> (Vec<Switch>, Vec<String>) {
        self.stored
            .ready()
            .map_or_else(|| (Vec::new(), Vec::new()), switches)
    }

    fn rows(switches: &[Switch]) -> Vec<Row> {
        let mut rows = Vec::new();
        for (at, switch) in switches.iter().enumerate() {
            rows.push(Row::Switch(at));
            if switch.on() && switch.ready {
                rows.push(Row::Who(at));
            }
        }
        assert!(rows.len() >= switches.len());
        rows
    }

    fn current(&self) -> Option<(Row, Switch)> {
        let (listed, _) = self.listed();
        let row = *self.all_rows(&listed).get(self.selected)?;
        let (Row::Switch(at) | Row::Who(at)) = row else {
            return None;
        };
        Some((row, listed[at].clone()))
    }

    fn current_write(&self) -> Option<String> {
        let (listed, _) = self.listed();
        let Row::Write(at) = *self.all_rows(&listed).get(self.selected)? else {
            return None;
        };
        Some(self.allowed()[at].clone())
    }

    fn moved(&mut self, to: usize) {
        let (listed, _) = self.listed();
        let count = self.all_rows(&listed).len();
        self.selected = to.min(count.saturating_sub(1));
        assert!(count == 0 || self.selected < count);
    }

    fn grant(&mut self, switch: &Switch, enabled: bool, who: Who, ctx: &mut Ctx) {
        let job = ConnectorsJob::Grant {
            project: self.project.as_str().to_owned(),
            connector: switch.name.clone(),
            enabled,
            who,
        };
        self.ask(job, ctx);
    }

    fn toggle(&mut self, ctx: &mut Ctx) {
        let Some((Row::Switch(_), switch)) = self.current() else {
            return;
        };
        let enabled = !switch.on();
        self.grant(&switch, enabled, switch.who(), ctx);
    }

    fn change(&mut self, forward: bool, ctx: &mut Ctx) {
        let Some((Row::Who(_), switch)) = self.current() else {
            return;
        };
        assert!(switch.on(), "only an On switch shows who");
        let options = WHO_CHOICES.map(|(who, _)| who);
        let who = turn(&options, &switch.who(), forward);
        self.grant(&switch, true, who, ctx);
    }

    fn revoke(&mut self, ctx: &mut Ctx) {
        let Some(name) = self.current_write() else {
            return;
        };
        let title = self
            .stored
            .ready()
            .map_or_else(|| safe(&name), |view| write_title(view, &name));
        self.revoking = Some(title);
        let project = self.project.as_str().to_owned();
        self.ask(
            ConnectorsJob::Revoke {
                project,
                connector: name,
            },
            ctx,
        );
    }

    fn take(&mut self, event: &ConnectorsEvent, ctx: &mut Ctx) {
        match event {
            ConnectorsEvent::Page(result) => self.page(result, ctx),
            ConnectorsEvent::Writes(result) => self.allowed_writes(result, ctx),
            _ => {}
        }
    }

    fn allowed_writes(&mut self, result: &Result<AllowedWrites, String>, ctx: &mut Ctx) {
        let Some(pending @ (Pending::Writes | Pending::Revoke)) = self.pending else {
            return;
        };
        if let Ok(writes) = result
            && writes.project != self.project.as_str()
        {
            return;
        }
        self.pending = None;
        let revoked = self.revoking.take();
        match (result, pending) {
            (Ok(writes), _) => {
                self.writes = Load::Ready(writes.clone());
                self.moved(self.selected);
                if let Some(title) = revoked {
                    ctx.say(Message::success(&writes_revoked(&title), ""));
                }
            }
            (Err(code), Pending::Writes) => self.writes = Load::Failed(code.clone()),
            (Err(code), _) => ctx.say(Message::error(NOT_CHANGED, &refusal(code))),
        }
        assert!(self.pending.is_none() && self.revoking.is_none());
    }

    fn page(&mut self, result: &Result<ConnectorsView, String>, ctx: &mut Ctx) {
        let Some(pending @ (Pending::Read | Pending::Change)) = self.pending else {
            return;
        };
        self.pending = None;
        match result {
            Ok(view) if view.project.as_deref() == Some(self.project.as_str()) => {
                self.stored = Load::Ready(view.clone());
                self.moved(self.selected);
                if pending == Pending::Read {
                    self.read_writes(ctx);
                }
            }
            Ok(_) => self.read(ctx),
            Err(code) => match (self.stored.ready(), pending) {
                (None, _) => self.stored = Load::Failed(code.clone()),
                (Some(_), Pending::Read) => ctx.say(Message::error(NOT_READ, &refusal(code))),
                (Some(_), _) => {
                    ctx.say(Message::error(NOT_CHANGED, &refusal(code)));
                    if code == "app_plugin_not_found" {
                        self.read(ctx);
                    }
                }
            },
        }
    }

    fn switch_line(&self, at: usize, switch: &Switch, width: usize, ctx: &Ctx) -> Line<'static> {
        let theme = &ctx.theme;
        let onoff = if switch.on() { "On" } else { "Off" };
        let right = if switch.ready {
            Line::raw(onoff)
        } else {
            styled(format!("{}  {onoff}", switch.word), Role::Warning, theme)
        };
        let content = row(
            width.saturating_sub(2),
            Line::raw(switch.title.clone()),
            right,
            ctx.glyphs,
        );
        let selected = self.selected == at;
        list_row(content, width, selected, true, theme, ctx.glyphs)
    }

    /// The page's lines and, for each row, the first and last line that must show with it.
    /// Inputs: the page read, the width and the context. Output: the lines and one span
    /// per row.
    fn body(
        &self,
        view: &ConnectorsView,
        width: usize,
        ctx: &Ctx,
    ) -> (Vec<Line<'static>>, Vec<(usize, usize)>) {
        let theme = &ctx.theme;
        let muted = |text: &str| {
            wrap(text, width)
                .into_iter()
                .map(|part| styled(part, Role::Muted, theme))
                .collect::<Vec<_>>()
        };
        let mut lines = vec![styled(TITLE, Role::Heading, theme)];
        lines.extend(muted(PURPOSE));
        lines.push(Line::default());
        let (listed, others) = switches(view);
        let mut spans = Vec::new();
        for row in Self::rows(&listed) {
            let at = spans.len();
            spans.push((if at == 0 { 0 } else { lines.len() }, lines.len()));
            lines.push(match row {
                Row::Switch(index) => self.switch_line(at, &listed[index], width, ctx),
                Row::Who(index) => {
                    let who = listed[index].who();
                    let word = WHO_CHOICES
                        .iter()
                        .find(|(choice, _)| *choice == who)
                        .map_or("", |(_, word)| *word);
                    let content = Line::raw(format!("  {WHO}  {word}"));
                    list_row(content, width, self.selected == at, true, theme, ctx.glyphs)
                }
                Row::Write(_) => unreachable!("the switch rows hold no allowed write"),
            });
        }
        if listed.is_empty() && others.is_empty() {
            lines.extend(muted(NONE));
        }
        if !others.is_empty() {
            lines.push(Line::default());
            lines.extend(muted(&not_ready_line(&others)));
        }
        if self.asks_once() {
            lines.push(Line::default());
            let heading = lines.len();
            lines.push(styled(WRITES, Role::Heading, theme));
            lines.extend(muted(WRITES_ABOUT));
            match &self.writes {
                Load::Ready(writes) if writes.connectors.is_empty() => {
                    lines.extend(muted(WRITES_NONE));
                }
                Load::Ready(writes) => {
                    for (index, name) in writes.connectors.iter().enumerate() {
                        let at = spans.len();
                        spans.push((if index == 0 { heading } else { lines.len() }, lines.len()));
                        let title = Line::raw(write_title(view, name));
                        let selected = self.selected == at;
                        lines.push(list_row(title, width, selected, true, theme, ctx.glyphs));
                    }
                }
                Load::Failed(code) => {
                    lines.push(styled(WRITES_FAILED, Role::Warning, theme));
                    lines.extend(muted(&refusal(code)));
                }
                Load::Idle | Load::Loading | Load::NotReady => lines.push(styled(
                    format!("Reading allowed writes{}", ctx.glyphs.ellipsis),
                    Role::Muted,
                    theme,
                )),
            }
        }
        assert_eq!(spans.len(), self.all_rows(&listed).len());
        (lines, spans)
    }
}

impl Screen for ProjectConnectorsPage {
    fn crumbs(&self) -> Vec<String> {
        vec![self.name.clone(), TITLE.into()]
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
        if self.pending.is_some() {
            return Vec::new();
        }
        let mut hints = Vec::new();
        let (listed, _) = self.listed();
        let rows = self.all_rows(&listed);
        if !rows.is_empty() {
            hints.push(hint(
                &format!("{}{}", ctx.glyphs.up, ctx.glyphs.down),
                "Move",
            ));
        }
        match rows.get(self.selected).copied() {
            Some(Row::Switch(at)) if listed[at].on() => hints.push(hint("Space", "Turn off")),
            Some(Row::Switch(_)) => hints.push(hint("Space", "Turn on")),
            Some(Row::Who(_)) => hints.push(hint(
                &format!("{}{}", ctx.glyphs.left, ctx.glyphs.right),
                "Change",
            )),
            Some(Row::Write(_)) => hints.push(hint("x", REVOKE)),
            None => {}
        }
        hints.push(hint("r", "Read again"));
        hints.push(hint("Esc", "Back"));
        hints
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        if self.stored.ready().is_none() {
            if matches!(self.stored, Load::Failed(_)) && *key == Key::Char('r') {
                self.read(ctx);
                return Flow::Handled;
            }
            return Flow::Ignored;
        }
        if self.pending.is_some() && !matches!(key, Key::Esc) {
            return Flow::Handled;
        }
        let (listed, _) = self.listed();
        let last = self.all_rows(&listed).len().saturating_sub(1);
        match key {
            Key::Up => self.moved(self.selected.saturating_sub(1)),
            Key::Down => self.moved(self.selected + 1),
            Key::Home => self.moved(0),
            Key::End => self.moved(last),
            Key::Char(' ') | Key::Enter => self.toggle(ctx),
            Key::Right => self.change(true, ctx),
            Key::Left => self.change(false, ctx),
            Key::Char('x') => self.revoke(ctx),
            Key::Char('r') => self.read(ctx),
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

    fn busy(&self, _ctx: &Ctx) -> bool {
        self.pending.is_some()
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let inner = Rect {
            x: area.x.saturating_add(GUTTER),
            width: area.width.saturating_sub(2 * GUTTER),
            ..area
        };
        let width = usize::from(inner.width).max(8);
        let room = usize::from(inner.height);
        let lines = match &self.stored {
            Load::Ready(view) => {
                let (lines, spans) = self.body(view, width, ctx);
                if let Some((first, last)) = spans.get(self.selected).copied() {
                    let draw = crate::tui::screens::settings::top_for(first, last, room);
                    self.top = self.top.min(first).max(draw);
                }
                lines.into_iter().skip(self.top).take(room).collect()
            }
            Load::Failed(code) => {
                let mut lines = vec![styled(NOT_READ, Role::Warning, &ctx.theme)];
                lines.extend(
                    wrap(&refusal(code), width)
                        .into_iter()
                        .map(|part| styled(part, Role::Muted, &ctx.theme)),
                );
                lines
            }
            Load::Idle | Load::Loading | Load::NotReady => vec![styled(
                format!("Reading connectors{}", ctx.glyphs.ellipsis),
                Role::Muted,
                &ctx.theme,
            )],
        };
        draw_lines(frame.buffer_mut(), inner, &lines);
        None
    }

    fn place(&self) -> Option<Place> {
        Some(Place::ProjectConnectors {
            id: self.project.clone(),
            name: self.name.clone(),
        })
    }
}
