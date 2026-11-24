use super::*;
use crate::policy::{AgentAccess, AgentPolicy};
fn chosen(names: &[&str]) -> SharedFolders {
    SharedFolders {
        scope: SharedScope::Chosen,
        chosen: names.iter().map(|name| (*name).to_owned()).collect(),
        ..SharedFolders::default()
    }
}

/// A policy saved before the setting reads All and Read-only with nothing picked, a save names the
/// choice and a pinned policy keeps it.
#[test]
fn a_policy_saved_before_shared_folders_reads_all_and_read_only_and_a_save_keeps_the_choice() {
    let old: AgentPolicy =
        serde_json::from_str(r#"{"access":"sandboxed","toolchains":[]}"#).unwrap();
    assert_eq!(old.shared_folders, SharedFolders::default());
    assert_eq!(old.shared_folders.scope, SharedScope::All);
    assert_eq!(old.shared_folders.access, SharedAccess::ReadOnly);
    assert!(old.shared_folders.plain_reach());
    let own = AgentPolicy {
        shared_folders: SharedFolders {
            access: SharedAccess::ReadWrite,
            ..chosen(&["program"])
        },
        ..AgentPolicy::default()
    };
    let saved = serde_json::to_string(&own).unwrap();
    assert!(
        saved.contains(r#""shared_folders":{"scope":"chosen","access":"read_write","chosen":["program"],"secrets":[]}"#),
        "{saved}"
    );
    let folder = tempfile::tempdir().unwrap();
    assert_eq!(own.pinned(folder.path()).shared_folders, own.shared_folders);
    assert!(own.validate().is_ok());
}

/// Every option has its stable id and label, the default first, the same words the desktop
/// app shows.
#[test]
fn every_shared_folders_option_has_an_id_and_a_label_with_the_default_first() {
    let scopes: Vec<(&str, &str)> = SharedScope::ALL
        .iter()
        .map(|scope| (scope.id(), scope.label()))
        .collect();
    assert_eq!(
        scopes,
        [
            ("all", "All local folders"),
            ("chosen", "Chosen folders"),
            ("none", "None")
        ]
    );
    let accesses: Vec<(&str, &str)> = SharedAccess::ALL
        .iter()
        .map(|access| (access.id(), access.label()))
        .collect();
    assert_eq!(
        accesses,
        [("read_only", "Read-only"), ("read_write", "Read-write")]
    );
    assert_eq!(SharedScope::default(), SharedScope::ALL[0]);
    assert_eq!(SharedAccess::default(), SharedAccess::ALL[0]);
    for scope in SharedScope::ALL {
        let text = serde_json::to_string(&scope).unwrap();
        assert_eq!(text, format!("\"{}\"", scope.id()));
    }
    for access in SharedAccess::ALL {
        let text = serde_json::to_string(&access).unwrap();
        assert_eq!(text, format!("\"{}\"", access.id()));
    }
}

/// A choice a client sends is refused for a picked name with a slash, a dot name, an engine
/// folder in any case, a quote or control character, too many entries, a repeat, or a
/// shared-back secret that is absolute or climbs out; a plain choice passes.
#[test]
fn a_shared_folders_choice_out_of_shape_is_refused() {
    assert!(chosen(&["program", "data"]).validate().is_ok());
    let secrets = |paths: &[&str]| SharedFolders {
        secrets: paths.iter().map(|path| (*path).to_owned()).collect(),
        ..SharedFolders::default()
    };
    assert!(secrets(&[".env", "config/.env.local"]).validate().is_ok());
    for bad in [
        chosen(&["a/b"]),
        chosen(&[".."]),
        chosen(&["."]),
        chosen(&[""]),
        chosen(&[".git"]),
        chosen(&[".CrazyAgent"]),
        chosen(&["it's"]),
        chosen(&["a\"b"]),
        chosen(&["a\\b"]),
        chosen(&["line\nbreak"]),
        chosen(&["program", "program"]),
        chosen(&[&"x".repeat(256)]),
        secrets(&["/etc/passwd"]),
        secrets(&["../outside"]),
        secrets(&["config//.env"]),
        secrets(&[".git/config"]),
        secrets(&[".env", ".env"]),
        secrets(&[&format!("{}/x", "y".repeat(600))]),
    ] {
        assert_eq!(bad.validate(), Err(INVALID), "{bad:?}");
    }
    let many: Vec<String> = (0..=CHOSEN_MAX).map(|index| format!("f{index}")).collect();
    let refs: Vec<&str> = many.iter().map(String::as_str).collect();
    assert_eq!(chosen(&refs).validate(), Err(INVALID));
    let many: Vec<String> = (0..=SECRETS_MAX).map(|index| format!("s{index}")).collect();
    let refs: Vec<&str> = many.iter().map(String::as_str).collect();
    assert_eq!(secrets(&refs).validate(), Err(INVALID));
    let policy = AgentPolicy {
        shared_folders: chosen(&["a/b"]),
        ..AgentPolicy::default()
    };
    assert_eq!(policy.validate(), Err("agent_policy_invalid"));
}

/// A shared-back secret the choice keeps closed shows as not reachable: every one under None, under
/// Chosen folders the ones outside a picked folder, none under All.
#[test]
fn a_shared_back_secret_outside_the_shared_folders_shows_as_not_reachable() {
    let secrets = vec![
        ".env".to_owned(),
        "program/.env".to_owned(),
        "config/key.pem".to_owned(),
    ];
    let all = SharedFolders {
        secrets: secrets.clone(),
        ..SharedFolders::default()
    };
    assert!(all.unreachable_secrets().is_empty());
    let picked = SharedFolders {
        secrets: secrets.clone(),
        ..chosen(&["program"])
    };
    assert_eq!(picked.unreachable_secrets(), vec![".env", "config/key.pem"]);
    let none = SharedFolders {
        scope: SharedScope::None,
        ..picked
    };
    assert_eq!(none.unreachable_secrets(), secrets);
}

/// The project context names the folder and what each choice shares, keeps secrets closed,
/// says toolchains are never installed, and stays within its bound with the longest folder
/// and every picked name.
#[test]
fn the_shared_folders_section_says_what_each_choice_shares_within_its_bound() {
    let folder = Path::new("/Users/owner/CrazyAgent/blog");
    let all = context_section(
        Some(folder),
        AgentAccess::Sandboxed,
        &SharedFolders::default(),
    );
    assert!(all.contains("## Shared project folders"), "{all}");
    assert!(
        all.contains("/Users/owner/CrazyAgent/blog holds folders"),
        "{all}"
    );
    assert!(all.contains("read-only to you"), "{all}");
    assert!(all.contains("Secret files there"), "{all}");
    assert!(
        all.contains("Never install or download a toolchain"),
        "{all}"
    );
    let write = context_section(
        Some(folder),
        AgentAccess::OwnBranch,
        &SharedFolders {
            access: SharedAccess::ReadWrite,
            ..SharedFolders::default()
        },
    );
    assert!(write.contains("You may change files"), "{write}");
    let picked = context_section(Some(folder), AgentAccess::Sandboxed, &chosen(&["program"]));
    assert!(
        picked.contains("only these are shared with you: `program`"),
        "{picked}"
    );
    let none = context_section(
        Some(folder),
        AgentAccess::Sandboxed,
        &SharedFolders {
            scope: SharedScope::None,
            ..SharedFolders::default()
        },
    );
    assert!(none.contains("are not shared with you"), "{none}");
    assert!(!none.contains("read-only to you"), "{none}");
    let full = context_section(Some(folder), AgentAccess::Full, &SharedFolders::default());
    assert!(full.contains("owner's permissions"), "{full}");
    let long = PathBuf::from(format!("/{}", "d".repeat(1020)));
    let names: Vec<String> = (0..CHOSEN_MAX)
        .map(|index| format!("{index}{}", "n".repeat(250)))
        .collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    for level in AgentAccess::ALL {
        for shared in [SharedFolders::default(), chosen(&refs)] {
            let text = context_section(Some(&long), level, &shared);
            assert!(text.len() <= SECTION_BYTES_MAX, "{}", text.len());
        }
    }
    let unknown = context_section(None, AgentAccess::Sandboxed, &SharedFolders::default());
    assert!(
        unknown.contains("the project folder holds folders"),
        "{unknown}"
    );
}

/// Every secret name pattern folds into a Seatbelt pattern with no bracket class left
/// unclosed, and the pattern list holds what the owner asked to keep closed.
#[test]
fn the_secret_names_hold_every_kind_the_owner_asked_for() {
    for name in SECRET_NAMES {
        let folded = crate::paths::fold_case(name);
        assert_eq!(
            folded.matches('[').count(),
            folded.matches(']').count(),
            "{name}"
        );
        assert!(!folded.contains('"'), "{name}");
    }
    let joined = SECRET_NAMES.join(" ");
    for kind in [
        r"\.env",
        r"\.git-credentials",
        "keystore",
        "jks",
        "tfvars",
        "tfstate",
        "account",
        r"\.docker/config\.json",
        r"\.kube",
        "age",
        "secrets?",
        "pem",
        "p12",
        "id_(rsa|dsa|ecdsa|ed25519).*",
        r".*\.tfvars(\..*)?",
        "credentials",
        "crt",
    ] {
        assert!(joined.contains(kind), "{kind}");
    }
}

/// A part naming a Windows drive, or anything after a colon, is not a plain name, so no
/// picked folder or shared-back secret path can name another drive or replace the project
/// folder it is joined onto.
#[test]
fn a_drive_or_colon_part_is_not_a_plain_name() {
    for name in ["C:", "C:foo", "c:", "D:secret", "file:stream", ":"] {
        assert!(!plain_name(name), "{name}");
    }
    for path in ["C:/secret", "keys/C:secret", "D:", "keys/file:stream"] {
        let shared = SharedFolders {
            secrets: vec![path.to_owned()],
            ..SharedFolders::default()
        };
        assert_eq!(shared.validate(), Err(INVALID), "{path}");
    }
    assert_eq!(chosen(&["C:"]).validate(), Err(INVALID));
}

/// On Windows a shared-back secret path either is refused or joins to a path under the
/// project folder made of plain parts only, never one a drive part sends elsewhere.
#[cfg(windows)]
#[test]
fn a_joined_secret_path_stays_under_the_project_folder_on_windows() {
    fn owner_git(_root: &Path) -> Result<Option<std::process::Command>, ()> {
        Ok(None)
    }
    fn harness_folders() -> Vec<&'static str> {
        Vec::new()
    }
    fn state_folder(root: &Path) -> PathBuf {
        root.to_path_buf()
    }
    fn untracked(_root: &Path) -> Option<Vec<String>> {
        Some(Vec::new())
    }
    let hooks = crate::access::AccessHooks {
        owner_git,
        harness_folders,
        state_folder,
        untracked,
    };
    let root = tempfile::tempdir().unwrap();
    for path in ["keys/.env", "C:/secret", "keys/C:secret", "D:"] {
        let shared = SharedFolders {
            secrets: vec![path.to_owned()],
            ..SharedFolders::default()
        };
        if shared.validate().is_err() {
            continue;
        }
        let made = plan(&hooks, root.path(), &shared).unwrap();
        for revealed in &made.revealed {
            let below = revealed.strip_prefix(root.path()).unwrap();
            assert!(
                below
                    .components()
                    .all(|part| matches!(part, std::path::Component::Normal(_))),
                "{path}"
            );
        }
    }
}
