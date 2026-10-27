use super::{Plugins, plugin_plain, tilde};
use crate::skills::SkillDetail;
use crate::tui::client::settings::{PluginChange, SettingsEvent, SettingsJob};
use crate::tui::ctx::{Ctx, Load};
use crate::tui::frame::{Hint, Message, hint};
use crate::tui::keys::Key;
use crate::tui::screens::project_markdown::{self, MdLine};
use crate::tui::screens::settings::draw_page;
use crate::tui::screens::settings_accounts::{send, settings_event};
use crate::tui::screens::{Flow, Place, Screen, SettingsSection};
use crate::tui::seam::Arrival;
use crate::tui::text::{cut, join, styled, thumb, with_bar, wrap};
use crate::tui::theme::{Role, Theme};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::Line;

const TAB_ROWS: usize = 2;

/// Drops the frontmatter block at the top of a SKILL.md, which the page shows as the
/// skill's name and description instead. Input: the file text. Output: the Markdown body.
pub fn skill_body(text: &str) -> &str {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = text.split_inclusive('\n');
    if lines.next().map(str::trim_end) != Some("---") {
        return text;
    }
    let mut end = text.find('\n').map_or(text.len(), |at| at + 1);
    for line in lines {
        end += line.len();
        if line.trim_end() == "---" {
            return text[end..].trim_start_matches(['\n', '\r']);
        }
    }
    text
}

/// A file size as the page lists it. Input: bytes. Output: such as "812 B" or "4.2 KB".
pub fn file_size(bytes: u64) -> String {
    const KIB: u64 = 1024;
    if bytes < KIB {
        format!("{bytes} B")
    } else if bytes < KIB * KIB {
        format!("{:.1} KB", bytes as f64 / KIB as f64)
    } else {
        format!("{:.1} MB", bytes as f64 / (KIB * KIB) as f64)
    }
}

fn md_line(line: &MdLine, theme: &Theme) -> Line<'static> {
    join(line.iter().map(|(text, role)| match role {
        project_markdown::Role::Heading | project_markdown::Role::Bold => {
            styled(text.clone(), Role::Heading, theme)
        }
        project_markdown::Role::Code | project_markdown::Role::Muted => {
            styled(text.clone(), Role::Muted, theme)
        }
        project_markdown::Role::Link => styled(text.clone(), Role::Accent, theme),
        project_markdown::Role::Plain | project_markdown::Role::Italic => Line::raw(text.clone()),
    }))
}

/// Settings, Plugins and skills, one skill read in full: its switch, description and
/// folder, the rendered SKILL.md and the folder's files. Input: keys and the engine's
/// skill detail. Output: the scrolled page; Space turns the skill on or off for every
/// project and Esc goes back to the list.
pub struct SkillDetailPage {
    id: String,
    name: String,
    load: Load<SkillDetail>,
    waiting: bool,
    changing: Option<String>,
    top: usize,
    window: usize,
}

impl SkillDetailPage {
    /// The page for one skill with nothing read yet.
    /// Inputs: the skill id and name. Output: the screen; `shown` asks the engine.
    pub fn new(id: String, name: String) -> Self {
        Self {
            id,
            name,
            load: Load::Idle,
            waiting: false,
            changing: None,
            top: 0,
            window: 1,
        }
    }

    /// The page with the skill already read.
    /// Input: the detail. Output: the screen.
    #[cfg(test)]
    pub fn with(detail: SkillDetail) -> Self {
        let mut page = Self::new(detail.skill.id.clone(), detail.skill.name.clone());
        page.load = Load::Ready(detail);
        page
    }

    fn reload(&mut self, ctx: &mut Ctx) {
        if self.waiting {
            return;
        }
        if !matches!(self.load, Load::Ready(_)) {
            self.load = Load::Loading;
        }
        self.waiting = true;
        send(
            ctx,
            SettingsJob::SkillDetail {
                id: self.id.clone(),
            },
        );
    }

    fn toggle(&mut self, ctx: &mut Ctx) {
        let Load::Ready(detail) = &self.load else {
            return;
        };
        if self.changing.is_some() {
            return;
        }
        let enabled = !detail.skill.enabled;
        self.changing = Some(format!(
            "{} is {}.",
            detail.skill.name,
            if enabled { "on" } else { "off" }
        ));
        let id = detail.skill.id.clone();
        send(
            ctx,
            SettingsJob::Plugin(PluginChange::SetSkill { id, enabled }),
        );
    }

    /// The body lines of the page, before scrolling.
    /// Inputs: the content width and context. Output: the lines below the tab strip.
    pub fn lines(&self, limit: usize, ctx: &Ctx) -> Vec<Line<'static>> {
        let theme = &ctx.theme;
        let detail = match &self.load {
            Load::Ready(detail) => detail,
            Load::Failed(code) => {
                return vec![
                    styled(self.name.clone(), Role::Accent, theme),
                    Line::default(),
                    Line::raw(plugin_plain(code)),
                    Line::raw("Press r to try again."),
                ];
            }
            Load::NotReady => {
                return vec![
                    styled(self.name.clone(), Role::Accent, theme),
                    Line::default(),
                    Line::raw("Skill details are not ready in this build."),
                ];
            }
            Load::Idle | Load::Loading => {
                return vec![
                    styled(self.name.clone(), Role::Accent, theme),
                    Line::default(),
                    styled("Reading SKILL.md...", Role::Muted, theme),
                ];
            }
        };
        let skill = &detail.skill;
        let mut lines = vec![join([
            styled(skill.name.clone(), Role::Accent, theme),
            Line::raw("  "),
            if skill.enabled {
                styled("On for every project", Role::Success, theme)
            } else {
                styled("Off", Role::Muted, theme)
            },
        ])];
        lines.extend(wrap(&skill.description, limit).into_iter().map(Line::raw));
        lines.push(styled(
            cut(
                &format!("From {}", tilde(&skill.source, ctx)),
                limit,
                ctx.glyphs.ellipsis,
            ),
            Role::Muted,
            theme,
        ));
        lines.push(Line::default());
        lines.extend(
            project_markdown::render(skill_body(&detail.text), limit)
                .iter()
                .map(|line| md_line(line, theme)),
        );
        lines.push(Line::default());
        let total = detail.files.len() + detail.files_more as usize;
        lines.push(styled(format!("Files  {total}"), Role::Heading, theme));
        for file in &detail.files {
            let size = file_size(file.bytes);
            let room = limit.saturating_sub(size.len() + 2).max(8);
            let path = cut(&file.path, room, ctx.glyphs.ellipsis);
            lines.push(join([
                Line::raw(format!("{path}  ")),
                styled(size, Role::Muted, theme),
            ]));
        }
        if detail.files_more > 0 {
            lines.push(styled(
                format!("And {} more", detail.files_more),
                Role::Muted,
                theme,
            ));
        }
        lines
    }

    fn scroll(&mut self, by: isize) {
        self.top = self.top.saturating_add_signed(by);
    }
}

impl Screen for SkillDetailPage {
    fn crumbs(&self) -> Vec<String> {
        vec![
            "Settings".into(),
            "Plugins and skills".into(),
            self.name.clone(),
        ]
    }

    fn hints(&self, _ctx: &Ctx) -> Vec<Hint> {
        vec![
            hint("\u{2191}\u{2193}", "Scroll"),
            hint("Space", "On or off"),
            hint("Esc", "Back"),
        ]
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let page = isize::try_from(self.window.max(1)).unwrap_or(1);
        match key {
            Key::Esc => return Flow::Replace(Box::new(Plugins::new())),
            Key::Char(' ') => self.toggle(ctx),
            Key::Char('r') if matches!(self.load, Load::Failed(_)) => self.reload(ctx),
            Key::Up => self.scroll(-1),
            Key::Down => self.scroll(1),
            Key::PageUp => self.scroll(-page),
            Key::PageDown => self.scroll(page),
            Key::Home => self.top = 0,
            Key::End => self.top = usize::MAX,
            _ => return Flow::Ignored,
        }
        Flow::Handled
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        match settings_event(arrival) {
            Some(SettingsEvent::SkillDetail(result)) if self.waiting => {
                self.waiting = false;
                match result {
                    Ok(detail) if detail.skill.id == self.id => {
                        self.name = detail.skill.name.clone();
                        self.load = Load::Ready(detail.clone());
                    }
                    Ok(_) => self.load = Load::Failed("skill_page_invalid".into()),
                    Err(code) if matches!(self.load, Load::Ready(_)) => ctx.say(Message::error(
                        "Could not refresh the skill.",
                        plugin_plain(code),
                    )),
                    Err(code) => self.load = Load::Failed(code.clone()),
                }
            }
            Some(SettingsEvent::PluginChanged(result)) => {
                let Some(done) = self.changing.take() else {
                    return Flow::Handled;
                };
                match result {
                    Ok(_) => ctx.say(Message::success(&done, "")),
                    Err(code) => ctx.say(Message::error(plugin_plain(code), "")),
                }
                self.reload(ctx);
            }
            _ => {}
        }
        Flow::Handled
    }

    fn shown(&mut self, ctx: &mut Ctx) {
        if matches!(self.load, Load::Idle) {
            self.reload(ctx);
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let limit = (area.width as usize).saturating_sub(4 + 1).max(8);
        let lines = self.lines(limit, ctx);
        let window = (area.height as usize).saturating_sub(TAB_ROWS).max(1);
        self.window = window;
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
        self.waiting || self.changing.is_some()
    }

    fn place(&self) -> Option<Place> {
        Some(Place::Settings(SettingsSection::Plugins))
    }
}
