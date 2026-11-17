use super::{draw_page, step};
use crate::restore::RestoreState;
use crate::tui::client::app_setup::{AppSetupEvent, AppSetupJob};
use crate::tui::client::settings::{SettingsEvent, SettingsJob};
use crate::tui::clock::{expiry, local_offset, now_ms};
use crate::tui::ctx::Ctx;
use crate::tui::frame::{Hint, Message, Modal, hint, sections_hint};
use crate::tui::keys::Key;
use crate::tui::screens::settings_accounts::{send, settings_event};
use crate::tui::screens::{Flow, Place, Screen, SettingsSection};
use crate::tui::seam::Arrival;
use crate::tui::text::{styled, wrap};
use crate::tui::theme::Role;
use crate::tui::widgets::buttons;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::Line;
use std::path::Path;
use std::time::SystemTime;

/// The three things a restore does, which the confirm says before anything runs, in the
/// desktop's words.
pub const RESTORE_CONFIRM: [&str; 3] = [
    "Everything written since the update to 2.5 is replaced. CrazyAgent keeps it in a dated copy in its backups folder.",
    "Install CrazyAgent 2.4.2 again to use the 2.4 data. The next start of 2.5 updates it again.",
    "Grants you revoked and plugins you stopped trusting after the update come back.",
];
/// The page's line when there is no pinned 2.4 backup to bring back.
pub const RESTORE_NONE: &str =
    "There's no 2.4 backup to bring back. CrazyAgent keeps it for 30 days after the update to 2.5.";
/// The page's line while a restore runs.
pub const RESTORE_RUNNING: &str =
    "Your 2.4 data is being brought back. CrazyAgent's engine stays stopped until it's done.";

const RESTORING: &str = "CrazyAgent is bringing back your 2.4 data, so its engine stays stopped. \
    Wait a minute, then open the TUI again.";
const RESTORED: &str = "Your 2.4 data is back. Install CrazyAgent 2.4.2 to use it. \
    To go back to 2.5 instead, run crazyagent service start; that updates the data again.";
const AGENTS_RUNNING: &str =
    "Agents are running. Stop every agent and research run, then bring back your 2.4 data.";
const BUSY: &str = "Something else had CrazyAgent's data open, so your 2.4 data wasn't brought \
    back. Close other CrazyAgent windows, then try again.";
const FAILED: &str =
    "Your 2.4 data wasn't brought back, and CrazyAgent went on with your 2.5 data. Try again.";
const BUTTON: &str = "Bring back 2.4 data";
const TEXT_WIDTH: usize = 72;
const MODAL_TEXT: usize = 54;

/// The owner's sentence for a code a restore, or a start it holds, ends with. Any other code
/// reads as a restore that did not run. Input: the plain code. Output: the sentence.
pub fn plain(code: &str) -> &'static str {
    let said = match code {
        "engine_restoring" => RESTORING,
        "engine_restored" => RESTORED,
        "storage_restore_unavailable" => RESTORE_NONE,
        "storage_restore_agents_running" => AGENTS_RUNNING,
        "storage_restore_busy" => BUSY,
        _ => FAILED,
    };
    assert!(said.ends_with('.'));
    said
}

/// The problem a failed connect to the TUI's own engine shows: the restore that holds the
/// engine, as the desktop's connect names it (L5 F1), else the problem unchanged. Inputs:
/// the problem, the engine root and whether its login service still runs a process.
/// Output: `engine_restoring`, `engine_restored` or the problem.
pub fn held(problem: String, root: &Path, running: bool) -> String {
    assert!(!problem.is_empty(), "a failed connect names its problem");
    let Some(code) = crate::restore::blocks_start(root, running) else {
        return problem;
    };
    assert!(holds(code));
    code.to_owned()
}

/// Whether a code is one a restore holds the engine with, which the TUI shows in the
/// restore's own sentence. Input: the code. Output: the answer.
pub fn holds(code: &str) -> bool {
    code == crate::restore::ENGINE_RESTORING || code == crate::restore::ENGINE_RESTORED
}

/// What the page shows: whether the button is offered, the line under the tabs and the last
/// restore's failure, if any, as the desktop's Restore 2.4 shows them.
#[derive(Debug, PartialEq, Eq)]
pub struct Offer {
    pub offer: bool,
    pub line: String,
    pub failure: Option<String>,
}

/// The page for the pinned backup's end and the last restore. Inputs: the end in epoch
/// milliseconds, the record's state, now and the UTC offset. Output: what the page shows; a
/// restore is offered only while the backup lasts and none runs or brought the data back.
pub fn offer(until_ms: Option<i64>, last: Option<&RestoreState>, now: i64, offset: i64) -> Offer {
    let failure = match last {
        Some(RestoreState::Failed { code }) => Some(plain(code).to_owned()),
        _ => None,
    };
    let (offer, line) = match (last, until_ms) {
        (Some(RestoreState::Pending), _) => (false, RESTORE_RUNNING.to_owned()),
        (Some(RestoreState::Restored { .. }), _) => (false, RESTORED.to_owned()),
        (_, None) => (false, RESTORE_NONE.to_owned()),
        (_, Some(until)) => (
            true,
            format!(
                "You can bring back your 2.4 data until {}.",
                expiry(until, now, offset)
            ),
        ),
    };
    let shown = Offer {
        offer,
        line,
        failure,
    };
    assert!(!shown.offer || until_ms.is_some());
    assert!(shown.failure.is_none() || matches!(last, Some(RestoreState::Failed { .. })));
    shown
}

/// Whether Settings shows Restore 2.4 for the engine at `root`: it has a pinned 2.4 backup or
/// a restore record. Input: the engine root. Output: the answer.
pub fn offered_at(root: &Path) -> bool {
    assert!(root.is_absolute());
    crate::restore::restorable_until(root).is_some() || crate::restore::state(root).is_some()
}

/// A time as epoch milliseconds. Input: the time. Output: the count, or none before 1970.
fn epoch_ms(time: SystemTime) -> Option<i64> {
    let ms = time
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()?
        .as_millis();
    i64::try_from(ms).ok().filter(|ms| *ms > 0)
}

/// Reads the page from the default engine's backups folder and restore record, without the
/// engine. Input: the context. Output: what the page shows; nothing to restore without a
/// default engine.
fn read(ctx: &Ctx) -> Offer {
    let root = ctx.engine_root.as_deref();
    let until_ms = root
        .and_then(crate::restore::restorable_until)
        .and_then(epoch_ms);
    let last = root.and_then(crate::restore::state);
    offer(until_ms, last.as_ref(), now_ms(), local_offset())
}

/// Settings, Restore 2.4 (L5): the pinned 2.4 backup's end, the last restore, and the button
/// that asks first with the three things a restore does, then sends it once. Input: keys and
/// the restore's reply. Output: the page; an admitted restore stops the engine.
pub struct RestorePage {
    shown: Offer,
    asking: bool,
    confirm_focused: bool,
    sending: bool,
}

impl RestorePage {
    /// The page read from the default engine's folder. Input: the context. Output: the screen.
    pub fn new(ctx: &Ctx) -> Self {
        Self::with(read(ctx))
    }

    /// The page showing `shown`. Input: what it shows. Output: the screen, asking nothing.
    pub fn with(shown: Offer) -> Self {
        Self {
            shown,
            asking: false,
            confirm_focused: false,
            sending: false,
        }
    }

    fn confirm_key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        assert!(self.asking);
        assert!(!self.sending);
        match key {
            Key::Esc => self.asking = false,
            Key::Left | Key::Right | Key::Tab | Key::BackTab => {
                self.confirm_focused = !self.confirm_focused;
            }
            Key::Enter if self.confirm_focused => {
                self.asking = false;
                self.sending = true;
                let generation = ctx.next_generation();
                send(
                    ctx,
                    SettingsJob::AppSetup(AppSetupJob::Restore { generation }),
                );
            }
            Key::Enter => self.asking = false,
            _ => {}
        }
        Flow::Handled
    }

    fn answered(&mut self, result: &Result<(), String>, ctx: &mut Ctx) {
        assert!(self.sending);
        self.sending = false;
        match result {
            Ok(()) => {
                self.shown = Offer {
                    offer: false,
                    line: RESTORE_RUNNING.to_owned(),
                    failure: None,
                };
                ctx.say(Message::success(
                    "Bringing back your 2.4 data.",
                    "Install CrazyAgent 2.4.2 to use it.",
                ));
            }
            Err(code) => ctx.say(Message::warning(plain(code), "")),
        }
        assert!(!self.sending);
    }
}

impl Screen for RestorePage {
    fn crumbs(&self) -> Vec<String> {
        vec!["Settings".into(), SettingsSection::Restore.label().into()]
    }

    fn hints(&self, ctx: &Ctx) -> Vec<Hint> {
        let mut hints = Vec::with_capacity(3);
        if self.shown.offer && !self.sending {
            hints.push(hint("Enter", BUTTON));
        }
        hints.push(sections_hint(ctx.glyphs));
        hints.push(hint("Esc", "Home"));
        hints
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        if self.asking {
            return self.confirm_key(key, ctx);
        }
        match key {
            Key::Enter if self.shown.offer && !self.sending => {
                self.asking = true;
                self.confirm_focused = false;
                Flow::Handled
            }
            _ => Flow::Ignored,
        }
    }

    fn step(&mut self, forward: bool, ctx: &mut Ctx) -> Flow {
        step(SettingsSection::Restore, forward, ctx)
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        if matches!(arrival, Arrival::Disconnected(_)) && self.sending {
            self.sending = false;
            self.shown = read(ctx);
            return Flow::Handled;
        }
        if let Some(SettingsEvent::AppSetup(AppSetupEvent::Restored(result))) =
            settings_event(arrival)
            && self.sending
        {
            self.answered(result, ctx);
        }
        Flow::Handled
    }

    fn shown(&mut self, ctx: &mut Ctx) {
        if !self.asking && !self.sending {
            self.shown = read(ctx);
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let width = usize::from(ctx.width)
            .saturating_sub(4)
            .clamp(20, TEXT_WIDTH);
        let mut content = vec![
            styled(
                "Bring back your data as it was before the update to 2.5.",
                Role::Muted,
                &ctx.theme,
            ),
            Line::default(),
        ];
        content.extend(wrap(&self.shown.line, width).into_iter().map(Line::raw));
        if let Some(failure) = &self.shown.failure {
            content.push(Line::default());
            let said = format!("The last restore didn't run. {failure}");
            content.extend(
                wrap(&said, width)
                    .into_iter()
                    .map(|line| styled(line, Role::Warning, &ctx.theme)),
            );
        }
        if self.sending {
            content.push(Line::default());
            let asking = format!("Asking the engine{}", ctx.glyphs.ellipsis);
            content.push(styled(asking, Role::Muted, &ctx.theme));
        } else if self.shown.offer {
            content.push(Line::default());
            content.push(buttons(&[BUTTON], Some(0), &ctx.theme));
        }
        draw_page(frame, area, SettingsSection::Restore, content, None, ctx)
    }

    fn modal(&self, ctx: &Ctx) -> Option<(Modal, Vec<Hint>)> {
        if !self.asking {
            return None;
        }
        let mut body = vec![Line::raw("Bring back your 2.4 data?"), Line::default()];
        for said in RESTORE_CONFIRM {
            let bullet = format!("{} {said}", ctx.glyphs.dot);
            body.extend(wrap(&bullet, MODAL_TEXT).into_iter().map(Line::raw));
        }
        body.push(Line::default());
        body.push(buttons(
            &["Cancel", BUTTON],
            Some(usize::from(self.confirm_focused)),
            &ctx.theme,
        ));
        let arrows = format!("{}{}", ctx.glyphs.left, ctx.glyphs.right);
        let modal = Modal {
            width: 60,
            title: SettingsSection::Restore.label().into(),
            body,
            top: None,
        };
        let keys = vec![
            hint(&arrows, "Choose"),
            hint("Enter", "Confirm"),
            hint("Esc", "Cancel"),
        ];
        Some((modal, keys))
    }

    fn place(&self) -> Option<Place> {
        Some(Place::Settings(SettingsSection::Restore))
    }
}

#[cfg(all(test, unix))]
mod tests;
