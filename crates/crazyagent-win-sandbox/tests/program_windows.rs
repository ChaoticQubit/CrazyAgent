#![cfg(windows)]

use crazyagent_win_sandbox::{elevated, program_trusted};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Runs one Windows tool and asserts it succeeded, naming the tool, its arguments and its
/// error output when it did not. Inputs: the tool and its arguments.
/// Output: none.
fn ran(tool: &str, args: &[&std::ffi::OsStr]) {
    let done = Command::new(tool).args(args).output().unwrap();
    assert!(
        done.status.success(),
        "{tool} {args:?}: {}",
        String::from_utf8_lossy(&done.stderr)
    );
}

/// Changes a path's access list, owner or label with icacls, asserting the change took.
/// Inputs: the path and the icacls arguments.
/// Output: none.
fn icacls(path: &Path, change: &[&str]) {
    let mut args = vec![path.as_os_str()];
    args.extend(change.iter().map(std::ffi::OsStr::new));
    ran("icacls", &args);
}

/// A fresh folder under the profile, where only this user, SYSTEM and the administrators
/// may change anything, with an empty program in it. Output: the folder guard and the
/// program path.
fn own_program() -> (tempfile::TempDir, PathBuf) {
    let folder = tempfile::tempdir_in(std::env::var_os("USERPROFILE").unwrap()).unwrap();
    let own = folder.path().join("own");
    std::fs::create_dir_all(&own).unwrap();
    let program = own.join("tool.exe");
    std::fs::write(&program, "").unwrap();
    (folder, program)
}

/// The curl Windows ships passes, as does a program in a folder only this user and the
/// system may change. A relative path, a folder and a missing file are refused, since
/// none of them is a program the engine can judge and start.
#[test]
fn the_shipped_curl_and_a_program_only_this_user_may_change_pass() {
    let shipped = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join("System32")
        .join("curl.exe");
    program_trusted(&shipped).unwrap();
    assert!(program_trusted(Path::new("curl.exe")).is_err());
    let (_folder, program) = own_program();
    program_trusted(&program).unwrap();
    assert!(program_trusted(program.parent().unwrap()).is_err());
    assert!(program_trusted(&program.with_file_name("gone.exe")).is_err());
}

/// A program another account may write, one every app container may write, one a low
/// integrity process may write through its label, and one in a folder the users group may
/// add a file to are each refused; each passes again once the grant or label is gone.
#[test]
fn a_program_another_account_or_a_lower_process_may_change_is_refused() {
    let (_folder, program) = own_program();
    let own = program.parent().unwrap().to_path_buf();
    for (path, grant) in [
        (&program, "*S-1-5-32-545:(W)"),
        (&program, "*S-1-15-2-1:(W)"),
        (&own, "*S-1-5-32-545:(AD)"),
    ] {
        icacls(path, &["/grant", grant]);
        assert!(program_trusted(&program).is_err(), "{path:?} {grant}");
        let sid = grant.split(':').next().unwrap();
        icacls(path, &["/remove:g", sid]);
        program_trusted(&program).unwrap();
    }
    icacls(&program, &["/setintegritylevel", "L"]);
    assert!(program_trusted(&program).is_err());
    icacls(&program, &["/setintegritylevel", "M"]);
    program_trusted(&program).unwrap();
}

/// A program with a second name is refused, as anyone who can reach the other name could
/// hold or change the file under it, whatever the folder of the first name allows.
/// The second name sits beside the first one here.
#[test]
fn a_program_with_a_second_name_is_refused() {
    let (_folder, program) = own_program();
    std::fs::hard_link(&program, program.with_file_name("twin.exe")).unwrap();
    assert!(program_trusted(&program).is_err());
}

/// A program reached through a junction is judged at its real place too: it passes while
/// the real folder is this user's, and is refused once the users group may add a file
/// there, though the path as written never names that folder.
#[test]
fn a_program_through_a_junction_is_judged_at_its_real_place() {
    let (folder, program) = own_program();
    let real = program.parent().unwrap().to_path_buf();
    let junction = folder.path().join("through");
    ran(
        "cmd",
        &[
            "/c".as_ref(),
            "mklink".as_ref(),
            "/J".as_ref(),
            junction.as_os_str(),
            real.as_os_str(),
        ],
    );
    let written = junction.join("tool.exe");
    program_trusted(&written).unwrap();
    icacls(&real, &["/grant", "*S-1-5-32-545:(AD)"]);
    assert!(program_trusted(&written).is_err());
}

/// The engine's token reads as elevated exactly when Windows lists the high mandatory
/// level in the process's groups, as an elevated administrator's process has, so the
/// harness install refusal follows the real token on every runner.
#[test]
fn the_token_reads_as_elevated_exactly_at_the_high_mandatory_level()
-> Result<(), Box<dyn std::error::Error>> {
    let groups = Command::new("whoami").arg("/groups").output()?;
    assert!(groups.status.success());
    let high = String::from_utf8_lossy(&groups.stdout).contains("S-1-16-12288");
    assert_eq!(elevated()?, high);
    Ok(())
}
