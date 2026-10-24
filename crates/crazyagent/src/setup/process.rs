use super::{InstallError, Version};
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::io::Read;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const OUTPUT_BYTES_MAX: usize = 64 * 1024;
const POLL: Duration = Duration::from_millis(50);

/// Runs one program with only the given environment, no input and a time bound, then
/// stops its whole process group. Inputs: the absolute program, its arguments, the
/// environment pairs, the working folder and the bound. Output: whether it exited with
/// success and at most `OUTPUT_BYTES_MAX` bytes of its standard output, or `setup_timeout`.
pub fn run(
    program: &Path,
    args: &[OsString],
    env: &[(&str, OsString)],
    cwd: &Path,
    limit: Duration,
) -> Result<(bool, Vec<u8>), InstallError> {
    assert!(program.is_absolute());
    let mut command = Command::new(program);
    command
        .args(args)
        .env_clear()
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    for (name, value) in env {
        command.env(name, value);
    }
    let mut child =
        crate::spawn_gate::spawn(&mut command).map_err(|_| InstallError("setup_install_failed"))?;
    let pid = child.id();
    let stdout = child.stdout.take();
    let reader = std::thread::Builder::new()
        .name("crazyagent-setup-output".into())
        .spawn(move || {
            let mut kept = Vec::new();
            if let Some(stdout) = stdout {
                let mut limited = stdout.take(OUTPUT_BYTES_MAX as u64);
                let _ = limited.read_to_end(&mut kept);
                let _ = std::io::copy(&mut limited.into_inner(), &mut std::io::sink());
            }
            kept
        });
    let deadline = Instant::now() + limit;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if Instant::now() >= deadline => break Err(InstallError("setup_timeout")),
            Ok(None) => std::thread::sleep(POLL),
            Err(_) => break Err(InstallError("setup_install_failed")),
        }
    };
    stop(pid);
    let _ = child.kill();
    let _ = child.wait();
    let output = match reader {
        Ok(reader) => reader.join().unwrap_or_default(),
        Err(_) => Vec::new(),
    };
    assert!(output.len() <= OUTPUT_BYTES_MAX);
    Ok((status?.success(), output))
}

fn stop(pid: u32) {
    if let Some(pid) = i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    {
        assert!(pid.as_raw_nonzero().get() > 1);
        let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
    }
}

/// Reads the version a program printed for `--version` or `-V`, such as `uv 0.9.5 (...)`.
/// Input: the program's output. Output: the second word of its first line as a version.
pub fn version_from(output: &[u8]) -> Result<Version, InstallError> {
    let text = std::str::from_utf8(output).map_err(|_| InstallError("setup_version_unreadable"))?;
    let line = text.lines().next().unwrap_or_default();
    let word = line
        .split_whitespace()
        .nth(1)
        .ok_or(InstallError("setup_version_unreadable"))?;
    Version::parse(word)
}

/// Returns the lowercase hex SHA-256 of one file, read as a stream.
/// Input: the file path. Output: 64 hex characters, or `setup_store_unavailable`.
pub fn file_sha256(path: &Path) -> Result<String, InstallError> {
    let mut file =
        std::fs::File::open(path).map_err(|_| InstallError("setup_store_unavailable"))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher).map_err(|_| InstallError("setup_store_unavailable"))?;
    let text: String = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert_eq!(text.len(), 64);
    Ok(text)
}

/// Tells whether a text is exactly 64 lowercase hex characters, the shape of a SHA-256.
pub fn is_sha256(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Creates one folder owned by this user with mode 0700, or checks an existing one.
/// Input: the folder path. Output: `setup_store_unavailable` when it can't be created or
/// is a link, another user's folder or open to others.
pub fn private_folder(path: &Path) -> Result<(), InstallError> {
    let unavailable = InstallError("setup_store_unavailable");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .map_err(|_| unavailable)?;
    let metadata = std::fs::symlink_metadata(path).map_err(|_| unavailable)?;
    if !metadata.is_dir()
        || metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(unavailable);
    }
    Ok(())
}

/// Makes a fresh private staging folder inside a parent, named with a random suffix.
/// Inputs: the parent folder and a name prefix. Output: the new folder path.
pub fn staging_folder(parent: &Path, prefix: &str) -> Result<PathBuf, InstallError> {
    private_folder(parent)?;
    let random =
        crate::engine::owner::random_id().map_err(|_| InstallError("setup_store_unavailable"))?;
    let path = parent.join(format!(".{prefix}-{}", &random[..24]));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&path)
        .map_err(|_| InstallError("setup_store_unavailable"))?;
    Ok(path)
}
