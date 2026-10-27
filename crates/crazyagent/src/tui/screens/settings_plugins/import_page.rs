use super::{Plugins, plugin_plain, tilde};
use crate::skills::import::{ImportCandidate, ImportList, ImportState, Imported};
use crate::tui::client::settings::{SettingsEvent, SettingsJob};
use crate::tui::ctx::{Ctx, Load};
use crate::tui::frame::{Hint, Message, hint};
use crate::tui::keys::Key;
use crate::tui::screens::settings::draw_page;
use crate::tui::screens::settings_accounts::{send, settings_event};
use crate::tui::screens::{Flow, Place, Screen, SettingsSection};
use crate::tui::seam::Arrival;
use crate::tui::text::{cut, join, list_row, styled, thumb, with_bar, wrap};
use crate::tui::theme::Role;
use crate::tui::widgets::checkbox;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::Line;
use std::collections::BTreeSet;

const TAB_ROWS: usize = 2;
const ROWS_PER_SKILL: usize = 2;
const INTRO: &str = "From ~/.claude/skills and ~/.codex/skills. Ticked skills are copied into CrazyAgent and turned on for every project. Dotfiles and files that look like keys or passwords are left out, and your folders do not change.";

/// Says whether a found skill can be ticked: new, or added with different files.
/// Input: its state. Output: true for new and differing skills.
pub fn importable(state: &ImportState) -> bool {
    matches!(state, ImportState::New | ImportState::Differs { .. })
}

/// The plain line for one ticked skill's import. Inputs: its name and the engine's
/// outcome. Output: what was imported and left out, or why it was not imported.
pub fn outcome_line(name: &str, result: &Result<Imported, String>) -> String {
    match result {
        Ok(imported) if imported.skipped_total == 0 => format!("Imported {name}."),
        Ok(imported) => {
            let total = imported.skipped_total as usize;
            assert!(imported.skipped.len() <= total);
            let rest = total - imported.skipped.len();
            let more = if rest > 0 {
                format!(" and {rest} more")
            } else {
                String::new()
            };
            format!(
                "Imported {name}. Left out {}{more}.",
                imported.skipped.join(", ")
            )
        }
        Err(code) => format!("Could not import {name}: {}", plugin_plain(code)),
    }
}

fn state_word(state: &ImportState) -> (&'static str, Role) {
    match state {
        ImportState::New => ("", Role::Muted),
        ImportState::Added { .. } => ("Already added", Role::Muted),
        ImportState::Differs { .. } => ("Differs", Role::Warning),
        ImportState::Unusable { .. } => ("Can't import", Role::Error),
    }
}

/// Settings, Plugins and skills, Import my skills: the skills found in the owner's
/// `~/.claude/skills` and `~/.codex/skills`, read only once this page opens. Input: keys
/// and the engine's scan. Output: the list; Space ticks a skill, Enter imports the ticked
/// ones and Esc goes back.
pub struct ImportSkillsPage {
    load: Load<ImportList>,
    waiting: bool,
    importing: bool,
    ticked: BTreeSet<String>,
    selected: usize,
    outcomes: Vec<(bool, String)>,
    top: usize,
    window: usize,
}

impl Default for ImportSkillsPage {
    fn default() -> Self {
        Self::new()
    }
}

impl ImportSkillsPage {
    /// The page with nothing read yet. Output: the screen; `shown` asks the engine to scan.
    pub fn new() -> Self {
        Self {
            load: Load::Idle,
            waiting: false,
            importing: false,
            ticked: BTreeSet::new(),
            selected: 0,
            outcomes: Vec::new(),
            top: 0,
            window: 1,
        }
    }

    /// The page with a scan already read. Input: the scan. Output: the screen.
    #[cfg(test)]
    pub fn with(list: ImportList) -> Self {
        let mut page = Self::new();
        page.load = Load::Ready(list);
        page
    }

    fn found(&self) -> &[ImportCandidate] {
        match &self.load {
            Load::Ready(list) => &list.found,
            _ => &[],
        }
    }

    fn scan(&mut self, ctx: &mut Ctx) {
        if self.waiting || self.importing {
            return;
        }
        if !matches!(self.load, Load::Ready(_)) {
            self.load = Load::Loading;
        }
        self.waiting = true;
        send(ctx, SettingsJob::ImportScan);
    }

    fn tick(&mut self) {
        let Some(found) = self.found().get(self.selected) else {
            return;
        };
        if !importable(&found.state) {
            return;
        }
        let folder = found.folder.clone();
        if !self.ticked.remove(&folder) {
            self.ticked.insert(folder);
        }
    }

    fn picks(&self) -> Vec<(String, Option<String>)> {
        self.found()
            .iter()
            .filter(|found| self.ticked.contains(&found.folder) && importable(&found.state))
            .map(|found| {
                let replace = match &found.state {
                    ImportState::Differs { id } => Some(id.clone()),
                    _ => None,
                };
                (found.folder.clone(), replace)
            })
            .collect()
    }

    fn import(&mut self, ctx: &mut Ctx) {
        let picks = self.picks();
        if picks.is_empty() || self.importing || self.waiting {
            return;
        }
        self.importing = true;
        send(ctx, SettingsJob::ImportSkills { picks });
    }

    fn row(
        &self,
        index: usize,
        found: &ImportCandidate,
        limit: usize,
        ctx: &Ctx,
    ) -> [Line<'static>; 2] {
        let theme = &ctx.theme;
        let glyphs = ctx.glyphs;
        let (word, role) = state_word(&found.state);
        let tick = if importable(&found.state) {
            checkbox(self.ticked.contains(&found.folder), &found.name)
        } else {
            format!("[-] {}", found.name)
        };
        let room = limit.saturating_sub(word.len() + 4).max(8);
        let head = join([
            Line::raw(cut(&tick, room, glyphs.ellipsis)),
            Line::raw("  "),
            styled(word, role, theme),
        ]);
        let about = match &found.state {
            ImportState::Unusable { code } => plugin_plain(code).to_owned(),
            ImportState::Differs { .. } => {
                "Importing replaces the added copy and keeps its project switches.".to_owned()
            }
            _ => found.line.clone(),
        };
        let from = format!("{}: {}", found.from.label(), tilde(&found.folder, ctx));
        let detail = if about.is_empty() {
            from
        } else {
            format!("{about}  {from}")
        };
        [
            list_row(head, limit, index == self.selected, true, theme, glyphs),
            styled(
                cut(&format!("    {detail}"), limit, glyphs.ellipsis),
                Role::Muted,
                theme,
            ),
        ]
    }

    /// The body lines of the page before scrolling, and the line of the selected skill.
    /// Inputs: the content width and context. Output: the lines below the tab strip.
    pub fn lines(&self, limit: usize, ctx: &Ctx) -> (Vec<Line<'static>>, Option<usize>) {
        let theme = &ctx.theme;
        let mut lines = vec![styled("Import my skills", Role::Accent, theme)];
        lines.extend(
            wrap(INTRO, limit)
                .into_iter()
                .map(|part| styled(part, Role::Muted, theme)),
        );
        lines.push(Line::default());
        for (ok, outcome) in &self.outcomes {
            let role = if *ok { Role::Success } else { Role::Error };
            lines.extend(
                wrap(outcome, limit)
                    .into_iter()
                    .map(|part| styled(part, role, theme)),
            );
        }
        if !self.outcomes.is_empty() {
            lines.push(Line::default());
        }
        let list = match &self.load {
            Load::Ready(list) => list,
            Load::Failed(code) => {
                lines.push(Line::raw(plugin_plain(code)));
                lines.push(Line::raw("Press r to try again."));
                return (lines, None);
            }
            Load::NotReady => {
                lines.push(Line::raw("Importing skills is not ready in this build."));
                return (lines, None);
            }
            Load::Idle | Load::Loading => {
                lines.push(styled("Looking for your skills...", Role::Muted, theme));
                return (lines, None);
            }
        };
        if list.found.is_empty() {
            lines.push(Line::raw("No skills found in those folders."));
            return (lines, None);
        }
        let first = lines.len();
        for (index, found) in list.found.iter().enumerate() {
            lines.extend(self.row(index, found, limit, ctx));
        }
        if list.more > 0 {
            lines.push(styled(
                format!("And {} more not shown", list.more),
                Role::Muted,
                theme,
            ));
        }
        (lines, Some(first + self.selected * ROWS_PER_SKILL))
    }
}

impl Screen for ImportSkillsPage {
    fn crumbs(&self) -> Vec<String> {
        vec![
            "Settings".into(),
            "Plugins and skills".into(),
            "Import my skills".into(),
        ]
    }

    fn hints(&self, _ctx: &Ctx) -> Vec<Hint> {
        vec![
            hint("Space", "Tick"),
            hint("Enter", "Import ticked"),
            hint("r", "Look again"),
            hint("Esc", "Back"),
        ]
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let count = self.found().len();
        match key {
            Key::Esc => return Flow::Replace(Box::new(Plugins::new())),
            Key::Char(' ') => self.tick(),
            Key::Enter => self.import(ctx),
            Key::Char('r') => self.scan(ctx),
            Key::Up => self.selected = self.selected.saturating_sub(1),
            Key::Down => self.selected = (self.selected + 1).min(count.saturating_sub(1)),
            Key::Home => self.selected = 0,
            Key::End => self.selected = count.saturating_sub(1),
            _ => return Flow::Ignored,
        }
        Flow::Handled
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        match settings_event(arrival) {
            Some(SettingsEvent::ImportScan(result)) if self.waiting => {
                self.waiting = false;
                match result {
                    Ok(list) => {
                        self.ticked.retain(|folder| {
                            list.found
                                .iter()
                                .any(|found| &found.folder == folder && importable(&found.state))
                        });
                        self.selected = self.selected.min(list.found.len().saturating_sub(1));
                        self.load = Load::Ready(list.clone());
                    }
                    Err(code) if matches!(self.load, Load::Ready(_)) => ctx.say(Message::error(
                        "Could not look for your skills again.",
                        plugin_plain(code),
                    )),
                    Err(code) => self.load = Load::Failed(code.clone()),
                }
            }
            Some(SettingsEvent::Imported(result)) if self.importing => {
                self.importing = false;
                match result {
                    Ok(done) => {
                        let names: Vec<(String, String)> = self
                            .found()
                            .iter()
                            .map(|found| (found.folder.clone(), found.name.clone()))
                            .collect();
                        self.outcomes = done
                            .iter()
                            .map(|(folder, outcome)| {
                                let name = names
                                    .iter()
                                    .find(|(known, _)| known == folder)
                                    .map_or(folder.as_str(), |(_, name)| name.as_str());
                                (outcome.is_ok(), outcome_line(name, outcome))
                            })
                            .collect();
                        let imported = done.iter().filter(|(_, outcome)| outcome.is_ok()).count();
                        if imported > 0 {
                            let lead = if imported == 1 {
                                "Imported 1 skill.".to_owned()
                            } else {
                                format!("Imported {imported} skills.")
                            };
                            ctx.say(Message::success(&lead, ""));
                        }
                        self.ticked.clear();
                    }
                    Err(code) => ctx.say(Message::error(
                        "Could not import your skills.",
                        plugin_plain(code),
                    )),
                }
                self.scan(ctx);
            }
            _ => {}
        }
        Flow::Handled
    }

    fn shown(&mut self, ctx: &mut Ctx) {
        if matches!(self.load, Load::Idle) {
            self.scan(ctx);
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let limit = (area.width as usize).saturating_sub(4 + 1).max(8);
        let (lines, at) = self.lines(limit, ctx);
        let window = (area.height as usize).saturating_sub(TAB_ROWS).max(1);
        self.window = window;
        if let Some(at) = at {
            if at < self.top {
                self.top = at;
            } else if at + ROWS_PER_SKILL > self.top + window {
                self.top = (at + ROWS_PER_SKILL).saturating_sub(window);
            }
        }
        self.top = self.top.min(lines.len().saturating_sub(window));
        let total = lines.len();
        let shown: Vec<Line<'static>> = lines.into_iter().skip(self.top).take(window).collect();
        let content = if total > window {
            let (top, size) = thumb(window, self.top, total);
            with_bar(shown, limit + 1, top, size, &ctx.theme, ctx.glyphs)
        } else {
            shown
        };
        assert!(content.len() <= window);
        draw_page(frame, area, SettingsSection::Plugins, content, None, ctx)
    }

    fn busy(&self, _ctx: &Ctx) -> bool {
        self.waiting || self.importing
    }

    fn place(&self) -> Option<Place> {
        Some(Place::Settings(SettingsSection::Plugins))
    }
}
