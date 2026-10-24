#![forbid(unsafe_code)]

#[cfg(unix)]
use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};
#[cfg(unix)]
use std::io::{self, Write};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(unix)]
use std::process::Command;
use std::process::ExitCode;
#[cfg(unix)]
use std::time::{Duration, Instant};

#[cfg(unix)]
fn main() -> ExitCode {
    match execute() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("resource probe: {error}");
            ExitCode::from(2)
        }
    }
}

/// Refuses every resource probe role where Unix resource limits do not exist.
/// Takes no input. Reports `resource_limits_unavailable` on stderr and returns exit code 2,
/// the same code as a failed probe on Unix.
#[cfg(not(unix))]
fn main() -> ExitCode {
    eprintln!("resource probe: resource_limits_unavailable");
    ExitCode::from(2)
}

#[cfg(unix)]
fn execute() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).take(4).collect();
    let core_limit = Rlimit {
        current: Some(0),
        maximum: Some(0),
    };
    setrlimit(Resource::Core, core_limit)?;
    match args.as_slice() {
        [role] if role == "limit-cpu" => limit_and_exec(Resource::Cpu, 2),
        [role] if role == "limit-as" => limit_and_exec(Resource::As, 64 * 1024 * 1024),
        [role] if role == "limit-as-small" => limit_and_exec(Resource::As, 16 * 1024 * 1024),
        [role] if role == "limit-rss" => limit_and_exec(Resource::Rss, 64 * 1024 * 1024),
        [role] if role == "limit-nproc" => limit_and_exec(Resource::Nproc, 4),
        [role] if role == "memory-baseline" => allocate(),
        [role] if role == "process-baseline" => spawn_children(),
        [role, resource] if role == "inherited" => {
            let resource = match resource.as_str() {
                "Cpu" => Resource::Cpu,
                "As" => Resource::As,
                "Rss" => Resource::Rss,
                "Nproc" => Resource::Nproc,
                _ => return Err("unknown inherited resource".into()),
            };
            let limit = getrlimit(resource);
            println!(
                "inherited {resource:?} soft={:?} hard={:?}",
                limit.current, limit.maximum
            );
            io::stdout().flush()?;
            match resource {
                Resource::Cpu => {
                    burn_cpu();
                    Ok(())
                }
                value if value == Resource::As || value == Resource::Rss => allocate(),
                Resource::Nproc => spawn_children(),
                _ => unreachable!(),
            }
        }
        [role] if role == "sleep-child" => {
            std::thread::sleep(Duration::from_millis(100));
            Ok(())
        }
        _ => Err("expected a fixed bounded resource probe role".into()),
    }
}

#[cfg(unix)]
fn limit_and_exec(resource: Resource, limit: u64) -> Result<(), Box<dyn std::error::Error>> {
    let result = setrlimit(
        resource,
        Rlimit {
            current: Some(limit),
            maximum: Some(limit),
        },
    );
    println!("set-limit {resource:?} requested={limit} result={result:?}");
    io::stdout().flush()?;
    result?;
    let error = Command::new(std::env::current_exe()?)
        .args(["inherited", &format!("{resource:?}")])
        .exec();
    Err(error.into())
}

#[cfg(unix)]
fn allocate() -> Result<(), Box<dyn std::error::Error>> {
    let size = 24 * 1024 * 1024;
    let mut allocation: Vec<u8> = Vec::new();
    match allocation.try_reserve_exact(size) {
        Ok(()) => {
            allocation.resize(size, 0);
            for (index, value) in allocation.iter_mut().enumerate() {
                *value = (index % 251) as u8;
            }
            let checksum: u64 = allocation.iter().map(|value| u64::from(*value)).sum();
            std::hint::black_box(&allocation);
            println!("allocated-and-touched={size} checksum={checksum}");
        }
        Err(error) => println!("allocation-denied size={size} error={error}"),
    }
    Ok(())
}

#[cfg(unix)]
fn burn_cpu() {
    let started = Instant::now();
    let mut work = 1_u64;
    while started.elapsed() < Duration::from_secs(4) {
        work = std::hint::black_box(work.wrapping_mul(3).wrapping_add(1));
    }
    println!("cpu-workload-completed={work}");
}

#[cfg(unix)]
fn spawn_children() -> Result<(), Box<dyn std::error::Error>> {
    let mut children = Vec::new();
    for index in 0..6 {
        match Command::new(std::env::current_exe()?)
            .arg("sleep-child")
            .spawn()
        {
            Ok(child) => children.push(child),
            Err(error) => println!("spawn-denied index={index} error={error}"),
        }
    }
    println!("children-started={}", children.len());
    for mut child in children {
        let status = child.wait()?;
        if !status.success() {
            return Err(format!("disposable child failed: {status}").into());
        }
    }
    Ok(())
}
