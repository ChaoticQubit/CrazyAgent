use super::master_command;
use crate::engine::claude_host::MasterTools;
use crate::provider_retry::ProviderRetry;
use std::collections::BTreeMap;
use std::path::Path;

const CODEX: &str = r"C:\registered\codex.exe";
const HOME: &str = r"C:\private\profile";
const WORKTREE: &str = r"C:\leased\worktree";

/// V244-WIN-MASTER S3 (D4): the Windows Codex master command carries the Unix settings and
/// environment, less `PATH` (the boundary's System32 one stays) and the process group (a
/// job holds the tree), and records every folder of its walk untrusted. It used to be
/// refused as `master_host_unavailable`.
#[test]
fn a_windows_codex_master_command_carries_the_unix_settings_without_a_path() {
    let command = master_command(
        Path::new(CODEX),
        Path::new(HOME),
        Path::new(WORKTREE),
        "admitted-model",
        None,
        ProviderRetry::RetryBriefly,
    )
    .unwrap();
    assert_eq!(command.get_program(), CODEX);
    assert_eq!(command.get_current_dir(), Some(Path::new(WORKTREE)));
    let args = command
        .get_args()
        .map(|arg| arg.to_str().unwrap())
        .collect::<Vec<_>>();
    for required in [
        "model=\"admitted-model\"",
        "model_provider=\"openai\"",
        "cli_auth_credentials_store=\"file\"",
        "sandbox_mode=\"read-only\"",
        "approval_policy=\"never\"",
        "features.shell_tool=false",
        "features.apply_patch_freeform=false",
        "features.unified_exec=false",
        "web_search=\"disabled\"",
        "mcp_servers={}",
    ] {
        assert!(
            args.windows(2).any(|pair| pair == ["-c", required]),
            "{required}"
        );
    }
    assert!(args.ends_with(&["app-server", "--listen", "stdio://"]));
    let environment = command
        .get_envs()
        .map(|(key, value)| (key.to_str().unwrap(), value.unwrap().to_str().unwrap()))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        environment,
        BTreeMap::from([
            ("CODEX_HOME", HOME),
            ("HOME", HOME),
            ("LANG", "C"),
            ("LC_ALL", "C"),
            ("TMPDIR", HOME),
        ])
    );
    let trust = args
        .iter()
        .find(|arg| arg.starts_with("projects="))
        .unwrap();
    let parsed: toml_edit::DocumentMut = trust.parse().unwrap();
    for folder in Path::new(WORKTREE).ancestors() {
        assert_eq!(
            parsed["projects"][folder.to_str().unwrap()]["trust_level"].as_str(),
            Some("untrusted"),
            "{folder:?}"
        );
    }
}

/// A Windows Codex master command whose program, home or folder is not a plain drive path,
/// or whose model could carry a setting, is refused as `master_runtime_binding`, the paths
/// by the rule the boundary's `wrap` holds them to.
#[test]
fn a_windows_codex_master_command_refuses_a_path_that_is_not_a_plain_drive_path() {
    let refused = |executable: &str, home: &str, cwd: &str, model: &str| {
        master_command(
            Path::new(executable),
            Path::new(home),
            Path::new(cwd),
            model,
            None,
            ProviderRetry::RetryBriefly,
        )
        .map(drop)
        .map_err(|error| error.0)
    };
    for unplain in [
        "codex.exe",
        r"\registered\codex.exe",
        r"C:registered\codex.exe",
        r"C:\registered\..\codex.exe",
        r"\\?\C:\registered\codex.exe",
        r"\\server\share\codex.exe",
        "/registered/codex",
    ] {
        for (executable, home, cwd) in [
            (unplain, HOME, WORKTREE),
            (CODEX, unplain, WORKTREE),
            (CODEX, HOME, unplain),
        ] {
            assert_eq!(
                refused(executable, home, cwd, "model"),
                Err("master_runtime_binding".to_owned()),
                "{executable} {home} {cwd}"
            );
        }
    }
    for model in ["", "model\"\nfeatures.shell_tool=true", "--profile"] {
        assert_eq!(
            refused(CODEX, HOME, WORKTREE, model),
            Err("master_runtime_binding".to_owned()),
            "{model}"
        );
    }
}

/// A Windows Codex master with its tools on stays refused as `master_host_unavailable`
/// until its team server's mailbox works on Windows (S8).
#[test]
fn a_windows_codex_master_command_with_its_tools_on_stays_refused() {
    let tools = MasterTools {
        engine: Path::new(r"C:\engine\crazyagent.exe"),
        drop: Path::new(r"C:\turn\tmp\drop"),
        mailbox: Path::new(r"C:\turn\tmp\mailbox"),
        off: &[],
    };
    assert_eq!(
        master_command(
            Path::new(CODEX),
            Path::new(HOME),
            Path::new(WORKTREE),
            "admitted-model",
            Some(tools),
            ProviderRetry::RetryBriefly,
        )
        .map(drop)
        .map_err(|error| error.0),
        Err("master_host_unavailable".to_owned())
    );
}
