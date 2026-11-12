use super::*;
use crate::sandbox::policy::{AgentAccess, AgentPolicy};

fn chosen(names: &[&str]) -> SharedFolders {
    SharedFolders {
        scope: SharedScope::Chosen,
        chosen: names.iter().map(|name| (*name).to_owned()).collect(),
        ..SharedFolders::default()
    }
}

#[cfg(unix)]
fn git(root: &Path, args: &[&str]) {
    let mut command = std::process::Command::new(crate::research::runner::setup_git());
    command
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Shared Test")
        .env("GIT_AUTHOR_EMAIL", "shared@example.invalid")
        .env("GIT_COMMITTER_NAME", "Shared Test")
        .env("GIT_COMMITTER_EMAIL", "shared@example.invalid")
        .current_dir(root);
    let status = crate::spawn_gate::status(&mut command);
    assert!(
        status.as_ref().is_ok_and(std::process::ExitStatus::success),
        "git {args:?}: {status:?}"
    );
}

/// A repository with a tracked `src/main.rs` and `.gitignore`, an untracked `src/local.rs`,
/// a wholly untracked `program/` and ignored `build/`, an untracked `notes.txt`, and an agent
/// worktree folder under `.crazyagent/`.
#[cfg(unix)]
fn project() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src").join("main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(root.join(".gitignore"), "build/\n").unwrap();
    git(&root, &["init", "-q", "--template="]);
    git(&root, &["add", "src/main.rs", ".gitignore"]);
    git(&root, &["commit", "-q", "-m", "init"]);
    std::fs::write(root.join("src").join("local.rs"), "\n").unwrap();
    std::fs::create_dir_all(root.join("program")).unwrap();
    std::fs::write(root.join("program").join("data.csv"), "a,b\n").unwrap();
    std::fs::create_dir_all(root.join("build")).unwrap();
    std::fs::write(root.join("build").join("out.bin"), "x").unwrap();
    std::fs::write(root.join("notes.txt"), "notes\n").unwrap();
    let agent = root.join(".crazyagent").join("worktrees").join("agent-1");
    std::fs::create_dir_all(&agent).unwrap();
    std::fs::write(agent.join("work.txt"), "work\n").unwrap();
    (temp, root)
}

/// DEC-147.9: a policy saved before the setting reads All and Read-only with nothing picked,
/// a save names the choice and a pinned policy keeps it.
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

/// All with Read-only asks git nothing: a folder outside git still plans, with only the
/// shared-back secrets.
#[test]
fn all_with_read_only_plans_without_git_and_reveals_only_the_shared_back_secrets() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let shared = SharedFolders {
        secrets: vec!["config/.env".to_owned()],
        ..SharedFolders::default()
    };
    let plan = plan(&root, &shared).unwrap();
    assert!(plan.kept_out.is_empty() && plan.writes.is_empty());
    assert_eq!(plan.revealed, vec![root.join("config").join(".env")]);
}

/// None keeps out every path git does not track, ignored or not, but never `.git` or the
/// engine folder that holds the agent's own worktree, and never a tracked path.
#[cfg(unix)]
#[test]
fn none_keeps_out_every_untracked_path_but_the_engine_folders() {
    let (_temp, root) = project();
    let shared = SharedFolders {
        scope: SharedScope::None,
        ..SharedFolders::default()
    };
    let plan = plan(&root, &shared).unwrap();
    for kept in ["program", "build", "notes.txt", "src/local.rs"] {
        assert!(plan.kept_out.contains(&root.join(kept)), "{kept}: {plan:?}");
    }
    assert!(
        plan.kept_out
            .iter()
            .all(|path| !path.starts_with(root.join(".crazyagent"))
                && !path.starts_with(root.join(".git"))
                && *path != root.join("src").join("main.rs")),
        "{plan:?}"
    );
    assert!(plan.writes.is_empty());
}

/// Chosen folders keeps out only the untracked paths under folders the owner did not pick.
#[cfg(unix)]
#[test]
fn chosen_folders_keeps_out_only_the_untracked_paths_of_folders_not_picked() {
    let (_temp, root) = project();
    let plan = plan(&root, &chosen(&["program", "src"])).unwrap();
    assert!(plan.kept_out.contains(&root.join("build")), "{plan:?}");
    assert!(plan.kept_out.contains(&root.join("notes.txt")), "{plan:?}");
    assert!(!plan.kept_out.contains(&root.join("program")), "{plan:?}");
    assert!(
        !plan.kept_out.contains(&root.join("src").join("local.rs")),
        "{plan:?}"
    );
    assert!(plan.writes.is_empty(), "Read-only writes nothing: {plan:?}");
}

/// Read-write opens only shared top-level folders git does not track at all: a folder with
/// tracked files, a link and a folder not picked stay closed to writes.
#[cfg(unix)]
#[test]
fn read_write_opens_only_wholly_untracked_shared_top_level_folders() {
    let (_temp, root) = project();
    std::fs::create_dir_all(root.join("elsewhere")).unwrap();
    std::os::unix::fs::symlink(root.join("elsewhere"), root.join("linked")).unwrap();
    let all = SharedFolders {
        access: SharedAccess::ReadWrite,
        ..SharedFolders::default()
    };
    let plan = plan(&root, &all).unwrap();
    assert!(plan.writes.contains(&root.join("program")), "{plan:?}");
    assert!(plan.writes.contains(&root.join("build")), "{plan:?}");
    assert!(!plan.writes.contains(&root.join("src")), "{plan:?}");
    assert!(!plan.writes.contains(&root.join("linked")), "{plan:?}");
    assert!(!plan.writes.contains(&root.join("notes.txt")), "{plan:?}");
    assert!(plan.kept_out.is_empty(), "{plan:?}");
    let picked = SharedFolders {
        access: SharedAccess::ReadWrite,
        ..chosen(&["program"])
    };
    let plan = plan(&root, &picked).unwrap();
    assert_eq!(plan.writes, vec![root.join("program")]);
}

/// A launch with more untracked paths to keep out than the bound, or whose paths git cannot
/// list, is refused, never run as All.
#[cfg(unix)]
#[test]
fn a_launch_past_the_keep_out_bound_or_without_git_is_refused() {
    let (_temp, root) = project();
    for index in 0..=LOCAL_PATHS_MAX {
        std::fs::write(root.join(format!("loose-{index}.txt")), "x").unwrap();
    }
    let none = SharedFolders {
        scope: SharedScope::None,
        ..SharedFolders::default()
    };
    assert_eq!(plan(&root, &none), Err(TOO_MANY));
    let outside = tempfile::tempdir().unwrap();
    let missing = outside.path().canonicalize().unwrap().join("gone");
    assert_eq!(plan(&missing, &none), Err(UNLISTED));
}

/// A wider choice than All with Read-only is refused off macOS before any grant.
#[test]
fn a_wider_choice_is_refused_off_macos() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let home = root.join("home");
    let tmp = root.join("tmp");
    let project = root.join("project");
    for folder in [&home, &tmp, &project] {
        std::fs::create_dir_all(folder).unwrap();
    }
    let sandbox = ProjectSandbox::new(
        &project,
        &home,
        &tmp,
        super::super::SandboxNetwork::Only(Vec::new()),
    )
    .unwrap();
    let none = SharedFolders {
        scope: SharedScope::None,
        ..SharedFolders::default()
    };
    let result = apply(sandbox, &project, &none);
    if cfg!(target_os = "macos") {
        assert_ne!(result.err(), Some(NEEDS_MACOS));
    } else {
        assert_eq!(result.err(), Some(NEEDS_MACOS));
    }
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

/// The Chosen folders picker offers the project's top-level real folders with plain names,
/// sorted, never `.git`, `.crazyagent` or a link.
#[cfg(unix)]
#[test]
fn the_picker_offers_top_level_real_folders_without_engine_folders_or_links() {
    let (_temp, root) = project();
    std::fs::create_dir_all(root.join("elsewhere")).unwrap();
    std::os::unix::fs::symlink(root.join("elsewhere"), root.join("linked")).unwrap();
    assert_eq!(
        local_folders(&root),
        vec!["build", "elsewhere", "program", "src"]
    );
}

/// Every secret name pattern folds into a Seatbelt pattern with no bracket class left
/// unclosed, and the pattern list holds what the owner asked to keep closed.
#[test]
fn the_secret_names_hold_every_kind_the_owner_asked_for() {
    for name in SECRET_NAMES {
        let folded = crate::sandbox::fold_case(name);
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
        "id_(rsa",
        "credentials",
        "crt",
    ] {
        assert!(joined.contains(kind), "{kind}");
    }
}
