use super::app_setup::{setup_plain, setup_reason};
use super::settings_accounts::{plain, send, settings_event};
use super::settings_harnesses::plain_update;
use super::{Flow, Place, Screen, open};
use crate::harnesses::HarnessEntry;
use crate::harnesses::update::HarnessVersions;
use crate::setup::{Dependency, DependencyState};
use crate::tui::client::app_setup::{AppSetupEvent, AppSetupJob};
use crate::tui::client::settings::{SettingsEvent, SettingsJob};
use crate::tui::ctx::Ctx;
use crate::tui::frame::{Hint, draw_lines, hint};
use crate::tui::keys::Key;
use crate::tui::seam::Arrival;
use crate::tui::text::{cut, join, styled, wrap};
use crate::tui::theme::Role;
use crate::tui::widgets::{buttons, checkbox};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};
use std::collections::{BTreeMap, BTreeSet};

const TEXT_WIDTH: usize = 72;
const SUB_INDENT: usize = 6;
const READS_MAX: usize = 600;
const LABEL: &str = "Harnesses";
const TITLE: &str = "Choose your harnesses";
const INTRO: &str = "A harness is the coding agent a master or agent runs in. RCLI comes with CrazyAgent; pick any others to install now, or add them later in Settings, Harnesses.";
const BUNDLED: &str = "Comes with CrazyAgent.";
const INSTALLED: &str = "Already on this Mac.";
const INSTALLS_WITH: &str = "Installs with";
const READING: &str = "Reading what's on this Mac.";
const WAITING: &str = "Waiting to install.";
const INSTALLING: &str = "Installing now.";
const DONE: &str = "Installed.";
pub(super) const TRY_AGAIN: &str = "Try again";
const GUIDE: &str = "How to install it yourself";
const INSTALL: &str = "Install picked harnesses";
const SKIP: &str = "Skip for now";
const NEXT: &str = "Continue";
pub(super) const INSTALL_NODE: &str = "Install Node.js";
pub(super) const INSTALLING_NODE: &str = "Installing Node.js.";
pub(super) const CANT_RUN: &str = "Can't run";
const NO_TOOL: &str = "harness_install_no_tool";
const NEEDS_NODE: &str = "harness_install_needs_node";
const FAILED: &str = "harness_install_failed";
const RCLI: &str = "rcli";
const CATALOG: &str = "catalog";
const QUEUE: &str = "queue";
const SETUP: &str = "setup";

/// The harnesses the step offers, in order: RCLI, which comes with CrazyAgent, then the ones
/// CrazyAgent can launch, by catalog id and name, the same list as the desktop step.
const OFFERED: [(&str, &str); 3] = [
    (RCLI, "RCLI"),
    ("codex", "Codex"),
    ("claude-code", "Claude Code"),
];

/// Where one offered harness stands on the step, the same kinds as the desktop step's rows.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Row {
    Reading,
    Bundled,
    Installed {
        version: Option<String>,
    },
    Offer {
        line: String,
    },
    Refused {
        reason: String,
        guide: Option<String>,
    },
    Waiting,
    Installing,
    Done {
        version: Option<String>,
    },
    Failed {
        reason: String,
    },
    Blocked {
        line: String,
    },
}

impl Row {
    /// Tells whether the owner can pick the row to install: only one with an install line.
    /// Input: the row. Output: the answer.
    fn tickable(&self) -> bool {
        matches!(self, Self::Offer { .. })
    }

    fn ended(&self) -> bool {
        matches!(
            self,
            Self::Done { .. } | Self::Failed { .. } | Self::Installed { .. } | Self::Blocked { .. }
        )
    }

    fn badge(&self) -> &'static str {
        match self {
            Self::Bundled => "Included",
            Self::Installed { .. } | Self::Done { .. } => "Installed",
            Self::Offer { .. } => "Not installed",
            Self::Refused { .. } => "Can't install",
            Self::Reading => "Reading",
            Self::Waiting => "Waiting",
            Self::Installing => "Installing",
            Self::Failed { .. } => "Failed",
            Self::Blocked { .. } => CANT_RUN,
        }
    }

    fn line(&self) -> String {
        let version = |value: &Option<String>| {
            value
                .as_ref()
                .map_or_else(String::new, |found| format!(" Version {found}."))
        };
        match self {
            Self::Bundled => BUNDLED.to_owned(),
            Self::Installed { version: found } => format!("{INSTALLED}{}", version(found)),
            Self::Offer { line } => format!("{INSTALLS_WITH} {line}"),
            Self::Refused { reason, .. } | Self::Failed { reason } => reason.clone(),
            Self::Blocked { line } => line.clone(),
            Self::Reading => READING.to_owned(),
            Self::Waiting => WAITING.to_owned(),
            Self::Installing => INSTALLING.to_owned(),
            Self::Done { version: found } => format!("{DONE}{}", version(found)),
        }
    }

    fn retry(&self) -> bool {
        matches!(
            self,
            Self::Failed { .. } | Self::Refused { .. } | Self::Blocked { .. }
        )
    }
}

/// Where the step's one Node.js install stands while an offered harness needs Node, the same
/// kinds as the desktop step's: nothing to offer, the offer, the install running, or its
/// failure with the reason setup gave.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum NodeOffer {
    None,
    Offer,
    Installing,
    Failed { reason: String },
}

/// What the step offers for Node.js, from whether an offered harness needs it and the Node
/// row of setup. A row not settled while a setup run goes on reads installing, since setup
/// finds Node before it marks the row. Inputs: the need, the Node row's state, none while
/// unread, and whether a setup run goes on. Output: the offer.
pub(super) fn node_offer(needed: bool, node: Option<&DependencyState>, busy: bool) -> NodeOffer {
    if !needed {
        return NodeOffer::None;
    }
    match node {
        Some(DependencyState::Installing) => NodeOffer::Installing,
        Some(DependencyState::Failed { reason }) => NodeOffer::Failed {
            reason: reason.clone(),
        },
        Some(
            DependencyState::Found { .. }
            | DependencyState::Bundled { .. }
            | DependencyState::System { .. }
            | DependencyState::Installed { .. }
            | DependencyState::Unsupported,
        ) => NodeOffer::None,
        _ if busy => NodeOffer::Installing,
        None | Some(DependencyState::Missing { .. } | DependencyState::Refused { .. }) => {
            NodeOffer::Offer
        }
        Some(DependencyState::Checking) => NodeOffer::None,
    }
}

/// Tells whether the step reads again for Node while a harness needs it: setup's Node row is
/// still being checked, or it reads found or installed while the harness read still needs
/// Node, as when it was read just before Node was linked. Inputs: the need and the Node row's
/// state, none while unread. Output: the answer.
fn node_waits(needed: bool, node: Option<&DependencyState>) -> bool {
    needed
        && matches!(
            node,
            Some(
                DependencyState::Checking
                    | DependencyState::Found { .. }
                    | DependencyState::Installed { .. }
            )
        )
}

/// Tells whether a screen with the Node.js offer, this step or Settings, Harnesses, reads
/// again for Node: while Node installs, or while `node_waits` says so. Callers bound it with
/// their own read cap. Inputs: the need, the Node row's state, none while unread, and whether
/// a setup run goes on. Output: the answer.
pub(super) fn node_reads_again(needed: bool, node: Option<&DependencyState>, busy: bool) -> bool {
    node_offer(needed, node, busy) == NodeOffer::Installing || node_waits(needed, node)
}

/// Tells whether an offered harness needs Node.js: npm can't install it without Node, or the
/// resolver found it but it can't run without Node. Input: its versions read.
/// Output: the answer.
pub(super) fn node_needed(versions: &HarnessVersions) -> bool {
    versions.refusal.as_deref() == Some(NEEDS_NODE)
        || versions
            .needs
            .as_ref()
            .is_some_and(|needs| needs.program.as_deref() == Some("node"))
}

/// Where an offered harness stands, in the desktop step's order: RCLI comes with the app; a
/// running or queued install wins; a found harness the resolver can't run says what it needs;
/// a failed install of a missing harness or a fresh install shows its result; else installed,
/// the install line, or why it can't install. Inputs: the id, its catalog row, its versions
/// (none while unread) and the queued ids.
fn harness_row(
    id: &str,
    entry: Option<&HarnessEntry>,
    versions: Option<&HarnessVersions>,
    queued: &[String],
) -> Row {
    if id == RCLI {
        return Row::Bundled;
    }
    let Some(versions) = versions else {
        return Row::Reading;
    };
    if versions.running {
        return Row::Installing;
    }
    if queued.iter().any(|waiting| waiting == id) {
        return Row::Waiting;
    }
    let present = entry.is_some_and(|entry| entry.executable.is_some());
    if let Some(needs) = versions.needs.as_ref().filter(|_| present) {
        return Row::Blocked {
            line: super::project_markdown::sanitize(&needs.line),
        };
    }
    match &versions.last {
        Some(last) if !last.ok && !present => {
            return Row::Failed {
                reason: plain_update(last.code.as_deref().unwrap_or(FAILED)).to_owned(),
            };
        }
        Some(last) if last.ok && last.from.is_none() && present => {
            return Row::Done {
                version: last.to.clone(),
            };
        }
        _ => {}
    }
    if present {
        return Row::Installed {
            version: versions.installed.clone(),
        };
    }
    match &versions.install {
        Some(line) => Row::Offer { line: line.clone() },
        None => Row::Refused {
            reason: plain_update(versions.refusal.as_deref().unwrap_or(NO_TOOL)).to_owned(),
            guide: versions.guide.clone(),
        },
    }
}

/// How far the sent picks are: how many ended, how many failed and whether any still waits
/// or installs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Progress {
    done: usize,
    total: usize,
    failed: usize,
    running: bool,
}

/// The progress sentence under the rows, the desktop's words. Input: the counts.
/// Output: the line.
fn progress_line(progress: Progress) -> String {
    let Progress {
        done,
        total,
        failed,
        running,
    } = progress;
    if running {
        return format!(
            "Installing: {done} of {total} done. You can continue; the installs keep going."
        );
    }
    let tail = if failed > 0 {
        format!(", {failed} failed. Each failure shows its reason above")
    } else {
        String::new()
    };
    format!("Installs finished: {done} of {total} done{tail}.")
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Choice {
    #[default]
    Install,
    Skip,
}

/// First run's Harnesses step after Setup, in the desktop words: RCLI picked and locked,
/// Codex and Claude Code to pick when the engine can install them, each row saying what
/// installing runs or why it can't, r to try a row again, n to install Node.js while a harness
/// needs it, and Install, Skip or Continue. Picks go to the engine's install queue, which keeps
/// going after the step. Input: keys and the engine's reads. Output: the step.
pub struct HarnessSetup {
    entries: Vec<HarnessEntry>,
    versions: BTreeMap<&'static str, Result<HarnessVersions, String>>,
    queued: Vec<String>,
    picks: Vec<&'static str>,
    focus: usize,
    choice: Choice,
    asking: BTreeSet<&'static str>,
    sending: Option<Vec<String>>,
    node: Option<DependencyState>,
    setup_busy: bool,
    sending_node: bool,
    reread: bool,
    started: bool,
    loaded: bool,
    reads: usize,
    problem: String,
}

impl HarnessSetup {
    /// A new Harnesses step with RCLI focused and nothing picked; it reads the engine when
    /// shown. Input: none. Output: the screen.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            versions: BTreeMap::new(),
            queued: Vec::new(),
            picks: Vec::new(),
            focus: 0,
            choice: Choice::Install,
            asking: BTreeSet::new(),
            sending: None,
            node: None,
            setup_busy: false,
            sending_node: false,
            reread: false,
            started: false,
            loaded: false,
            reads: 0,
            problem: String::new(),
        }
    }

    fn row(&self, id: &str) -> Row {
        let entry = self.entries.iter().find(|entry| entry.id == id);
        match self.versions.get(id) {
            Some(Err(code)) => Row::Refused {
                reason: plain_update(code).to_owned(),
                guide: None,
            },
            Some(Ok(versions)) => harness_row(id, entry, Some(versions), &self.queued),
            None => harness_row(id, entry, None, &self.queued),
        }
    }

    fn progress(&self) -> Progress {
        let rows: Vec<Row> = self.picks.iter().map(|id| self.row(id)).collect();
        Progress {
            done: rows.iter().filter(|row| row.ended()).count(),
            total: rows.len(),
            failed: rows
                .iter()
                .filter(|row| matches!(row, Row::Failed { .. } | Row::Blocked { .. }))
                .count(),
            running: rows.iter().any(|row| !row.ended()),
        }
    }

    fn node_needed(&self) -> bool {
        self.versions
            .values()
            .any(|read| read.as_ref().is_ok_and(node_needed))
    }

    fn node_offer(&self) -> NodeOffer {
        node_offer(self.node_needed(), self.node.as_ref(), self.setup_busy)
    }

    fn pending(&self) -> bool {
        let moving = OFFERED
            .iter()
            .any(|(id, _)| matches!(self.row(id), Row::Waiting | Row::Installing));
        moving
            || (self.started && self.progress().running)
            || node_reads_again(self.node_needed(), self.node.as_ref(), self.setup_busy)
    }

    fn offer(&self) -> bool {
        !self.picks.is_empty() && !self.started
    }

    fn read(&mut self, ctx: &mut Ctx) {
        if !self.asking.is_empty() || self.reads >= READS_MAX {
            return;
        }
        self.loaded = true;
        self.reread = false;
        self.reads += 1;
        self.asking.insert(CATALOG);
        send(ctx, SettingsJob::HarnessList);
        self.asking.insert(QUEUE);
        send(ctx, SettingsJob::HarnessQueued);
        self.asking.insert(SETUP);
        send(ctx, SettingsJob::AppSetup(AppSetupJob::Status));
        for (id, _) in OFFERED.iter().filter(|(id, _)| *id != RCLI) {
            self.asking.insert(*id);
            send(
                ctx,
                SettingsJob::HarnessVersions {
                    id: (*id).to_owned(),
                    look: false,
                },
            );
        }
        assert!(self.asking.len() == OFFERED.len() + 2);
    }

    fn install(&mut self, ids: Vec<String>, ctx: &mut Ctx) {
        if ids.is_empty() || self.sending.is_some() {
            return;
        }
        assert!(ids.len() <= OFFERED.len() && ids.iter().all(|id| id != RCLI));
        self.problem.clear();
        self.sending = Some(ids.clone());
        send(ctx, SettingsJob::HarnessInstallMany { ids });
    }

    fn install_node(&mut self, ctx: &mut Ctx) {
        let wanted = matches!(
            self.node_offer(),
            NodeOffer::Offer | NodeOffer::Failed { .. }
        );
        if !wanted || self.sending_node || self.sending.is_some() {
            return;
        }
        self.problem.clear();
        self.sending_node = true;
        let generation = ctx.next_generation();
        send(
            ctx,
            SettingsJob::AppSetup(AppSetupJob::Install {
                name: Dependency::Node,
                generation,
            }),
        );
    }

    fn leave(ctx: &mut Ctx) -> Flow {
        Flow::Replace(open(&Place::FirstRunAccounts, ctx))
    }

    fn enter(&mut self, ctx: &mut Ctx) -> Flow {
        if !self.offer() || self.choice == Choice::Skip {
            return Self::leave(ctx);
        }
        let ids = self.picks.iter().map(|id| (*id).to_owned()).collect();
        self.install(ids, ctx);
        Flow::Handled
    }

    fn pick(&mut self) {
        let (id, _) = OFFERED[self.focus];
        if self.started || !self.row(id).tickable() {
            return;
        }
        if let Some(at) = self.picks.iter().position(|picked| *picked == id) {
            self.picks.remove(at);
        } else {
            self.picks.push(id);
        }
        assert!(self.picks.len() < OFFERED.len());
    }

    fn try_again(&mut self, ctx: &mut Ctx) {
        let (id, _) = OFFERED[self.focus];
        match self.row(id) {
            Row::Failed { .. } => {
                if !self.picks.contains(&id) {
                    self.picks.push(id);
                }
                self.install(vec![id.to_owned()], ctx);
            }
            Row::Refused { .. } | Row::Blocked { .. } => {
                self.problem.clear();
                self.read(ctx);
            }
            _ => {}
        }
    }

    fn arrived(&mut self, event: &SettingsEvent, ctx: &mut Ctx) {
        match event {
            SettingsEvent::Harnesses(result) => {
                let asked = self.asking.remove(CATALOG);
                match result {
                    Ok(entries) => self.entries.clone_from(entries),
                    Err(code) if asked => self.problem = plain(code).to_owned(),
                    Err(_) => {}
                }
            }
            SettingsEvent::HarnessQueue(result) => {
                if !self.asking.remove(QUEUE) {
                    return;
                }
                match result {
                    Ok(ids) => self.queued.clone_from(ids),
                    Err(code) => self.problem = plain(code).to_owned(),
                }
            }
            SettingsEvent::HarnessVersions(id, result) => {
                let Some(&(offered, _)) = OFFERED
                    .iter()
                    .find(|(known, _)| *known == id.as_str() && *known != RCLI)
                else {
                    return;
                };
                if self.asking.remove(offered) {
                    self.versions.insert(offered, result.clone());
                }
            }
            SettingsEvent::AppSetup(AppSetupEvent::Status(result)) => {
                if !self.asking.remove(SETUP) {
                    return;
                }
                self.setup_busy = result.as_ref().is_ok_and(|status| status.running);
                self.node = result.as_ref().ok().and_then(|status| {
                    status
                        .dependencies
                        .iter()
                        .find(|tool| tool.name == Dependency::Node)
                        .map(|tool| tool.state.clone())
                });
            }
            SettingsEvent::AppSetup(AppSetupEvent::Installed(result)) => {
                if !self.sending_node {
                    return;
                }
                self.sending_node = false;
                match result {
                    Ok(()) => {
                        self.node = Some(DependencyState::Installing);
                        self.reads = 0;
                        self.reread = true;
                        self.read(ctx);
                    }
                    Err(code) => self.problem = setup_plain(code),
                }
            }
            SettingsEvent::HarnessesQueued(ids, result) => {
                if self.sending.as_ref() != Some(ids) {
                    return;
                }
                self.sending = None;
                match result {
                    Ok(()) => {
                        self.started = true;
                        self.reads = 0;
                        self.read(ctx);
                    }
                    Err(code) => self.problem = plain_update(code).to_owned(),
                }
            }
            _ => {}
        }
    }

    fn rows(&self, limit: usize, ctx: &Ctx) -> Vec<Line<'static>> {
        let theme = &ctx.theme;
        let room = limit.saturating_sub(SUB_INDENT).max(8);
        let pad = " ".repeat(SUB_INDENT);
        let mut lines = Vec::new();
        for (index, (id, name)) in OFFERED.iter().enumerate() {
            let row = self.row(id);
            let on = row == Row::Bundled || self.picks.contains(id);
            let mark = if index == self.focus { "> " } else { "  " };
            let head = format!("{mark}{}", checkbox(on, name));
            let head = if index == self.focus {
                Span::styled(head, theme.selection)
            } else {
                Span::raw(head)
            };
            lines.push(Line::from(vec![
                head,
                Span::raw("  "),
                Span::styled(row.badge(), theme.muted),
            ]));
            let role = if matches!(
                row,
                Row::Failed { .. } | Row::Refused { .. } | Row::Blocked { .. }
            ) {
                Role::Error
            } else {
                Role::Muted
            };
            for part in wrap(&row.line(), room) {
                lines.push(join([Line::raw(pad.clone()), styled(part, role, theme)]));
            }
            if let Row::Refused {
                guide: Some(guide), ..
            } = &row
            {
                let link = cut(
                    &super::project_markdown::sanitize(guide),
                    room.saturating_sub(GUIDE.len() + 2).max(8),
                    ctx.glyphs.ellipsis,
                );
                lines.push(Line::raw(format!("{pad}{GUIDE}: {link}")));
            }
        }
        lines
    }

    fn footer(&self, limit: usize, ctx: &Ctx) -> Vec<Line<'static>> {
        let theme = &ctx.theme;
        let mut lines = Vec::new();
        let progress = self.progress();
        if self.started && progress.total > 0 {
            lines.extend(
                wrap(&progress_line(progress), limit)
                    .into_iter()
                    .map(Line::raw),
            );
        }
        match self.node_offer() {
            NodeOffer::Installing => lines.push(Line::raw(INSTALLING_NODE)),
            NodeOffer::Failed { reason } => lines.extend(
                wrap(&setup_reason(&reason), limit)
                    .into_iter()
                    .map(|part| styled(part, Role::Error, theme)),
            ),
            NodeOffer::None | NodeOffer::Offer => {}
        }
        if !self.problem.is_empty() {
            lines.extend(
                wrap(&self.problem, limit)
                    .into_iter()
                    .map(|part| styled(part, Role::Error, theme)),
            );
        }
        if self.sending.is_some() {
            lines.push(ctx.loading("Starting the installs"));
        } else if self.sending_node {
            lines.push(ctx.loading(INSTALL_NODE));
        } else if self.offer() {
            let focused = match self.choice {
                Choice::Install => 0,
                Choice::Skip => 1,
            };
            lines.push(buttons(&[INSTALL, SKIP], Some(focused), theme));
        } else {
            lines.push(buttons(&[NEXT], Some(0), theme));
        }
        lines
    }
}

impl Default for HarnessSetup {
    fn default() -> Self {
        Self::new()
    }
}

impl Screen for HarnessSetup {
    fn crumbs(&self) -> Vec<String> {
        vec![LABEL.into()]
    }

    fn hints(&self, ctx: &Ctx) -> Vec<Hint> {
        if self.sending.is_some() || self.sending_node {
            return vec![hint("Ctrl-Q", "Quit")];
        }
        let (id, _) = OFFERED[self.focus];
        let row = self.row(id);
        let enter = if !self.offer() {
            NEXT
        } else if self.choice == Choice::Install {
            INSTALL
        } else {
            SKIP
        };
        let mut hints = vec![
            hint("Enter", enter),
            hint(&format!("{}{}", ctx.glyphs.up, ctx.glyphs.down), "Move"),
        ];
        if !self.started && row.tickable() {
            hints.push(hint(
                "Space",
                if self.picks.contains(&id) {
                    "Untick"
                } else {
                    "Tick"
                },
            ));
        }
        if row.retry() {
            hints.push(hint("r", TRY_AGAIN));
        }
        if matches!(
            self.node_offer(),
            NodeOffer::Offer | NodeOffer::Failed { .. }
        ) {
            hints.push(hint("n", INSTALL_NODE));
        }
        if self.offer() {
            hints.push(hint(
                &format!("{}{}", ctx.glyphs.left, ctx.glyphs.right),
                "Choose",
            ));
            hints.push(hint("c", SKIP));
        }
        hints.push(hint("Ctrl-Q", "Quit"));
        hints
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        if self.sending.is_some() || self.sending_node {
            return Flow::Handled;
        }
        match key {
            Key::Up => {
                self.focus = self.focus.saturating_sub(1);
                Flow::Handled
            }
            Key::Down => {
                self.focus = (self.focus + 1).min(OFFERED.len() - 1);
                Flow::Handled
            }
            Key::Left | Key::Right | Key::Tab | Key::BackTab => {
                self.choice = match self.choice {
                    Choice::Install => Choice::Skip,
                    Choice::Skip => Choice::Install,
                };
                Flow::Handled
            }
            Key::Char(' ') => {
                self.pick();
                Flow::Handled
            }
            Key::Char('r') => {
                self.try_again(ctx);
                Flow::Handled
            }
            Key::Char('n') => {
                self.install_node(ctx);
                Flow::Handled
            }
            Key::Char('c') => Self::leave(ctx),
            Key::Enter => self.enter(ctx),
            Key::Esc => Flow::Handled,
            _ => Flow::Ignored,
        }
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        if let Some(event) = settings_event(arrival) {
            self.arrived(event, ctx);
        }
        if self.reread && self.asking.is_empty() {
            self.read(ctx);
        }
        assert!(self.asking.len() <= OFFERED.len() + 2);
        Flow::Handled
    }

    fn shown(&mut self, ctx: &mut Ctx) {
        if !self.loaded || self.pending() {
            self.read(ctx);
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        assert!(self.focus < OFFERED.len());
        let theme = &ctx.theme;
        let limit = (area.width as usize).saturating_sub(4).clamp(6, TEXT_WIDTH);
        let mut lines = vec![styled(TITLE, Role::Heading, theme), Line::default()];
        lines.extend(wrap(INTRO, limit).into_iter().map(Line::raw));
        lines.push(Line::default());
        lines.extend(self.rows(limit, ctx));
        let footer = self.footer(limit, ctx);
        let room = (area.height as usize).saturating_sub(footer.len() + 1);
        lines.truncate(room);
        lines.push(Line::default());
        lines.extend(footer);
        let lines: Vec<Line<'static>> = lines
            .into_iter()
            .map(|line| join([Line::raw("  "), line]))
            .collect();
        draw_lines(frame.buffer_mut(), area, &lines);
        None
    }

    fn busy(&self, _ctx: &Ctx) -> bool {
        self.sending.is_some() || self.sending_node
    }

    fn place(&self) -> Option<Place> {
        Some(Place::HarnessSetup)
    }
}

#[cfg(all(test, unix))]
mod tests;
