pub mod account_choice;
pub mod pool_editor;
pub mod pools;
pub mod profile;
pub mod replay;
pub mod restore;
pub mod spend;

use super::{Flow, Screen, SettingsSection};
use crate::tui::ctx::{Ctx, Load};
use crate::tui::frame::{Tab, draw_lines, tab_strip};
use crate::tui::glyphs::Section;
use crate::tui::text::indent;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::Line;

fn glyph(section: SettingsSection) -> Option<Section> {
    Some(match section {
        SettingsSection::Accounts => Section::Accounts,
        SettingsSection::Pools => Section::Pools,
        SettingsSection::Harnesses => Section::Harnesses,
        SettingsSection::Plugins => Section::Plugins,
        SettingsSection::Process => Section::Process,
        SettingsSection::Memory => Section::Memory,
        SettingsSection::Profile => Section::Profile,
        SettingsSection::Restore => return None,
    })
}

/// The Settings sections shown, in tab order: every section, then Restore 2.4 for an engine
/// with a 2.4 backup or a restore record, or while its page is open (L5).
/// Inputs: the active section and the context. Output: the sections, the active one among
/// them.
pub fn sections(active: SettingsSection, ctx: &Ctx) -> Vec<SettingsSection> {
    let mut shown = SettingsSection::ALL.to_vec();
    if ctx.restore_shown || active == SettingsSection::Restore {
        shown.push(SettingsSection::Restore);
    }
    assert!(shown.contains(&active));
    assert!(!SettingsSection::ALL.contains(&SettingsSection::Restore));
    shown
}

fn count<T>(load: &Load<T>, show: impl Fn(&T) -> String) -> String {
    load.ready().map(show).unwrap_or_default()
}

/// The Settings tab strip with counts from the engine.
/// Inputs: the active section and the context. Output: two lines; a count is
/// shown only when the engine reported it.
pub fn tabs(active: SettingsSection, ctx: &Ctx) -> [Line<'static>; 2] {
    let shown = sections(active, ctx);
    let tabs: Vec<Tab> = shown
        .iter()
        .map(|section| Tab {
            label: section.label().into(),
            section: glyph(*section),
            count: match section {
                SettingsSection::Accounts => {
                    count(&ctx.store.accounts, |rows| rows.len().to_string())
                }
                SettingsSection::Pools => count(&ctx.store.pools, |pools| pools.len().to_string()),
                SettingsSection::Harnesses => count(&ctx.store.harnesses, |(found, all)| {
                    format!("{found} of {all}")
                }),
                SettingsSection::Memory => count(&ctx.store.memory, ToString::to_string),
                SettingsSection::Plugins
                | SettingsSection::Process
                | SettingsSection::Profile
                | SettingsSection::Restore => String::new(),
            },
        })
        .collect();
    let active = shown
        .iter()
        .position(|section| *section == active)
        .expect("the active section is shown");
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
    let next = section.step(forward, &sections(section, ctx));
    Flow::Replace(open(next, ctx))
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
        SettingsSection::Restore => Box::new(restore::RestorePage::new(ctx)),
    }
}

/// Builds the Add account flow.
/// Input: the context. Output: its first screen.
pub fn open_add_account(_ctx: &mut Ctx) -> Box<dyn Screen> {
    Box::new(super::settings_add_account::AddAccount::new())
}
