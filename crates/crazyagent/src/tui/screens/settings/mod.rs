pub mod pool_editor;
pub mod pools;
pub mod profile;

use super::{Flow, Screen, SettingsSection};
use crate::tui::ctx::{Ctx, Load};
use crate::tui::frame::{Tab, draw_lines, tab_strip};
use crate::tui::glyphs::Section;
use crate::tui::text::indent;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::Line;

fn glyph(section: SettingsSection) -> Section {
    match section {
        SettingsSection::Accounts => Section::Accounts,
        SettingsSection::Pools => Section::Pools,
        SettingsSection::Harnesses => Section::Harnesses,
        SettingsSection::Plugins => Section::Plugins,
        SettingsSection::Process => Section::Process,
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
                SettingsSection::Accounts => {
                    count(&ctx.store.accounts, |rows| rows.len().to_string())
                }
                SettingsSection::Pools => count(&ctx.store.pools, |pools| pools.len().to_string()),
                SettingsSection::Harnesses => count(&ctx.store.harnesses, |(found, all)| {
                    format!("{found} of {all}")
                }),
                SettingsSection::Memory => count(&ctx.store.memory, ToString::to_string),
                SettingsSection::Plugins | SettingsSection::Process | SettingsSection::Profile => {
                    String::new()
                }
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
        SettingsSection::Plugins => Box::new(super::settings_plugins::Plugins::new()),
        SettingsSection::Process => {
            Box::new(super::process::ProcessPage::new(super::process::Scope::App))
        }
        SettingsSection::Profile => Box::new(profile::ProfilePage::new(ctx)),
    }
}

/// Builds the Add account flow.
/// Input: the context. Output: its first screen.
pub fn open_add_account(_ctx: &mut Ctx) -> Box<dyn Screen> {
    Box::new(super::settings_add_account::AddAccount::new())
}
