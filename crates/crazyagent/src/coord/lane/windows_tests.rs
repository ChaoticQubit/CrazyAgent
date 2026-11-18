use super::{NOT_STARTED, run};
use std::ffi::OsString;
use std::path::{Component, PathBuf};

/// A copy of the system `cmd.exe` named `probe.exe` in a new folder at the current drive's
/// root, where any local user may make one. Output: the folder, removed when dropped.
fn planted() -> tempfile::TempDir {
    let root = std::path::absolute("\\").unwrap();
    let folder = tempfile::Builder::new()
        .prefix("crazyagent-lane-")
        .tempdir_in(&root)
        .unwrap();
    let shell = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join("System32")
        .join("cmd.exe");
    std::fs::copy(shell, folder.path().join("probe.exe")).unwrap();
    folder
}

/// The planted probe named by `..` steps from the current folder up to the drive root.
/// Input: the planted folder. Output: that relative name.
fn up_to(folder: &tempfile::TempDir) -> PathBuf {
    let here = std::env::current_dir().unwrap();
    let mut relative = PathBuf::new();
    for _ in here
        .components()
        .filter(|part| matches!(part, Component::Normal(_)))
    {
        relative.push("..");
    }
    relative.push(folder.path().file_name().unwrap());
    relative.push("probe.exe");
    assert!(!relative.is_absolute(), "{}", relative.display());
    relative
}

/// Runs `program` through the Windows lane with arguments that make the probe exit 7.
/// Input: the program. Output: the lane's code and what it wrote.
fn lane(program: OsString) -> (i32, String) {
    let arguments: Vec<OsString> = vec![
        "--".into(),
        program,
        "/d".into(),
        "/c".into(),
        "exit".into(),
        "7".into(),
    ];
    let mut out = Vec::new();
    let code = run(&arguments, &mut out);
    (code, String::from_utf8_lossy(&out).into_owned())
}

/// An agent's program named relative to its current folder still runs through the Windows
/// lane, which makes it absolute before the spawn gate refuses relative programs. The program
/// here is the planted probe, reached by `..` steps from the current folder up to the drive
/// root (V244-WIN-ROOT-RELATIVE-PROGRAM).
#[test]
fn the_windows_lane_still_runs_a_program_named_relative_to_its_folder() {
    let folder = planted();
    let (code, out) = lane(up_to(&folder).into_os_string());
    assert_eq!(code, 7, "{out}");
}

/// A program named from the drive root, or from a drive's current folder, is refused by the
/// Windows lane rather than made absolute against a drive another local user may have planted
/// it on; the planted probe would exit 7 (rv-code2 #537 F1).
#[test]
fn the_windows_lane_refuses_a_program_named_from_a_drive_root_or_a_drive_s_folder() {
    let folder = planted();
    let rooted = PathBuf::from("\\")
        .join(folder.path().file_name().unwrap())
        .join("probe.exe");
    let here = std::env::current_dir().unwrap();
    let Some(Component::Prefix(drive)) = here.components().next() else {
        panic!("the current folder has no drive: {}", here.display());
    };
    let mut on_drive = drive.as_os_str().to_owned();
    on_drive.push(up_to(&folder));
    for program in [rooted.into_os_string(), on_drive] {
        let (code, out) = lane(program.clone());
        assert_eq!(code, NOT_STARTED, "{program:?}: {out}");
        assert!(
            out.contains("spawn_relative_program_refused"),
            "{program:?}: {out}"
        );
    }
}
