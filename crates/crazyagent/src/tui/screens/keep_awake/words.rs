use crate::keep_awake::{
    BatteryFloor, Cap, EndReason, HeatEnd, KeepAwakeSettings, KeepAwakeView, LidRule, LidSupport,
    MINUTES_MAX, Mode, Reason, Want,
};
use crate::paths::Platform;

/// The longest Keep awake label the header shows, in characters.
pub(in crate::tui) const LABEL_CHARS_MAX: usize = 34;

const COUNT_MAX: u32 = 999;
const MINUTE_MS: i64 = 60_000;
const MINUTES_SHOWN_MAX: i64 = 999 * 60 + 59;
const REASON_CHARS_MAX: usize = 200;
const PERCENT_MAX: u8 = 100;
const CAP_HOURS_MAX: u8 = 24;

/// How the header draws a state: a hold, a hold a closed lid would end, an end by
/// itself, or not holding (DEC-167 addendum 5, the desktop's four tones).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::tui) enum Tone {
    Hold,
    Limit,
    Ended,
    Plain,
}

/// Which place a label is written for: the desktop control's words on the Keep awake
/// screen, the same words in the header with the four resting ones led by the feature's
/// name, or the short form the header falls back to in a narrow terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::tui) enum Form {
    Words,
    Header,
    Short,
}

fn holding(view: &KeepAwakeView) -> bool {
    view.state.want != Want::Nothing
}

/// The time until a hold ends, rounded up to the minute and never zero. Inputs: the end
/// and now in milliseconds, and the form. Output: "2h 14m", "3h" or "59m"; the short form
/// drops the space.
pub(in crate::tui) fn time_left(ends_at_ms: Option<i64>, now_ms: i64, form: Form) -> String {
    let left = ends_at_ms.map_or(1, |end| {
        end.saturating_sub(now_ms)
            .saturating_add(MINUTE_MS - 1)
            .div_euclid(MINUTE_MS)
    });
    let minutes = left.clamp(1, MINUTES_SHOWN_MAX);
    assert!((1..=MINUTES_SHOWN_MAX).contains(&minutes));
    let (hours, rest) = (minutes / 60, minutes % 60);
    match (hours, rest, form) {
        (0, rest, _) => format!("{rest}m"),
        (hours, 0, _) => format!("{hours}h"),
        (hours, rest, Form::Short) => format!("{hours}h{rest}m"),
        (hours, rest, Form::Words | Form::Header) => format!("{hours}h {rest}m"),
    }
}

/// The label of a hold that ended by itself, or none when nothing ended or the owner
/// ended it (the timer ran out, End, or mode Off). Inputs: the view and the form. Output:
/// the label or none.
fn ended(view: &KeepAwakeView, form: Form) -> Option<String> {
    let long = form != Form::Short;
    Some(match view.state.ended? {
        EndReason::Battery { pct } if long => {
            format!("Ended: battery {}%", pct.min(PERCENT_MAX))
        }
        EndReason::Battery { pct } => format!("Ended: batt {}%", pct.min(PERCENT_MAX)),
        EndReason::BatteryUnread if long => "Ended: battery unreadable".into(),
        EndReason::BatteryUnread => "Ended: batt ?".into(),
        EndReason::Heat => "Ended: heat".into(),
        EndReason::Cap { hours } if long => {
            format!("Ended: {} h limit", hours.clamp(1, CAP_HOURS_MAX))
        }
        EndReason::Cap { .. } => "Ended: limit".into(),
        EndReason::HoldLost if long => "Ended: hold lost".into(),
        EndReason::HoldLost => "Ended: lost".into(),
        EndReason::Manual | EndReason::Off => return None,
    })
}

/// The label of a running hold that a closed lid would end, or none. Inputs: the view and
/// the form. Output: the label or none.
fn lid_limit(view: &KeepAwakeView, form: Form) -> Option<&'static str> {
    if !holding(view) {
        return None;
    }
    let short = form == Form::Short;
    match &view.lid_support {
        LidSupport::IdleOnlyOnBattery if short => Some("Idle only"),
        LidSupport::IdleOnlyOnBattery => Some("Idle only: on battery"),
        LidSupport::NeedsMacos13 if short => Some("Idle only (macOS 13)"),
        LidSupport::NeedsMacos13 => Some("Lid-closed hold needs macOS 13"),
        LidSupport::NeedsApproval => Some("Needs approval"),
        _ => None,
    }
}

/// The label when nothing is held and nothing ended by itself: mode Off, Manual without a
/// timer, Auto standing down, or Auto at rest. The header's words lead with "Keep awake:"
/// since nothing else in them names the feature. Inputs: the view and the form. Output:
/// the label.
fn resting(view: &KeepAwakeView, form: Form) -> &'static str {
    match (view.settings.mode, view.work > 0, form) {
        (Mode::Off, _, Form::Words) => "Off",
        (Mode::Off, _, Form::Header) => "Keep awake: off",
        (Mode::Off, _, Form::Short) => "Awake off",
        (Mode::Manual, _, Form::Words) => "Manual, no timer",
        (Mode::Manual, _, Form::Header) => "Keep awake: manual, no timer",
        (Mode::Manual, _, Form::Short) => "Manual",
        (Mode::Auto, true, Form::Words) => "Paused until work ends",
        (Mode::Auto, true, Form::Header) => "Keep awake: paused until work ends",
        (Mode::Auto, true, Form::Short) => "Paused",
        (Mode::Auto, false, Form::Words) => "Auto, nothing running",
        (Mode::Auto, false, Form::Header) => "Keep awake: auto, nothing running",
        (Mode::Auto, false, Form::Short) => "Auto",
    }
}

/// Says how many agents run, with counts above 999 as "999+" so the label keeps its
/// length. Inputs: the count and the form. Output: "Awake while 3 agents run" or
/// "Awake: 3 agents".
fn working(work: u32, form: Form) -> String {
    let count = if work > COUNT_MAX {
        format!("{COUNT_MAX}+")
    } else {
        work.to_string()
    };
    match (form == Form::Short, work == 1) {
        (false, true) => format!("Awake while {count} agent runs"),
        (false, false) => format!("Awake while {count} agents run"),
        (true, true) => format!("Awake: {count} agent"),
        (true, false) => format!("Awake: {count} agents"),
    }
}

/// The label for one view, in the desktop control's words (DEC-167 addenda 5 to
/// 7). An end by itself wins, then not holding, then while holding a lid limit, the timer,
/// the grace, a held lid and the working agents. Inputs: the view, now in milliseconds and
/// the form. Output: the label, 1 to `LABEL_CHARS_MAX` characters.
pub(in crate::tui) fn label(view: &KeepAwakeView, now_ms: i64, form: Form) -> String {
    let label = pick(view, now_ms, form);
    let length = label.chars().count();
    assert!((1..=LABEL_CHARS_MAX).contains(&length), "{label}");
    label
}

fn pick(view: &KeepAwakeView, now_ms: i64, form: Form) -> String {
    if let Some(ended) = ended(view, form) {
        return ended;
    }
    if !holding(view) || view.settings.mode == Mode::Off {
        return resting(view, form).into();
    }
    if let Some(limit) = lid_limit(view, form) {
        return limit.into();
    }
    let left = time_left(view.state.ends_at_ms, now_ms, form);
    match (view.state.reason, form == Form::Short) {
        (Reason::Timer, false) => format!("Awake, {left} left"),
        (Reason::Timer, true) => format!("Awake {left}"),
        (Reason::Grace, false) => format!("Awake {left} more, work done"),
        (Reason::Grace, true) => format!("Awake {left} more"),
        _ if matches!(view.state.want, Want::IdleAndLid { .. }) => "Awake, lid OK".into(),
        _ => working(view.work, form),
    }
}

/// The tone the header draws one view in. Input: the view. Output: the tone.
pub(in crate::tui) fn tone(view: &KeepAwakeView) -> Tone {
    if ended(view, Form::Words).is_some() {
        return Tone::Ended;
    }
    if !holding(view) || view.settings.mode == Mode::Off {
        return Tone::Plain;
    }
    match lid_limit(view, Form::Words) {
        Some(_) => Tone::Limit,
        None => Tone::Hold,
    }
}

/// What ends a hold by itself, from the battery, heat and cap settings; a part set to Off,
/// Never or No limit drops out. Input: the settings. Output: such as "At 10% battery,
/// critical heat, or 24 h", or "Never".
pub(in crate::tui) fn ends_by_itself(settings: &KeepAwakeSettings) -> String {
    let battery = match settings.battery_floor {
        BatteryFloor::Off => None,
        BatteryFloor::Pct5 => Some("5% battery"),
        BatteryFloor::Pct10 => Some("10% battery"),
        BatteryFloor::Pct20 => Some("20% battery"),
    };
    let heat = match settings.heat_end {
        HeatEnd::Critical => Some("critical heat"),
        HeatEnd::Serious => Some("serious heat"),
        HeatEnd::Never => None,
    };
    let cap = match settings.cap {
        Cap::H4 => Some("4 h"),
        Cap::H8 => Some("8 h"),
        Cap::H24 => Some("24 h"),
        Cap::None => None,
    };
    let parts: Vec<&str> = [battery, heat, cap].into_iter().flatten().collect();
    assert!(parts.len() <= 3);
    match parts.as_slice() {
        [] => "Never".into(),
        [only] => format!("At {only}"),
        [first, second] => format!("At {first} or {second}"),
        [first, second, third] => format!("At {first}, {second}, or {third}"),
        _ => unreachable!("three settings end a hold by itself"),
    }
}

/// What the lid rule does with a closed lid. Input: the rule. Output: the words.
pub(in crate::tui) fn lid_closed(rule: LidRule) -> &'static str {
    match rule {
        LidRule::AcOnly => "Hold on AC power only",
        LidRule::Always => "Hold always",
        LidRule::Off => "Sleep as usual",
    }
}

/// Why this computer cannot hold with the lid closed, or none when it can, the rule is
/// Off or only the power source stands in the way. Input: the lid support. Output: the
/// reason, cut to a bounded length, or none.
pub(in crate::tui) fn lid_reason(lid: &LidSupport) -> Option<String> {
    let reason = match lid {
        LidSupport::NeedsMacos13 => "Needs macOS 13 or newer",
        LidSupport::NeedsApproval => "Waits for your approval in System Settings",
        LidSupport::HelperMissing => "Not set up on this computer yet",
        LidSupport::NotTurnedOn => "Not turned on yet",
        LidSupport::LockedByPolicy => "Locked by your organization",
        LidSupport::NotAvailable { reason } | LidSupport::RemoveHelper { reason } => {
            return Some(reason.chars().take(REASON_CHARS_MAX).collect());
        }
        LidSupport::Held
        | LidSupport::Off
        | LidSupport::IdleOnlyOnBattery
        | LidSupport::SleepNotRestored => return None,
    };
    Some(reason.into())
}

/// The Keep awake settings in the order the Settings page lists them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::tui) enum Setting {
    Mode,
    Grace,
    Cap,
    Lid,
    LidDisplay,
    BatteryFloor,
    HeatEnd,
    HeatNotice,
    DisplayAwake,
}

/// Why this computer cannot honour one setting, or none when it can. Inputs: the setting
/// and the view. Output: the reason shown on the setting's line, or none.
pub(in crate::tui) fn unavailable(setting: Setting, view: &KeepAwakeView) -> Option<String> {
    match setting {
        Setting::HeatEnd | Setting::HeatNotice => {
            crate::keep_awake::power::heat_unavailable(view.platform).map(str::to_owned)
        }
        Setting::LidDisplay if view.platform != Platform::MacOs => Some("Only on macOS".into()),
        Setting::Lid => lid_reason(&view.lid_support),
        _ => None,
    }
}

/// What a closed lid does on this computer right now: why it cannot hold when there is a
/// reason, the battery limit, else the lid rule's own words. Input: the view. Output: the
/// words for the screen's Lid closed line.
pub(in crate::tui) fn lid_now(view: &KeepAwakeView) -> String {
    if let Some(reason) = lid_reason(&view.lid_support) {
        return reason;
    }
    match view.lid_support {
        LidSupport::IdleOnlyOnBattery => "Idle only: on battery".into(),
        LidSupport::Off => lid_closed(LidRule::Off).into(),
        _ => lid_closed(view.settings.lid).into(),
    }
}

/// Reads the minutes typed into the custom field. Input: the typed text. Output: a whole
/// number from 1 to 1440, or none for anything else.
pub(in crate::tui) fn custom_minutes(typed: &str) -> Option<u16> {
    let text = typed.trim();
    if text.is_empty() || text.len() > 4 || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let minutes: u16 = text.parse().ok()?;
    (1..=MINUTES_MAX).contains(&minutes).then_some(minutes)
}

/// Whether a timer can start: every mode but Off, where the engine refuses one. Input:
/// the view. Output: the answer.
pub(in crate::tui) fn can_start(view: &KeepAwakeView) -> bool {
    view.settings.mode != Mode::Off
}

/// Whether Extend has a timer to move: something is held and the timer is the reason.
/// Input: the view. Output: the answer.
pub(in crate::tui) fn can_extend(view: &KeepAwakeView) -> bool {
    holding(view) && view.state.reason == Reason::Timer
}

/// Whether End has a hold to end. Input: the view. Output: the answer.
pub(in crate::tui) fn can_end(view: &KeepAwakeView) -> bool {
    holding(view)
}
