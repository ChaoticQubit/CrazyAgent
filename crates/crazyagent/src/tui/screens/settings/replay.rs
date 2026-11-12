use crate::learning::MarkStep;
use crate::tui::client::OwnerEvent;
use crate::tui::client::learning::LearningQuery;
use crate::tui::ctx::{Ctx, Load};
use crate::tui::frame::{Hint, Message, hint};
use crate::tui::keys::Key;
use crate::tui::screens::intake::not_ready;
use crate::tui::screens::settings_accounts::plain;
use crate::tui::screens::{Flow, Place};
use crate::tui::seam::{Arrival, Request};
use crate::tui::text::{join, styled, wrap};
use crate::tui::theme::Role;
use ratatui::text::Line;

const TITLE: &str = "Learn CrazyAgent";
const BODY: &str =
    "Bring back the guide on Home, or walk through first-run setup again with your saved answers.";
const GUIDE: &str = "Show the learning guide again";
const FIRST_RUN: &str = "Run first-run setup again";
const GUIDE_KEY: &str = "Show the guide again";
const FIRST_RUN_KEY: &str = "First-run setup again";
const GUIDE_FAILED: &str = "Couldn't show the guide again.";
const FIRST_RUN_FAILED: &str = "Couldn't run first-run setup again.";
const NOT_READY: &str = "The learning guide isn't ready in this build.";

/// One Profile replay entry (DEC-153 slice 4): the learning guide on Home again, or
/// first run again with the saved profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Entry {
    Guide,
    FirstRun,
}

/// The Learn CrazyAgent entries on Settings, Profile, the TUI twin of the Profile dialog's
/// section. Input: g and f while no field types, and the answers to its own clears.
/// Output: each entry clears the skipped mark first, then opens Home with the guide or
/// first run; a refused clear stays on the page with its reason.
#[derive(Debug, Default)]
pub struct Replay {
    sent: Option<(Entry, u64)>,
}

impl Replay {
    /// The two entries for the Keys overlay, in words that fit its column.
    /// Input: none. Output: g and f.
    pub fn keys() -> [Hint; 2] {
        [hint("g", GUIDE_KEY), hint("f", FIRST_RUN_KEY)]
    }

    /// Asks for the learning read when none is held, so the section can show.
    /// Input: the context. Output: none; the read is queued on the context.
    pub fn shown(ctx: &mut Ctx) {
        if !matches!(ctx.store.learning, Load::Idle | Load::Failed(_)) {
            return;
        }
        if matches!(ctx.store.learning, Load::Idle) {
            ctx.store.learning = Load::Loading;
        }
        ctx.request(Request::Learning(Box::new(LearningQuery::Progress)));
    }

    /// Takes g or f: sends the clear of the skipped mark and waits for its answer; a
    /// second press while one is out does nothing. Input: the key, passed only while no
    /// field types, and the context. Output: the flow, None for any other key.
    pub fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Option<Flow> {
        let entry = match key {
            Key::Char('g') => Entry::Guide,
            Key::Char('f') => Entry::FirstRun,
            _ => return None,
        };
        if self.sent.is_none() {
            let generation = ctx.next_generation();
            ctx.request(Request::Learning(Box::new(LearningQuery::Mark {
                step: MarkStep::Skipped,
                on: false,
                generation,
            })));
            self.sent = Some((entry, generation));
        }
        Some(Flow::Handled)
    }

    /// Takes the answer to its own clear: Home or first run once it stands, first run
    /// too on an engine without learning marks, else the reason on this page.
    /// Input: the arrival and the context. Output: the flow, None for any other arrival.
    pub fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Option<Flow> {
        let Arrival::Owner(owner) = arrival else {
            return None;
        };
        let OwnerEvent::Learning(event) = owner.as_ref() else {
            return None;
        };
        let LearningQuery::Mark { generation, .. } = event.query else {
            return None;
        };
        let (entry, sent) = self.sent?;
        if generation != sent {
            return None;
        }
        self.sent = None;
        let failed = |lead: &str, code: &str, ctx: &mut Ctx| {
            ctx.say(Message::error(lead, plain(code)));
            Flow::Handled
        };
        Some(match (entry, &event.result) {
            (Entry::Guide, Ok(_)) => Flow::Go(Place::Home),
            (Entry::FirstRun, Ok(_)) => Flow::Go(Place::Welcome),
            (Entry::FirstRun, Err(code)) if not_ready(code) => Flow::Go(Place::Welcome),
            (Entry::Guide, Err(code)) if not_ready(code) => {
                ctx.say(Message::error(NOT_READY, ""));
                Flow::Handled
            }
            (Entry::Guide, Err(code)) => failed(GUIDE_FAILED, code.as_str(), ctx),
            (Entry::FirstRun, Err(code)) => failed(FIRST_RUN_FAILED, code.as_str(), ctx),
        })
    }

    /// The section under the profile form once the engine's learning read is in: the
    /// heading, one sentence and both entries with their keys, the one out shown as
    /// loading. Input: the width and the context. Output: the lines, none before the read.
    pub fn lines(&self, limit: usize, ctx: &Ctx) -> Vec<Line<'static>> {
        if ctx.store.learning.ready().is_none() {
            return Vec::new();
        }
        let theme = &ctx.theme;
        let mut lines = vec![Line::default(), styled(TITLE, Role::Heading, theme)];
        lines.extend(
            wrap(BODY, limit)
                .into_iter()
                .map(|line| styled(line, Role::Muted, theme)),
        );
        for (entry, key, words) in [
            (Entry::Guide, "g", GUIDE),
            (Entry::FirstRun, "f", FIRST_RUN),
        ] {
            lines.push(match self.sent {
                Some((sent, _)) if sent == entry => ctx.loading(words),
                _ => join([
                    styled(key, Role::Accent, theme),
                    Line::raw(format!("  {words}")),
                ]),
            });
        }
        lines
    }
}

#[cfg(test)]
mod tests;
