#![cfg(target_os = "macos")]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const SCRIPT: &str = "scripts/gate/clean-mac.sh";
const BUILD_FAILED: i32 = 3;
const SHAPE_FAILED: i32 = 1;
const FAKE_CARGO: &str = "#!/bin/sh\n\
case \" $* \" in\n\
\x20 *\" --no-run \"*)\n\
\x20   echo 'error: linking with `cc` failed: exit status: 1'\n\
\x20   exit \"$FAKE_BUILD_RC\" ;;\n\
esac\n\
if [ -n \"$FAKE_SHAPE_LINE\" ]; then\n\
\x20 printf '%s\\n' \"$FAKE_SHAPE_LINE\" >> \"$CRAZYAGENT_CLEAN_MAC_DIR/summary.txt\"\n\
fi\n\
exit \"$FAKE_TEST_RC\"\n";

/// What one gate script run came to: its exit code and its summary file.
struct Run {
    code: i32,
    summary: String,
}

/// Runs a copy of the gate script in a fake repo whose `cargo` stand-in answers the build
/// step and the test step with the given exit codes, and adds a shape line to the summary
/// during the test step when one is given. Inputs: the build and test exit codes and the
/// shape line. Output: the run.
fn gate(build: i32, test: i32, shape: Option<&str>) -> Run {
    let real = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    let script = repo.join(SCRIPT);
    fs::create_dir_all(script.parent().unwrap()).unwrap();
    fs::copy(real.join(SCRIPT), &script).unwrap();
    fs::create_dir_all(repo.join("crates/crazyagent")).unwrap();
    fs::write(
        repo.join("crates/crazyagent/Cargo.toml"),
        "[features]\nclean-mac-gate = []\n",
    )
    .unwrap();
    let bin = temp.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    fs::write(bin.join("cargo"), FAKE_CARGO).unwrap();
    fs::set_permissions(bin.join("cargo"), fs::Permissions::from_mode(0o755)).unwrap();
    let gate: PathBuf = temp.path().join("gate");
    let status = crazyagent::spawn_gate::status(
        Command::new("/bin/bash")
            .arg(&script)
            .env_clear()
            .env("HOME", temp.path())
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("CRAZYAGENT_CLEAN_MAC_DIR", &gate)
            .env("FAKE_BUILD_RC", build.to_string())
            .env("FAKE_TEST_RC", test.to_string())
            .env("FAKE_SHAPE_LINE", shape.unwrap_or(""))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .unwrap();
    Run {
        code: status.code().expect("the gate script exits with a code"),
        summary: fs::read_to_string(gate.join("summary.txt")).unwrap_or_default(),
    }
}

/// A build that fails with no `error[E` line, such as a link failure or a refused lockfile,
/// is a failed build (exit 3), never a failed shape, judged by the build step's exit code.
#[test]
fn a_build_that_fails_without_a_compile_error_line_exits_as_a_failed_build() {
    let run = gate(101, 101, None);
    assert_eq!(run.code, BUILD_FAILED, "{}", run.summary);
    assert!(run.summary.contains("BUILD FAILED"), "{}", run.summary);
}

/// A test run that ends with no shape line, such as a test binary killed before it starts,
/// ran no shape, so it exits as a failed build (exit 3), never as a failed shape.
#[test]
fn a_test_run_that_ran_no_shape_exits_as_a_failed_build() {
    let run = gate(0, 101, None);
    assert_eq!(run.code, BUILD_FAILED, "{}", run.summary);
    assert!(run.summary.contains("NO SHAPE RAN"), "{}", run.summary);
}

/// A shape that ran and failed still exits 1 once the build passed.
#[test]
fn a_shape_that_failed_after_a_good_build_exits_as_a_failed_shape() {
    let run = gate(
        0,
        101,
        Some("a-nothing-installed: FAIL during live master start"),
    );
    assert_eq!(run.code, SHAPE_FAILED, "{}", run.summary);
    assert!(
        run.summary.contains("a-nothing-installed: FAIL"),
        "{}",
        run.summary
    );
}
