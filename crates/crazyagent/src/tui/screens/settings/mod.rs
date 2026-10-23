pub mod pool_editor;
pub mod pools;
pub mod profile;

use super::{Flow, Place, Screen, SettingsSection};
use crate::tui::ctx::{Ctx, Load};
use crate::tui::frame::{Hint, Tab, draw_lines, hint, sections_hint, tab_strip};
use crate::tui::glyphs::Section;
use crate::tui::keys::Key;
use crate::tui::text::{indent, styled, wrap};
use crate::tui::theme::Role;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::Line;

fn glyph(section: SettingsSection) -> Section {
    match section {
        SettingsSection::Accounts => Section::Accounts,
        SettingsSection::Pools => Section::Pools,
        SettingsSection::Harnesses => Section::Harnesses,
        SettingsSection::Plugins => Section::Plugins,
        SettingsSection::Memory => Section::Memory,
        SettingsSection::Profile => Section::Profile,
    }
}

fn count<T>(load: &Load<T>, show: impl Fn(&T) -> String) -> String {
    load.ready().map(show).unwrap_or_default()
}

/// The Settings tab strip with counts from the engine.
/// Inputs: the active section and the context. Output: two lines; a count is
/// shown only when the engine reported it.
pub fn tabs(active: SettingsSection, ctx: &Ctx) -> [Line<'static>; 2] {
    let tabs: Vec<Tab> = SettingsSection::ALL
        .iter()
        .map(|section| Tab {
            label: section.label().into(),
            section: Some(glyph(*section)),
            count: match section {
                SettingsSection::Accounts => count(&ctx.store.accounts, |rows| rows.len().to_string()),
                SettingsSection::Pools => count(&ctx.store.pools, |pools| pools.len().to_string()),
                SettingsSection::Harnesses => {
                    count(&ctx.store.harnesses, |(found, all)| format!("{found} of {all}"))
                }
                SettingsSection::Memory => count(&ctx.store.memory, ToString::to_string),
                SettingsSection::Plugins | SettingsSection::Profile => String::new(),
            },
        })
        .collect();
    let active = SettingsSection::ALL
        .iter()
        .position(|section| *section == active)
        .expect("every section is listed");
    tab_strip(ctx.width as usize, &tabs, active, &ctx.theme, ctx.glyphs)
}

/// Draws a Settings page: the tab strip, then the content indented two columns.
/// Inputs: frame, area, section, content lines, the content cursor and the
/// context. Output: the absolute cursor position.
pub fn draw_page(
    frame: &mut Frame,
    area: Rect,
    section: SettingsSection,
    content: Vec<Line<'static>>,
    cursor: Option<Position>,
    ctx: &Ctx,
) -> Option<Position> {
    let [top, under] = tabs(section, ctx);
    let mut lines = vec![top, under];
    lines.extend(indent(content, 2));
    draw_lines(frame.buffer_mut(), area, &lines);
    cursor.map(|at| Position::new(area.x + 2 + at.x, area.y + 2 + at.y))
}

/// The neighbouring Settings section as a replacement screen.
/// Inputs: the current section, direction and context. Output: the flow.
pub fn step(section: SettingsSection, forward: bool, ctx: &mut Ctx) -> Flow {
    Flow::Replace(open(section.step(forward), ctx))
}

/// Builds the screen for one Settings section.
/// Inputs: the section and context. Output: the section's screen.
pub fn open(section: SettingsSection, ctx: &mut Ctx) -> Box<dyn Screen> {
    match section {
        SettingsSection::Pools => Box::new(pools::Pools::new()),
        SettingsSection::Accounts => Box::new(super::settings_accounts::Accounts::new()),
        SettingsSection::Harnesses => Box::new(super::settings_harnesses::Harnesses::new()),
        SettingsSection::Memory => Box::new(super::settings_memory::Memory::new()),
        SettingsSection::Profile => Box::new(profile::ProfilePage::new(ctx)),
        other => Box::new(Pending::new(other)),
    }
}

/// Builds the Add account flow.
/// Input: the context. Output: its first screen.
pub fn open_add_account(_ctx: &mut Ctx) -> Box<dyn Screen> {
    Box::new(super::settings_add_account::AddAccount::new())
}

/// A Settings section whose engine call is not in this build.
/// Input: the section. Output: its tab strip and "Not ready yet."; it shows no
/// data.
pub struct Pending {
    section: SettingsSection,
}

impl Pending {
    /// The placeholder for one section.
    /// Input: the section. Output: the screen.
    pub fn new(section: SettingsSection) -> Self {
        Self { section }
    }
}

impl Screen for Pending {
    fn crumbs(&self) -> Vec<String> {
        vec!["Settings".into(), self.section.label().into()]
    }

    fn hints(&self, ctx: &Ctx) -> Vec<Hint> {
        vec![sections_hint(ctx.glyphs), hint("Esc", "Home")]
    }

    fn key(&mut self, _key: &Key, _ctx: &mut Ctx) -> Flow {
        Flow::Ignored
    }

    fn step(&mut self, forward: bool, ctx: &mut Ctx) -> Flow {
        step(self.section, forward, ctx)
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let mut content = vec![
            styled(self.section.label(), Role::Accent, &ctx.theme),
            Line::default(),
            Line::raw("Not ready yet."),
        ];
        content.extend(
            wrap(
                "This build of CrazyAgent cannot show this section yet. Nothing here is hidden from you.",
                area.width as usize - 4,
            )
            .into_iter()
            .map(|line| styled(line, Role::Muted, &ctx.theme)),
        );
        draw_page(frame, area, self.section, content, None, ctx)
    }

    fn place(&self) -> Option<Place> {
        Some(Place::Settings(self.section))
    }
}
