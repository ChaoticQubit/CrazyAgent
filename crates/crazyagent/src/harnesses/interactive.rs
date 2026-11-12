#[cfg(unix)]
use super::launch::CLAUDE_NATIVE_TEAM_TOOLS;
use super::launch::{
    AccountBinding, CLAUDE_DENY_FLAG, CLAUDE_MCP_CONNECT_LIMIT, CLAUDE_NO_MCP, CLAUDE_TOOLS_OFF,
    CODEX_NO_USER_INPUT, LOCAL_PLACEHOLDER_KEY, LaunchError, LaunchRequest, SANDBOX_EXEC, Secret,
    TOOLS_OFF_PATH, TOOLS_ON_PATH, claude_bare, claude_denied, codex_launch_settings, failure,
    harness_id, key_file_text, validate,
};
use crate::coord::settings::{HarnessKind, TerminalDisplay};
use crate::launch_context::{Installed, NativeTeam};
use crate::sandbox::toolchains::{self, ToolHomes};
use std::ffi::OsStr;
#[cfg(unix)]
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

pub const KEY_FILE: &str = ".crazyagent-key";
pub const CLAUDE_PROJECT_NAME_MAX: usize = 200;
#[cfg(unix)]
const CLAUDE_CONFIG_BYTES_MAX: u64 = 1024 * 1024;
#[cfg(unix)]
const CLAUDE_KEY_TAIL_CHARS: usize = 20;
const LOCALE: &str = "en_US.UTF-8";
/// Runs a tools-on Codex agent as an app-server and a TUI attached to it with `--remote`, so
/// the engine can steer the agent's thread natively (DEC-104 K2b). The app-server starts on
/// the home's `unix://` control socket; when it has not bound within 5 s, or exits, the
/// TUI runs alone as before. Arguments: the Codex program, the conversation to resume or
/// an empty word, then the `-c` settings both take. The app-server ends with the TUI.
const CODEX_BRIDGE_SCRIPT: &str = r#"e=$1; r=$2; shift 2
s=$CODEX_HOME/app-server-control/app-server-control.sock
/bin/rm -f "$s"
"$e" app-server --listen unix:// "$@" </dev/null >/dev/null 2>&1 &
a=$!
i=0
while [ ! -S "$s" ] && kill -0 "$a" 2>/dev/null && [ "$i" -lt 50 ]; do /bin/sleep 0.1; i=$((i+1)); done
if [ -S "$s" ]; then set -- --remote unix:// "$@"; else kill "$a" 2>/dev/null; fi
if [ -n "$r" ]; then "$e" "$@" resume "$r"; else "$e" "$@"; fi
c=$?
kill "$a" 2>/dev/null
exit "$c"
"#;
/// Runs a tools-off Codex agent's TUI on its own app server instead of the shared one: the
/// shared app server keeps unified exec on, and Codex 0.164 refuses it to a session whose
/// tools-off settings turn unified exec off, so that agent would exit at its first line.
/// The engine then reaches the agent by typing instead of the K2b bridge.
const CODEX_NO_DAEMON: &str = "--no-daemon";
const CLAUDE_TOOLS_ON: [&str; 4] = [
    "--tools",
    "default",
    "--permission-mode",
    "bypassPermissions",
];
const CLAUDE_TEAMMATE_MODE: [&str; 2] = ["--teammate-mode", "tmux"];
/// Makes Claude Code 2.1.309 render its system prompt fresh on every request. Signed in, it
/// otherwise records the prompt of a conversation's first request and sends that record on
/// every resume, so a relaunch would run with the instructions of the session's first launch,
/// not the ones it installed (QR2).
const CLAUDE_PROMPT_FRESH: [&str; 2] = ["--system-prompt-snapshot", "off"];
const CLAUDE_AGENT_TEAMS: (&str, &str) = ("CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS", "1");
/// Gives a native-team lead, and through its teammate script every teammate, Claude Code's
/// task list tools. Claude Code 2.1.309 offers TaskCreate, TaskGet, TaskUpdate and TaskList
/// to a model newer than its fixed list only with this set or with `--tools` naming one, and
/// a lead without them cannot share its team's tasks (OP-3).
const CLAUDE_TODO_TOOLS: (&str, &str) = ("CLAUDE_CODE_ENABLE_TODO_TOOLS", "1");
const CLAUDE_TEAMMATE_COMMAND: &str = "CLAUDE_CODE_TEAMMATE_COMMAND";
#[cfg(unix)]
const CLAUDE_INSTRUCTIONS_FLAG: &str = "--append-system-prompt-file";
const CLAUDE_BARE: &str = "--bare";
#[cfg(unix)]
const TEAMMATE_SCRIPT_BYTES_MAX: usize = 64 * 1024;
/// The variables of a native lead's command that name a place in its home, which its
/// teammates get at the same place in their own home (TPH-2 C).
/// `CLAUDE_SECURESTORAGE_CONFIG_DIR` stays the lead's, where the engine keeps the
/// account's sign-in copy that teammates read.
#[cfg(unix)]
const TEAMMATE_HOME_VARIABLES: [&str; 4] = [
    "CLAUDE_CONFIG_DIR",
    "CLAUDE_CODE_TMPDIR",
    "HOME",
    "CARGO_HOME",
];
/// The variables of a native lead's command that name a place in the lead's own temp folder,
/// which its teammates get at the same place in the short temp folder they share instead.
#[cfg(unix)]
const TEAMMATE_TMP_VARIABLES: [&str; 2] = ["TMPDIR", "TMPPREFIX"];

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
/// replies and task blocks instead of reading the screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Transcript {
    CodexRollouts { sessions: PathBuf },
    ClaudeSession { file: PathBuf },
    Unavailable,
}

/// What one harness kind's terminal starts with for one terminal display (DEC-141): the
/// variables and the arguments its own program takes, and whether the kind can draw that
/// display at all. A kind that cannot starts as it always does, and the settings say so.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DisplaySwitches {
    pub variables: &'static [(&'static str, &'static str)],
    pub arguments: &'static [&'static str],
    pub honoured: bool,
}

/// Maps a terminal display to one harness kind's own switches, the only place a display is
/// harness-specific (DEC-141). Claude Code takes variables: Classic turns its alternate
/// screen off, which makes Claude Code 2.1.309 use its classic renderer; Fullscreen sets 3
/// rows a wheel notch, the step Claude Code picks itself for xterm.js, and asks for the
/// fullscreen renderer, which Claude Code otherwise picks per account (R8-FS). Codex takes
/// its `tui.alternate_screen` setting, `never` for Classic and `always` for Fullscreen, its
/// own view with mouse reporting on (OB-8); a `-c` setting, because the app server a tools-on
/// launch starts first takes `-c` and exits on `--no-alt-screen`. RCLI runs one turn at a
/// time with no view of its own, so it has no switches and only Classic holds for it.
/// Inputs: the kind and the display. Output: the switches.
pub fn display_switches(harness: HarnessKind, display: TerminalDisplay) -> DisplaySwitches {
    let (variables, arguments, honoured): (&'static [(&str, &str)], &'static [&str], bool) =
        match (harness, display) {
            (HarnessKind::ClaudeCode, TerminalDisplay::Classic) => {
                (&[("CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN", "1")], &[], true)
            }
            (HarnessKind::ClaudeCode, TerminalDisplay::Fullscreen) => (
                &[
                    ("CLAUDE_CODE_SCROLL_SPEED", "3"),
                    ("CLAUDE_CODE_NO_FLICKER", "1"),
                ],
                &[],
                true,
            ),
            (HarnessKind::Codex, TerminalDisplay::Classic) => {
                (&[], &["-c", "tui.alternate_screen=\"never\""], true)
            }
            (HarnessKind::Codex, TerminalDisplay::Fullscreen) => {
                (&[], &["-c", "tui.alternate_screen=\"always\""], true)
            }
            (HarnessKind::Rcli, TerminalDisplay::Classic) => (&[], &[], true),
            (HarnessKind::Rcli, TerminalDisplay::Fullscreen) => (&[], &[], false),
        };
    DisplaySwitches {
        variables,
        arguments,
        honoured,
    }
}

/// The terminal displays one harness kind can draw, in the order settings list them, for the
/// settings screens to say which kind stays as it is under the chosen display (DEC-141).
/// Input: the kind. Output: the displays.
pub fn displays(harness: HarnessKind) -> Vec<TerminalDisplay> {
    TerminalDisplay::ALL
        .into_iter()
        .filter(|display| display_switches(harness, *display).honoured)
        .collect()
}

/// Whether a harness has an interactive app the engine can run in a terminal pane.
/// `HeadlessOnly` keeps the existing piped launch and carries the plain reason.
/// `Unavailable` has an app whose replies the engine cannot read yet, so no launch works.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Support {
    Terminal,
    HeadlessOnly { reason: &'static str },
    Unavailable { reason: &'static str },
}

/// What one run adds to a launch request: the session id the harness records under, the
/// key file `write_key` made for a keyed account, which only its path reaches argv, and
/// whether the app resumes the conversation it recorded under that id instead of starting
/// one. Codex records under its own id, so a fresh Codex run does not use `session_id`.
#[derive(Clone, Copy, Debug)]
pub struct Run<'a> {
    pub session_id: &'a str,
    pub key_file: Option<&'a Path>,
    pub resume: bool,
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
/// Input: a catalog id or a connection harness name. Output: `Terminal` when the engine reads
/// its session record, `Unavailable` for the other apps until a reader exists, or
/// `HeadlessOnly` with the reason for RCLI, which runs only as a master, and unknown names.
pub fn support(harness: &str) -> Support {
    match super::spec(harness).map(|spec| spec.id) {
        Some("codex" | "claude-code") => Support::Terminal,
        Some(id) if PLAIN.iter().any(|plain| plain.id == id) => Support::Unavailable {
            reason: "CrazyAgent cannot read its replies yet",
        },
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
/// project context. Its `TMPDIR` is its short temp folder, or for a native lead the lead's
/// own folder in it, which its teammates cannot reach. A key reaches the harness only
/// through the key file in `run`.
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
    let key_file = run.key_file.map(key_file_text).transpose()?;
    let home = text(request.account.home())?;
    let cwd = request
        .working_dir
        .canonicalize()
        .map_err(|_| failure("launch_working_dir_unavailable"))?;
    let mut argv = match request.sandbox_profile.as_deref() {
        Some(profile) => vec![SANDBOX_EXEC.to_owned(), "-f".to_owned(), text(profile)?],
        None => Vec::new(),
    };
    let homes = ToolHomes::owner().map_err(|_| failure("launch_environment_refused"))?;
    let mut env = vec![
        ("HOME".to_owned(), home.clone()),
        (
            "PATH".to_owned(),
            toolchains::with_agent_git(
                request.account.home(),
                if request.tools {
                    toolchains::agent_path(&homes, TOOLS_ON_PATH)
                } else {
                    TOOLS_OFF_PATH.to_owned()
                },
            ),
        ),
        ("LANG".to_owned(), LOCALE.to_owned()),
        ("LC_ALL".to_owned(), LOCALE.to_owned()),
    ];
    if let Some(identity) = &request.git_identity {
        env.extend(
            identity
                .variables()
                .map(|(name, value)| (name.to_owned(), value.to_owned())),
        );
    }
    let executable = text(&request.executable)?;
    let native_lead =
        harness_id(request) == "claude-code" && installed.and_then(Installed::native).is_some();
    let display = installed
        .map(Installed::terminal_display)
        .unwrap_or_default();
    let transcript = match harness_id(request) {
        "codex" => {
            let resume = if run.resume {
                Some(session_id_checked(run.session_id)?.to_owned())
            } else {
                None
            };
            let settings = codex(request, installed, &cwd, key_file.as_deref(), display)?;
            if request.tools {
                argv.extend([
                    "/bin/sh".to_owned(),
                    "-c".to_owned(),
                    CODEX_BRIDGE_SCRIPT.to_owned(),
                    "codex-bridge".to_owned(),
                    executable,
                    resume.unwrap_or_default(),
                ]);
                argv.extend(settings);
            } else {
                argv.extend([executable, CODEX_NO_DAEMON.to_owned()]);
                argv.extend(settings);
                if let Some(resume) = resume {
                    argv.extend(["resume".to_owned(), resume]);
                }
            }
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
            argv.extend(claude(request, installed, run, key_file.as_deref())?);
            let team = installed.and_then(Installed::native);
            if let Some(team) = team {
                tmux_on_path(&mut env, &team.tmux)?;
            }
            let teammates = team
                .map(|_| {
                    text(&crate::launch_context::teammate_command(
                        request.account.home(),
                    ))
                })
                .transpose()?;
            env.extend(claude_environment(
                request,
                &home,
                teammates.as_deref(),
                display,
            ));
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
    for (name, value) in toolchains::agent_env(request.account.home(), &homes) {
        env.push((name.to_owned(), text(Path::new(&value))?));
    }
    if let Some(tmp) = toolchains::existing_tmp(request.account.home()) {
        let tmp = if native_lead {
            toolchains::native_lead_tmp(&tmp)
        } else {
            tmp
        };
        env.push(("TMPDIR".to_owned(), text(&tmp)?));
    }
    if let Some(port) = installed.and_then(Installed::model_proxy) {
        env.extend(
            crate::sandbox::model_proxy::variables(port)
                .map(|(name, value)| (name.to_owned(), value)),
        );
    }
    let env = agent_env(&env)?;
    let built = InteractiveCommand {
        argv,
        cwd,
        env,
        transcript,
    };
    if let Some(key) = request.account.key() {
        assert!(!exposes(&built, key));
    }
    assert_eq!(built.argv[0] == SANDBOX_EXEC, !request.unconfined);
    Ok(built)
}

/// Passes a pane's variables through the one agent environment builder, so a pane gets
/// the same refusals and git settings as every other agent process.
/// Input: the variables in order. Output: them with [`crate::sandbox::AGENT_GIT_ENV`]
/// appended, or `launch_environment_refused`.
fn agent_env(env: &[(String, String)]) -> Result<Vec<(String, String)>, LaunchError> {
    let base: Vec<(&str, &OsStr)> = env
        .iter()
        .map(|(name, value)| (name.as_str(), OsStr::new(value)))
        .collect();
    crate::sandbox::agent_env(&base, std::iter::empty())
        .map_err(|_| failure("launch_environment_refused"))?
        .into_iter()
        .map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| failure("launch_environment_refused"))
}

fn exposes(command: &InteractiveCommand, key: &Secret) -> bool {
    let key = key.reveal();
    command.argv.iter().any(|argument| argument.contains(key))
        || command
            .env
            .iter()
            .any(|(name, value)| name.contains(key) || value.contains(key))
}

/// The same place under another folder, as text: the folder itself for an empty rest, else
/// the rest joined to it. Inputs: the folder and the rest of a path. Output: the text, or
/// `launch_path_invalid`.
#[cfg(unix)]
fn moved(to: &Path, rest: &Path) -> Result<String, LaunchError> {
    if rest.as_os_str().is_empty() {
        text(to)
    } else {
        text(&to.join(rest))
    }
}

fn text(path: &Path) -> Result<String, LaunchError> {
    path.to_str()
        .filter(|text| !text.chars().any(char::is_control))
        .map(str::to_owned)
        .ok_or_else(|| failure("launch_path_invalid"))
}

/// The settings a Codex terminal launch runs under. A repository that brings a `.codex`
/// folder runs untrusted so Codex loads nothing from it, any other stays trusted so it
/// never meets Codex's folder access screen; approval and sandbox come from these settings
/// either way. The project's terminal display adds Codex's own switches for it last. Inputs:
/// the request, the installed context, if any, the working folder, the key file and the
/// display. Output: the arguments.
fn codex(
    request: &LaunchRequest,
    installed: Option<&Installed>,
    cwd: &Path,
    key_file: Option<&str>,
    display: TerminalDisplay,
) -> Result<Vec<String>, LaunchError> {
    let trust = super::codex_folder::terminal_entry(cwd)?;
    let mut arguments = Vec::new();
    for setting in codex_launch_settings(request, installed, key_file)
        .into_iter()
        .chain((!request.tools).then(|| CODEX_NO_USER_INPUT.to_owned()))
        .chain(["check_for_update_on_startup=false".to_owned(), trust])
    {
        arguments.push("-c".to_owned());
        arguments.push(setting);
    }
    arguments.extend(
        display_switches(HarnessKind::Codex, display)
            .arguments
            .iter()
            .map(|argument| (*argument).to_owned()),
    );
    Ok(arguments)
}

fn claude(
    request: &LaunchRequest,
    installed: Option<&Installed>,
    run: &Run<'_>,
    key_file: Option<&str>,
) -> Result<Vec<String>, LaunchError> {
    let mut settings = serde_json::json!({
        "attribution": {"commit": "", "pr": ""},
        "disableAllHooks": true,
        "includeCoAuthoredBy": false,
        "skipDangerousModePermissionPrompt": true,
    });
    if let Some(key_file) = key_file {
        settings["apiKeyHelper"] = format!("/bin/cat '{key_file}'").into();
    }
    if installed.is_some_and(Installed::runs_hooks) {
        let removed = settings
            .as_object_mut()
            .and_then(|fields| fields.remove("disableAllHooks"));
        assert_eq!(removed, Some(serde_json::Value::Bool(true)));
    }
    let mut arguments = vec![
        "--strict-mcp-config".to_owned(),
        "--setting-sources".to_owned(),
        String::new(),
        "--settings".to_owned(),
        settings.to_string(),
        if run.resume {
            "--resume"
        } else {
            "--session-id"
        }
        .to_owned(),
        session_id_checked(run.session_id)?.to_owned(),
    ];
    arguments.extend(CLAUDE_PROMPT_FRESH.map(str::to_owned));
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
    let native = installed.is_some_and(Installed::native_team);
    if native && !request.tools {
        return Err(failure("launch_native_team_needs_tools"));
    }
    if request.tools {
        arguments.extend(CLAUDE_TOOLS_ON.map(str::to_owned));
    } else {
        arguments.extend(CLAUDE_TOOLS_OFF.map(str::to_owned));
    }
    let denied = claude_denied(request.tools, native, installed);
    if !denied.is_empty() {
        arguments.push(CLAUDE_DENY_FLAG.to_owned());
        arguments.extend(denied);
    }
    if native {
        arguments.extend(CLAUDE_TEAMMATE_MODE.map(str::to_owned));
    }
    if claude_bare(&request.account) {
        arguments.push(CLAUDE_BARE.to_owned());
    }
    Ok(arguments)
}

/// Builds Claude Code's own variables for one launch, with its switches for the project's
/// terminal display. A native-team lead also gets agent teams and the task list tools turned
/// on and the command its teammates run. Inputs: the request, the agent home, for a
/// native-team lead the teammate command path, and the display. Output: the variables.
fn claude_environment(
    request: &LaunchRequest,
    home: &str,
    teammates: Option<&str>,
    display: TerminalDisplay,
) -> Vec<(String, String)> {
    let mut environment = vec![
        ("CLAUDE_CONFIG_DIR".to_owned(), home.to_owned()),
        (
            "CLAUDE_SECURESTORAGE_CONFIG_DIR".to_owned(),
            home.to_owned(),
        ),
        ("CLAUDE_CODE_TMPDIR".to_owned(), home.to_owned()),
        ("DISABLE_TELEMETRY".to_owned(), "1".to_owned()),
        (
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".to_owned(),
            "1".to_owned(),
        ),
    ];
    environment.extend(
        display_switches(HarnessKind::ClaudeCode, display)
            .variables
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned())),
    );
    if request.tools {
        environment.push(("SHELL".to_owned(), "/bin/zsh".to_owned()));
        environment.push((
            CLAUDE_MCP_CONNECT_LIMIT.0.to_owned(),
            CLAUDE_MCP_CONNECT_LIMIT.1.to_owned(),
        ));
    }
    if let Some(command) = teammates {
        assert!(request.tools);
        environment.push((
            CLAUDE_AGENT_TEAMS.0.to_owned(),
            CLAUDE_AGENT_TEAMS.1.to_owned(),
        ));
        environment.push((
            CLAUDE_TODO_TOOLS.0.to_owned(),
            CLAUDE_TODO_TOOLS.1.to_owned(),
        ));
        environment.push((CLAUDE_TEAMMATE_COMMAND.to_owned(), command.to_owned()));
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

/// Tells whether a stored conversation id has the form Codex and Claude Code resume by.
/// Input: the id. Output: true for a lowercase UUID.
pub fn resumable(id: &str) -> bool {
    session_id_checked(id).is_ok()
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

/// Puts the folder of the tmux program a native-team lead splits teammate panes with at the
/// end of its PATH, unless PATH already names it, so no other program in that folder hides
/// one the agent finds first. Inputs: the variables and the tmux program. Output: Ok, or
/// `launch_path_invalid`.
fn tmux_on_path(env: &mut [(String, String)], tmux: &Path) -> Result<(), LaunchError> {
    let folder = tmux
        .parent()
        .map(text)
        .transpose()?
        .filter(|folder| !folder.is_empty() && !folder.contains(':'))
        .ok_or_else(|| failure("launch_path_invalid"))?;
    let (_, path) = env
        .iter_mut()
        .find(|(name, _)| name == "PATH")
        .ok_or_else(|| failure("launch_path_invalid"))?;
    if !path.split(':').any(|entry| entry == folder) {
        path.push(':');
        path.push_str(&folder);
    }
    assert!(path.split(':').any(|entry| entry == folder));
    Ok(())
}

/// Writes the command a native-team lead's Claude Code runs for each teammate
/// (`CLAUDE_CODE_TEAMMATE_COMMAND`): the lead's executable and variables, the teammates'
/// instruction file and `--bare`, which Claude Code does not pass on, and Agent and TeamCreate
/// denied. Claude Code 2.1.309 appends the lead's `--mcp-config`, `--strict-mcp-config`,
/// `--plugin-dir`, `--settings`, deny rules and tools itself, so the script repeats none of
/// them and no MCP server loads twice. It runs in the lead's pane, inside the lead's sandbox.
/// The variables naming the lead's home name the teammates' own home instead (TPH-2 C), and
/// those naming the lead's own temp folder name the short temp folder the teammates share;
/// a script that would still name the lead's own temp folder is refused.
/// Inputs: the agent home, the lead's built command, its request and what its teammates get.
/// Output: Ok, or a plain code.
#[cfg(unix)]
pub fn write_teammate_command(
    home: &Path,
    built: &InteractiveCommand,
    request: &LaunchRequest,
    team: &NativeTeam,
) -> Result<(), LaunchError> {
    let refused = || failure("launch_teammate_command_invalid");
    let start = if built.argv.first().map(String::as_str) == Some(SANDBOX_EXEC) {
        3
    } else {
        0
    };
    let executable = built.argv.get(start).ok_or_else(refused)?;
    let instructions = text(&team.instructions)?;
    let mates = crate::launch_context::mates_home(home);
    let tmp =
        toolchains::existing_tmp(home).map(|shared| (toolchains::native_lead_tmp(&shared), shared));
    let mut words = vec!["/usr/bin/env".to_owned()];
    for (name, value) in &built.env {
        let path = Path::new(value);
        let value = if let Ok(rest) = path.strip_prefix(home)
            && TEAMMATE_HOME_VARIABLES.contains(&name.as_str())
        {
            moved(&mates, rest)?
        } else if let Some((own, shared)) = &tmp
            && let Ok(rest) = path.strip_prefix(own)
            && TEAMMATE_TMP_VARIABLES.contains(&name.as_str())
        {
            moved(shared, rest)?
        } else {
            value.clone()
        };
        words.push(format!("{name}={value}"));
    }
    words.extend([
        executable.clone(),
        CLAUDE_INSTRUCTIONS_FLAG.to_owned(),
        instructions,
    ]);
    if built.argv[start..]
        .iter()
        .any(|argument| argument == CLAUDE_BARE)
    {
        words.push(CLAUDE_BARE.to_owned());
    }
    words.push(CLAUDE_DENY_FLAG.to_owned());
    words.extend(CLAUDE_NATIVE_TEAM_TOOLS.map(str::to_owned));
    if let Some((own, _)) = &tmp {
        let own = text(own)?;
        if words.iter().any(|word| word.contains(&own)) {
            return Err(refused());
        }
    }
    let mut script = String::from("#!/bin/sh\nexec");
    for word in &words {
        if word.is_empty() || word.chars().any(char::is_control) {
            return Err(refused());
        }
        script.push_str(" '");
        script.push_str(&word.replace('\'', "'\\''"));
        script.push('\'');
    }
    script.push_str(" \"$@\"\n");
    if script.len() > TEAMMATE_SCRIPT_BYTES_MAX {
        return Err(refused());
    }
    if let Some(key) = request.account.key() {
        assert!(!script.contains(key.reveal()));
    }
    let path = crate::launch_context::teammate_command(home);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o700)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC).bits() as i32)
        .open(&path)
        .map_err(|_| failure("launch_teammate_command_unavailable"))?;
    file.write_all(script.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|_| failure("launch_teammate_command_unavailable"))?;
    let metadata = file
        .metadata()
        .map_err(|_| failure("launch_teammate_command_unavailable"))?;
    assert_eq!(metadata.mode() & 0o777, 0o700);
    assert_eq!(metadata.len(), script.len() as u64);
    Ok(())
}

/// Refuses a native-team lead where no tmux teammate panes exist. Takes the agent home,
/// command, request and team, all unused. Returns `launch_teammate_command_unavailable`.
#[cfg(not(unix))]
pub fn write_teammate_command(
    _home: &Path,
    _built: &InteractiveCommand,
    _request: &LaunchRequest,
    _team: &NativeTeam,
) -> Result<(), LaunchError> {
    Err(failure("launch_teammate_command_unavailable"))
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
/// starts: onboarding is marked done, the project folder trusted and the local placeholder
/// key approved (Claude Code records a key by its last 20 characters), keeping every other
/// setting, such as a sign-in. Inputs: the config home and the project folder.
/// Output: Ok, or a plain code when the existing file is unsafe or not a JSON object.
#[cfg(unix)]
pub fn prepare_claude_home(home: &Path, project: &Path) -> Result<(), LaunchError> {
    super::launch::validate_home(home)?;
    let project = text(project)?;
    let path = home.join(".claude.json");
    let mut config = match read_claude_config(&path)? {
        Some(bytes) => serde_json::from_slice::<serde_json::Value>(&bytes)
            .map_err(|_| failure("launch_claude_config_invalid"))?,
        None => serde_json::json!({}),
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
    assert!(LOCAL_PLACEHOLDER_KEY.len() <= CLAUDE_KEY_TAIL_CHARS);
    let responses = object
        .entry("customApiKeyResponses")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| failure("launch_claude_config_invalid"))?;
    for (list, approved) in [("approved", true), ("rejected", false)] {
        let keys = responses
            .entry(list)
            .or_insert_with(|| serde_json::json!([]))
            .as_array_mut()
            .ok_or_else(|| failure("launch_claude_config_invalid"))?;
        keys.retain(|key| key != LOCAL_PLACEHOLDER_KEY);
        if approved {
            keys.push(LOCAL_PLACEHOLDER_KEY.into());
        }
    }
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

/// Reads Claude Code's config file through one open that never follows a link or waits on
/// a FIFO, then checks the opened file itself, so an entry swapped in after any earlier
/// look is never read. Takes the path. Returns its bytes, `None` when there is none, or a
/// plain code when it is not a regular file of at most 1 MiB or cannot be read.
#[cfg(unix)]
fn read_claude_config(path: &Path) -> Result<Option<Vec<u8>>, LaunchError> {
    use rustix::io::Errno;
    let opened = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32)
        .open(path);
    let file = match opened {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error)
            if [Errno::LOOP, Errno::NXIO, Errno::OPNOTSUPP]
                .iter()
                .any(|errno| error.raw_os_error() == Some(errno.raw_os_error())) =>
        {
            return Err(failure("launch_claude_config_unsafe"));
        }
        Err(_) => return Err(failure("launch_claude_config_unavailable")),
    };
    let metadata = file
        .metadata()
        .map_err(|_| failure("launch_claude_config_unavailable"))?;
    if !metadata.is_file() || metadata.len() > CLAUDE_CONFIG_BYTES_MAX {
        return Err(failure("launch_claude_config_unsafe"));
    }
    let mut bytes = Vec::new();
    file.take(CLAUDE_CONFIG_BYTES_MAX + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| failure("launch_claude_config_unavailable"))?;
    if bytes.len() as u64 > CLAUDE_CONFIG_BYTES_MAX {
        return Err(failure("launch_claude_config_unsafe"));
    }
    Ok(Some(bytes))
}

#[cfg(all(test, unix))]
mod tests;
