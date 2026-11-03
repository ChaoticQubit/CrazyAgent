use super::new_project_view::Look;
use super::project_trust::{LOWERING_LINE, RAISE_WARNING, TrustEdit, TrustKey, summary};
use crate::trust::{Gap, LevelView, Origin, Rule, Trust, TrustView};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn view(trust: Trust, origin: Origin) -> TrustView {
    TrustView {
        trust,
        origin,
        levels: Trust::ALL
            .into_iter()
            .map(|trust| LevelView {
                trust,
                label: trust.label().to_owned(),
                description: trust.description().to_owned(),
            })
            .collect(),
        rules: vec![Rule {
            id: "agent_access".into(),
            text: "Agents run Sandboxed only.".into(),
            reason: "Git rights would let repository content steer an agent.".into(),
        }],
        gaps: vec![Gap {
            id: "G1".into(),
            text: "No CPU caps on macOS".into(),
            effect: "Hostile code can slow the machine".into(),
        }],
        version: 3,
    }
}

fn text(edit: &TrustEdit, room: usize) -> String {
    edit.body(&Look::plain(), room)
        .iter()
        .map(|row| row.text())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn raising_trust_in_the_tui_asks_first_and_lowering_saves_at_once() {
    let mut raise = TrustEdit::of(&view(Trust::Untrusted, Origin::Cloned));
    assert!(text(&raise, 40).contains("Set when CrazyAgent cloned this repository"));
    assert_eq!(raise.key(key(KeyCode::Right)), TrustKey::Handled);
    assert_eq!(raise.trust, Trust::Reviewed);
    assert_eq!(raise.key(key(KeyCode::Enter)), TrustKey::Handled);
    assert!(raise.confirm_raise);
    assert!(
        text(&raise, 40)
            .replace('\n', " ")
            .contains(&RAISE_WARNING[..40])
    );
    assert_eq!(raise.key(key(KeyCode::Char('n'))), TrustKey::Handled);
    assert!(!raise.confirm_raise);
    raise.key(key(KeyCode::Enter));
    assert_eq!(
        raise.key(key(KeyCode::Char('y'))),
        TrustKey::Save {
            trust: Trust::Reviewed,
            confirm_raise: true
        }
    );

    let mut lower = TrustEdit::of(&view(Trust::Reviewed, Origin::Default));
    lower.key(key(KeyCode::Left));
    assert_eq!(lower.trust, Trust::Untrusted);
    assert!(
        text(&lower, 40)
            .replace('\n', " ")
            .contains(&LOWERING_LINE[..40])
    );
    assert_eq!(
        lower.key(key(KeyCode::Enter)),
        TrustKey::Save {
            trust: Trust::Untrusted,
            confirm_raise: false
        }
    );

    let mut same = TrustEdit::of(&view(Trust::Untrusted, Origin::Owner));
    assert_eq!(same.key(key(KeyCode::Enter)), TrustKey::Close);
}

#[test]
fn the_tui_trust_box_lists_the_rules_and_gaps_and_scrolls_in_a_short_terminal() {
    let edit = TrustEdit::of(&view(Trust::Untrusted, Origin::Owner));
    let tall = text(&edit, 60);
    assert!(tall.contains("Locked while Untrusted"));
    assert!(tall.contains("- Agents run Sandboxed only."));
    assert!(tall.contains("Why: Git rights"));
    assert!(tall.contains("Not covered by any level"));
    assert!(tall.contains("- G1 No CPU caps on macOS"));
    assert!(!tall.contains("Down for more"));

    let mut short = edit.clone();
    let first = text(&short, 14);
    assert!(first.contains("Down for more"));
    assert!(first.contains("Locked while Untrusted"));
    for _ in 0..20 {
        short.key(key(KeyCode::Down));
    }
    let last = text(&short, 14);
    assert!(last.contains("Hostile code can slow the machine"));
    assert!(!last.contains("Locked while Untrusted"));
    assert!(last.lines().count() <= 14 + 4);

    assert_eq!(summary(None), "Loading...");
    assert_eq!(summary(Some(&edit.view)), "Untrusted");
}
