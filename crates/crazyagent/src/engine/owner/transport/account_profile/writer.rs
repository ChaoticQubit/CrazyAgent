use super::*;
use crate::engine::native_auth_host::RegisteredNativeAuthHost;
use crate::storage::account_profile::Work;
use rustix::fs::{Mode, OFlags, fcntl_getfl, fcntl_setfl};
use rustix::process::{Pid, Signal};
use std::os::fd::OwnedFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::Instant as WallInstant;

struct Held {
    file: File,
    path: PathBuf,
    device: u64,
    inode: u64,
    directory: bool,
}

impl Held {
    fn open(path: PathBuf, directory: bool) -> Result<Self, EngineError> {
        let file = File::from(
            rustix::fs::open(
                &path,
                OFlags::RDONLY
                    | OFlags::NOFOLLOW
                    | OFlags::CLOEXEC
                    | OFlags::NONBLOCK
                    | if directory {
                        OFlags::DIRECTORY
                    } else {
                        OFlags::empty()
                    },
                Mode::empty(),
            )
            .map_err(|_| unknown())?,
        );
        let metadata = file.metadata().map_err(|_| unknown())?;
        let held = Self {
            file,
            path,
            device: metadata.dev(),
            inode: metadata.ino(),
            directory,
        };
        held.verify()?;
        Ok(held)
    }

    fn verify(&self) -> Result<(), EngineError> {
        let current = std::fs::symlink_metadata(&self.path).map_err(|_| unknown())?;
        let retained = self.file.metadata().map_err(|_| unknown())?;
        for metadata in [&current, &retained] {
            if metadata.dev() != self.device
                || metadata.ino() != self.inode
                || metadata.uid() != rustix::process::getuid().as_raw()
                || metadata.mode() & 0o7777 != if self.directory { 0o700 } else { 0o600 }
                || (self.directory && !metadata.is_dir())
                || (!self.directory && (!metadata.is_file() || metadata.nlink() != 1))
            {
                return Err(unknown());
            }
        }
        Ok(())
    }
}

struct Supervised {
    work: Option<Box<Work>>,
    child: Option<Child>,
    group: Option<Pid>,
    settled: bool,
}

impl Supervised {
    fn cleanup(&mut self) -> bool {
        let Some(child) = &mut self.child else {
            self.settled = true;
            return true;
        };
        let Some(group) = self.group else {
            return false;
        };
        let deadline = WallInstant::now() + Duration::from_secs(2);
        while WallInstant::now() < deadline {
            let reaped = child.try_wait().ok().flatten().is_some();
            match rustix::process::test_kill_process_group(group) {
                Err(rustix::io::Errno::SRCH) if reaped => {
                    self.settled = true;
                    return true;
                }
                Err(rustix::io::Errno::SRCH | rustix::io::Errno::PERM) => {}
                Ok(()) => {
                    let _ = rustix::process::kill_process_group(group, Signal::KILL);
                }
                Err(_) => return false,
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }
}

impl Drop for Supervised {
    fn drop(&mut self) {
        if !self.settled
            && !self.cleanup()
            && let Some(work) = self.work.take()
        {
            std::mem::forget(work);
        }
    }
}

pub(super) async fn run(
    runtime: &OwnerRuntimeHandle,
    work: Box<Work>,
) -> Result<(Box<Work>, bool), EngineError> {
    let executable = match runtime.registered_rcli_private_key_executable() {
        Ok(executable) => executable,
        Err(_) => return Ok((work, false)),
    };
    tokio::task::spawn_blocking(move || {
        let mut process = Supervised {
            work: Some(work),
            child: None,
            group: None,
            settled: false,
        };
        let result = perform(&mut process, &executable).unwrap_or(false);
        if !process.cleanup() {
            return Err(unknown());
        }
        Ok((
            process.work.take().expect("settled writer retains custody"),
            result,
        ))
    })
    .await
    .map_err(|_| unknown())?
}

fn perform(
    process: &mut Supervised,
    executable: &RegisteredNativeAuthHost,
) -> Result<bool, EngineError> {
    let work = process.work.as_mut().ok_or_else(unknown)?;
    let selection = crate::rcli_host::configured_sdk_connection(&work.receipt.connection)
        .map_err(|_| unknown())?;
    let home = work.guard.profile_path()?;
    let held_home = Held::open(home.clone(), true)?;
    let held_sdk = Held::open(home.join(".backboard"), true)?;
    let held_config = Held::open(home.join(".backboard/config.json"), false)?;
    let config = bounded_file(&held_config.file, 65536)?;
    use sha2::{Digest, Sha256};
    if format!("{:x}", Sha256::digest(config)) != work.receipt.profile_config_sha256 {
        return Err(unknown());
    }
    let metadata = serde_json::json!({
        "protocol_version": 1, "request_id": work.request.request_id.as_str(),
        "generation": work.receipt.profile_generation, "provider": selection.provider.as_str(),
        "home_dir": home, "home_device": held_home.device.to_string(), "home_inode": held_home.inode.to_string(),
        "backboard_device": held_sdk.device.to_string(), "backboard_inode": held_sdk.inode.to_string(),
        "config_device": held_config.device.to_string(), "config_inode": held_config.inode.to_string(),
        "config_sha256": work.receipt.profile_config_sha256,
    });
    let metadata = serde_json::to_string(&metadata).map_err(|_| unknown())?;
    if metadata.len() > 8192 || work.receipt.profile_generation > 9_007_199_254_740_991 {
        return Err(unknown());
    }
    let mut secret = work.secret.take().ok_or_else(unknown)?.into_bytes();
    let path = executable.verified_path()?;
    let _digest = executable.verified_sha256()?;
    let mut command = Command::new(path);
    command
        .args(["--private-key-write", &metadata])
        .env_clear()
        .env("HOME", &home)
        .env("TMPDIR", &home)
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .current_dir(&home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    assert_eq!(command.get_current_dir(), Some(home.as_path()));
    let child = command.spawn().map_err(|_| unknown())?;
    let group = Pid::from_raw(i32::try_from(child.id()).expect("OS child PID range"))
        .expect("positive child PID");
    process.group = Some(group);
    process.child = Some(child);
    let child = process.child.as_mut().ok_or_else(unknown)?;
    let mut input = Some(OwnedFd::from(child.stdin.take().ok_or_else(unknown)?));
    let output = OwnedFd::from(child.stdout.take().ok_or_else(unknown)?);
    let errors = OwnedFd::from(child.stderr.take().ok_or_else(unknown)?);
    for descriptor in [input.as_ref().ok_or_else(unknown)?, &output, &errors] {
        let flags = fcntl_getfl(descriptor).map_err(|_| unknown())?;
        fcntl_setfl(descriptor, flags | OFlags::NONBLOCK).map_err(|_| unknown())?;
    }
    let mut frame = Vec::with_capacity(secret.len() + 4);
    frame.extend_from_slice(&(secret.len() as u32).to_be_bytes());
    frame.extend_from_slice(&secret);
    secret.fill(0);
    let deadline = WallInstant::now() + Duration::from_secs(5);
    let mut sent = 0;
    let mut stdout = Vec::new();
    let mut stderr_bytes = 0;
    let mut stdout_eof = false;
    let mut stderr_eof = false;
    let observed = (|| {
        while WallInstant::now() < deadline {
            if let Some(descriptor) = input.as_ref() {
                match rustix::io::write(descriptor, &frame[sent..]) {
                    Ok(size) => sent += size,
                    Err(rustix::io::Errno::AGAIN | rustix::io::Errno::INTR) => {}
                    Err(_) => return Err(unknown()),
                }
                if sent == frame.len() {
                    input.take();
                }
            }
            drain(&output, &mut stdout, &mut stdout_eof, 4100)?;
            drain_errors(&errors, &mut stderr_bytes, &mut stderr_eof)?;
            if let Some(status) = child.try_wait().map_err(|_| unknown())?
                && stdout_eof
                && stderr_eof
            {
                return Ok(status.success() && sent == frame.len() && stderr_bytes == 0);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(false)
    })();
    frame.fill(0);
    drop(input);
    let okay = observed?;
    if !okay || stdout.len() < 5 {
        return Ok(false);
    }
    let size = u32::from_be_bytes(stdout[..4].try_into().map_err(|_| unknown())?) as usize;
    if size > 4096 || size + 4 != stdout.len() {
        return Ok(false);
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Ack {
        protocol_version: u32,
        request_id: RequestId,
        generation: u64,
        provider: String,
        status: String,
    }
    let ack: Ack = serde_json::from_slice(&stdout[4..]).map_err(|_| unknown())?;
    if ack.protocol_version != 1
        || ack.request_id != work.request.request_id
        || ack.generation != work.receipt.profile_generation
        || ack.provider != selection.provider.as_str()
        || ack.status != "configured_only"
    {
        return Ok(false);
    }
    for held in [&held_home, &held_sdk, &held_config] {
        held.verify()?;
    }
    work.guard.revalidate_retained()?;
    executable.verified_path()?;
    executable.verified_sha256()?;
    Ok(true)
}

fn bounded_file(file: &File, maximum: usize) -> Result<Vec<u8>, EngineError> {
    let size =
        usize::try_from(file.metadata().map_err(|_| unknown())?.len()).map_err(|_| unknown())?;
    if size == 0 || size > maximum {
        return Err(unknown());
    }
    let mut bytes = vec![0; size];
    file.read_exact_at(&mut bytes, 0).map_err(|_| unknown())?;
    Ok(bytes)
}

fn drain(
    descriptor: &OwnedFd,
    bytes: &mut Vec<u8>,
    eof: &mut bool,
    maximum: usize,
) -> Result<(), EngineError> {
    let mut buffer = [0; 1024];
    for _ in 0..8 {
        match rustix::io::read(descriptor, &mut buffer) {
            Ok(0) => {
                *eof = true;
                return Ok(());
            }
            Ok(size) => {
                if size > maximum.saturating_sub(bytes.len()) {
                    return Err(unknown());
                }
                bytes.extend_from_slice(&buffer[..size]);
            }
            Err(rustix::io::Errno::AGAIN) => return Ok(()),
            Err(rustix::io::Errno::INTR) => {}
            Err(_) => return Err(unknown()),
        }
    }
    Ok(())
}

fn drain_errors(
    descriptor: &OwnedFd,
    count: &mut usize,
    eof: &mut bool,
) -> Result<(), EngineError> {
    let mut bytes = Vec::new();
    drain(
        descriptor,
        &mut bytes,
        eof,
        4096usize.saturating_sub(*count),
    )?;
    *count += bytes.len();
    bytes.fill(0);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exited_unreaped_writer_group_settles_cleanly() {
        let child = Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .process_group(0)
            .spawn()
            .unwrap();
        let group = Pid::from_raw(i32::try_from(child.id()).unwrap()).unwrap();
        let deadline = WallInstant::now() + Duration::from_secs(5);
        while rustix::process::test_kill_process_group(group) != Err(rustix::io::Errno::PERM) {
            assert!(
                WallInstant::now() < deadline,
                "writer did not become an exited unreaped group"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut supervised = Supervised {
            work: None,
            child: Some(child),
            group: Some(group),
            settled: false,
        };
        assert!(supervised.cleanup());
        assert!(supervised.settled);
    }
}
