#[cfg(target_os = "macos")]
use super::project_access::FULL_ACCESS_WARNING;
use super::project_access::{
    AccessEdit, AccessKey, IDE_GIT_HOOKS, IDE_GIT_HOOKS_LINE, LOWERING_STOPS_AGENTS,
    NETWORK_IGNORED_UNDER_FULL, TOOLCHAIN_OFF_STOPS_AGENTS, network_help, summary,
};
use crate::sandbox::policy::{
    AgentAccess, AgentNetwork, LevelView, NetworkView, PolicyView, ToolchainView,
};
use crate::sandbox::toolchains::Toolchain;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn view() -> PolicyView {
    PolicyView {
        access: AgentAccess::Sandboxed,
        network: AgentNetwork::default(),
        levels: AgentAccess::ALL
            .into_iter()
            .map(|access| LevelView {
                access,
                label: access.label().to_owned(),
                unavailable: access.unavailable().map(str::to_owned),
            })
            .collect(),
        networks: AgentNetwork::ALL
            .into_iter()
            .map(|network| NetworkView {
                network,
                label: network.label().to_owned(),
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
        ide_git_hooks: false,
        version: 0,
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
        .rows(&look, usize::MAX)
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

#[test]
fn the_none_choice_says_it_limits_agents_to_their_models_own_hosts_and_full_access_makes_it_moot() {
    assert_eq!(
        network_help(AgentNetwork::None),
        "None limits agents to their model: a hosted model's own hosts, through the engine's proxy, or only the local model's port. The plugin check reaches nothing."
    );
    for network in AgentNetwork::ALL {
        let help = network_help(network);
        assert!(
            !help.contains("only the model")
                && !help.contains("port 443 (HTTPS to any host)")
                && !help.contains("does not yet limit"),
            "{help}"
        );
    }
    let look = super::new_project_view::Look::plain();
    let note_shows = |access: AgentAccess| {
        let shown = AccessEdit::of(&PolicyView { access, ..view() })
            .rows(&look, usize::MAX)
            .iter()
            .map(|row| row.text())
            .collect::<Vec<_>>()
            .join(" ");
        shown
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .contains(NETWORK_IGNORED_UNDER_FULL)
    };
    assert!(
        note_shows(AgentAccess::Full),
        "the note must show under Full"
    );
    assert!(
        !note_shows(AgentAccess::Sandboxed),
        "the note must not show under Sandboxed"
    );
}

#[test]
fn the_access_box_says_beside_the_level_that_lowering_it_stops_agents_running_above_it() {
    let edit = AccessEdit::of(&view());
    let look = super::new_project_view::Look::plain();
    let rows: Vec<String> = edit
        .rows(&look, usize::MAX)
        .iter()
        .map(|row| row.text())
        .collect();
    let level = rows
        .iter()
        .position(|row| row.contains("Access      "))
        .expect("the level row");
    let toolchains = rows
        .iter()
        .position(|row| row.contains("Toolchains agents may use"))
        .expect("the toolchain heading");
    let beside = rows[level..toolchains].join(" ");
    let words = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(words(&beside).contains(LOWERING_STOPS_AGENTS), "{beside}");
    assert_eq!(
        LOWERING_STOPS_AGENTS,
        "Lowering the level stops agents running above it; they restart at the new level. It does not reach processes a Full access agent started outside its terminal."
    );
}

#[test]
fn the_access_box_says_beside_the_toolchains_that_turning_one_off_stops_agents_running_with_it() {
    let edit = AccessEdit::of(&view());
    let look = super::new_project_view::Look::plain();
    let rows: Vec<String> = edit
        .rows(&look, usize::MAX)
        .iter()
        .map(|row| row.text())
        .collect();
    let heading = rows
        .iter()
        .position(|row| row.contains("Toolchains agents may use"))
        .expect("the toolchain heading");
    let beside = rows[heading..].join(" ");
    let words = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        words(&beside).contains(TOOLCHAIN_OFF_STOPS_AGENTS),
        "{beside}"
    );
    assert_eq!(
        TOOLCHAIN_OFF_STOPS_AGENTS,
        "Turning a toolchain off stops agents, research jobs and master turns running with it; their next start goes without it."
    );
}

#[test]
fn the_access_box_saves_the_ide_hooks_switch_on_its_last_row_with_its_line() {
    let mut edit = AccessEdit::of(&view());
    assert!(!edit.ide_git_hooks);
    for _ in 0..6 {
        assert_eq!(edit.key(key(KeyCode::Down)), AccessKey::Handled);
    }
    assert_eq!(edit.cursor, 4);
    let look = super::new_project_view::Look::plain();
    let rows: Vec<String> = edit
        .rows(&look, usize::MAX)
        .iter()
        .map(|row| row.text())
        .collect();
    let words = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    let switch = rows
        .iter()
        .position(|row| row.contains(IDE_GIT_HOOKS))
        .expect("the IDE hooks row");
    assert!(rows[switch].starts_with("> [ ] "), "{}", rows[switch]);
    assert!(words(&rows[switch..].join(" ")).contains(IDE_GIT_HOOKS_LINE));
    assert_eq!(IDE_GIT_HOOKS, "IDE git runs the project's hooks");
    assert!(IDE_GIT_HOOKS_LINE.contains("never signed"));
    assert!(!IDE_GIT_HOOKS_LINE.contains("Sandboxed terminal"));
    assert_eq!(edit.key(key(KeyCode::Char(' '))), AccessKey::Handled);
    assert!(edit.ide_git_hooks);
    assert_eq!(edit.allowed, [true, false]);
    let AccessKey::Save(policy) = edit.key(key(KeyCode::Enter)) else {
        panic!("Enter saves");
    };
    assert!(policy.ide_git_hooks);
    assert_eq!(policy.access, AgentAccess::Sandboxed);
    assert!(policy.toolchains.is_empty());

    let on = AccessEdit::of(&PolicyView {
        ide_git_hooks: true,
        ..view()
    });
    assert!(on.ide_git_hooks);
    assert!(on.policy().ide_git_hooks);
    let mut off = on.clone();
    off.cursor = 4;
    assert_eq!(off.key(key(KeyCode::Left)), AccessKey::Handled);
    assert!(!off.policy().ide_git_hooks);
}
