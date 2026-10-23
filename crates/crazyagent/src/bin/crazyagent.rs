#![forbid(unsafe_code)]

use crazyagent::qualification::{
    AdmissionContext, Capability, Outcome, QualificationReport, admit, load_report,
};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{self, Write};
use std::os::unix::fs::FileTypeExt;
use std::path::Path;
use std::process::ExitCode;

const ARGUMENTS_MAX: usize = 32;
const USAGE: &str = "crazyagent qualification report --source-revision REV --profile-id PROFILE\n\
crazyagent engine RUNTIME_DIR GLOBAL_TOML PROJECT_TOML SOCKET_NAME [--writable-settings] [--codex-executable ABS_PATH] [--claude-executable ABS_PATH] [--rcli-executable ABS_PATH]\n\
crazyagent tui SOCKET\n\
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
        println!("crazyagent {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    if args == ["tui", "--help"] {
        println!("crazyagent tui SOCKET");
        return ExitCode::SUCCESS;
    }
    if args.first().is_some_and(|arg| arg == "tui") {
        let result = if args.len() != 2 {
            Err("usage: crazyagent tui SOCKET".to_owned())
        } else {
            crazyagent::engine::Client::new(
                Path::new(&args[1]).to_path_buf(),
                std::time::Duration::from_secs(1),
            )
            .map_err(|e| e.to_string())
            .and_then(|client| crazyagent::tui::run_tui(client).map_err(|e| e.to_string()))
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

fn run_engine(args: &[OsString]) -> Result<(), String> {
    let (writable, codex, claude, rcli) = engine_options_with_claude(args)?;
    let runtime_dir = Path::new(&args[1]);
    let socket_name = args[4].to_str().ok_or("invalid socket name")?;
    crazyagent::protocol::SessionId::new(socket_name).map_err(|e| e.to_string())?;
    assert!((5..=12).contains(&args.len()));
    assert!(!socket_name.contains('/'));
    let config = if writable {
        None
    } else {
        Some(
            crazyagent::config::resolve_config(Path::new(&args[2]), Path::new(&args[3]))
                .map_err(|e| e.to_string())?,
        )
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    runtime.block_on(async {
        let mut interrupt =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
                .map_err(|e| e.to_string())?;
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .map_err(|e| e.to_string())?;
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let signal = tokio::spawn(async move {
            tokio::select! {
                _ = interrupt.recv() => {},
                _ = terminate.recv() => {},
            }
            stop.send_replace(true);
        });
        let result = async {
            let mut service = if writable {
                crazyagent::engine::bootstrap::EngineService::start_with_settings_sources(
                    runtime_dir,
                    socket_name,
                    Path::new(&args[2]),
                    Path::new(&args[3]),
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
                .map_err(|e| e.to_string())
                .and_then(|executable| {
                    service
                        .register_native_auth_host(&executable)
                        .map_err(|e| e.to_string())
                })
                .and_then(|()| {
                    if let Some(executable) = codex.as_deref() {
                        service
                            .register_codex_executable(executable)
                            .map_err(|e| e.to_string())?;
                    }
                    if let Some(executable) = claude.as_deref() {
                        service
                            .register_claude_executable(executable)
                            .map_err(|e| e.to_string())?;
                    }
                    if let Some(executable) = rcli.as_deref() {
                        service
                            .register_rcli_executable(executable)
                            .map_err(|e| e.to_string())?;
                    }
                    Ok(())
                })
                .and_then(|()| service.ensure_ready(&stopped).map_err(|e| e.to_string()))
                .and_then(|()| match codex.as_deref().filter(|_| writable) {
                    None => Ok(None),
                    Some(executable) => crazyagent::research::service::start_for_engine(
                        runtime_dir,
                        Path::new(&args[2]),
                        Path::new(&args[3]),
                        executable,
                        claude.as_deref(),
                    )
                    .map(Some),
                })
                .and_then(|research| {
                    let mut executables = std::collections::BTreeMap::new();
                    if let Some(executable) = codex.as_deref() {
                        executables.insert("codex", executable.to_path_buf());
                    }
                    if let Some(executable) = claude.as_deref() {
                        executables.insert("claude-code", executable.to_path_buf());
                    }
                    if !writable || executables.is_empty() {
                        return Ok((research, None));
                    }
                    match crazyagent::team::runtime::start_for_engine(
                        runtime_dir,
                        Path::new(&args[2]),
                        Path::new(&args[3]),
                        executables,
                    ) {
                        Ok(team) => Ok((research, Some(team))),
                        Err(error) => {
                            if let Some(research) = research {
                                research.stop();
                            }
                            Err(error)
                        }
                    }
                })
                .and_then(|(research, team)| {
                    let mut output = io::stdout().lock();
                    match writeln!(output, "ready").and_then(|()| output.flush()) {
                        Ok(()) => Ok((research, team)),
                        Err(error) => {
                            if let Some(research) = research {
                                research.stop();
                            }
                            Err(error.to_string())
                        }
                    }
                });
            let (research, team) = match ready {
                Ok(started) => started,
                Err(error) => {
                    return match service.shutdown().await {
                        Ok(()) => Err(error),
                        Err(cleanup) => Err(format!("{error}; cleanup: {cleanup}")),
                    };
                }
            };
            let result = service.run(stopped).await.map_err(|e| e.to_string());
            if let Some(research) = research {
                let _ = tokio::task::spawn_blocking(move || research.stop()).await;
            }
            if let Some(team) = team {
                let _ = tokio::task::spawn_blocking(move || team.stop()).await;
            }
            result
        }
        .await;
        signal.abort();
        let _ = signal.await;
        result
    })
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
