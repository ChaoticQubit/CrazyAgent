use crate::pools::MemberKind;
use crate::protocol::CredentialRef;
use crate::tui::client::settings::{SettingsEvent, SettingsJob};
use crate::tui::ctx::{Ctx, Load};
use crate::tui::frame::{Hint, Message, Modal, hint};
use crate::tui::keys::Key;
use crate::tui::screens::Flow;
use crate::tui::screens::settings_accounts::{load_accounts, send, settings_event};
use crate::tui::screens::usage::refusal;
use crate::tui::seam::{Arrival, account_labels};
use crate::tui::text::{styled, wrap};
use crate::tui::theme::Role;
use crate::tui::widgets::buttons;
use crate::usage::spend::{
    AllLimited, AttemptState, LimitedPool, SpendAttempt, SpendConfirm, SpendSettings, SpendView,
};
use ratatui::text::Line;
use std::collections::HashMap;

const READ_SINCE_MS: i64 = 60 * 60 * 1000;
const MODAL_TEXT: usize = 50;
const POOLS_SHOWN_MAX: usize = 8;
const READ_EVERY_MS: i64 = 60 * 1000;
const OPEN_READ_EVERY_MS: i64 = 5 * 1000;

/// The spend part of Settings, Pools in the terminal (DEC-154 slice 3): the two spend
/// settings, the notice for a pool whose every account is at its limit, and "Use a free
/// reset" with its confirm. The same words as the desktop's spend-settings.tsx.
pub(in crate::tui) struct Spend {
    view: Option<SpendView>,
    asking: Option<Ask>,
    read_ms: Option<i64>,
}

struct Ask {
    account: CredentialRef,
    name: String,
    use_focused: bool,
}

impl Default for Spend {
    fn default() -> Self {
        Self::new()
    }
}

/// The words of a Spend confirmation option. Input: the option. Output: the words.
pub(in crate::tui) fn confirm_words(value: SpendConfirm) -> &'static str {
    match value {
        SpendConfirm::ConfirmEach => "Confirm each",
        SpendConfirm::OneClick => "One click",
        SpendConfirm::Off => "Off",
    }
}

/// The words of an all-limited option. Input: the option. Output: the words.
pub(in crate::tui) fn all_limited_words(value: AllLimited) -> &'static str {
    match value {
        AllLimited::Notice => "Notice",
        AllLimited::NoHint => "No hint",
        AllLimited::AutoSpend => "Spend on its own",
    }
}

/// The help line of a Spend confirmation option. Input: the option. Output: the sentence.
pub(in crate::tui) fn confirm_help(value: SpendConfirm) -> &'static str {
    match value {
        SpendConfirm::ConfirmEach => "Each free reset asks you before CrazyAgent uses it.",
        SpendConfirm::OneClick => "A free reset is used as soon as you click.",
        SpendConfirm::Off => {
            "CrazyAgent never uses a free reset. Use them in the provider's own app."
        }
    }
}

/// The help line of the all-limited setting; with spending off, auto-spend says it does
/// nothing. Input: the settings. Output: the sentence.
pub(in crate::tui) fn all_limited_help(settings: SpendSettings) -> &'static str {
    match settings.all_limited {
        AllLimited::AutoSpend if !settings.spends() => {
            "Spending is off, so CrazyAgent never uses a free reset on its own."
        }
        AllLimited::Notice => {
            "When every account in a pool is at its limit, Pools names the accounts that hold a free reset."
        }
        AllLimited::NoHint => "Nothing is shown when every account in a pool is at its limit.",
        AllLimited::AutoSpend => {
            "When every account in a pool is at its limit, CrazyAgent uses one free reset on its own: at most once an hour per pool, once per limit window per account, and only on a fresh count."
        }
    }
}

/// The next Spend confirmation option, wrapping. Input: the option. Output: the next.
pub(in crate::tui) fn next_confirm(value: SpendConfirm) -> SpendConfirm {
    match value {
        SpendConfirm::ConfirmEach => SpendConfirm::OneClick,
        SpendConfirm::OneClick => SpendConfirm::Off,
        SpendConfirm::Off => SpendConfirm::ConfirmEach,
    }
}

/// The next all-limited option, wrapping. Input: the option. Output: the next.
pub(in crate::tui) fn next_all_limited(value: AllLimited) -> AllLimited {
    match value {
        AllLimited::Notice => AllLimited::NoHint,
        AllLimited::NoHint => AllLimited::AutoSpend,
        AllLimited::AutoSpend => AllLimited::Notice,
    }
}

/// One plain sentence for where an account's latest spend stands, the desktop's words.
/// Input: the attempt. Output: the sentence, naming an unknown code.
pub(in crate::tui) fn outcome_words(attempt: &SpendAttempt) -> String {
    match attempt.state {
        AttemptState::Queued => return "Using a free reset now.".into(),
        AttemptState::Sent => return "Sent. Waiting for the provider's answer.".into(),
        AttemptState::Settled => {}
    }
    let code = attempt.outcome.as_deref().unwrap_or("outcome_unknown");
    let words = match code {
        "reset" => "Free reset used. The limits update within a minute.",
        "nothing_to_reset" => {
            "Nothing to reset: no limit on this account is full now. The free reset is kept."
        }
        "no_credit" => "This account has no free reset left.",
        "already_redeemed" => "This free reset was already used.",
        "outcome_unknown" => {
            "The provider didn't say whether the reset was used. Check the limits after the next refresh."
        }
        "usage_spend_off" => "Spending free resets is off. Turn it on in Settings, Pools.",
        "usage_spend_unconfirmed" => {
            "Spend confirmation now asks first. Use a free reset again and confirm it."
        }
        "usage_spend_no_account" => "This account is no longer in your settings.",
        "usage_spend_not_in_app" => "This account can't spend a free reset from CrazyAgent.",
        "usage_spend_not_signed_in" => {
            "This account wasn't signed in or was busy. Sign in again in Accounts, then try again."
        }
        "usage_spend_unavailable" => {
            "Spending this free reset from the app isn't available yet. Use the provider's own app."
        }
        "usage_spend_failed" => "The free reset couldn't be sent. Try again later.",
        "usage_spend_refused" => "The provider turned this free reset down. None was used.",
        "usage_sign_in_lapsed" => {
            "The provider no longer accepts this sign-in. Sign in again in Accounts."
        }
        "usage_sign_in_rejected" => {
            "This account's sign-in was rejected. Sign in again in Accounts."
        }
        "usage_rate_limited" => {
            "The provider asked CrazyAgent to wait. Try again in a few minutes."
        }
        "usage_request_invalid" => "CrazyAgent couldn't ask the provider to use a free reset.",
        "usage_reply_invalid" => {
            "The provider's answer wasn't one CrazyAgent can read. Check the limits after the next refresh."
        }
        other => return format!("Something went wrong. Try again. ({other})"),
    };
    words.into()
}

/// The notice line of a pool whose every account is at its limit, the desktop's words.
/// Inputs: the pool and the account names. Output: the sentence.
pub(in crate::tui) fn limited_pool_line(
    pool: &LimitedPool,
    names: &HashMap<String, String>,
) -> String {
    let place = match &pool.name {
        Some(name) => format!("Every account in {name} is at its limit."),
        None => "Every account in a pool is at its limit.".into(),
    };
    if pool.with_resets.is_empty() {
        return format!("{place} None of them has a free reset.");
    }
    let named: Vec<String> = pool
        .with_resets
        .iter()
        .map(|account| {
            names
                .get(account.as_str())
                .cloned()
                .unwrap_or_else(|| "an account".into())
        })
        .collect();
    let list = match named.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
        _ => named.join(""),
    };
    format!("{place} Free resets: {list}.")
}

impl Spend {
    /// The spend part with nothing read yet. Input: none. Output: the part.
    pub(in crate::tui) fn new() -> Self {
        Self {
            view: None,
            asking: None,
            read_ms: None,
        }
    }

    /// Reads the spend view with the usage report when it is due, and the account names
    /// once. The shell shows Pools again every 5 s, so a read is due a minute after the last
    /// one, or 5 s after it while a spend is still out. Inputs: the context and the time.
    /// Output: none.
    pub(in crate::tui) fn shown(&mut self, ctx: &mut Ctx, now_ms: i64) {
        if matches!(ctx.store.accounts, Load::NotReady | Load::Idle) {
            load_accounts(ctx);
        }
        let every = if self.open() {
            OPEN_READ_EVERY_MS
        } else {
            READ_EVERY_MS
        };
        if self
            .read_ms
            .is_some_and(|at| at <= now_ms && now_ms - at < every)
        {
            return;
        }
        self.reload(ctx, now_ms);
    }

    fn reload(&mut self, ctx: &mut Ctx, now_ms: i64) {
        send(
            ctx,
            SettingsJob::Usage {
                since_ms: Some(now_ms - READ_SINCE_MS),
                offset_minutes: 0,
            },
        );
        self.read_ms = Some(now_ms);
    }

    fn open(&self) -> bool {
        self.view.as_ref().is_some_and(|view| {
            view.attempts
                .iter()
                .any(|attempt| attempt.state != AttemptState::Settled)
        })
    }

    fn names(ctx: &Ctx) -> HashMap<String, String> {
        ctx.store
            .accounts
            .ready()
            .map(|accounts| account_labels(accounts))
            .unwrap_or_default()
    }

    fn in_app(account: &CredentialRef, ctx: &Ctx) -> bool {
        ctx.store.pools.ready().is_some_and(|pools| {
            pools.iter().any(|pool| {
                pool.members
                    .iter()
                    .any(|member| &member.id == account && member.kind == MemberKind::Chatgpt)
            })
        })
    }

    fn attempt(&self, account: &CredentialRef) -> Option<&SpendAttempt> {
        self.view
            .as_ref()?
            .attempts
            .iter()
            .find(|attempt| &attempt.account == account)
    }

    /// The account "Use a free reset" acts on: the first account named in a limited pool
    /// notice that the app can spend for and whose latest spend is not still out. None when
    /// spending is off. Input: the context. Output: the account.
    pub(in crate::tui) fn target(&self, ctx: &Ctx) -> Option<CredentialRef> {
        let view = self.view.as_ref()?;
        if !view.settings.spends() {
            return None;
        }
        view.limited_pools
            .iter()
            .flat_map(|pool| pool.with_resets.iter())
            .find(|account| {
                Self::in_app(account, ctx)
                    && self
                        .attempt(account)
                        .is_none_or(|attempt| attempt.state == AttemptState::Settled)
            })
            .cloned()
    }

    /// The keys this part adds to Pools once the spend view is read. Input: the context.
    /// Output: the hints.
    pub(in crate::tui) fn hints(&self, ctx: &Ctx) -> Vec<Hint> {
        if self.view.is_none() {
            return Vec::new();
        }
        let mut hints = vec![hint("c", "Spend confirmation"), hint("l", "When limited")];
        if self.target(ctx).is_some() {
            hints.push(hint("u", "Use a free reset"));
        }
        hints
    }

    /// Whether the confirm is open, so Pools hands it every key. Output: the answer.
    pub(in crate::tui) fn asking(&self) -> bool {
        self.asking.is_some()
    }

    /// Handles this part's keys: c and l step a setting and store it, u uses a free reset
    /// (asking first under Confirm each), and the confirm's own keys. Inputs: the key and the
    /// context. Output: Handled, or Ignored for a key this part does not take.
    pub(in crate::tui) fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        if let Some(ask) = &mut self.asking {
            match key {
                Key::Left | Key::Right => ask.use_focused = !ask.use_focused,
                Key::Enter if ask.use_focused => {
                    let account = ask.account.clone();
                    self.asking = None;
                    send(
                        ctx,
                        SettingsJob::Spend {
                            account,
                            confirmed: true,
                        },
                    );
                }
                Key::Enter | Key::Esc => self.asking = None,
                _ => {}
            }
            return Flow::Handled;
        }
        let Some(view) = &self.view else {
            return Flow::Ignored;
        };
        let settings = view.settings;
        match key {
            Key::Char('c') => send(
                ctx,
                SettingsJob::SetSpend(SpendSettings {
                    confirm: next_confirm(settings.confirm),
                    ..settings
                }),
            ),
            Key::Char('l') => send(
                ctx,
                SettingsJob::SetSpend(SpendSettings {
                    all_limited: next_all_limited(settings.all_limited),
                    ..settings
                }),
            ),
            Key::Char('u') => {
                let Some(account) = self.target(ctx) else {
                    return Flow::Ignored;
                };
                if settings.confirm == SpendConfirm::ConfirmEach {
                    let name = Self::names(ctx)
                        .remove(account.as_str())
                        .unwrap_or_else(|| "this account".into());
                    self.asking = Some(Ask {
                        account,
                        name,
                        use_focused: false,
                    });
                } else {
                    send(
                        ctx,
                        SettingsJob::Spend {
                            account,
                            confirmed: false,
                        },
                    );
                }
            }
            _ => return Flow::Ignored,
        }
        Flow::Handled
    }

    /// Takes the usage report's spend view and the answers to a setting change or a spend.
    /// Inputs: the arrival and the context. Output: none.
    pub(in crate::tui) fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) {
        match settings_event(arrival) {
            Some(SettingsEvent::Usage(Ok(report))) => self.view = Some(report.spend.clone()),
            Some(SettingsEvent::SpendSet(Ok(()))) => self.reload(ctx, crate::tui::clock::now_ms()),
            Some(SettingsEvent::SpendSet(Err(code))) => ctx.say(Message::error(
                "Could not change the setting.",
                refusal(code),
            )),
            Some(SettingsEvent::Spent(Ok(()))) => {
                ctx.say(Message::success(
                    "Using a free reset.",
                    "Its outcome shows under Free resets.",
                ));
                self.reload(ctx, crate::tui::clock::now_ms());
            }
            Some(SettingsEvent::Spent(Err(code))) => {
                ctx.say(Message::error(
                    "Could not use the free reset.",
                    refusal(code),
                ));
                self.reload(ctx, crate::tui::clock::now_ms());
            }
            _ => {}
        }
    }

    /// The confirm of a spend under Confirm each. Input: the context. Output: the modal and
    /// its hints, or none.
    pub(in crate::tui) fn modal(&self, ctx: &Ctx) -> Option<(Modal, Vec<Hint>)> {
        let ask = self.asking.as_ref()?;
        let mut body = vec![
            Line::raw(format!("Use a free reset on {}?", ask.name)),
            Line::default(),
        ];
        body.extend(
            wrap(
                "CrazyAgent asks the provider to use one of this account's free resets now, so its limits start over. It uses the provider's private call that may break. This can't be undone.",
                MODAL_TEXT,
            )
            .into_iter()
            .map(Line::raw),
        );
        body.push(Line::default());
        body.push(buttons(
            &["Cancel", "Use free reset"],
            Some(usize::from(ask.use_focused)),
            &ctx.theme,
        ));
        let arrows = format!("{}{}", ctx.glyphs.left, ctx.glyphs.right);
        Some((
            Modal {
                width: 54,
                title: "Use a free reset".into(),
                body,
                top: None,
            },
            vec![
                hint(&arrows, "Choose"),
                hint("Enter", "Confirm"),
                hint("Esc", "Cancel"),
            ],
        ))
    }

    /// The Free resets block under the pools once the spend view is read: both settings with
    /// their help, the limited pool notices and the latest spend of each account they name.
    /// Inputs: the context and the width. Output: the lines, none before the first read.
    pub(in crate::tui) fn lines(&self, ctx: &Ctx, limit: usize) -> Vec<Line<'static>> {
        let Some(view) = &self.view else {
            return Vec::new();
        };
        let settings = view.settings;
        let theme = &ctx.theme;
        let mut lines = vec![Line::default(), styled("Free resets", Role::Accent, theme)];
        let say = |text: String, lines: &mut Vec<Line<'static>>| {
            lines.extend(wrap(&text, limit.max(8)).into_iter().map(Line::raw));
        };
        say(
            format!(
                "Spend confirmation (c): {}. {}",
                confirm_words(settings.confirm),
                confirm_help(settings.confirm)
            ),
            &mut lines,
        );
        say(
            format!(
                "When every pool account is limited (l): {}. {}",
                all_limited_words(settings.all_limited),
                all_limited_help(settings)
            ),
            &mut lines,
        );
        let names = Self::names(ctx);
        for pool in view.limited_pools.iter().take(POOLS_SHOWN_MAX) {
            say(limited_pool_line(pool, &names), &mut lines);
            for account in &pool.with_resets {
                if let Some(attempt) = self.attempt(account) {
                    let name = names
                        .get(account.as_str())
                        .cloned()
                        .unwrap_or_else(|| "An account".into());
                    say(format!("{name}: {}", outcome_words(attempt)), &mut lines);
                }
            }
        }
        if let Some(account) = self.target(ctx) {
            let name = names
                .get(account.as_str())
                .cloned()
                .unwrap_or_else(|| "an account".into());
            say(
                format!("Press u to use a free reset on {name}."),
                &mut lines,
            );
        }
        lines
    }
}

#[cfg(all(test, unix))]
mod tests;
