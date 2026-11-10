use std::fs;
use std::path::Path;
use std::process::Command;

pub(crate) const FORWARD_REQUEST: &str = "forward-request";
const FORWARD_ANSWER: &str = "forward-answer";
const ENGINE_STEP_TIME: std::time::Duration = std::time::Duration::from_secs(180);
pub(crate) const FORWARDER_VARIABLES: [&str; 3] = [
    "CRAZYAGENT_IDE_GIT_NODE",
    "CRAZYAGENT_IDE_GIT_FORWARD",
    "CRAZYAGENT_IDE_GIT_SECRET",
];

/// Stands in for the extension's forwarder, the socket hop the live rows cover: writes a
/// script in the server's data folder that names its slot and waits for the engine's code,
/// and returns the variables the shim reads. Input: the server's data folder. Output: the
/// variables.
pub(crate) fn forwarder_env(home: &Path) -> [(&'static str, std::ffi::OsString); 3] {
    let script = home.join("forward.sh");
    let (request, answer) = (home.join(FORWARD_REQUEST), home.join(FORWARD_ANSWER));
    let (request, answer) = (request.display(), answer.display());
    fs::write(
        &script,
        format!(
            "printf %s \"$2\" > '{request}.tmp'\n/bin/mv '{request}.tmp' '{request}'\nwhile [ ! -e '{answer}' ]; do /bin/sleep 0.02; done\ncode=$(/bin/cat '{answer}')\n/bin/rm -f '{answer}'\nexit \"$code\"\n"
        ),
    )
    .unwrap();
    [
        (FORWARDER_VARIABLES[0], "/bin/sh".into()),
        (FORWARDER_VARIABLES[1], script.into_os_string()),
        (FORWARDER_VARIABLES[2], "row-secret".into()),
    ]
}

/// Runs a git under the installed profile while playing the engine: each slot the stand-in
/// forwarder names goes through the real engine step outside the sandbox. Inputs: the
/// command, the server's data folder, in the engine's folder, and the project folder.
/// Output: the process's output.
pub(crate) fn output_with_engine(
    command: &mut Command,
    home: &Path,
    root: &Path,
) -> std::process::Output {
    let mut child = crate::spawn_gate::spawn(command).unwrap();
    let (request, answer) = (home.join(FORWARD_REQUEST), home.join(FORWARD_ANSWER));
    let private = home.join(crate::ide::git::PRIVATE_INDEX);
    let deadline = std::time::Instant::now() + ENGINE_STEP_TIME;
    while child.try_wait().unwrap().is_none() {
        assert!(std::time::Instant::now() < deadline, "git never ended");
        if let Ok(slot) = fs::read_to_string(&request) {
            fs::remove_file(&request).unwrap();
            let code = super::carry_index(&private, home.parent().unwrap(), root, &slot);
            fs::write(&answer, code.to_string()).unwrap();
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    child.wait_with_output().unwrap()
}
