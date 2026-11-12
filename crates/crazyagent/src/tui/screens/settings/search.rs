use super::app_plugins::{megabytes, problem_text, runs_line, safe, sentence};
use crate::app_plugins::RunsView;
use crate::engine::owner::SearchSaved;
use crate::semantic::{ModelState, SearchChoice, SearchOption, SearchView};
use crate::tui::client::app_plugins::{AppPluginsEvent, AppPluginsJob};
use crate::tui::client::settings::{SettingsEvent, SettingsJob};
use crate::tui::ctx::Ctx;
use crate::tui::frame::{Hint, Message, Modal, draw_lines, hint};
use crate::tui::keys::Key;
use crate::tui::screens::settings_accounts::{send, settings_event};
use crate::tui::screens::{Flow, Place, Screen};
use crate::tui::seam::Arrival;
use crate::tui::text::{indent, list_row, styled, wrap};
use crate::tui::theme::Role;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::Line;

#[cfg(all(test, unix))]
mod tests;

const MODAL_WIDTH: u16 = 66;
const INTRO: &str = "How agents' search over sessions and memories ranks what it finds. Full text always works; an embedding plugin adds matches by meaning.";
/// What the page says after a save named a version another window had replaced.
pub(in crate::tui) const SEARCH_STALE: &str =
    "Search settings changed in another window. This page now shows the saved ones; choose again.";

/// The host an HTTP plugin's address names, as the engine compares it before it sends.
/// Input: the address. Output: the host, or None when the text is no address.
pub(in crate::tui) fn host_of(url: &str) -> Option<String> {
    url::Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_owned))
}

/// Whether the page has something moving to read again for: a model download that runs,
/// or a usable chosen plugin that has not embedded everything yet. Input: the page read.
/// Output: true while either moves.
pub(in crate::tui) fn polling(view: &SearchView) -> bool {
    if matches!(view.model, ModelState::Downloading { .. }) {
        return true;
    }
    let SearchChoice::Plugin { name } = &view.choice else {
        return false;
    };
    view.options
        .iter()
        .find(|option| option.name == *name)
        .is_some_and(|option| option.usable && view.progress.embedded < view.progress.total)
}

fn size_of(model: &ModelState) -> u64 {
    match model {
        ModelState::NotDownloaded { size_bytes }
        | ModelState::Downloading { size_bytes, .. }
        | ModelState::Ready { size_bytes }
        | ModelState::Failed { size_bytes, .. } => *size_bytes,
    }
}

/// Settings, Search, not built yet: it draws nothing and sends nothing.
pub(in crate::tui) struct SearchPage;

impl SearchPage {
    /// The page. Output: the screen.
    pub fn new() -> Self {
        Self
    }
}

impl Screen for SearchPage {
    fn crumbs(&self) -> Vec<String> {
        vec!["Settings".into(), "Search".into()]
    }

    fn hints(&self, _ctx: &Ctx) -> Vec<Hint> {
        Vec::new()
    }

    fn key(&mut self, _key: &Key, _ctx: &mut Ctx) -> Flow {
        Flow::Ignored
    }

    fn draw(&mut self, _frame: &mut Frame, _area: Rect, _ctx: &Ctx) -> Option<Position> {
        None
    }

    fn place(&self) -> Option<Place> {
        Some(Place::SearchSettings)
    }
}
