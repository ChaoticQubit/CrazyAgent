use std::path::{Path, PathBuf};
use std::process::Command;

const PROBE: &str = "CRAZYAGENT_LOW_PROBE";

/// The low mandatory level's SID, which `whoami /groups` prints for a low integrity token.
pub(crate) const LOW_LEVEL_SID: &str = "S-1-16-4096";

/// What the low probe tries on one path: read its bytes, or create or overwrite it.
#[derive(Clone, Copy)]
pub(crate) enum Try {
    Read,
    Write,
}

/// A copy of this test binary whose file carries a low mandatory label, so a process started
/// from it runs at low integrity, the lower of its parent's level and its file's.
/// Input: the folder to copy into. Output: the copy's path.
pub(crate) fn low_probe(folder: &Path) -> PathBuf {
    let probe = folder.join("low-probe.exe");
    std::fs::copy(std::env::current_exe().unwrap(), &probe).unwrap();
    let status = kernel_processes::spawn_gate::status(
        Command::new("icacls")
            .args([probe.to_str().unwrap(), "/setintegritylevel", "low"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null()),
    )
    .unwrap();
    assert!(status.success());
    probe
}

/// Runs the low probe on each path and returns what it printed: the groups of its own token,
/// then per path `READ`, `DENIED`, `WROTE` or `REFUSED` and the path, never the bytes read.
/// Inputs: the probe and the tries. Output: the printed text.
pub(crate) fn probe(probe: &Path, tries: &[(Try, &Path)]) -> String {
    assert!(!tries.is_empty());
    let test = format!("{}::child", module_path!().split_once("::").unwrap().1);
    let line: Vec<String> = tries
        .iter()
        .map(|(what, path)| {
            let verb = match what {
                Try::Read => "read",
                Try::Write => "write",
            };
            format!("{verb}|{}", path.to_str().unwrap())
        })
        .collect();
    let mut command = Command::new(probe);
    command
        .args([
            test.as_str(),
            "--exact",
            "--nocapture",
            "--test-threads",
            "1",
        ])
        .env(PROBE, line.join("\n"));
    let output = kernel_processes::spawn_gate::collected(&mut command).unwrap();
    assert!(output.status.success(), "{output:?}");
    let seen = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        seen.contains(LOW_LEVEL_SID),
        "the probe runs at low: {seen}"
    );
    seen
}

/// The low probe's body: runs only with `CRAZYAGENT_LOW_PROBE` set, prints the groups of its
/// own token from whoami, then makes each try and prints its outcome.
#[test]
fn child() {
    let Ok(line) = std::env::var(PROBE) else {
        return;
    };
    let whoami = PathBuf::from(std::env::var("SystemRoot").unwrap())
        .join("System32")
        .join("whoami.exe");
    let groups =
        kernel_processes::spawn_gate::collected(Command::new(whoami).arg("/groups")).unwrap();
    println!("GROUPS {}", String::from_utf8_lossy(&groups.stdout));
    for item in line.lines() {
        match item.split_once('|') {
            Some(("read", path)) => match std::fs::read(path) {
                Ok(_) => println!("READ {path}"),
                Err(problem) => println!("DENIED {path} {problem}"),
            },
            Some(("write", path)) => match std::fs::write(path, "low") {
                Ok(()) => println!("WROTE {path}"),
                Err(problem) => println!("REFUSED {path} {problem}"),
            },
            _ => panic!("unknown probe try {item}"),
        }
    }
}
