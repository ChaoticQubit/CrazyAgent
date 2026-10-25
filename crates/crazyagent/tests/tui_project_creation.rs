#![cfg(unix)]

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

struct Driver {
    child: Child,
    stdout: PathBuf,
    stderr: PathBuf,
    _root: tempfile::TempDir,
}

impl Driver {
    fn start() -> Self {
        let root = tempfile::tempdir().unwrap();
        let stdout = root.path().join("stdout");
        let stderr = root.path().join("stderr");
        let child = crazyagent::spawn_gate::spawn(
            Command::new("python3")
                .env_remove("PYTHONOPTIMIZE")
                .env("PYTHONDONTWRITEBYTECODE", "1")
                .arg(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../scripts/qualification/tui_project_creation.py"
                ))
                .arg(env!("CARGO_BIN_EXE_crazyagent"))
                .stdin(Stdio::null())
                .stdout(File::create(&stdout).unwrap())
                .stderr(File::create(&stderr).unwrap()),
        )
        .unwrap();
        assert!(child.id() > 0);
        assert_ne!(stdout, stderr);
        eprintln!("owned creation driver started pid={}", child.id());
        Self {
            child,
            stdout,
            stderr,
            _root: root,
        }
    }

    fn wait(&mut self) -> Result<ExitStatus, String> {
        let deadline = Instant::now() + Duration::from_secs(35);
        loop {
            for path in [&self.stdout, &self.stderr] {
                let bytes = std::fs::metadata(path)
                    .map_err(|error| error.to_string())?
                    .len();
                if bytes > 1048576 {
                    return Err(format!("creation driver stream exceeded one MiB: {path:?}"));
                }
            }
            if let Some(status) = self.child.try_wait().map_err(|error| error.to_string())? {
                return Ok(status);
            }
            if Instant::now() >= deadline {
                return Err("creation driver exceeded its 35-second deadline".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn diagnostics(&self) -> String {
        let tail = |path: &PathBuf| {
            let mut file = File::open(path).unwrap();
            let bytes = file.metadata().unwrap().len();
            file.seek(SeekFrom::Start(bytes.saturating_sub(65536)))
                .unwrap();
            let mut output = Vec::new();
            file.take(65536).read_to_end(&mut output).unwrap();
            String::from_utf8_lossy(&output).into_owned()
        };
        format!(
            "pid={}\nstdout: {}\nstderr: {}",
            self.child.id(),
            tail(&self.stdout),
            tail(&self.stderr)
        )
    }
}

impl Drop for Driver {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = crazyagent::spawn_gate::status(
                Command::new("/bin/kill")
                    .args(["-s", "TERM", &self.child.id().to_string()])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null()),
            );
            let deadline = Instant::now() + Duration::from_secs(10);
            while matches!(self.child.try_wait(), Ok(None)) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if matches!(self.child.try_wait(), Ok(None)) {
                eprintln!("creation driver forced cleanup pid={}", self.child.id());
                let _ = self.child.kill();
            }
        }
        let status = self.child.wait();
        eprintln!(
            "owned creation driver reaped pid={} status={status:?}",
            self.child.id()
        );
    }
}

#[test]
fn unseeded_tui_declares_exact_project_persists_first_brief_and_reopens() {
    let mut driver = Driver::start();
    let status = driver.wait();
    let diagnostics = driver.diagnostics();
    assert!(status.is_ok(), "{status:?}\n{diagnostics}");
    assert!(status.unwrap().success(), "{diagnostics}");
    eprintln!("{diagnostics}");
}
