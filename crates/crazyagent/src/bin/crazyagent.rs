#![forbid(unsafe_code)]

use crazyagent::qualification::{
    AdmissionContext, Capability, Outcome, QualificationReport, admit, load_report,
};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;

const ARGUMENTS_MAX: usize = 32;
const USAGE: &str = "crazyagent qualification report --source-revision REV --profile-id PROFILE\n\
crazyagent engine RUNTIME_DIR GLOBAL_TOML PROJECT_TOML SOCKET_NAME [--writable-settings]\n\
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

fn run_engine(args: &[OsString]) -> Result<(), String> {
    let writable = args.len() == 6 && args[5] == "--writable-settings";
    if args.len() != 5 && !writable {
        return Err(
            "usage: crazyagent engine RUNTIME_DIR GLOBAL_TOML PROJECT_TOML SOCKET_NAME [--writable-settings]".into(),
        );
    }
    let runtime_dir = Path::new(&args[1]);
    let socket_name = args[4].to_str().ok_or("invalid socket name")?;
    crazyagent::protocol::SessionId::new(socket_name).map_err(|e| e.to_string())?;
    assert!((5..=6).contains(&args.len()));
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
            let ready = service
                .ensure_ready(&stopped)
                .map_err(|e| e.to_string())
                .and_then(|()| {
                    let mut output = io::stdout().lock();
                    writeln!(output, "ready")
                        .and_then(|()| output.flush())
                        .map_err(|e| e.to_string())
                });
            if let Err(error) = ready {
                return match service.shutdown().await {
                    Ok(()) => Err(error),
                    Err(cleanup) => Err(format!("{error}; cleanup: {cleanup}")),
                };
            }
            service.run(stopped).await.map_err(|e| e.to_string())
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
