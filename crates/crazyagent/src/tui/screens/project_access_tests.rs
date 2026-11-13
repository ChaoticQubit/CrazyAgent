#[cfg(target_os = "macos")]
use super::project_access::FULL_ACCESS_WARNING;
use super::project_access::{
    AGENT_FILES_IGNORED_UNDER_FULL, AccessEdit, AccessKey, IDE_AGENT_FILES, IDE_GIT_HOOKS,
    IDE_GIT_HOOKS_LINE, LOWERING_STOPS_AGENTS, NETWORK_IGNORED_UNDER_FULL, POLICY_RESTARTS_IDE,
    SHARED_FOLDERS, SHARED_FOLDERS_IGNORED_UNDER_FULL, SHARED_FOLDERS_NEXT_START,
    SHARED_SECRETS_LINE, TOOLCHAIN_OFF_STOPS_AGENTS, agent_files_help, network_help, summary,
};
use crate::sandbox::policy::{
    AgentAccess, AgentNetwork, IdeAgentFiles, LevelView, NetworkView, PolicyView, ToolchainView,
};
use crate::sandbox::shared_folders::{
    CHOSEN_MAX, OFF_MACOS_LINE, SharedAccess, SharedFolders, SharedScope, UNREACHABLE_SECRETS_LINE,
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
        tool_cache: Default::default(),
        tool_caches: Vec::new(),
        ide_agent_files: Default::default(),
        shared_folders: Default::default(),
        local_folders: Vec::new(),
        version: 0,
        apply_changes: Default::default(),
        apply_choices: Vec::new(),
        grants: Vec::new(),
        temp_folder: Default::default(),
        grant_log: Vec::new(),
    }
}

/// F08: a TUI save keeps the grants and the Apply access changes choice the project saved,
/// so saving the level from the TUI never revokes a grant.
#[test]
fn a_tui_save_keeps_the_saved_grants_and_apply_choice() {
    use crate::sandbox::policy::{AccessApply, AccessGrant, GrantView};
    let grant = AccessGrant::Port { port: 47_821 };
    let edit = AccessEdit::of(&PolicyView {
        apply_changes: AccessApply::NextStart,
        grants: vec![GrantView {
            grant: grant.clone(),
            label: grant.label(),
        }],
        ..view()
    });
    let policy = edit.policy();
    assert_eq!(policy.grants, [grant]);
    assert_eq!(policy.apply_changes, AccessApply::NextStart);
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
fn the_access_box_saves_the_ide_hooks_switch_on_its_row_with_its_line() {
    let mut edit = AccessEdit::of(&view());
    assert!(!edit.ide_git_hooks);
    for _ in 0..4 {
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

/// T8: the access box keeps the project's tool cache choice when it saves.
#[test]
fn the_access_box_keeps_the_tool_cache_choice_when_it_saves() {
    let scope = crate::sandbox::policy::ToolCacheScope::PerAgent;
    let edit = AccessEdit::of(&PolicyView {
        tool_cache: scope,
        ..view()
    });
    assert_eq!(edit.policy().tool_cache, scope);
}

/// DEC-124: the agent folders choice has its own row after the IDE hooks switch, cycles
/// through its three choices with each one's help, says it does not apply at Full access and
/// is what Save sends.
#[test]
fn the_access_box_saves_the_agent_folders_choice_on_its_row_with_its_help() {
    let mut edit = AccessEdit::of(&view());
    assert_eq!(edit.ide_agent_files, IdeAgentFiles::AllowAll);
    for _ in 0..5 {
        assert_eq!(edit.key(key(KeyCode::Down)), AccessKey::Handled);
    }
    assert_eq!(edit.cursor, 5);
    let look = super::new_project_view::Look::plain();
    let words = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    let shown = |edit: &AccessEdit| -> Vec<String> {
        edit.rows(&look, usize::MAX)
            .iter()
            .map(|row| row.text())
            .collect()
    };
    let mut seen = Vec::new();
    for _ in 0..IdeAgentFiles::ALL.len() {
        let rows = shown(&edit);
        let row = rows
            .iter()
            .position(|row| row.contains(IDE_AGENT_FILES))
            .expect("the agent folders row");
        assert!(rows[row].starts_with("> "), "{}", rows[row]);
        assert!(
            rows[row].contains(&format!("< {} >", edit.ide_agent_files.label())),
            "{}",
            rows[row]
        );
        assert!(
            words(&rows[row..].join(" ")).contains(agent_files_help(edit.ide_agent_files)),
            "{rows:?}"
        );
        assert!(!words(&rows.join(" ")).contains(AGENT_FILES_IGNORED_UNDER_FULL));
        seen.push(edit.ide_agent_files);
        assert_eq!(edit.key(key(KeyCode::Right)), AccessKey::Handled);
    }
    assert_eq!(seen, IdeAgentFiles::ALL);
    assert_eq!(edit.ide_agent_files, IdeAgentFiles::AllowAll);
    assert_eq!(edit.key(key(KeyCode::Left)), AccessKey::Handled);
    assert_eq!(edit.ide_agent_files, IdeAgentFiles::ReadOnly);
    assert!(!edit.ide_git_hooks);
    let AccessKey::Save(policy) = edit.key(key(KeyCode::Enter)) else {
        panic!("Enter saves");
    };
    assert_eq!(policy.ide_agent_files, IdeAgentFiles::ReadOnly);
    assert_eq!(policy.access, AgentAccess::Sandboxed);
    assert!(!policy.ide_git_hooks);

    let saved = AccessEdit::of(&PolicyView {
        ide_agent_files: IdeAgentFiles::NoDeleteOrRename,
        access: AgentAccess::Full,
        ..view()
    });
    assert_eq!(saved.ide_agent_files, IdeAgentFiles::NoDeleteOrRename);
    assert_eq!(
        saved.policy().ide_agent_files,
        IdeAgentFiles::NoDeleteOrRename
    );
    assert!(words(&shown(&saved).join(" ")).contains(AGENT_FILES_IGNORED_UNDER_FULL));
    assert_eq!(
        AGENT_FILES_IGNORED_UNDER_FULL,
        "Full access runs the IDE without a sandbox, so this choice does not apply."
    );
    assert!(agent_files_help(IdeAgentFiles::ReadOnly).contains("Edit anyway"));
    assert!(agent_files_help(IdeAgentFiles::NoDeleteOrRename).contains("Edit anyway included"));
}

#[test]
fn the_access_box_says_beside_the_level_that_saving_restarts_the_projects_ide() {
    let edit = AccessEdit::of(&view());
    let look = super::new_project_view::Look::plain();
    let rows: Vec<String> = edit
        .rows(&look, usize::MAX)
        .iter()
        .map(|row| row.text())
        .collect();
    let words = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    assert_eq!(
        POLICY_RESTARTS_IDE,
        "Changing the level, the IDE hooks switch or the agent folders choice below Full access, or the network choice at Own branch restarts this project's IDE at once. Its terminals end, and it reopens within a minute."
    );
    assert!(words(&rows.join(" ")).contains(POLICY_RESTARTS_IDE));
}

/// The Full access confirm box says what Full access means for the IDE, in the desktop's words.
/// macOS only: Linux runs agents Sandboxed alone in this version, so Own branch and Full
/// access are refused there (`LINUX_UNAVAILABLE`, asserted in `sandbox::policy::tests`).
#[cfg(target_os = "macos")]
#[test]
fn the_full_access_box_says_what_full_access_means_for_the_ide() {
    use super::project_access::FULL_ACCESS_IDE;
    let mut edit = AccessEdit::of(&view());
    edit.key(key(KeyCode::Left));
    assert_eq!(edit.access, AgentAccess::Full);
    assert_eq!(edit.key(key(KeyCode::Enter)), AccessKey::Handled);
    assert!(edit.confirm_full);
    let look = super::new_project_view::Look::plain();
    let shown = edit
        .rows(&look, usize::MAX)
        .iter()
        .map(|row| row.text())
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert_eq!(
        FULL_ACCESS_IDE,
        "The IDE runs with your user's permissions. The project's network choice does not limit it. The AI lock and extension approvals are written at each IDE start, and anything inside the IDE can change them until the next one. Workspace trust is on. Until you trust the folder, IDE git and extensions that need trust stay off."
    );
    assert!(shown.contains(FULL_ACCESS_IDE), "{shown}");
}

/// The IDE hooks line says the switch does nothing at Full access, before when it applies.
#[test]
fn the_ide_hooks_line_says_full_access_runs_the_owners_git() {
    assert!(IDE_GIT_HOOKS_LINE.contains("No effect at Full access: git runs as yours."));
    assert!(IDE_GIT_HOOKS_LINE.ends_with("Applies when the IDE next starts."));
}

fn shared_view(shared: SharedFolders, local: &[&str]) -> PolicyView {
    PolicyView {
        shared_folders: shared,
        local_folders: local.iter().map(|name| (*name).to_owned()).collect(),
        ..view()
    }
}

fn shown_words(edit: &AccessEdit) -> String {
    let look = super::new_project_view::Look::plain();
    edit.rows(&look, usize::MAX)
        .iter()
        .map(|row| row.text())
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn shown_rows(edit: &AccessEdit) -> Vec<String> {
    let look = super::new_project_view::Look::plain();
    edit.rows(&look, usize::MAX)
        .iter()
        .map(|row| row.text())
        .collect()
}

/// DEC-147.9: every shared folders choice and both access choices are reached with the
/// keys from the shared folders rows after the agent folders row, each with its help, and
/// Save sends exactly the pair picked.
#[test]
fn the_access_box_saves_every_shared_folders_choice_and_access_with_its_help() {
    for (step, scope) in SharedScope::ALL.into_iter().enumerate() {
        for access in SharedAccess::ALL {
            let mut edit = AccessEdit::of(&shared_view(
                SharedFolders::default(),
                &["build", "program"],
            ));
            for _ in 0..6 {
                assert_eq!(edit.key(key(KeyCode::Down)), AccessKey::Handled);
            }
            assert_eq!(
                edit.cursor, 6,
                "the shared folders row follows the agent folders row"
            );
            for _ in 0..step {
                assert_eq!(edit.key(key(KeyCode::Right)), AccessKey::Handled);
            }
            assert_eq!(edit.shared.scope, scope);
            let rows = shown_rows(&edit);
            let row = rows
                .iter()
                .position(|row| row.contains(&format!("< {} >", scope.label())))
                .expect("the shared folders row");
            assert!(rows[row].starts_with("> "), "{}", rows[row]);
            assert!(shown_words(&edit).contains(scope.help()), "{rows:?}");
            for _ in 0..12 {
                assert_eq!(edit.key(key(KeyCode::Down)), AccessKey::Handled);
            }
            if access == SharedAccess::ReadWrite {
                assert_eq!(edit.key(key(KeyCode::Right)), AccessKey::Handled);
            }
            assert_eq!(edit.shared.access, access);
            let rows = shown_rows(&edit);
            let row = rows
                .iter()
                .position(|row| row.contains(&format!("< {} >", access.label())))
                .expect("the shared access row");
            assert!(rows[row].starts_with("> "), "{}", rows[row]);
            assert!(shown_words(&edit).contains(access.help()), "{rows:?}");
            let AccessKey::Save(policy) = edit.key(key(KeyCode::Enter)) else {
                panic!("Enter saves");
            };
            assert_eq!(policy.shared_folders.scope, scope);
            assert_eq!(policy.shared_folders.access, access);
            assert!(policy.shared_folders.chosen.is_empty());
            assert!(policy.validate().is_ok());
        }
    }
}

/// Chosen folders lists the project folder's top-level folders under its row, one switch
/// each; Save sends the picked ones, and a pick past the engine's bound is refused with why.
#[test]
fn chosen_folders_picks_top_level_folders_on_their_rows_within_the_bound() {
    let chosen = SharedFolders {
        scope: SharedScope::Chosen,
        ..SharedFolders::default()
    };
    let mut edit = AccessEdit::of(&shared_view(chosen.clone(), &["build", "program"]));
    for _ in 0..6 {
        edit.key(key(KeyCode::Down));
    }
    let rows = shown_rows(&edit);
    assert!(rows.iter().any(|row| row.contains("[ ] build")), "{rows:?}");
    assert!(
        rows.iter().any(|row| row.contains("[ ] program")),
        "{rows:?}"
    );
    edit.key(key(KeyCode::Down));
    edit.key(key(KeyCode::Down));
    assert_eq!(edit.key(key(KeyCode::Char(' '))), AccessKey::Handled);
    assert!(
        shown_rows(&edit)
            .iter()
            .any(|row| row.starts_with("> ") && row.contains("[x] program"))
    );
    let AccessKey::Save(policy) = edit.key(key(KeyCode::Enter)) else {
        panic!("Enter saves");
    };
    assert_eq!(policy.shared_folders.chosen, vec!["program".to_owned()]);
    assert_eq!(policy.shared_folders.scope, SharedScope::Chosen);

    let local: Vec<String> = (0..=CHOSEN_MAX)
        .map(|index| format!("f{index:02}"))
        .collect();
    let full = SharedFolders {
        scope: SharedScope::Chosen,
        chosen: local[..CHOSEN_MAX].to_vec(),
        ..SharedFolders::default()
    };
    let names: Vec<&str> = local.iter().map(String::as_str).collect();
    let mut edit = AccessEdit::of(&shared_view(full, &names));
    for _ in 0..(7 + CHOSEN_MAX) {
        edit.key(key(KeyCode::Down));
    }
    assert!(shown_rows(&edit).iter().any(|row| row.starts_with("> ") && row.contains(&format!("[ ] {}", local[CHOSEN_MAX]))));
    edit.key(key(KeyCode::Char(' ')));
    assert_eq!(
        edit.shared.chosen.len(),
        CHOSEN_MAX,
        "no pick past the bound"
    );
    assert_eq!(
        edit.error.as_deref(),
        Some(format!("Pick at most {CHOSEN_MAX} folders.").as_str())
    );
    edit.key(key(KeyCode::Up));
    edit.key(key(KeyCode::Char(' ')));
    assert_eq!(
        edit.shared.chosen.len(),
        CHOSEN_MAX - 1,
        "an unpick always works"
    );
    assert_eq!(edit.error, None);
}

/// A save keeps the secrets the owner shared back in the desktop app and a picked folder that
/// is gone, which still shows so it can be unpicked; the box says what stays closed, when a
/// change applies, the Linux and Windows gap and that Full access makes the choice moot.
#[test]
fn the_access_box_keeps_shared_back_secrets_and_says_what_stays_closed() {
    let saved = SharedFolders {
        scope: SharedScope::Chosen,
        access: SharedAccess::ReadWrite,
        chosen: vec!["old".into()],
        secrets: vec![".env".into()],
    };
    let edit = AccessEdit::of(&shared_view(saved.clone(), &["program"]));
    assert_eq!(edit.folders, vec!["old".to_owned(), "program".to_owned()]);
    assert_eq!(
        edit.policy().shared_folders,
        saved,
        "a save keeps every part of the choice"
    );
    let words = shown_words(&edit);
    assert!(words.contains(SHARED_FOLDERS));
    assert!(words.contains(SHARED_SECRETS_LINE), "{words}");
    assert!(words.contains("Shared back for reading: .env."), "{words}");
    assert!(
        words.contains(&format!("{UNREACHABLE_SECRETS_LINE} .env.")),
        "Chosen folders without a picked folder around it keeps .env closed: {words}"
    );
    assert!(words.contains(SHARED_FOLDERS_NEXT_START), "{words}");
    assert!(words.contains(OFF_MACOS_LINE), "{words}");
    assert!(!words.contains(SHARED_FOLDERS_IGNORED_UNDER_FULL));
    let full = AccessEdit::of(&PolicyView {
        access: AgentAccess::Full,
        ..shared_view(saved, &["program"])
    });
    assert!(shown_words(&full).contains(SHARED_FOLDERS_IGNORED_UNDER_FULL));
    let before = AccessEdit::of(&view());
    assert_eq!(
        before.policy().shared_folders,
        SharedFolders::default(),
        "All with Read-only by default"
    );
    let all = AccessEdit::of(&shared_view(
        SharedFolders {
            secrets: vec![".env".into()],
            ..SharedFolders::default()
        },
        &["program"],
    ));
    assert!(!shown_words(&all).contains(UNREACHABLE_SECRETS_LINE));
}
