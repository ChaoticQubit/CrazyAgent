use crate::engine::owner::AccountExtras;
use crate::usage::extras::{LimitExtras, ResetKind, Resets, Spend, UnreadClass};
use std::collections::HashMap;

/// The separator between the parts of an account's limits line, the same as the desktop's.
pub const PART_GAP: &str = " \u{b7} ";

/// The words for free resets the last read found no count of, the same as the desktop's.
pub const UNREAD: &str = "Free resets: couldn't read";

/// The words for a free resets count the engine could not read, by why, the same as the
/// desktop's: the plain words with a short reason, or off when the owner turned the read
/// off. Input: the class. Output: the words.
pub fn unread_text(_class: UnreadClass) -> &'static str {
    UNREAD
}

/// The count an account holds now: its stored count, or none when not reported or when the
/// next expiry has passed, which the engine also reads as not reported until read again.
/// Inputs: the contract and now. Output: the resets, or none.
fn live(extras: &LimitExtras, now_ms: i64) -> Option<&Resets> {
    let resets = extras.resets.as_ref()?;
    assert!(resets.count <= crate::usage::extras::RESETS_MAX);
    match resets.next_expiry_ms {
        Some(at) if at <= now_ms => None,
        _ => Some(resets),
    }
}

fn kind_words(kind: ResetKind) -> &'static str {
    match kind {
        ResetKind::FiveHour => "5-hour",
        ResetKind::Weekly => "weekly",
        ResetKind::Model => "model",
        ResetKind::AllLimits => "all limits",
    }
}

fn counted(count: u32) -> String {
    if count == 1 {
        "1 free reset".into()
    } else {
        format!("{count} free resets")
    }
}

/// An account's free resets in words, the same as the desktop's: "No free resets", "2 free
/// resets (5-hour, weekly), next expires Nov 20", with "(may break)" for a private read, "Free
/// resets: couldn't read" or "Free resets: not reported by <harness>". Inputs: the contract,
/// now and the local offset in seconds. Output: the words, never a bare 0.
pub fn resets_text(extras: &LimitExtras, now_ms: i64, offset: i64) -> String {
    let Some(resets) = live(extras, now_ms) else {
        if let Some(class) = extras.resets_unread {
            return unread_text(class).to_owned();
        }
        return format!("Free resets: {}", extras.not_reported);
    };
    let label = if extras.may_break { " (may break)" } else { "" };
    if resets.count == 0 {
        return format!("No free resets{label}");
    }
    let kinds = if resets.kinds.is_empty() {
        String::new()
    } else {
        let words: Vec<&str> = resets.kinds.iter().copied().map(kind_words).collect();
        format!(" ({})", words.join(", "))
    };
    let next = resets.next_expiry_ms.map_or(String::new(), |at| {
        format!(
            ", next expires {}",
            crate::tui::clock::expiry(at, now_ms, offset)
        )
    });
    format!("{}{kinds}{next}{label}", counted(resets.count))
}

/// An account's credits in the provider's words, or not reported by its harness. Input: the
/// contract. Output: the words.
pub fn credits_text(extras: &LimitExtras) -> String {
    format!(
        "Credits: {}",
        extras.credits.as_deref().unwrap_or(&extras.not_reported)
    )
}

/// The one line under an account's limits, the same on Pools, Metrics and the TUI: free
/// resets, credits and where to spend, with two not reported fields said once. Inputs: the
/// contract, now and the local offset in seconds. Output: the line.
pub fn extras_line(extras: &LimitExtras, now_ms: i64, offset: i64) -> String {
    let mut parts = if live(extras, now_ms).is_none()
        && extras.resets_unread.is_none()
        && extras.credits.is_none()
    {
        vec![format!("Free resets and credits: {}", extras.not_reported)]
    } else {
        vec![resets_text(extras, now_ms, offset), credits_text(extras)]
    };
    if let Spend::Elsewhere { place } = &extras.spend {
        parts.push(format!("Spend at {place}"));
    }
    assert!(!parts.is_empty() && parts.len() <= 3);
    parts.join(PART_GAP)
}

/// The notice for a new free reset, the same as the desktop's. Inputs: the account's name and
/// its count now, at least one. Output: the sentence.
pub fn new_reset_line(name: &str, count: u32) -> String {
    assert!(count >= 1, "a new free reset leaves at least one");
    format!("{name} has a new free reset. {} now.", counted(count))
}

/// How often the shell reads the stored extras for the new free reset notice, in ms.
pub const RESET_WATCH_MS: i64 = 60_000;

/// How long before the first look a gain still counts as new, the same as the desktop's: one
/// usage poll and some slack, so a reset that arrived while the TUI was closed is not told.
pub const FRESH_GAIN_MS: i64 = 15 * 60_000;

/// The most new free resets one look tells, so a burst never floods the message line.
pub const NOTICES_PER_LOOK_MAX: usize = 3;

/// The shell's watch for new free resets: when it last asked for the stored extras, and each
/// account's last gain stamp seen, none before the first look.
#[derive(Debug, Default)]
pub struct ResetWatch {
    asked_ms: Option<i64>,
    known: Option<HashMap<String, i64>>,
}

impl ResetWatch {
    /// Tells whether the shell asks for the stored extras now: never asked, `RESET_WATCH_MS`
    /// since the last ask, or a clock that went back; a yes marks the ask. Input: now.
    /// Output: the answer.
    pub fn due(&mut self, now_ms: i64) -> bool {
        let due = self
            .asked_ms
            .is_none_or(|at| at > now_ms || now_ms - at >= RESET_WATCH_MS);
        if due {
            self.asked_ms = Some(now_ms);
        }
        due
    }

    /// Picks the new free resets to tell from one read, the same as the desktop's: each
    /// account whose gain stamp changed since the last look, and for an account not seen
    /// before only a gain from the last `FRESH_GAIN_MS`; a count not reported now is never
    /// told. Inputs: the read and now. Output: the accounts and counts to tell, at most
    /// `NOTICES_PER_LOOK_MAX`.
    pub fn look(&mut self, rows: &[AccountExtras], now_ms: i64) -> Vec<(String, u32)> {
        let mut seen = HashMap::new();
        let mut send = Vec::new();
        for row in rows {
            let Some(gained) = row.extras.resets_gained_ms else {
                continue;
            };
            let account = row.account.as_str();
            seen.insert(account.to_owned(), gained);
            let Some(resets) = live(&row.extras, now_ms).filter(|resets| resets.count >= 1) else {
                continue;
            };
            let fresh = match self.known.as_ref().and_then(|known| known.get(account)) {
                None => gained > now_ms - FRESH_GAIN_MS,
                Some(before) => *before != gained,
            };
            if fresh && send.len() < NOTICES_PER_LOOK_MAX {
                send.push((account.to_owned(), resets.count));
            }
        }
        self.known = Some(seen);
        assert!(send.len() <= NOTICES_PER_LOOK_MAX);
        send
    }
}

/// The one message for a look's new free resets: each account by its label, joined, never by
/// its id. Inputs: the accounts and counts to tell and the labels by account id. Output: the
/// message, or none when none arrived.
pub fn reset_notice(send: &[(String, u32)], labels: &HashMap<String, String>) -> Option<String> {
    assert!(send.len() <= NOTICES_PER_LOOK_MAX);
    if send.is_empty() {
        return None;
    }
    let lines: Vec<String> = send
        .iter()
        .map(|(account, count)| {
            new_reset_line(
                labels.get(account).map_or("An account", String::as_str),
                *count,
            )
        })
        .collect();
    Some(lines.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::extras::Adapter;

    const NOW: i64 = 1_794_474_000_000;
    const TODAY_1430: i64 = 1_794_493_800_000;
    const NOV_20: i64 = 1_795_161_600_000;

    fn extras() -> LimitExtras {
        LimitExtras {
            harness: "Codex".into(),
            adapter: Adapter::Codex,
            may_break: false,
            resets: None,
            credits: None,
            spend: Spend::NotReported,
            not_reported: "not reported by Codex".into(),
            resets_checked_ms: None,
            credits_checked_ms: None,
            resets_gained_ms: None,
            resets_unread: None,
        }
    }

    fn counted(count: u32, kinds: Vec<ResetKind>, next_expiry_ms: Option<i64>) -> LimitExtras {
        LimitExtras {
            resets: Some(Resets {
                count,
                kinds,
                next_expiry_ms,
            }),
            ..extras()
        }
    }

    #[test]
    fn a_resets_read_that_found_no_count_reads_couldnt_read_never_none_or_not_reported() {
        let claude = LimitExtras {
            harness: "Claude Code".into(),
            adapter: Adapter::Claude,
            may_break: true,
            not_reported: "not reported by Claude Code".into(),
            resets_checked_ms: Some(NOW),
            resets_unread: Some(UnreadClass::Unspecified),
            ..extras()
        };
        assert_eq!(resets_text(&claude, NOW, 0), "Free resets: couldn't read");
        assert_eq!(
            extras_line(&claude, NOW, 0),
            "Free resets: couldn't read \u{b7} Credits: not reported by Claude Code"
        );
        let never = LimitExtras {
            resets_checked_ms: None,
            resets_unread: None,
            ..claude
        };
        assert_eq!(
            extras_line(&never, NOW, 0),
            "Free resets and credits: not reported by Claude Code"
        );
    }

    /// Each reason a count could not be read has its own short words, and "Don't read" says
    /// off in Settings; none of them reads as none or as not reported.
    #[test]
    fn each_unread_class_reads_couldnt_read_with_its_reason_and_off_reads_off_in_settings() {
        for (class, words) in [
            (UnreadClass::Unspecified, "Free resets: couldn't read"),
            (
                UnreadClass::Asker,
                "Free resets: couldn't read (Anthropic answers only Claude Code)",
            ),
            (
                UnreadClass::Withheld,
                "Free resets: couldn't read (Anthropic withheld them)",
            ),
            (
                UnreadClass::NoReason,
                "Free resets: couldn't read (Anthropic gave no reason)",
            ),
            (
                UnreadClass::NoAnswer,
                "Free resets: couldn't read (no answer)",
            ),
            (
                UnreadClass::NotUnderstood,
                "Free resets: couldn't read (answer not understood)",
            ),
            (UnreadClass::Off, "Free resets: off in Settings"),
        ] {
            let claude = LimitExtras {
                harness: "Claude Code".into(),
                adapter: Adapter::Claude,
                may_break: true,
                not_reported: "not reported by Claude Code".into(),
                resets_unread: Some(class),
                ..extras()
            };
            assert_eq!(unread_text(class), words);
            assert_eq!(resets_text(&claude, NOW, 0), words, "{class:?}");
            assert_eq!(
                extras_line(&claude, NOW, 0),
                format!("{words} \u{b7} Credits: not reported by Claude Code")
            );
        }
    }

    #[test]
    fn a_known_zero_reads_no_free_resets_and_an_absent_count_names_its_harness_never_0() {
        assert_eq!(
            resets_text(&counted(0, vec![], None), NOW, 0),
            "No free resets"
        );
        let gemini = LimitExtras {
            harness: "Gemini CLI".into(),
            adapter: Adapter::NotReported,
            not_reported: "not reported by Gemini CLI".into(),
            ..extras()
        };
        assert_eq!(
            resets_text(&gemini, NOW, 0),
            "Free resets: not reported by Gemini CLI"
        );
    }

    #[test]
    fn counts_kinds_and_the_next_expiry_read_as_on_the_desktop() {
        assert_eq!(
            resets_text(&counted(1, vec![], None), NOW, 0),
            "1 free reset"
        );
        assert_eq!(
            resets_text(&counted(2, vec![], None), NOW, 0),
            "2 free resets"
        );
        assert_eq!(
            resets_text(
                &counted(
                    3,
                    vec![ResetKind::FiveHour, ResetKind::Weekly, ResetKind::Model],
                    None
                ),
                NOW,
                0
            ),
            "3 free resets (5-hour, weekly, model)"
        );
        assert_eq!(
            resets_text(&counted(1, vec![ResetKind::AllLimits], None), NOW, 0),
            "1 free reset (all limits)"
        );
        assert_eq!(
            resets_text(&counted(2, vec![ResetKind::Weekly], Some(NOV_20)), NOW, 0),
            "2 free resets (weekly), next expires Nov 20"
        );
        assert_eq!(
            resets_text(&counted(2, vec![], Some(TODAY_1430)), NOW, 0),
            "2 free resets, next expires today 14:30"
        );
        assert_eq!(
            resets_text(&counted(2, vec![], Some(TODAY_1430)), NOW, -19_800),
            "2 free resets, next expires today 09:00"
        );
    }

    #[test]
    fn a_count_past_its_next_expiry_reads_not_reported() {
        for at in [NOW - 1, NOW] {
            assert_eq!(
                resets_text(&counted(2, vec![], Some(at)), NOW, 0),
                "Free resets: not reported by Codex"
            );
        }
    }

    #[test]
    fn a_private_read_carries_may_break_and_a_not_reported_count_does_not() {
        let claude = |resets: Option<Resets>| LimitExtras {
            harness: "Claude Code".into(),
            adapter: Adapter::Claude,
            may_break: true,
            not_reported: "not reported by Claude Code".into(),
            resets,
            ..extras()
        };
        let some = |count| {
            Some(Resets {
                count,
                kinds: vec![],
                next_expiry_ms: None,
            })
        };
        assert_eq!(
            resets_text(&claude(some(4)), NOW, 0),
            "4 free resets (may break)"
        );
        assert_eq!(
            resets_text(&claude(some(0)), NOW, 0),
            "No free resets (may break)"
        );
        assert_eq!(
            resets_text(&claude(None), NOW, 0),
            "Free resets: not reported by Claude Code"
        );
    }

    #[test]
    fn the_line_joins_resets_credits_and_where_to_spend_as_on_the_desktop() {
        assert_eq!(
            credits_text(&LimitExtras {
                credits: Some("$4.20 left".into()),
                ..extras()
            }),
            "Credits: $4.20 left"
        );
        assert_eq!(credits_text(&extras()), "Credits: not reported by Codex");
        assert_eq!(
            extras_line(&extras(), NOW, 0),
            "Free resets and credits: not reported by Codex"
        );
        let both = LimitExtras {
            credits: Some("$4.20 left".into()),
            ..counted(2, vec![ResetKind::FiveHour], Some(NOV_20))
        };
        assert_eq!(
            extras_line(&both, NOW, 0),
            "2 free resets (5-hour), next expires Nov 20 \u{b7} Credits: $4.20 left"
        );
        assert_eq!(
            extras_line(&counted(0, vec![], None), NOW, 0),
            "No free resets \u{b7} Credits: not reported by Codex"
        );
        let key = LimitExtras {
            harness: "OpenRouter".into(),
            adapter: Adapter::OpenRouter,
            not_reported: "not reported by OpenRouter".into(),
            credits: Some("$3.10 left".into()),
            spend: Spend::Elsewhere {
                place: "openrouter.ai".into(),
            },
            ..extras()
        };
        assert_eq!(
            extras_line(&key, NOW, 0),
            "Free resets: not reported by OpenRouter \u{b7} Credits: $3.10 left \u{b7} Spend at openrouter.ai"
        );
    }

    #[test]
    fn a_new_free_reset_names_the_account_and_the_count_now() {
        assert_eq!(
            new_reset_line("Work", 1),
            "Work has a new free reset. 1 free reset now."
        );
        assert_eq!(
            new_reset_line("Work", 3),
            "Work has a new free reset. 3 free resets now."
        );
    }

    fn row(account: &str, count: Option<u32>, gained: Option<i64>) -> AccountExtras {
        AccountExtras {
            account: crate::protocol::CredentialRef::new(account).unwrap(),
            extras: LimitExtras {
                resets: count.map(|count| Resets {
                    count,
                    kinds: vec![],
                    next_expiry_ms: None,
                }),
                resets_gained_ms: gained,
                resets_unread: None,
                ..extras()
            },
        }
    }

    #[test]
    fn a_new_free_reset_is_told_once_per_arrival_and_never_an_old_or_unreported_one() {
        let mut watch = ResetWatch::default();
        let rows = [
            row("acct-a", Some(2), Some(NOW - 60_000)),
            row("acct-b", Some(1), None),
        ];
        assert_eq!(
            watch.look(&rows, NOW),
            [("acct-a".to_owned(), 2)],
            "a gain from the last poll is told on the first look"
        );
        assert!(
            watch.look(&rows, NOW + 60_000).is_empty(),
            "the same gain is never told twice"
        );
        assert_eq!(
            watch.look(
                &[row("acct-a", Some(3), Some(NOW + 120_000))],
                NOW + 180_000
            ),
            [("acct-a".to_owned(), 3)]
        );
        assert!(
            watch
                .look(&[row("acct-a", None, Some(NOW + 240_000))], NOW + 300_000)
                .is_empty(),
            "a count that is not reported now is never told as new"
        );
        let old = [row("acct-a", Some(2), Some(NOW - 2 * 3_600_000))];
        assert!(
            ResetWatch::default().look(&old, NOW).is_empty(),
            "a gain from long before the first look is not new"
        );
        let many: Vec<AccountExtras> = (0..NOTICES_PER_LOOK_MAX + 2)
            .map(|index| row(&format!("acct-{index}"), Some(1), Some(NOW)))
            .collect();
        assert_eq!(
            ResetWatch::default().look(&many, NOW).len(),
            NOTICES_PER_LOOK_MAX
        );
    }

    #[test]
    fn the_reset_notice_names_each_account_by_its_label_and_joins_one_looks_arrivals() {
        let labels = HashMap::from([
            ("acct-a".to_owned(), "Work".to_owned()),
            ("acct-b".to_owned(), "Home".to_owned()),
        ]);
        assert_eq!(reset_notice(&[], &labels), None);
        assert_eq!(
            reset_notice(&[("acct-a".into(), 1)], &labels).as_deref(),
            Some("Work has a new free reset. 1 free reset now.")
        );
        assert_eq!(
            reset_notice(&[("acct-a".into(), 2), ("acct-b".into(), 1)], &labels).as_deref(),
            Some(
                "Work has a new free reset. 2 free resets now. Home has a new free reset. 1 free reset now."
            )
        );
        assert_eq!(
            reset_notice(&[("acct-gone".into(), 1)], &labels).as_deref(),
            Some("An account has a new free reset. 1 free reset now."),
            "an account with no row is never named by its id"
        );
    }

    #[test]
    fn the_reset_watch_asks_once_a_minute_and_again_after_a_clock_that_went_back() {
        let mut watch = ResetWatch::default();
        assert!(watch.due(NOW));
        assert!(
            !watch.due(NOW + 5_000),
            "the shell's 5 s tick never asks so soon"
        );
        assert!(watch.due(NOW + RESET_WATCH_MS));
        assert!(watch.due(NOW), "a clock that went back asks again");
    }
}
