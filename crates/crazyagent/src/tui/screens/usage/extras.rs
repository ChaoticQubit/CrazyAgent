use crate::usage::extras::{LimitExtras, ResetKind, Resets, Spend};

/// The separator between the parts of an account's limits line, the same as the desktop's.
pub const PART_GAP: &str = " \u{b7} ";

/// The count an account holds now: its stored count, or none when not reported or when the
/// next expiry has passed, which the engine also reads as not reported until read again.
/// Inputs: the contract and now. Output: the resets, or none.
fn live(extras: &LimitExtras, now_ms: i64) -> Option<&Resets> {
    let _ = (extras, now_ms);
    None
}

/// An account's free resets in words, the same as the desktop's: "No free resets", "2 free
/// resets (5-hour, weekly), next expires Nov 20", with "(may break)" for a private read, or
/// "Free resets: not reported by <harness>". Inputs: the contract, now and the local offset in
/// seconds. Output: the words, never a bare 0.
pub fn resets_text(extras: &LimitExtras, now_ms: i64, offset: i64) -> String {
    let _ = (extras, now_ms, offset, live(extras, now_ms));
    String::new()
}

/// An account's credits in the provider's words, or not reported by its harness. Input: the
/// contract. Output: the words.
pub fn credits_text(extras: &LimitExtras) -> String {
    let _ = extras;
    String::new()
}

/// The one line under an account's limits, the same on Pools, Metrics and the TUI: free
/// resets, credits and where to spend, with two not reported fields said once. Inputs: the
/// contract, now and the local offset in seconds. Output: the line.
pub fn extras_line(extras: &LimitExtras, now_ms: i64, offset: i64) -> String {
    let _ = (extras, now_ms, offset);
    String::new()
}

/// The notice for a new free reset, the same as the desktop's. Inputs: the account's name and
/// its count now, at least one. Output: the sentence.
pub fn new_reset_line(name: &str, count: u32) -> String {
    let _ = (name, count);
    String::new()
}

#[allow(dead_code)]
fn kind_words(kind: ResetKind) -> &'static str {
    let _ = kind;
    ""
}

#[allow(dead_code)]
fn spend_place(spend: &Spend) -> Option<&str> {
    let _ = spend;
    None
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
}
