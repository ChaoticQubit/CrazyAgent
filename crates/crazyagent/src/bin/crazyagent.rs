#![forbid(unsafe_code)]

use crazyagent::qualification::{
    AdmissionContext, Capability, Outcome, QualificationReport, admit, load_report,
};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{self, Write};
#[cfg(unix)]
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

const ARGUMENTS_MAX: usize = 32;
const USAGE: &str = "crazyagent qualification report --source-revision REV --profile-id PROFILE\n\
crazyagent --version\n\
crazyagent engine [--home DIR] [--codex-executable ABS_PATH] [--claude-executable ABS_PATH] [--rcli-executable ABS_PATH]\n\
crazyagent engine RUNTIME_DIR GLOBAL_TOML PROJECT_TOML SOCKET_NAME [--writable-settings] [--codex-executable ABS_PATH] [--claude-executable ABS_PATH] [--rcli-executable ABS_PATH]\n\
crazyagent tui\n\
crazyagent tui SOCKET\n\
crazyagent service install|uninstall|status|start|stop|link-cli\n\
crazyagent update [--check] [--channel stable|dev]\n\
crazyagent team-mcp DROP_FILE\n\
crazyagent team-mcp-live MAILBOX_DIR\n\
crazyagent qualification admit --report PATH --source-revision REV --profile-id PROFILE \
--evidence-dir DIR --require CAPABILITY [--require CAPABILITY]\n\
Capabilities: confinement, live_recovery, native_auth, child_control\n\
Report records unavailable capabilities without native/provider execution.\n\
Admission verifies supplied evidence against caller-selected source/profile and requirements; \
the caller must obtain those expectations from trusted policy.";

struct Arguments {
    operation: String,
    options: BTreeMap<String, OsString>,
    required: Vec<Capability>,
}

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os()
        .skip(1)
        .take(ARGUMENTS_MAX + 1)
        .collect();
    if args == ["--help"] {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    if args == ["--version"] {
        println!("{}", crazyagent::version::line());
        return ExitCode::SUCCESS;
    }
    if args.first().is_some_and(|arg| arg == "update") {
        return match crazyagent::update::run(&args[1..]) {
            Ok(report) => {
                println!("{report}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("{error}");
                ExitCode::FAILURE
            }
        };
    }
    if args == ["tui", "--help"] {
        println!("crazyagent tui\ncrazyagent tui SOCKET");
        return ExitCode::SUCCESS;
    }
    if args.first().is_some_and(|arg| arg == "tui") {
        return match run_tui_command(&args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("{error}");
                ExitCode::FAILURE
            }
        };
    }
    if args.first().is_some_and(|arg| arg == "service") {
        return match run_service(&args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("{error}");
                ExitCode::FAILURE
            }
        };
    }
    if args
        .first()
        .is_some_and(|arg| arg == "native-auth-custodian")
    {
        return match run_native_auth_custodian(&args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("{error}");
                ExitCode::FAILURE
            }
        };
    }
    if args
        .first()
        .is_some_and(|arg| arg == crazyagent::team::tool::SERVER_COMMAND)
    {
        let result = if args.len() != 2 {
            Err("usage: crazyagent team-mcp DROP_FILE".to_owned())
        } else {
            crazyagent::team::tool::serve(
                io::stdin().lock(),
                io::stdout().lock(),
                Path::new(&args[1]),
            )
            .map_err(|e| e.to_string())
        };
        return match result {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("{error}");
                ExitCode::FAILURE
            }
        };
    }
    if args
        .first()
        .is_some_and(|arg| arg == crazyagent::team::tool::LIVE_SERVER_COMMAND)
    {
        let result = if args.len() != 2 {
            Err("usage: crazyagent team-mcp-live MAILBOX_DIR".to_owned())
        } else {
            crazyagent::team::tool::serve_live(
                io::stdin().lock(),
                io::stdout().lock(),
                Path::new(&args[1]),
            )
            .map_err(|e| e.to_string())
        };
        return match result {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("{error}");
                ExitCode::FAILURE
            }
        };
    }
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    if args
        .first()
        .is_some_and(|arg| arg == crazyagent::credentials::GIT_HELPER_COMMAND)
    {
        return match run_git_credential(&args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("{error}");
                ExitCode::FAILURE
            }
        };
    }
    if args.first().is_some_and(|arg| arg == "engine") {
        return match run_engine(&args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("{error}");
                ExitCode::FAILURE
            }
        };
    }
    match execute(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err((code, error)) => {
            eprintln!("{error}");
            ExitCode::from(code)
        }
    }
}

/// Answers git's credential helper call with the engine's own store item, so a token
/// never reaches git's config, argv or environment. Input: the subcommand arguments
/// NAMESPACE REFERENCE USERNAME OPERATION with git's request on stdin. Output: the answer
/// on stdout for `get`, nothing for other operations, or the store's error code.
#[cfg(any(target_os = "linux", target_os = "windows"))]
fn run_git_credential(args: &[OsString]) -> Result<(), String> {
    let [_, namespace, reference, username, operation] = args else {
        return Err(format!(
            "usage: crazyagent {} NAMESPACE REFERENCE USERNAME OPERATION",
            crazyagent::credentials::GIT_HELPER_COMMAND
        ));
    };
    let text = |arg: &OsString| {
        arg.to_str()
            .map(str::to_owned)
            .ok_or_else(|| "credential_name_invalid".to_owned())
    };
    crazyagent::credentials::answer_git_credential(
        &text(namespace)?,
        &text(reference)?,
        &text(username)?,
        &text(operation)?,
        io::stdin().lock(),
        io::stdout().lock(),
    )
    .map_err(|error| error.to_string())
}

#[cfg(unix)]
fn run_native_auth_custodian(args: &[OsString]) -> Result<(), String> {
    let usage = "usage: crazyagent native-auth-custodian RUNTIME_DIR";
    let codex = match args.len() {
        2 => None,
        6 if (args[2] == "--codex-executable" && args[4] == "--codex-sha256")
            || (args[2] == "--claude-executable" && args[4] == "--claude-sha256") =>
        {
            let executable = Path::new(&args[3]);
            if !executable.is_absolute() {
                return Err("native_auth_host_executable_unsafe".into());
            }
            let digest = args[5]
                .to_str()
                .filter(|value| {
                    value.len() == 64
                        && value
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                })
                .ok_or("native_auth_host_executable_digest_invalid")?;
            Some((executable, digest))
        }
        _ => return Err(usage.into()),
    };
    let input = rustix::io::fcntl_dupfd_cloexec(io::stdin(), 3)
        .map(std::fs::File::from)
        .map_err(|_| "native_auth_bootstrap_stdin_unavailable".to_owned())?;
    if !input
        .metadata()
        .map_err(|_| "native_auth_bootstrap_stdin_unavailable".to_owned())?
        .file_type()
        .is_fifo()
    {
        return Err("native_auth_bootstrap_input_not_pipe".into());
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime
        .block_on(async {
            if let Some((executable, digest)) = codex {
                if args[2] == "--claude-executable" {
                    return crazyagent::engine::run_native_auth_custodian_with_claude(
                        Path::new(&args[1]),
                        input.into(),
                        executable,
                        digest,
                    )
                    .await;
                }
                crazyagent::engine::run_native_auth_custodian_with_codex(
                    Path::new(&args[1]),
                    input.into(),
                    executable,
                    digest,
                )
                .await
            } else {
                crazyagent::engine::run_native_auth_custodian(Path::new(&args[1]), input.into())
                    .await
            }
        })
        .map_err(|error| error.to_string())
}

/// Refuses the native sign-in custodian, which needs Unix fd handoff and process groups.
/// Takes the command arguments, unused. Returns `native_auth_host_unavailable`.
#[cfg(not(unix))]
fn run_native_auth_custodian(_args: &[OsString]) -> Result<(), String> {
    Err("native_auth_host_unavailable".into())
}

/// Registers the requests that stop the engine: SIGINT and SIGTERM.
/// Must run inside the Tokio runtime. Returns a future that completes on the
/// first request, or the registration error.
#[cfg(unix)]
fn shutdown_requested() -> Result<impl std::future::Future<Output = ()> + Send + 'static, String> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut interrupt = signal(SignalKind::interrupt()).map_err(|e| e.to_string())?;
    let mut terminate = signal(SignalKind::terminate()).map_err(|e| e.to_string())?;
    Ok(async move {
        tokio::select! {
            _ = interrupt.recv() => {},
            _ = terminate.recv() => {},
        }
    })
}

/// Registers the requests that stop the engine: Ctrl-C and console close.
/// Must run inside the Tokio runtime. Returns a future that completes on the
/// first request, or the registration error.
#[cfg(windows)]
fn shutdown_requested() -> Result<impl std::future::Future<Output = ()> + Send + 'static, String> {
    use tokio::signal::windows::{ctrl_c, ctrl_close};
    let mut interrupt = ctrl_c().map_err(|e| e.to_string())?;
    let mut close = ctrl_close().map_err(|e| e.to_string())?;
    Ok(async move {
        tokio::select! {
            _ = interrupt.recv() => {},
            _ = close.recv() => {},
        }
    })
}

fn engine_options(args: &[OsString]) -> Result<(bool, Option<&Path>), String> {
    let usage = "usage: crazyagent engine RUNTIME_DIR GLOBAL_TOML PROJECT_TOML SOCKET_NAME [--writable-settings] [--codex-executable ABS_PATH] [--claude-executable ABS_PATH]";
    if !(5..=8).contains(&args.len()) {
        return Err(usage.into());
    }
    let mut writable = false;
    let mut codex = None;
    let mut index = 5;
    while index < args.len() {
        match args[index].to_str() {
            Some("--writable-settings") if !writable => {
                writable = true;
                index += 1;
            }
            Some("--codex-executable") if codex.is_none() => {
                let executable = args.get(index + 1).ok_or(usage)?;
                let executable = Path::new(executable);
                if !executable.is_absolute() {
                    return Err("native_auth_host_executable_unsafe".into());
                }
                codex = Some(executable);
                index += 2;
            }
            _ => return Err(usage.into()),
        }
        assert!(index <= args.len());
    }
    Ok((writable, codex))
}

type EngineOptions = (
    bool,
    Option<std::path::PathBuf>,
    Option<std::path::PathBuf>,
    Option<std::path::PathBuf>,
);

fn engine_options_with_claude(args: &[OsString]) -> Result<EngineOptions, String> {
    if !(5..=12).contains(&args.len()) {
        return Err(USAGE.into());
    }
    let mut filtered = Vec::with_capacity(args.len());
    let mut claude = None;
    let mut rcli = None;
    let mut index = 0;
    while index < args.len() {
        if index >= 5
            && (args[index] == "--claude-executable" || args[index] == "--rcli-executable")
        {
            let slot = if args[index] == "--claude-executable" {
                &mut claude
            } else {
                &mut rcli
            };
            if slot.is_some() {
                return Err(USAGE.into());
            }
            let path = Path::new(args.get(index + 1).ok_or(USAGE)?);
            if !path.is_absolute() {
                return Err("native_auth_host_executable_unsafe".into());
            }
            *slot = Some(path.to_path_buf());
            index += 2;
        } else {
            filtered.push(args[index].clone());
            index += 1;
        }
        assert!(index <= args.len());
    }
    let (writable, codex) = engine_options(&filtered)?;
    Ok((writable, codex.map(Path::to_path_buf), claude, rcli))
}

/// One engine start, read from the positional form or from the defaults.
struct EngineStart {
    root: PathBuf,
    global: PathBuf,
    project: PathBuf,
    socket_name: String,
    writable: bool,
    harnesses: crazyagent::paths::Harnesses,
    found_on_path: [bool; 3],
}

const DEFAULT_ENGINE_USAGE: &str = "usage: crazyagent engine [--home DIR] [--codex-executable ABS_PATH] [--claude-executable ABS_PATH] [--rcli-executable ABS_PATH]";

fn uses_defaults(args: &[OsString]) -> bool {
    args.get(1)
        .is_none_or(|arg| arg.to_str().is_some_and(|arg| arg.starts_with("--")))
}

/// Reads the default engine form: no positional arguments, an optional `--home` and the
/// harness flags. Input: the command arguments. Output: the start with its layout prepared
/// and every harness resolved for this start only, or a usage error.
fn default_engine_start(args: &[OsString]) -> Result<EngineStart, String> {
    let mut home = None;
    let mut explicit = crazyagent::paths::Harnesses::default();
    let mut index = 1;
    while index < args.len() {
        let flag = args[index].to_str().ok_or(DEFAULT_ENGINE_USAGE)?;
        let value = PathBuf::from(args.get(index + 1).ok_or(DEFAULT_ENGINE_USAGE)?);
        let slot = match flag {
            "--home" => &mut home,
            "--codex-executable" => &mut explicit.codex,
            "--claude-executable" => &mut explicit.claude,
            "--rcli-executable" => &mut explicit.rcli,
            _ => return Err(DEFAULT_ENGINE_USAGE.into()),
        };
        if slot.is_some() {
            return Err(DEFAULT_ENGINE_USAGE.into());
        }
        if !value.is_absolute() {
            return Err(if flag == "--home" {
                "--home must be an absolute folder path".into()
            } else {
                "native_auth_host_executable_unsafe".into()
            });
        }
        *slot = Some(value);
        index += 2;
        assert!(index <= args.len());
    }
    let layout = match home {
        Some(root) => crazyagent::paths::Layout::new(root),
        None => crazyagent::paths::Layout::current(),
    }
    .map_err(|e| e.to_string())?;
    layout.prepare().map_err(|e| e.to_string())?;
    let executable = current_executable()?;
    let found_on_path = [
        explicit.codex.is_none(),
        explicit.claude.is_none(),
        explicit.rcli.is_none(),
    ];
    let harnesses = crazyagent::paths::resolve_harnesses(
        explicit,
        std::env::var_os("PATH").as_deref(),
        executable.parent(),
    );
    Ok(EngineStart {
        root: layout.root().to_path_buf(),
        global: layout.global_settings(),
        project: layout.project_settings(),
        socket_name: crazyagent::paths::SOCKET_NAME.into(),
        writable: true,
        harnesses,
        found_on_path,
    })
}

fn positional_engine_start(args: &[OsString]) -> Result<EngineStart, String> {
    let (writable, codex, claude, rcli) = engine_options_with_claude(args)?;
    assert!((5..=12).contains(&args.len()));
    let socket_name = args[4].to_str().ok_or("invalid socket name")?.to_owned();
    Ok(EngineStart {
        root: PathBuf::from(&args[1]),
        global: PathBuf::from(&args[2]),
        project: PathBuf::from(&args[3]),
        socket_name,
        writable,
        harnesses: crazyagent::paths::Harnesses {
            codex,
            claude,
            rcli,
        },
        found_on_path: [false; 3],
    })
}

/// The running executable with every link resolved, so a service and the RCLI lookup
/// name the real file. Output: its absolute path.
fn current_executable() -> Result<PathBuf, String> {
    std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|e| format!("cannot find the running crazyagent executable: {e}"))
}

type Register = fn(
    &mut crazyagent::engine::bootstrap::EngineService,
    &Path,
) -> Result<(), crazyagent::engine::EngineError>;

/// Registers the harness executables of one start. An explicit flag that cannot be
/// registered stops the engine; one found on PATH is dropped with a note on stderr, so
/// setup reports it as missing. Inputs: the service, the harnesses and which came from
/// PATH. Output: Ok with the harnesses left registered, or the explicit failure.
fn register_harnesses(
    service: &mut crazyagent::engine::bootstrap::EngineService,
    harnesses: &mut crazyagent::paths::Harnesses,
    found_on_path: [bool; 3],
) -> Result<(), String> {
    let slots: [(&str, &mut Option<PathBuf>, Register, bool); 3] = [
        (
            "codex",
            &mut harnesses.codex,
            crazyagent::engine::bootstrap::EngineService::register_codex_executable,
            found_on_path[0],
        ),
        (
            "claude",
            &mut harnesses.claude,
            crazyagent::engine::bootstrap::EngineService::register_claude_executable,
            found_on_path[1],
        ),
        (
            crazyagent::paths::RCLI_EXECUTABLE,
            &mut harnesses.rcli,
            crazyagent::engine::bootstrap::EngineService::register_rcli_executable,
            found_on_path[2],
        ),
    ];
    for (name, slot, register, optional) in slots {
        let Some(executable) = slot.clone() else {
            continue;
        };
        if let Err(error) = register(service, &executable) {
            if !optional {
                return Err(error.to_string());
            }
            eprintln!(
                "{name} at {} could not be registered ({error}); the engine runs without it",
                executable.display()
            );
            *slot = None;
        }
    }
    Ok(())
}

fn run_engine(args: &[OsString]) -> Result<(), String> {
    let start = if uses_defaults(args) {
        default_engine_start(args)?
    } else {
        positional_engine_start(args)?
    };
    let EngineStart {
        root,
        global,
        project,
        socket_name,
        writable,
        mut harnesses,
        found_on_path,
    } = start;
    let runtime_dir = root.as_path();
    let socket_name = socket_name.as_str();
    crazyagent::protocol::SessionId::new(socket_name).map_err(|e| e.to_string())?;
    assert!(!socket_name.contains('/'));
    let config = if writable {
        None
    } else {
        Some(crazyagent::config::resolve_config(&global, &project).map_err(|e| e.to_string())?)
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    runtime.block_on(async {
        let shutdown = shutdown_requested()?;
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let signal = tokio::spawn(async move {
            shutdown.await;
            stop.send_replace(true);
        });
        let result = async {
            let mut service = if writable {
                crazyagent::engine::bootstrap::EngineService::start_with_settings_sources(
                    runtime_dir,
                    socket_name,
                    &global,
                    &project,
                    &stopped,
                )
                .await
            } else {
                crazyagent::engine::bootstrap::EngineService::start(
                    runtime_dir,
                    socket_name,
                    config.as_ref().unwrap(),
                    &stopped,
                )
                .await
            }
            .map_err(|e| e.to_string())?;
            tokio::task::yield_now().await;
            let ready = std::env::current_exe()
                .and_then(std::fs::canonicalize)
                .map_err(|e| e.to_string())
                .and_then(|executable| {
                    service
                        .register_native_auth_host(&executable)
                        .map_err(|e| e.to_string())
                })
                .and_then(|()| register_harnesses(&mut service, &mut harnesses, found_on_path))
                .and_then(|()| service.ensure_ready(&stopped).map_err(|e| e.to_string()))
                .and_then(|()| match harnesses.codex.as_deref().filter(|_| writable) {
                    None => Ok(None),
                    Some(executable) => {
                        let agents = service
                            .owner_runtime()
                            .map(|handle| handle.agents())
                            .transpose()
                            .map_err(|error| error.0)?;
                        crazyagent::research::service::start_for_engine(
                            runtime_dir,
                            &global,
                            &project,
                            executable,
                            harnesses.claude.as_deref(),
                            agent_terminal(&service),
                            agents,
                        )
                        .map(Some)
                    }
                })
                .and_then(|research| {
                    if !writable {
                        return Ok((research, None));
                    }
                    match crazyagent::onboarding::service::start_for_engine(
                        runtime_dir,
                        &global,
                        &project,
                        harnesses.codex.as_deref(),
                        harnesses.claude.as_deref(),
                    ) {
                        Ok(onboarding) => Ok((research, Some(onboarding))),
                        Err(error) => {
                            stop_background(research, None);
                            Err(error)
                        }
                    }
                })
                .and_then(|(research, onboarding)| {
                    let mut executables = std::collections::BTreeMap::new();
                    if let Some(executable) = harnesses.codex.as_deref() {
                        executables.insert("codex", executable.to_path_buf());
                    }
                    if let Some(executable) = harnesses.claude.as_deref() {
                        executables.insert("claude-code", executable.to_path_buf());
                    }
                    if !writable || executables.is_empty() {
                        return Ok((research, onboarding, None));
                    }
                    let agents = match service
                        .owner_runtime()
                        .map(|handle| handle.agents())
                        .transpose()
                    {
                        Ok(agents) => agents,
                        Err(error) => {
                            stop_background(research, onboarding);
                            return Err(error.0);
                        }
                    };
                    match crazyagent::team::runtime::start_for_engine(
                        runtime_dir,
                        &global,
                        &project,
                        executables,
                        std::env::current_exe().ok().as_deref(),
                        agents,
                        agent_terminal(&service),
                    ) {
                        Ok(team) => Ok((research, onboarding, Some(team))),
                        Err(error) => {
                            stop_background(research, onboarding);
                            Err(error)
                        }
                    }
                })
                .and_then(|(research, onboarding, team)| {
                    let mut output = io::stdout().lock();
                    match writeln!(output, "ready").and_then(|()| output.flush()) {
                        Ok(()) => Ok((research, onboarding, team)),
                        Err(error) => {
                            stop_background(research, onboarding);
                            Err(error.to_string())
                        }
                    }
                });
            let (research, onboarding, team) = match ready {
                Ok(started) => started,
                Err(error) => {
                    return match service.shutdown().await {
                        Ok(()) => Err(error),
                        Err(cleanup) => Err(format!("{error}; cleanup: {cleanup}")),
                    };
                }
            };
            let usage_upkeep = if team.is_some() {
                let mut executables = std::collections::BTreeMap::new();
                if let Some(executable) = harnesses.codex.as_deref() {
                    executables.insert("codex", executable.to_path_buf());
                }
                if let Some(executable) = harnesses.claude.as_deref() {
                    executables.insert("claude-code", executable.to_path_buf());
                }
                match crazyagent::usage::service::start_for_engine(
                    runtime_dir,
                    &global,
                    &project,
                    executables,
                ) {
                    Ok(usage) => Some(usage),
                    Err(problem) => {
                        eprintln!("usage is not counted ({problem})");
                        None
                    }
                }
            } else {
                None
            };
            let terminal = service
                .owner_runtime()
                .and_then(|runtime| runtime.terminal().ok());
            let result = service.run(stopped).await.map_err(|e| e.to_string());
            if let Some(upkeep) = usage_upkeep {
                let _ = tokio::task::spawn_blocking(move || upkeep.stop()).await;
            }
            if research.is_some() || onboarding.is_some() {
                let _ = tokio::task::spawn_blocking(move || stop_background(research, onboarding))
                    .await;
            }
            if let Some(team) = team {
                let _ = tokio::task::spawn_blocking(move || team.stop()).await;
            }
            if let Some(terminal) = terminal {
                let quit = tokio::task::spawn_blocking(move || {
                    crazyagent::terminal::quit(terminal.as_ref())
                })
                .await;
                if let Ok(Err(problem)) = quit {
                    eprintln!("agent terminals left running ({problem})");
                }
            }
            result
        }
        .await;
        signal.abort();
        let _ = signal.await;
        result
    })
}

/// Opens the TUI. With no socket it uses the default engine and starts it when none
/// answers; with a socket it connects there as before. Either way an engine of another
/// version is named before the TUI opens. Input: the arguments. Output: the TUI result.
fn run_tui_command(args: &[OsString]) -> Result<(), String> {
    use std::io::IsTerminal;
    let (socket, layout) = match args {
        [_] => {
            let layout = crazyagent::paths::Layout::current().map_err(|e| e.to_string())?;
            (layout.socket(), Some(layout))
        }
        [_, socket] => (PathBuf::from(socket), None),
        _ => return Err("usage: crazyagent tui [SOCKET]".into()),
    };
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err("TUI requires terminal stdin and stdout".into());
    }
    let root = socket
        .parent()
        .ok_or("the engine socket has no folder")?
        .to_path_buf();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    runtime.block_on(prepare_engine(&root, layout.as_ref()))?;
    drop(runtime);
    crazyagent::engine::Client::new(socket, Duration::from_secs(1))
        .map_err(|e| e.to_string())
        .and_then(|client| crazyagent::tui::run_tui(client).map_err(|e| e.to_string()))
}

fn default_service(
    layout: &crazyagent::paths::Layout,
) -> Result<crazyagent::service::Service, String> {
    crazyagent::service::Service::current_user(
        crazyagent::paths::Platform::CURRENT,
        current_executable()?,
        layout,
    )
    .map_err(|e| e.to_string())
}

async fn prepare_engine(
    root: &Path,
    layout: Option<&crazyagent::paths::Layout>,
) -> Result<(), String> {
    use crazyagent::service::EngineCheck;
    let service = layout.map(default_service).transpose()?;
    if let (Some(layout), Some(service)) = (layout, service.as_ref()) {
        crazyagent::service::ensure_running(
            layout,
            service,
            &service.executable,
            std::env::var_os("PATH").as_deref(),
            crazyagent::service::READY_TIMEOUT,
        )
        .await
        .map_err(|e| e.to_string())?;
    }
    let check = crazyagent::service::check_engine(
        root,
        crazyagent::engine::owner::OwnerClientKind::Tui,
        Duration::from_secs(5),
    )
    .await;
    match check {
        EngineCheck::Ready { .. } | EngineCheck::Unavailable { .. } => Ok(()),
        EngineCheck::Mismatch { message, .. } => {
            offer_restart(&message, layout, service.as_ref()).await
        }
    }
}

/// Names an engine of another version and offers to restart it through the installed
/// service. Inputs: the message, the default layout and service when the TUI uses them.
/// Output: Ok once a restarted engine matches, or the reason the TUI cannot open.
async fn offer_restart(
    message: &str,
    layout: Option<&crazyagent::paths::Layout>,
    service: Option<&crazyagent::service::Service>,
) -> Result<(), String> {
    use std::io::BufRead;
    eprintln!("{message}");
    let installed = match service {
        Some(service) => service.status().map_err(|e| e.to_string())?.installed,
        None => false,
    };
    let (Some(layout), Some(service), true) = (layout, service, installed) else {
        return Err("Stop the running engine, start it again, then reopen the TUI.".into());
    };
    eprint!("Restart the engine through its service now? [y/N] ");
    io::stderr().flush().map_err(|e| e.to_string())?;
    let mut answer = String::new();
    io::stdin()
        .lock()
        .read_line(&mut answer)
        .map_err(|e| e.to_string())?;
    if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        return Err("The engine was not restarted.".into());
    }
    service.stop().map_err(|e| e.to_string())?;
    let since = crazyagent::service::log_length(layout);
    service.start().map_err(|e| e.to_string())?;
    crazyagent::service::wait_ready(layout, crazyagent::service::READY_TIMEOUT, None, since)
        .await
        .map_err(|e| e.to_string())?;
    match crazyagent::service::check_engine(
        layout.root(),
        crazyagent::engine::owner::OwnerClientKind::Tui,
        Duration::from_secs(5),
    )
    .await
    {
        crazyagent::service::EngineCheck::Ready { .. } => Ok(()),
        crazyagent::service::EngineCheck::Mismatch { message, .. } => Err(format!(
            "{message}. The service runs another crazyagent; reinstall it from this one: crazyagent service install"
        )),
        crazyagent::service::EngineCheck::Unavailable { reason } => Err(reason),
    }
}

/// Runs `crazyagent service ACTION` for the default engine service. Input: the arguments.
/// Output: Ok after the action and its report on stdout, or the failing step.
fn run_service(args: &[OsString]) -> Result<(), String> {
    let usage = "usage: crazyagent service install|uninstall|status|start|stop|link-cli";
    let [_, action] = args else {
        return Err(usage.into());
    };
    let action = action.to_str().ok_or(usage)?;
    let executable = current_executable()?;
    if action == "link-cli" {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .filter(|home| !home.is_empty())
            .map(PathBuf::from)
            .ok_or("HOME is not set")?;
        let link = crazyagent::service::link_cli(&executable, &home.join(".local").join("bin"))
            .map_err(|e| e.to_string())?;
        println!("Linked {} to {}", link.display(), executable.display());
        return Ok(());
    }
    let layout = crazyagent::paths::Layout::current().map_err(|e| e.to_string())?;
    let service = default_service(&layout)?;
    let wait = |layout: &crazyagent::paths::Layout, since: u64| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?
            .block_on(crazyagent::service::wait_ready(
                layout,
                crazyagent::service::READY_TIMEOUT,
                None,
                since,
            ))
            .map_err(|e| e.to_string())
    };
    let answering = crazyagent::service::answering(&layout.socket());
    match action {
        "install" => {
            if answering && !service.status().map_err(|e| e.to_string())?.running {
                return Err(format!(
                    "an engine the service did not start is running at {}; stop it, then install",
                    layout.root().display()
                ));
            }
            let since = crazyagent::service::log_length(&layout);
            service.install().map_err(|e| e.to_string())?;
            wait(&layout, since)?;
            println!(
                "Installed {} ({}); it runs {}",
                service.name,
                service.definition.display(),
                service.executable.display()
            );
        }
        "uninstall" => {
            service.uninstall().map_err(|e| e.to_string())?;
            println!("Removed {}", service.name);
        }
        "start" if answering => println!("The engine is already running"),
        "start" => {
            let since = crazyagent::service::log_length(&layout);
            service.start().map_err(|e| e.to_string())?;
            wait(&layout, since)?;
            println!("Started {}", service.name);
        }
        "stop" => {
            service.stop().map_err(|e| e.to_string())?;
            println!("Stopped {}", service.name);
        }
        "status" => {
            let status = service.status().map_err(|e| e.to_string())?;
            let running = match (status.running, status.pid) {
                (true, Some(pid)) => format!("yes (pid {pid})"),
                (true, None) => "yes".into(),
                (false, _) => "no".into(),
            };
            println!(
                "service: {}\ndefinition: {} ({})\nrunning: {running}\ndata folder: {}\nlog: {}",
                service.name,
                service.definition.display(),
                if status.installed {
                    "installed"
                } else {
                    "not installed"
                },
                layout.root().display(),
                layout.engine_log().display()
            );
        }
        _ => return Err(usage.into()),
    }
    Ok(())
}

/// Stops the research and onboarding services the engine started, research
/// first, each with every agent process it runs. Takes either service, or
/// none for one the engine did not start.
fn stop_background(
    research: Option<crazyagent::research::service::ResearchService>,
    onboarding: Option<crazyagent::onboarding::service::OnboardingService>,
) {
    if let Some(research) = research {
        research.stop();
    }
    if let Some(onboarding) = onboarding {
        onboarding.stop();
    }
}

/// Opens the engine's terminal host for team and research agents when it can run now.
/// Takes the engine service. Returns the host, or none after saying why on stderr; agents
/// then run headless until the engine restarts with tmux available.
fn agent_terminal(
    service: &crazyagent::engine::bootstrap::EngineService,
) -> Option<std::sync::Arc<dyn crazyagent::terminal::Host>> {
    let host = service.owner_runtime()?.terminal().ok()?;
    match host.alive() {
        Ok(_) => Some(host),
        Err(problem) => {
            eprintln!("agent terminals unavailable ({problem}); agents run headless");
            None
        }
    }
}

fn execute(args: Vec<OsString>) -> Result<(), (u8, String)> {
    let args = parse(args).map_err(|e| (2, format!("{e}\n{USAGE}")))?;
    let source = text_option(&args, "--source-revision").map_err(|e| (2, e))?;
    let profile = text_option(&args, "--profile-id").map_err(|e| (2, e))?;
    if args.operation == "report" {
        return write_report(source, profile).map_err(|e| (1, e));
    }
    let context =
        AdmissionContext::new(source, profile, Path::new(&args.options["--evidence-dir"]));
    let report = load_report(Path::new(&args.options["--report"]), context)
        .map_err(|e| (1, e.to_string()))?;
    admit(&report, &args.required).map_err(|e| (1, e.to_string()))?;
    let names: Vec<_> = args.required.iter().map(|c| c.as_str()).collect();
    writeln!(io::stdout().lock(), "admitted: {}", names.join(", ")).map_err(|e| (1, e.to_string()))
}

fn parse(args: Vec<OsString>) -> Result<Arguments, String> {
    if args.len() < 2
        || args.len() > ARGUMENTS_MAX
        || args[0] != "qualification"
        || !matches!(args[1].to_str(), Some("report" | "admit"))
    {
        return Err("expected qualification report or qualification admit".into());
    }
    let mut parsed = Arguments {
        operation: args[1].to_str().unwrap().into(),
        options: BTreeMap::new(),
        required: Vec::new(),
    };
    let (pairs, remainder) = args[2..].as_chunks::<2>();
    for pair in pairs {
        let flag = pair[0].to_str().ok_or("invalid option name")?;
        if flag == "--require" && parsed.operation == "admit" {
            let value = pair[1].to_str().ok_or("invalid capability name")?;
            let capability = Capability::ALL
                .into_iter()
                .find(|c| c.as_str() == value)
                .ok_or("unknown required capability")?;
            if parsed.required.contains(&capability) {
                return Err("duplicate required capability".into());
            }
            parsed.required.push(capability);
        } else {
            let allowed = matches!(flag, "--source-revision" | "--profile-id")
                || (parsed.operation == "admit" && matches!(flag, "--report" | "--evidence-dir"));
            if !allowed
                || pair[1].is_empty()
                || parsed
                    .options
                    .insert(flag.into(), pair[1].clone())
                    .is_some()
            {
                return Err("unknown, empty or duplicate option".into());
            }
        }
    }
    if !remainder.is_empty() {
        return Err("every option requires a value".into());
    }
    for name in ["--source-revision", "--profile-id"] {
        text_option(&parsed, name)?;
    }
    if parsed.operation == "admit"
        && (parsed.required.is_empty()
            || !parsed.options.contains_key("--report")
            || !parsed.options.contains_key("--evidence-dir"))
    {
        return Err("admit requires --report, --evidence-dir and at least one --require".into());
    }
    Ok(parsed)
}

fn text_option<'a>(args: &'a Arguments, name: &str) -> Result<&'a str, String> {
    let value = args
        .options
        .get(name)
        .and_then(|s| s.to_str())
        .ok_or_else(|| format!("missing or invalid {name}"))?;
    if value.trim().is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        return Err(format!("invalid {name}"));
    }
    Ok(value)
}

fn write_report(source: &str, profile: &str) -> Result<(), String> {
    let mut report = QualificationReport::new(source, profile);
    report
        .versions
        .insert("crazyagent".into(), env!("CARGO_PKG_VERSION").into());
    report.results = Capability::ALL
        .into_iter()
        .map(|c| (c, Outcome::Unavailable))
        .collect();
    report.gaps = Capability::ALL
        .into_iter()
        .map(|c| format!("{}: native/runtime evidence not collected", c.as_str()))
        .collect();
    let mut stdout = io::stdout().lock();
    serde_json::to_writer_pretty(&mut stdout, &report).map_err(|e| e.to_string())?;
    writeln!(stdout).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine_arguments() -> Vec<OsString> {
        [
            "engine",
            "/private/tmp/runtime",
            "/global.toml",
            "/project.toml",
            "e.sock",
        ]
        .into_iter()
        .map(OsString::from)
        .collect()
    }

    #[test]
    fn engine_options_preserve_writable_settings_with_codex_in_both_orders() {
        for options in [
            vec![
                "--writable-settings",
                "--codex-executable",
                "/official/codex",
            ],
            vec![
                "--codex-executable",
                "/official/codex",
                "--writable-settings",
            ],
        ] {
            let mut arguments = engine_arguments();
            arguments.extend(options.into_iter().map(OsString::from));
            let (writable, executable) = engine_options(&arguments).unwrap();
            assert!(writable);
            assert_eq!(executable, Some(Path::new("/official/codex")));
        }
    }

    #[test]
    fn engine_options_preserve_existing_no_codex_shapes() {
        let mut arguments = engine_arguments();
        assert_eq!(engine_options(&arguments).unwrap(), (false, None));
        arguments.push("--writable-settings".into());
        assert_eq!(engine_options(&arguments).unwrap(), (true, None));
    }

    #[test]
    fn engine_options_reject_duplicate_unknown_and_missing_options() {
        for options in [
            vec!["--writable-settings", "--writable-settings"],
            vec!["--codex-executable"],
            vec!["--unknown"],
            vec![
                "--codex-executable",
                "/official/codex",
                "--codex-executable",
            ],
        ] {
            let mut arguments = engine_arguments();
            arguments.extend(options.into_iter().map(OsString::from));
            assert!(engine_options(&arguments).is_err());
        }
    }
}
