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

#[derive(Debug, PartialEq)]
enum State {
    Loading,
    Failed(String),
    Ready(SearchView),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pending {
    Read,
    Save,
    Model,
}

/// The confirm Search has open: before an HTTP plugin is chosen (its name, host and the
/// engine's sends lines), before the built-in model downloads (the size it names, sent as
/// confirm_bytes), or before the model is deleted.
#[derive(Debug, PartialEq)]
enum Open {
    None,
    Http {
        option: Box<SearchOption>,
        host: String,
    },
    Download {
        bytes: u64,
    },
    Delete {
        bytes: u64,
    },
}

/// Settings, Search: Full text only and every embedding plugin as choices (an unusable one
/// with its reason), the embedding progress of a plugin choice and the built-in model's
/// download state. Enter saves a choice by version, asking `y` first for an HTTP plugin
/// with its host; `d`, `c` and `x` download, cancel and delete the model, the download and
/// delete after `y`. The page reads again on each shell refresh while something moves.
pub(in crate::tui) struct SearchPage {
    state: State,
    selected: usize,
    open: Open,
    pending: Option<Pending>,
    failure: Option<String>,
}

impl SearchPage {
    /// The page, loading until the engine answers. Output: the screen; `shown` reads.
    pub fn new() -> Self {
        Self {
            state: State::Loading,
            selected: 0,
            open: Open::None,
            pending: None,
            failure: None,
        }
    }

    fn ask(&mut self, job: AppPluginsJob, pending: Pending, ctx: &mut Ctx) {
        assert!(self.pending.is_none(), "one request at a time");
        self.pending = Some(pending);
        send(ctx, SettingsJob::AppPlugins(Box::new(job)));
    }

    fn read(&mut self, ctx: &mut Ctx) {
        if self.pending.is_none() {
            self.ask(AppPluginsJob::Search, Pending::Read, ctx);
        }
    }

    fn view(&self) -> Option<&SearchView> {
        match &self.state {
            State::Ready(view) => Some(view),
            _ => None,
        }
    }

    fn choose(&mut self, ctx: &mut Ctx) {
        let Some(view) = self.view() else {
            return;
        };
        let version = view.version;
        let (choice, option) = match self.selected {
            0 => (SearchChoice::FullText, None),
            index => {
                let Some(option) = view.options.get(index - 1) else {
                    return;
                };
                (
                    SearchChoice::Plugin {
                        name: option.name.clone(),
                    },
                    Some(option.clone()),
                )
            }
        };
        if choice == view.choice {
            return;
        }
        if let Some(option) = option {
            if !option.usable {
                ctx.say(Message::error(
                    "Not usable yet.",
                    &option.reason.as_deref().map_or_else(String::new, safe),
                ));
                return;
            }
            if let Some(RunsView::Http { url, .. }) = &option.runs {
                match host_of(url) {
                    Some(host) => {
                        self.failure = None;
                        self.open = Open::Http {
                            option: Box::new(option),
                            host,
                        };
                    }
                    None => ctx.say(Message::error(
                        "Not chosen.",
                        &sentence("search_choice_unavailable"),
                    )),
                }
                return;
            }
        }
        self.failure = None;
        self.ask(
            AppPluginsJob::SetSearch {
                choice,
                confirm_host: None,
                version,
            },
            Pending::Save,
            ctx,
        );
    }

    fn confirm(&mut self, ctx: &mut Ctx) {
        let Some(version) = self.view().map(|view| view.version) else {
            return;
        };
        let (job, pending) = match &self.open {
            Open::Http { option, host } => (
                AppPluginsJob::SetSearch {
                    choice: SearchChoice::Plugin {
                        name: option.name.clone(),
                    },
                    confirm_host: Some(host.clone()),
                    version,
                },
                Pending::Save,
            ),
            Open::Download { bytes } => (
                AppPluginsJob::Download {
                    confirm_bytes: *bytes,
                },
                Pending::Model,
            ),
            Open::Delete { .. } => (AppPluginsJob::DeleteModel, Pending::Model),
            Open::None => return,
        };
        self.failure = None;
        self.ask(job, pending, ctx);
    }

    fn apply(&mut self, event: &AppPluginsEvent, ctx: &mut Ctx) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        match event {
            AppPluginsEvent::Search(Ok(view)) => {
                if pending == Pending::Model {
                    self.open = Open::None;
                }
                self.state = State::Ready(view.clone());
            }
            AppPluginsEvent::Search(Err(code)) if pending == Pending::Read => {
                if self.view().is_none() {
                    self.state = State::Failed(code.clone());
                } else {
                    self.failure = Some(sentence(code));
                }
            }
            AppPluginsEvent::Search(Err(code)) | AppPluginsEvent::SearchSaved(Err(code)) => {
                self.failure = Some(sentence(code));
            }
            AppPluginsEvent::SearchSaved(Ok(SearchSaved::Saved(view))) => {
                self.open = Open::None;
                self.state = State::Ready((**view).clone());
                ctx.say(Message::success("Saved.", "Search setting saved."));
            }
            AppPluginsEvent::SearchSaved(Ok(SearchSaved::Stale(_))) => {
                self.open = Open::None;
                self.failure = Some(SEARCH_STALE.into());
                self.read(ctx);
            }
            AppPluginsEvent::Listing(_) | AppPluginsEvent::Preview { .. } => {
                self.pending = Some(pending);
            }
        }
    }

    fn model_lines(&self, model: &ModelState, limit: usize, ctx: &Ctx) -> Vec<Line<'static>> {
        let size = megabytes(size_of(model));
        let (text, keys) = match model {
            ModelState::NotDownloaded { .. } => (
                format!("Not on this computer. A {size} download, once; it then works offline."),
                "d Download",
            ),
            ModelState::Downloading { done_bytes, .. } => (
                format!("Downloading: {} of {size}", megabytes(*done_bytes)),
                "c Cancel download",
            ),
            ModelState::Ready { .. } => {
                (format!("Ready: {size} on this computer."), "x Delete model")
            }
            ModelState::Failed { problem, .. } => (problem_text(problem), "d Download again"),
        };
        let mut lines: Vec<Line<'static>> = wrap(&text, limit).into_iter().map(Line::raw).collect();
        lines.push(styled(keys, Role::Muted, &ctx.theme));
        lines
    }

    fn body(&self, limit: usize, ctx: &Ctx) -> Vec<Line<'static>> {
        let theme = &ctx.theme;
        let mut lines: Vec<Line<'static>> = wrap(INTRO, limit)
            .into_iter()
            .map(|part| styled(part, Role::Muted, theme))
            .collect();
        lines.push(Line::default());
        let view = match &self.state {
            State::Loading => {
                lines.push(styled("Reading Search settings...", Role::Muted, theme));
                return lines;
            }
            State::Failed(code) => {
                lines.extend(
                    wrap(&sentence(code), limit)
                        .into_iter()
                        .map(|part| styled(part, Role::Error, theme)),
                );
                return lines;
            }
            State::Ready(view) => view,
        };
        if let Some(failure) = &self.failure
            && self.open == Open::None
        {
            lines.extend(
                wrap(failure, limit)
                    .into_iter()
                    .map(|part| styled(part, Role::Error, theme)),
            );
        }
        if let Some(note) = &view.note {
            lines.extend(
                wrap(&safe(note), limit)
                    .into_iter()
                    .map(|part| styled(part, Role::Warning, theme)),
            );
        }
        lines.push(styled("Rank search with", Role::Heading, theme));
        let mark = |chosen: bool| if chosen { "(*) " } else { "( ) " };
        lines.push(list_row(
            Line::raw(format!(
                "{}Full text only",
                mark(view.choice == SearchChoice::FullText)
            )),
            limit,
            self.selected == 0,
            true,
            theme,
            ctx.glyphs,
        ));
        lines.push(styled(
            "      Exact words only. Nothing is downloaded or sent.",
            Role::Muted,
            theme,
        ));
        for (index, option) in view.options.iter().enumerate() {
            let chosen =
                matches!(&view.choice, SearchChoice::Plugin { name } if *name == option.name);
            lines.push(list_row(
                Line::raw(format!("{}{}", mark(chosen), safe(&option.title))),
                limit,
                self.selected == index + 1,
                true,
                theme,
                ctx.glyphs,
            ));
            let mut under: Vec<String> = option.runs.iter().map(runs_line).collect();
            if !option.usable
                && let Some(reason) = &option.reason
            {
                under.push(safe(reason));
            }
            for text in under {
                lines.extend(
                    wrap(&text, limit.saturating_sub(6))
                        .into_iter()
                        .map(|part| styled(format!("      {part}"), Role::Muted, theme)),
                );
            }
        }
        if matches!(view.choice, SearchChoice::Plugin { .. }) {
            lines.push(Line::raw(format!(
                "Embedded {} of {} sessions and memories.",
                view.progress.embedded, view.progress.total
            )));
        }
        lines.push(Line::default());
        lines.push(styled("Built-in model", Role::Heading, theme));
        lines.extend(self.model_lines(&view.model, limit, ctx));
        lines
    }
}

impl Default for SearchPage {
    fn default() -> Self {
        Self::new()
    }
}

impl Screen for SearchPage {
    fn crumbs(&self) -> Vec<String> {
        vec!["Settings".into(), "Search".into()]
    }

    fn hints(&self, ctx: &Ctx) -> Vec<Hint> {
        let Some(view) = self.view() else {
            return match self.state {
                State::Failed(_) => vec![hint("r", "Try again"), hint("Esc", "Back")],
                _ => vec![hint("Esc", "Back")],
            };
        };
        let mut hints = vec![
            hint("Enter", "Choose"),
            hint(&format!("{}{}", ctx.glyphs.up, ctx.glyphs.down), "Move"),
        ];
        match view.model {
            ModelState::NotDownloaded { .. } => hints.push(hint("d", "Download model")),
            ModelState::Failed { .. } => hints.push(hint("d", "Download again")),
            ModelState::Downloading { .. } => hints.push(hint("c", "Cancel download")),
            ModelState::Ready { .. } => hints.push(hint("x", "Delete model")),
        }
        hints.push(hint("r", "Read again"));
        hints.push(hint("Esc", "Back"));
        hints
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        if self.open != Open::None {
            match key {
                Key::Esc => {
                    self.open = Open::None;
                    self.failure = None;
                }
                Key::Char('y') if self.pending.is_none() => self.confirm(ctx),
                _ => {}
            }
            return Flow::Handled;
        }
        let Some(view) = self.view() else {
            return match key {
                Key::Char('r') => {
                    self.read(ctx);
                    Flow::Handled
                }
                _ => Flow::Ignored,
            };
        };
        let rows = view.options.len() + 1;
        let model = view.model.clone();
        let idle = self.pending.is_none();
        match key {
            Key::Char('r') => self.read(ctx),
            Key::Up => self.selected = self.selected.saturating_sub(1),
            Key::Down => self.selected = (self.selected + 1).min(rows - 1),
            Key::Enter | Key::Char(' ') if idle => self.choose(ctx),
            Key::Char('d')
                if idle
                    && matches!(
                        model,
                        ModelState::NotDownloaded { .. } | ModelState::Failed { .. }
                    ) =>
            {
                self.failure = None;
                self.open = Open::Download {
                    bytes: size_of(&model),
                };
            }
            Key::Char('c') if idle && matches!(model, ModelState::Downloading { .. }) => {
                self.ask(AppPluginsJob::CancelDownload, Pending::Model, ctx);
            }
            Key::Char('x') if idle && matches!(model, ModelState::Ready { .. }) => {
                self.failure = None;
                self.open = Open::Delete {
                    bytes: size_of(&model),
                };
            }
            _ => return Flow::Ignored,
        }
        Flow::Handled
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        if let Some(SettingsEvent::AppPlugins(event)) = settings_event(arrival) {
            self.apply(event, ctx);
        }
        Flow::Handled
    }

    fn shown(&mut self, ctx: &mut Ctx) {
        let moving = self.view().is_some_and(polling);
        if self.state == State::Loading || moving {
            self.read(ctx);
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let limit = usize::from(area.width).saturating_sub(4).max(8);
        draw_lines(frame.buffer_mut(), area, &indent(self.body(limit, ctx), 2));
        None
    }

    fn modal(&self, ctx: &Ctx) -> Option<(Modal, Vec<Hint>)> {
        let room = usize::from(MODAL_WIDTH) - 4;
        let mut body: Vec<Line<'static>> = Vec::new();
        if let Some(failure) = &self.failure {
            body.extend(
                wrap(failure, room)
                    .into_iter()
                    .map(|part| styled(part, Role::Error, &ctx.theme)),
            );
        }
        let (title, confirm) = match &self.open {
            Open::None => return None,
            Open::Http { option, host } => {
                let host = safe(host);
                body.extend(
                    wrap(
                        &format!(
                            "Once chosen, CrazyAgent sends these to {host} in the background:"
                        ),
                        room,
                    )
                    .into_iter()
                    .map(Line::raw),
                );
                for line in &option.sends {
                    body.extend(
                        wrap(&format!("- {}", safe(line)), room)
                            .into_iter()
                            .map(Line::raw),
                    );
                }
                (
                    format!("Send search text to {host}?"),
                    format!("Send to {host}"),
                )
            }
            Open::Download { bytes } => {
                body.extend(
                    wrap(
                        &format!(
                            "{}, once. CrazyAgent checks it before use; search uses full text until it is ready.",
                            megabytes(*bytes)
                        ),
                        room,
                    )
                    .into_iter()
                    .map(Line::raw),
                );
                (
                    "Download the built-in model?".to_owned(),
                    "Download".to_owned(),
                )
            }
            Open::Delete { bytes } => {
                body.extend(
                    wrap(
                        &format!(
                            "Frees {}. Search with the built-in model stops until you download it again.",
                            megabytes(*bytes)
                        ),
                        room,
                    )
                    .into_iter()
                    .map(Line::raw),
                );
                (
                    "Delete the built-in model?".to_owned(),
                    "Delete model".to_owned(),
                )
            }
        };
        Some((
            Modal {
                width: MODAL_WIDTH,
                title,
                body,
                top: None,
            },
            vec![hint("y", &confirm), hint("Esc", "Cancel")],
        ))
    }

    fn busy(&self, _ctx: &Ctx) -> bool {
        self.pending.is_some()
    }

    fn place(&self) -> Option<Place> {
        Some(Place::SearchSettings)
    }
}
