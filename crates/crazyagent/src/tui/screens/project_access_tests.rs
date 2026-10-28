use super::project_access::summary;
#[cfg(target_os = "macos")]
use super::project_access::{AccessEdit, AccessKey, FULL_ACCESS_WARNING};
use crate::sandbox::policy::{AgentAccess, LevelView, PolicyView, ToolchainView};
use crate::sandbox::toolchains::Toolchain;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn view() -> PolicyView {
    PolicyView {
        access: AgentAccess::Sandboxed,
        levels: AgentAccess::ALL
            .into_iter()
            .map(|access| LevelView {
                access,
                label: access.label().to_owned(),
                unavailable: access.unavailable().map(str::to_owned),
            })
            .collect(),
        toolchains: vec![
            ToolchainView {
                toolchain: Toolchain::Rust,
                label: "Rust".into(),
                installed: true,
                used: true,
                allowed: true,
            },
            ToolchainView {
                toolchain: Toolchain::Node,
                label: "Node".into(),
                installed: true,
                used: false,
                allowed: false,
            },
        ],
    }
}

#[cfg(target_os = "macos")]
#[test]
fn the_access_box_saves_only_changed_toolchains_and_full_access_needs_a_yes() {
    let mut edit = AccessEdit::of(&view());
    assert_eq!(edit.key(key(KeyCode::Right)), AccessKey::Handled);
    assert_eq!(edit.access, AgentAccess::OwnBranch);
    assert_eq!(edit.key(key(KeyCode::Down)), AccessKey::Handled);
    assert_eq!(edit.key(key(KeyCode::Down)), AccessKey::Handled);
    assert_eq!(edit.key(key(KeyCode::Char(' '))), AccessKey::Handled);
    let AccessKey::Save(policy) = edit.key(key(KeyCode::Enter)) else {
        panic!("Enter saves");
    };
    assert_eq!(policy.access, AgentAccess::OwnBranch);
    assert_eq!(policy.toolchains.len(), 1);
    assert_eq!(policy.toolchains[0].toolchain, Toolchain::Node);
    assert!(policy.toolchains[0].allowed);

    let mut edit = AccessEdit::of(&view());
    edit.key(key(KeyCode::Left));
    assert_eq!(edit.access, AgentAccess::Full);
    assert_eq!(edit.key(key(KeyCode::Enter)), AccessKey::Handled);
    assert!(edit.confirm_full);
    let look = super::new_project_view::Look::plain();
    let shown: String = edit
        .body(&look)
        .iter()
        .map(|row| row.text())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(shown.contains("they can read your keys"), "{shown}");
    assert!(FULL_ACCESS_WARNING.starts_with("Agents run with your user's permissions"));
    assert_eq!(edit.key(key(KeyCode::Char('n'))), AccessKey::Handled);
    assert!(!edit.confirm_full);
    assert_eq!(edit.key(key(KeyCode::Enter)), AccessKey::Handled);
    let AccessKey::Save(policy) = edit.key(key(KeyCode::Char('y'))) else {
        panic!("y saves Full access");
    };
    assert_eq!(policy.access, AgentAccess::Full);
    assert!(policy.toolchains.is_empty());
    assert_eq!(edit.key(key(KeyCode::Esc)), AccessKey::Close);
}

#[test]
fn the_settings_line_names_the_level_and_the_allowed_toolchains() {
    assert_eq!(summary(None), "Loading...");
    assert_eq!(summary(Some(&view())), "Sandboxed · Rust");
    let none = PolicyView {
        access: AgentAccess::OwnBranch,
        toolchains: Vec::new(),
        ..view()
    };
    assert_eq!(summary(Some(&none)), "Own branch · no toolchains");
}
