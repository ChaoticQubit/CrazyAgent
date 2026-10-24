use super::launch::{
    AccountBinding, CLAUDE_NO_MCP, CODEX_KEY_VARIABLE, LOCAL_PLACEHOLDER_KEY, LaunchError,
    LaunchRequest, SANDBOX_EXEC, Secret, TOOLS_OFF_PATH, TOOLS_ON_PATH, codex_configuration,
    failure, harness_id, validate,
};
use crate::launch_context::Installed;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

pub const KEY_FILE: &str = ".crazyagent-key";
pub const CLAUDE_PROJECT_NAME_MAX: usize = 200;
const CLAUDE_CONFIG_BYTES_MAX: u64 = 1024 * 1024;
const LOCALE: &str = "en_US.UTF-8";
const KEY_SCRIPT: &str = "CRAZYAGENT_API_KEY=\"$(/bin/cat -- \"$1\")\" || exit 1; export CRAZYAGENT_API_KEY; shift; exec \"$@\"";
const KEY_SCRIPT_NAME: &str = "crazyagent-key";
const CLAUDE_TOOLS_ON: [&str; 4] = [
    "--tools",
    "default",
    "--permission-mode",
    "bypassPermissions",
];
const CLAUDE_TOOLS_OFF: [&str; 3] = ["--tools", "", "--disable-slash-commands"];

/// What the terminal host runs for one interactive agent: the full argument list, starting
/// with the project sandbox wrapper, the working folder and the environment, plus where the
/// harness keeps its own session record. No secret is ever in `argv` or `env`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InteractiveCommand {
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
    pub transcript: Transcript,
}

/// Where a harness writes its own session record, which the engine reads for status,
/// replies and DELEGATE lines instead of reading the screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Transcript {
    CodexRollouts { sessions: PathBuf },
    ClaudeSession { file: PathBuf },
    Unavailable,
}

/// Whether a harness has an interactive app the engine can run in a terminal pane.
/// `HeadlessOnly` keeps the existing piped launch and carries the plain reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Support {
    Terminal,
    HeadlessOnly { reason: &'static str },
}

/// What one run adds to a launch request: the session id the harness records under and
/// the key file `write_key` made for a keyed account, which only its path reaches argv.
#[derive(Clone, Copy, Debug)]
pub struct Run<'a> {
    pub session_id: &'a str,
    pub key_file: Option<&'a Path>,
}

/// One harness without a CrazyAgent launch adapter: its interactive subcommand and the
/// flag that picks a model, when the vendor documents one. The owner signs in inside the pane.
struct Plain {
    id: &'static str,
    subcommand: &'static [&'static str],
    model_flag: Option<&'static str>,
}

const PLAIN: [Plain; 20] = [
    Plain {
        id: "opencode",
        subcommand: &[],
        model_flag: Some("--model"),
    },
    Plain {
        id: "gemini-cli",
        subcommand: &[],
        model_flag: Some("--model"),
    },
    Plain {
        id: "cursor-agent",
        subcommand: &[],
        model_flag: Some("--model"),
    },
    Plain {
        id: "fx",
        subcommand: &[],
        model_flag: None,
    },
    Plain {
        id: "antigravity",
        subcommand: &[],
        model_flag: None,
    },
    Plain {
        id: "openclaw",
        subcommand: &["tui"],
        model_flag: None,
    },
    Plain {
        id: "pi",
        subcommand: &[],
        model_flag: Some("--model"),
    },
    Plain {
        id: "amp",
        subcommand: &[],
        model_flag: None,
    },
    Plain {
        id: "goose",
        subcommand: &["session"],
        model_flag: None,
    },
    Plain {
        id: "aider",
        subcommand: &[],
        model_flag: Some("--model"),
    },
    Plain {
        id: "qwen-code",
        subcommand: &[],
        model_flag: Some("--model"),
    },
    Plain {
        id: "crush",
        subcommand: &[],
        model_flag: None,
    },
    Plain {
        id: "droid",
        subcommand: &[],
        model_flag: None,
    },
    Plain {
        id: "kiro",
        subcommand: &["chat"],
        model_flag: None,
    },
    Plain {
        id: "copilot",
        subcommand: &[],
        model_flag: Some("--model"),
    },
    Plain {
        id: "cline",
        subcommand: &[],
        model_flag: None,
    },
    Plain {
        id: "continue",
        subcommand: &[],
        model_flag: None,
    },
    Plain {
        id: "openhands",
        subcommand: &[],
        model_flag: None,
    },
    Plain {
        id: "auggie",
        subcommand: &[],
        model_flag: None,
    },
    Plain {
        id: "kimi",
        subcommand: &[],
        model_flag: None,
    },
];

/// Tells whether a catalog harness runs as its own interactive app in a terminal pane.
/// Input: a catalog id or a connection harness name. Output: `Terminal`, or `HeadlessOnly`
/// with the reason for RCLI, which runs only as a master, and for unknown names.
pub fn support(harness: &str) -> Support {
    match super::spec(harness).map(|spec| spec.id) {
        Some("codex" | "claude-code") => Support::Terminal,
        Some(id) if PLAIN.iter().any(|plain| plain.id == id) => Support::Terminal,
        Some("rcli") => Support::HeadlessOnly {
            reason: "runs only as a CrazyAgent master",
        },
        _ => Support::HeadlessOnly {
            reason: "not in the harness catalog",
        },
    }
}

/// Builds what the terminal host runs for one interactive agent: the harness's own app inside
/// the project sandbox, with its account home and, for Codex and Claude Code, the installed
/// project context. A key reaches the harness only through the key file in `run`.
/// Inputs: the launch request, the installed context and the run. Output: the command, or a
/// plain refusal code.
pub fn command(
    request: &LaunchRequest,
    installed: Option<&Installed>,
    run: &Run<'_>,
) -> Result<InteractiveCommand, LaunchError> {
    validate(request)?;
    if request.account.key().is_some() != run.key_file.is_some() {
        return Err(failure("launch_key_file_invalid"));
    }
    let key_file = run.key_file.map(plain_key_file).transpose()?;
    let home = text(request.account.home())?;
    let cwd = request
        .working_dir
        .canonicalize()
        .map_err(|_| failure("launch_working_dir_unavailable"))?;
    let profile = request
        .sandbox_profile
        .as_deref()
        .ok_or_else(|| failure("launch_sandbox_required"))?;
    let mut argv = vec![SANDBOX_EXEC.to_owned(), "-f".to_owned(), text(profile)?];
    let mut env = vec![
        ("HOME".to_owned(), home.clone()),
        (
            "PATH".to_owned(),
            if request.tools {
                TOOLS_ON_PATH
            } else {
                TOOLS_OFF_PATH
            }
            .to_owned(),
        ),
        ("LANG".to_owned(), LOCALE.to_owned()),
        ("LC_ALL".to_owned(), LOCALE.to_owned()),
    ];
    let executable = text(&request.executable)?;
    let transcript = match harness_id(request) {
        "codex" => {
            if let Some(key_file) = &key_file {
                assert!(KEY_SCRIPT.starts_with(CODEX_KEY_VARIABLE));
                argv.extend(
                    [
                        "/bin/sh",
                        "-c",
                        KEY_SCRIPT,
                        KEY_SCRIPT_NAME,
                        key_file.as_str(),
                    ]
                    .map(str::to_owned),
                );
            }
            argv.push(executable);
            argv.extend(codex(request, installed.is_some(), &cwd)?);
            env.extend([
                ("CODEX_HOME".to_owned(), home.clone()),
                ("TMPDIR".to_owned(), home),
            ]);
            Transcript::CodexRollouts {
                sessions: request.account.home().join("sessions"),
            }
        }
        "claude-code" => {
            argv.push(executable);
            argv.extend(claude(
                request,
                installed,
                run.session_id,
                key_file.as_deref(),
            )?);
            env.extend(claude_environment(request, &home));
            let folder = claude_project_folder(&cwd)
                .ok_or_else(|| failure("launch_working_dir_too_long"))?;
            Transcript::ClaudeSession {
                file: request
                    .account
                    .home()
                    .join("projects")
                    .join(folder)
                    .join(format!("{}.jsonl", session_id_checked(run.session_id)?)),
            }
        }
        id => {
            argv.push(executable);
            argv.extend(plain(id, request, installed)?);
            Transcript::Unavailable
        }
    };
    let built = InteractiveCommand {
        argv,
        cwd,
        env,
        transcript,
    };
    if let Some(key) = request.account.key() {
        assert!(!exposes(&built, key));
    }
    assert_eq!(built.argv[0], SANDBOX_EXEC);
    Ok(built)
}

fn exposes(command: &InteractiveCommand, key: &Secret) -> bool {
    let key = key.reveal();
    command.argv.iter().any(|argument| argument.contains(key))
        || command
            .env
            .iter()
            .any(|(name, value)| name.contains(key) || value.contains(key))
}

fn text(path: &Path) -> Result<String, LaunchError> {
    path.to_str()
        .filter(|text| !text.chars().any(char::is_control))
        .map(str::to_owned)
        .ok_or_else(|| failure("launch_path_invalid"))
}

fn plain_key_file(path: &Path) -> Result<String, LaunchError> {
    let file = text(path)?;
    if !path.is_absolute() || file.contains('\'') {
        return Err(failure("launch_key_file_invalid"));
    }
    Ok(file)
}

fn codex(request: &LaunchRequest, context: bool, cwd: &Path) -> Result<Vec<String>, LaunchError> {
    let folder = serde_json::to_string(&text(cwd)?)
        .map_err(|_| failure("launch_working_dir_unavailable"))?;
    let mut arguments = Vec::new();
    for setting in codex_configuration(request, context).into_iter().chain([
        "check_for_update_on_startup=false".to_owned(),
        format!("projects={{{folder}={{trust_level=\"trusted\"}}}}"),
    ]) {
        arguments.push("-c".to_owned());
        arguments.push(setting);
    }
    Ok(arguments)
}

fn claude(
    request: &LaunchRequest,
    installed: Option<&Installed>,
    session_id: &str,
    key_file: Option<&str>,
) -> Result<Vec<String>, LaunchError> {
    let mut settings = serde_json::json!({
        "disableAllHooks": true,
        "skipDangerousModePermissionPrompt": true,
    });
    if let Some(key_file) = key_file {
        settings["apiKeyHelper"] = format!("/bin/cat '{key_file}'").into();
    }
    let mut arguments = vec![
        "--strict-mcp-config".to_owned(),
        "--setting-sources".to_owned(),
        String::new(),
        "--settings".to_owned(),
        settings.to_string(),
        "--session-id".to_owned(),
        session_id_checked(session_id)?.to_owned(),
    ];
    match installed {
        Some(installed) => {
            for argument in installed.arguments() {
                arguments.push(
                    argument
                        .to_str()
                        .ok_or_else(|| failure("launch_path_invalid"))?
                        .to_owned(),
                );
            }
        }
        None => arguments.extend(CLAUDE_NO_MCP.map(str::to_owned)),
    }
    if let Some(model) = &request.model {
        arguments.extend(["--model".to_owned(), model.clone()]);
    }
    if request.tools {
        arguments.extend(CLAUDE_TOOLS_ON.map(str::to_owned));
    } else {
        arguments.extend(CLAUDE_TOOLS_OFF.map(str::to_owned));
    }
    if !matches!(request.account, AccountBinding::Subscription { .. }) {
        arguments.push("--bare".to_owned());
    }
    Ok(arguments)
}

fn claude_environment(request: &LaunchRequest, home: &str) -> Vec<(String, String)> {
    let mut environment = vec![
        ("CLAUDE_CONFIG_DIR".to_owned(), home.to_owned()),
        (
            "CLAUDE_SECURESTORAGE_CONFIG_DIR".to_owned(),
            home.to_owned(),
        ),
        ("DISABLE_TELEMETRY".to_owned(), "1".to_owned()),
        (
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".to_owned(),
            "1".to_owned(),
        ),
    ];
    if request.tools {
        environment.push(("SHELL".to_owned(), "/bin/zsh".to_owned()));
    }
    if matches!(request.account, AccountBinding::Subscription { .. }) {
        return environment;
    }
    if request.account.key().is_none() {
        environment.push((
            "ANTHROPIC_API_KEY".to_owned(),
            LOCAL_PLACEHOLDER_KEY.to_owned(),
        ));
    }
    if let Some(base_url) = request.account.base_url() {
        environment.push(("ANTHROPIC_BASE_URL".to_owned(), base_url.to_owned()));
    }
    if let Some(model) = &request.model {
        environment.push(("ANTHROPIC_SMALL_FAST_MODEL".to_owned(), model.clone()));
    }
    environment
}

fn plain(
    id: &str,
    request: &LaunchRequest,
    installed: Option<&Installed>,
) -> Result<Vec<String>, LaunchError> {
    let harness = PLAIN
        .iter()
        .find(|plain| plain.id == id)
        .ok_or_else(|| failure("launch_harness_unsupported"))?;
    if !matches!(request.account, AccountBinding::Subscription { .. }) {
        return Err(failure("launch_account_unsupported"));
    }
    if !request.tools {
        return Err(failure("launch_tools_off_unsupported"));
    }
    if installed.is_some() {
        return Err(failure("launch_context_unsupported"));
    }
    let mut arguments: Vec<String> = harness
        .subcommand
        .iter()
        .map(|part| (*part).to_owned())
        .collect();
    if let Some(model) = &request.model {
        let flag = harness
            .model_flag
            .ok_or_else(|| failure("launch_model_unsupported"))?;
        arguments.extend([flag.to_owned(), model.clone()]);
    }
    Ok(arguments)
}

/// Makes a fresh session id in the UUID form Claude Code requires for `--session-id`.
/// Output: a random version 4 UUID such as `0f8c2c1e-5a7b-4c3d-9e2f-1a2b3c4d5e6f`.
pub fn session_id() -> Result<String, LaunchError> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| failure("launch_session_id_unavailable"))?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    let id = format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    );
    assert!(session_id_checked(&id).is_ok());
    Ok(id)
}

fn session_id_checked(id: &str) -> Result<&str, LaunchError> {
    let shaped = id.len() == 36
        && id.char_indices().all(|(index, character)| match index {
            8 | 13 | 18 | 23 => character == '-',
            _ => character.is_ascii_digit() || ('a'..='f').contains(&character),
        });
    shaped
        .then_some(id)
        .ok_or_else(|| failure("launch_session_id_invalid"))
}

/// Names the folder under `<config>/projects/` where Claude Code keeps the transcripts of
/// sessions started in `cwd`: every character other than an ASCII letter or digit becomes
/// `-`. Input: the canonical working folder. Output: the name, or none past 200 characters,
/// where Claude Code adds a hash of its own.
pub fn claude_project_folder(cwd: &Path) -> Option<String> {
    let name: String = cwd
        .to_str()?
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect();
    (name.chars().count() <= CLAUDE_PROJECT_NAME_MAX).then_some(name)
}

/// Writes a keyed account's key into `<home>/.crazyagent-key`, readable only by this user,
/// replacing an earlier one, so the key reaches the harness without argv or environment.
/// Inputs: the account home, already checked by the launch, and the key. Output: the path.
#[cfg(unix)]
pub fn write_key(home: &Path, key: &Secret) -> Result<PathBuf, LaunchError> {
    super::launch::validate_home(home)?;
    let path = home.join(KEY_FILE);
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            std::fs::remove_file(&path).map_err(|_| failure("launch_key_file_unavailable"))?;
        }
        Ok(_) => return Err(failure("launch_key_file_unsafe")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(failure("launch_key_file_unavailable")),
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .map_err(|_| failure("launch_key_file_unavailable"))?;
    file.write_all(key.reveal().as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|_| failure("launch_key_file_unavailable"))?;
    let metadata = file
        .metadata()
        .map_err(|_| failure("launch_key_file_unavailable"))?;
    assert_eq!(metadata.mode() & 0o777, 0o600);
    assert_eq!(metadata.len(), key.reveal().len() as u64);
    Ok(path)
}

/// Refuses to write an account key file where no owner-only 0600 file can be made.
/// Takes the account home and key, both unused. Returns `launch_key_file_unavailable`.
#[cfg(not(unix))]
pub fn write_key(_home: &Path, _key: &Secret) -> Result<PathBuf, LaunchError> {
    Err(failure("launch_key_file_unavailable"))
}

/// Refuses to answer Claude Code's first-run questions where its config file cannot be
/// made owner-only. Takes the config home and project folder, both unused.
/// Returns `launch_claude_config_unavailable`.
#[cfg(not(unix))]
pub fn prepare_claude_home(_home: &Path, _project: &Path) -> Result<(), LaunchError> {
    Err(failure("launch_claude_config_unavailable"))
}

/// Answers Claude Code's first-run questions in an agent's own config home before its app
/// starts: onboarding is marked done and the project folder trusted, keeping every other
/// setting, such as a sign-in. Inputs: the config home and the project folder.
/// Output: Ok, or a plain code when the existing file is unsafe or not a JSON object.
#[cfg(unix)]
pub fn prepare_claude_home(home: &Path, project: &Path) -> Result<(), LaunchError> {
    super::launch::validate_home(home)?;
    let project = text(project)?;
    let path = home.join(".claude.json");
    let mut config = match std::fs::symlink_metadata(&path) {
        Ok(metadata)
            if metadata.is_file()
                && !metadata.file_type().is_symlink()
                && metadata.len() <= CLAUDE_CONFIG_BYTES_MAX =>
        {
            let bytes =
                std::fs::read(&path).map_err(|_| failure("launch_claude_config_unavailable"))?;
            serde_json::from_slice::<serde_json::Value>(&bytes)
                .map_err(|_| failure("launch_claude_config_invalid"))?
        }
        Ok(_) => return Err(failure("launch_claude_config_unsafe")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(_) => return Err(failure("launch_claude_config_unavailable")),
    };
    let object = config
        .as_object_mut()
        .ok_or_else(|| failure("launch_claude_config_invalid"))?;
    object.insert("hasCompletedOnboarding".into(), true.into());
    let projects = object
        .entry("projects")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| failure("launch_claude_config_invalid"))?;
    let entry = projects
        .entry(project)
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| failure("launch_claude_config_invalid"))?;
    entry.insert("hasTrustDialogAccepted".into(), true.into());
    let encoded =
        serde_json::to_vec_pretty(&config).map_err(|_| failure("launch_claude_config_invalid"))?;
    let staged = home.join(".claude.json.crazyagent");
    let _ = std::fs::remove_file(&staged);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&staged)
        .map_err(|_| failure("launch_claude_config_unavailable"))?;
    file.write_all(&encoded)
        .and_then(|()| file.sync_all())
        .map_err(|_| failure("launch_claude_config_unavailable"))?;
    std::fs::rename(&staged, &path).map_err(|_| failure("launch_claude_config_unavailable"))
}

#[cfg(test)]
mod tests;
